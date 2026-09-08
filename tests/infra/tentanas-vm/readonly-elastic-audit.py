# =============================================================================
# Plik: readonly-elastic-audit.py
# Opis: Odczytowy odbiór dwóch produkcyjnych macierzy na uzgodnionej VM E2.
# Przykład: sudo python3 - e2-xfs-two < readonly-elastic-audit.py
# =============================================================================

import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import uuid

VM_UUID = '16e0a47b-f61a-4a9c-8407-e2b1ce554d59'
GIB = 1024 ** 3
DISKS = {f'tn-16e0a47bf6-{role}': size * GIB for role, size in
         [('os', 12), ('data1', 32), ('data2', 32), ('parity', 40), ('cache', 1), ('spare', 40)]}
CASES = {'e2-xfs-two': ('xfs', ['data1'], ['parity', 'spare']),
         'e2-ext4-zero': ('ext4', ['data2'], [])}
ROOT = Path('/var/lib/tentanas/elastic')
UID = 0


def require(condition, message):
    if not condition:
        raise ValueError(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f'Powtórzony klucz JSON: {key}')
        result[key] = value
    return result


def decode_json(value):
    return json.loads(value, object_pairs_hook=unique_object)


def canonical_uuid(value):
    require(isinstance(value, str) and str(uuid.UUID(value)) == value, 'Niekanoniczny UUID')
    return value


def safe_parents(path):
    for parent in reversed(path.parents):
        info = parent.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == UID and
                not info.st_mode & 0o022, f'Obcy lub zapisywalny katalog: {parent}')


def open_read(path, mode=None):
    path = Path(path)
    safe_parents(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NOATIME)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_uid == UID,
                f'Nieprawidłowy plik: {path}')
        require(mode is None or stat.S_IMODE(info.st_mode) == mode, f'Tryb pliku: {path}')
        require(not info.st_mode & 0o022, f'Zapisywalny plik: {path}')
        return os.fdopen(fd, 'rb')
    except BaseException:
        os.close(fd)
        raise


def small_file(path, limit=128 * 1024, mode=None):
    with open_read(path, mode) as stream:
        value = stream.read(limit + 1)
    require(len(value) <= limit, f'Za duży plik: {path}')
    return value


def command(argv):
    result = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True,
                            text=True, timeout=20, check=False,
                            env={'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL': 'C'})
    return result


def parse_blkid(result):
    if result.returncode == 2 and not result.stdout and not result.stderr:
        return {}
    require(result.returncode == 0 and result.stdout and not result.stderr, 'Niepotwierdzony odczyt blkid')
    fields = {}
    for line in result.stdout.splitlines():
        key, separator, value = line.partition('=')
        require(separator and key and value and key not in fields, 'Niespójny wynik blkid')
        fields[key] = value
    return fields


def inventory(value):
    whole = []
    nodes = []

    def visit(node, parent=None):
        row = dict(node)
        row['parent'] = parent
        nodes.append(row)
        if row['type'] == 'disk':
            whole.append(row)
        for child in row.get('children') or []:
            visit(child, row['name'])

    for item in value['blockdevices']:
        visit(item)
    require(len(whole) == 6, 'VM nie ma dokładnie sześciu całych dysków')
    require({d['serial']: int(d['size']) for d in whole} == DISKS, 'Inne seriale lub rozmiary dysków VM')
    require(len({d['maj:min'] for d in whole}) == 6 and all(d['ro'] in (False, 0) for d in whole), 'Obce role/RO')
    require(len({d['name'] for d in nodes}) == len(nodes), 'Powtórzone urządzenie lsblk')
    return whole, nodes


def unescape_mount(value):
    return re.sub(r'\\([0-7]{3})', lambda match: chr(int(match[1], 8)), value)


def mount_rows(value):
    rows = []
    for line in value.splitlines():
        before, separator, after = line.partition(' - ')
        first, last = before.split(), after.split()
        require(separator and len(first) >= 6 and len(last) == 3, 'Nieprawidłowy mountinfo')
        rows.append({'major_minor': first[2], 'root': unescape_mount(first[3]),
                     'target': unescape_mount(first[4]), 'options': first[5],
                     'filesystem': last[0], 'source': unescape_mount(last[1])})
    return rows


def expected_paths(name):
    filesystem, data, parity = CASES[name]
    branch = Path('/mnt/tentanas-branches') / name
    roles = [('data', index, serial, str(branch / 'data' / f'd{index}'))
             for index, serial in enumerate(data, 1)]
    roles += [('parity', index, serial, str(branch / 'parity' / str(index)))
              for index, serial in enumerate(parity, 1)]
    config = f'/etc/tentanas/snapraid-{name}.conf'
    content = [f'/etc/tentanas/{name}-snapraid.content']
    content += [str(branch / 'parity' / str(i) / 'snapraid.content') for i in range(1, len(parity) + 1)]
    parity_files = [str(branch / 'parity' / str(i) / ('snapraid.parity' if i == 1 else 'snapraid.2-parity'))
                    for i in range(1, len(parity) + 1)]
    return filesystem, roles, config, content, parity_files


def validate_journal(journal, name, require_ready=True):
    filesystem, roles, *_ = expected_paths(name)
    spec = journal['spec']
    require(set(spec) == {'array_id', 'operation_id', 'owner', 'name', 'filesystem', 'data', 'parity'}, 'Inny kontrakt spec')
    for key in ['array_id', 'operation_id']:
        canonical_uuid(spec[key])
    canonical_uuid(journal['boot_id'])
    require(spec['name'] == name and spec['filesystem'] == filesystem, 'Inna macierz/FS')
    require(set(spec['owner']) == {'org_id', 'addon_id'} and all(isinstance(v, str) and v for v in spec['owner'].values()), 'Niepełny owner')
    require(journal['schema'] == 1, 'Inny schemat journala')
    if require_ready:
        require(journal['stage'] == 'ready' and journal['pending'] is None, 'Journal nie jest ready bez pending')
        require(journal['detail'] is None, 'Journal zawiera diagnostykę zamiast czystego wyniku')
        require(journal['formatted'] == [{role: index} for role, index, _, _ in roles], 'Inny komplet formatowań')
    require(len(spec['data']) == len(CASES[name][1]) and len(spec['parity']) == len(CASES[name][2]), 'Inna liczba ról')
    for role, index, suffix, _ in roles:
        disk = spec[role][index - 1]
        require(set(disk) == {'disk_id', 'wwn', 'serial', 'bytes', 'expected_uuid'}, 'Inny kontrakt dysku')
        require(disk['serial'] == f'tn-16e0a47bf6-{suffix}' and disk['bytes'] == DISKS[disk['serial']], 'Inny dysk roli')
        require(isinstance(disk['disk_id'], str) and disk['disk_id'], 'Brak disk_id')
        canonical_uuid(disk['expected_uuid'])
    require(len({disk['expected_uuid'] for disk in spec['data'] + spec['parity']}) == len(roles), 'Powtórzony UUID roli')
    sync = journal['sync_completed_at']
    if require_ready:
        require((isinstance(sync, str) and bool(sync)) if spec['parity'] else sync is None, 'Niespójny receipt sync')
    return spec


def capacity(path):
    value = os.statvfs(path)
    return {key: getattr(value, key) for key in ['f_frsize', 'f_blocks', 'f_bfree', 'f_bavail']}


def file_metric(path, expected_dev):
    if not os.path.lexists(path):
        return {'path': path, 'present': False}
    with open_read(path) as stream:
        before = os.fstat(stream.fileno())
        require(before.st_dev == expected_dev and before.st_size <= 40 * GIB, 'Plik poza właściwym FS lub za duży')
        digest = hashlib.sha256()
        while block := stream.read(1024 * 1024):
            digest.update(block)
        after = os.fstat(stream.fileno())
        require((before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                (after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'Plik zmienił się podczas odczytu')
    return {'path': path, 'present': True, 'bytes': before.st_size, 'sha256': digest.hexdigest(),
            'uid': before.st_uid, 'mode': oct(stat.S_IMODE(before.st_mode)),
            'device': before.st_dev, 'inode': before.st_ino, 'allocated': before.st_blocks * 512}


def config_directives(text):
    return [line.strip() for line in text.splitlines() if line.strip() and not line.lstrip().startswith('#')]


def compare(before, after):
    require(before['status'] == after['status'] == 'measured', 'Porównanie wymaga dwóch pełnych odczytów')
    require(before['case'] == after['case'] and before['boot_id'] != after['boot_id'], 'Inny przypadek lub ten sam boot')
    require(before['stable'] == after['stable'], 'Zmiana niezmiennego checkpointu po restarcie')
    return {'same_checkpoint': True, 'different_boot': True}


def audit(name, report):
    require(os.geteuid() == UID, 'Sonda wymaga root wyłącznie w uzgodnionej VM')
    require(Path('/sys/class/dmi/id/product_uuid').read_text().strip().lower() == VM_UUID, 'Inna VM')
    report['boot_id'] = canonical_uuid(Path('/proc/sys/kernel/random/boot_id').read_text().strip())
    safe_parents(ROOT / 'plik')
    require(stat.S_IMODE(ROOT.stat().st_mode) == 0o700, 'Katalog journala nie jest prywatny')
    with open_read(ROOT / '.storage.lock', 0o600) as lock:
        fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
        fd = os.open(ROOT, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_NOATIME)
        try:
            names = os.listdir(fd)
        finally:
            os.close(fd)
        require(len(names) <= 20, 'Nieoczekiwany rozmiar stanowiska E2')
        result = command(['/usr/bin/lsblk', '--json', '--bytes', '--paths', '--output',
                          'NAME,TYPE,SIZE,WWN,SERIAL,MAJ:MIN,RO,FSTYPE,UUID'])
        require(result.returncode == 0 and not result.stderr, 'Nieudany lsblk')
        whole, nodes = inventory(decode_json(result.stdout))
        report['inventory'] = [{key: d.get(key) for key in ['name', 'serial', 'wwn', 'size', 'maj:min', 'fstype', 'uuid']} for d in whole]
        mounts = mount_rows(Path('/proc/self/mountinfo').read_text())
        matches = []
        for filename in names:
            if filename.endswith('.json'):
                canonical_uuid(filename[:-5])
                raw = small_file(ROOT / filename, mode=0o600)
                journal = decode_json(raw)
                if journal['spec']['name'] == name:
                    require(journal['spec']['array_id'] == filename[:-5], 'Inna nazwa journala')
                    matches.append((journal, raw))
        require(len(matches) == 1, 'Brak jednego journala wybranego przypadku')
        journal, raw = matches[0]
        report['journal'] = {key: journal[key] for key in ['schema', 'stage', 'formatted', 'pending', 'boot_id', 'sync_completed_at']}
        report['journal']['sha256'] = hashlib.sha256(raw).hexdigest()
        spec = validate_journal(journal, name, require_ready=False)
        try:
            validate_journal(journal, name)
        except ValueError as error:
            report['violations'].append(str(error))
        report['identity'] = {key: spec[key] for key in ['array_id', 'operation_id', 'name', 'filesystem']}
        report['owner_sha256'] = hashlib.sha256(json.dumps(spec['owner'], sort_keys=True).encode()).hexdigest()
        root_dev = os.stat('/').st_dev
        root_mm = f'{os.major(root_dev)}:{os.minor(root_dev)}'
        ancestor = next(d for d in nodes if d['maj:min'] == root_mm)
        while ancestor['parent']:
            ancestor = next(d for d in nodes if d['name'] == ancestor['parent'])
        require(ancestor['serial'] == 'tn-16e0a47bf6-os', 'Root nie pochodzi z dysku OS')
        cache = next(d for d in whole if d['serial'] == 'tn-16e0a47bf6-cache')
        cache_stat = os.stat(cache['name'], follow_symlinks=False)
        require(stat.S_ISBLK(cache_stat.st_mode) and f'{os.major(cache_stat.st_rdev)}:{os.minor(cache_stat.st_rdev)}' == cache['maj:min'], 'Inny block device cache')
        require(not cache.get('children') and not any(m['major_minor'] == cache['maj:min'] for m in mounts), 'Cache użyty')
        require(not parse_blkid(command(['/usr/sbin/blkid', '-p', '-o', 'export', cache['name']])), 'Cache ma FS')
        wipe = command(['/usr/sbin/wipefs', '--no-act', '--json', cache['name']])
        require(wipe.returncode == 0 and not wipe.stderr and decode_json(wipe.stdout) == {'signatures': []}, 'Cache ma podpis lub błąd odczytu')
        filesystem, roles, config, content, parity_files = expected_paths(name)
        report['roles'] = []
        for role, index, suffix, target in roles:
            wanted = spec[role][index - 1]
            disk = next(d for d in whole if d['serial'] == wanted['serial'])
            observed_role = {'role': role, 'index': index, 'serial': wanted['serial'], 'disk_id': wanted['disk_id'],
                             'expected_uuid': wanted['expected_uuid'], 'target': target}
            report['roles'].append(observed_role)
            try:
                require(not disk.get('children') and (wanted['wwn'] or None) == (disk.get('wwn') or None), 'Inne WWN lub partycje wybranej roli')
                device = os.stat(disk['name'], follow_symlinks=False)
                require(stat.S_ISBLK(device.st_mode) and f'{os.major(device.st_rdev)}:{os.minor(device.st_rdev)}' == disk['maj:min'], 'Inny block device')
                fields = parse_blkid(command(['/usr/sbin/blkid', '-p', '-o', 'export', disk['name']]))
                observed_role['blkid'] = fields
                found = [m for m in mounts if m['target'] == target or m['major_minor'] == disk['maj:min']]
                observed_role['mounts'] = found
                require(fields.get('TYPE') == filesystem and fields.get('UUID') == wanted['expected_uuid'], 'Inny bezpośredni UUID/FS')
                require(len(found) == 1 and found[0]['target'] == target and found[0]['major_minor'] == disk['maj:min']
                        and found[0]['root'] == '/' and found[0]['filesystem'] == filesystem
                        and 'rw' in found[0]['options'].split(','), 'Brak dokładnego, pojedynczego mountu RW roli')
                require(os.stat(target).st_dev == device.st_rdev and device.st_rdev != root_dev, 'Branch nie jest właściwą FS')
                observed_role.update(uuid=fields['UUID'], mount=found[0], device=device.st_rdev, capacity=capacity(target))
            except (ValueError, OSError) as error:
                report['violations'].append(f'{role}/{index}: {error}')
        union = f'/mnt/{name}'
        union_rows = [m for m in mounts if m['target'] == union]
        options = {'branches': ':'.join(target + '=RW' for role, _, _, target in roles if role == 'data'),
                   'category.create': 'mfs', 'cache.files': 'off', 'minfreespace': '21474836480',
                   'moveonenospc': 'mfs', 'func.getattr': 'newest'}
        observed = None
        report['union'] = {'mounts': union_rows}
        try:
            require(len(union_rows) == 1 and union_rows[0]['filesystem'] == 'fuse.mergerfs', 'Brak pojedynczej unii mergerfs')
            observed = {key: os.getxattr(union + '/.mergerfs', 'user.mergerfs.' + key).decode() for key in options}
            report['union'].update(options=observed, capacity=capacity(union))
            require(observed == options, 'Inny profil opcji unii')
        except (ValueError, OSError) as error:
            report['violations'].append(str(error))
        report['files'] = []
        paths = [config] + content + parity_files
        for path in paths:
            require(all(not Path(path).is_relative_to(target) for role, _, _, target in roles if role == 'data'), 'Metadane pod branchem unii')
            parity_role = next((r for r in report['roles'] if r['role'] == 'parity' and Path(path).is_relative_to(r['target'])), None)
            try:
                report['files'].append(file_metric(path, parity_role.get('device') if parity_role else root_dev))
            except (ValueError, OSError) as error:
                report['files'].append({'path': path, 'present': None, 'error': str(error)})
                report['violations'].append(str(error))
        if spec['parity']:
            directives = config_directives(small_file(config).decode()) if report['files'][0]['present'] else None
            expected = [f'parity {parity_files[0]}', f'2-parity {parity_files[1]}']
            expected += ['content ' + path for path in content]
            expected += [f'data d1 {roles[0][3]}']
            expected += ['exclude ' + item for item in ['/lost+found/', '/tmp/', '*.unrecoverable', '.AppleDouble', '._AppleDouble', '.DS_Store']]
            expected += ['blocksize 256', 'autosave 500']
            if directives != expected:
                report['violations'].append('Brak config lub nie odpowiada produkcyjnemu generatorowi')
            report['content_copies_present'] = all(item['present'] for item in report['files'][1:4])
        else:
            if any(item['present'] is not False for item in report['files']):
                report['violations'].append('Artefakty SnapRAID lub błąd pomiaru przy zerowym parity')
        require(small_file(ROOT / f'{spec["array_id"]}.json', mode=0o600) == raw, 'Journal zmienił się podczas audytu')
        report['stable'] = {'spec_sha256': hashlib.sha256(json.dumps(spec, sort_keys=True).encode()).hexdigest(),
                            'formatted': journal['formatted'], 'sync_completed_at': journal['sync_completed_at'],
                            'roles': [{key: r.get(key) for key in ['role', 'index', 'serial', 'disk_id', 'uuid']} for r in report['roles']],
                            'files': [{key: f[key] for key in ['path', 'present', 'bytes', 'sha256'] if key in f} for f in report['files']],
                            'options': observed}
        report['status'] = 'measured' if not report['violations'] else 'refused'
        report['limits'] = ['Brak odczytu SQLite i dowodu syscalli; porównaj job/API osobno.',
                            'Receipt sync nie jest zachowanym stdout procesu ani dowodem ochrony payloadu.',
                            'Pomiar opcji nie jest testem ENOSPC; brakujące pliki po pustym sync pozostają jawne.',
                            'Dyski drugiego przypadku tylko zinwentaryzowane; cache sprawdzony jako pusty.']


def main(argv):
    require(len(argv) == 1 and argv[0] in CASES, 'Dozwolone: e2-xfs-two albo e2-ext4-zero')
    report = {'schema': 1, 'case': argv[0], 'vm_uuid': VM_UUID, 'status': 'refused', 'violations': []}
    try:
        audit(argv[0], report)
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
    print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    return 0 if report['status'] == 'measured' else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
