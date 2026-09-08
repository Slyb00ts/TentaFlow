# =============================================================================
# Plik: readonly-elastic-audit.py
# Opis: Odczytowy odbiór dwóch produkcyjnych macierzy na uzgodnionej VM E2.
# Przykład: sudo python3 - e2-xfs-two KONTRAKT SHA256 < readonly-elastic-audit.py
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

VM_UUID = '561fce56-b0f7-43da-9efe-1a690798337e'
GIB = 1024 ** 3
DISK_SIZES = {'os': 12, 'data1': 32, 'data2': 32, 'parity': 40, 'cache': 1, 'spare': 40}
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
    require(isinstance(value, str) and str(uuid.UUID(value)) == value and uuid.UUID(value).int != 0, 'Niekanoniczny UUID')
    return value


def validate_spec(spec, name, contract):
    filesystem, data, parity = CASES[name]
    vm = contract['vm']
    require(isinstance(spec, dict) and set(spec) ==
            {'array_id', 'operation_id', 'owner', 'name', 'filesystem', 'data', 'parity'}, 'Inny spec kontraktu')
    require(canonical_uuid(spec['array_id']) != canonical_uuid(spec['operation_id']), 'Powtórzone ID spec')
    require(spec['name'] == name and spec['filesystem'] == filesystem, 'Obcy spec przypadku')
    require(set(spec['owner']) == {'org_id', 'addon_id'} and all(isinstance(v, str) and
            re.fullmatch('[A-Za-z0-9_-]{1,128}', v) for v in spec['owner'].values()), 'Nieprawidłowy owner')
    for role, expected in [('data', data), ('parity', parity)]:
        require(isinstance(spec[role], list) and len(spec[role]) == len(expected), 'Inna liczba dysków spec')
        for disk, physical in zip(spec[role], expected):
            require(set(disk) == {'disk_id', 'wwn', 'serial', 'bytes', 'expected_uuid'} and
                    disk['serial'] == vm['disks'][physical]['serial'] and type(disk['bytes']) is int and
                    disk['bytes'] == vm['disks'][physical]['bytes'] and isinstance(disk['disk_id'], str) and
                    0 < len(disk['disk_id']) <= 256 and not re.search('[\x00-\x1f\x7f]', disk['disk_id']) and
                    (disk['wwn'] is None or isinstance(disk['wwn'], str) and 0 < len(disk['wwn']) <= 256 and
                     not re.search('[\x00-\x1f\x7f]', disk['wwn'])), 'Obcy dysk spec')
            canonical_uuid(disk['expected_uuid'])
    disks = spec['data'] + spec['parity']
    require(len({d['disk_id'] for d in disks}) == len(disks) and
            len({d['expected_uuid'] for d in disks}) == len(disks), 'Powtórzone dyski spec')
    return spec


def validate_contract(contract):
    require(isinstance(contract, dict) and set(contract) ==
            {'schema', 'stage', 'vm', 'deployment', 'nodeId', 'cases'}, 'Inne pola kontraktu')
    require(type(contract['schema']) is int and contract['schema'] == 1 and
            contract['stage'] in ('seed', 'create', 'postcreate'), 'Inny etap kontraktu')
    vm = contract['vm']
    require(set(vm) == {'uuid', 'baseUrl', 'manifestSha256', 'disks'} and
            canonical_uuid(vm['uuid']) == VM_UUID, 'Nieautoryzowana VM kontraktu')
    require(vm['baseUrl'] == 'https://127.0.0.1:34961', 'Inny endpoint stanowiska')
    hashes = [vm['manifestSha256']]
    prefix = uuid.UUID(vm['uuid']).hex[:10]
    require(set(vm['disks']) == set(DISK_SIZES), 'Niepełne role VM')
    for role, size in DISK_SIZES.items():
        disk = vm['disks'][role]
        require(set(disk) == {'serial', 'bytes'} and disk['serial'] == f'tn-{prefix}-{role}' and
                type(disk['bytes']) is int and disk['bytes'] == size * GIB, 'Inny dysk kontraktu')
    deployment = contract['deployment']
    require(set(deployment) == {'coreSha256', 'helperSha256', 'wasmSha256', 'helperVersion', 'coreVersion'} and
            deployment['helperVersion'] == deployment['coreVersion'] == '0.8.0', 'Inne wdrożenie')
    hashes += [deployment[key] for key in ('coreSha256', 'helperSha256', 'wasmSha256')]
    require(contract['nodeId'] is None if contract['stage'] == 'seed' else
            isinstance(contract['nodeId'], str) and re.fullmatch('[0-9a-f]{64}', contract['nodeId']) and
            contract['nodeId'] != '0' * 64, 'Inny node kontraktu')
    require(set(contract['cases']) == set(CASES), 'Inny zestaw przypadków')
    ids, filesystem_ids, disk_ids, owners = [], [], [], []
    for name, (filesystem, data, parity) in CASES.items():
        case = contract['cases'][name]
        require(set(case) == {'filesystem', 'dataRoles', 'parityRoles', 'cacheRoles', 'spec', 'pins'} and
                case['filesystem'] == filesystem and case['dataRoles'] == data and
                case['parityRoles'] == parity and case['cacheRoles'] == [], 'Inna topologia przypadku')
        spec, pins = case['spec'], case['pins']
        if spec is None:
            require(pins is None, 'Piny bez spec')
            continue
        require(contract['stage'] == 'postcreate', 'Spec przed postcreate')
        validate_spec(spec, name, contract)
        ids.extend([spec['array_id'], spec['operation_id']])
        owners.append(spec['owner'])
        disk_ids.extend(disk['disk_id'] for disk in spec['data'] + spec['parity'])
        filesystem_ids.extend(disk['expected_uuid'] for disk in spec['data'] + spec['parity'])
        require(isinstance(pins, dict) and set(pins) == {'ownerSha256', 'specSha256', 'configSha256'}, 'Brak pinów spec')
        require(pins['ownerSha256'] == hashlib.sha256(json.dumps(spec['owner'], sort_keys=True).encode()).hexdigest() and
                pins['specSha256'] == hashlib.sha256(json.dumps(spec, sort_keys=True).encode()).hexdigest(), 'Niezgodne piny spec/owner')
        if parity:
            hashes.append(pins['configSha256'])
        else:
            require(pins['configSha256'] is None, 'Pin config przy zerowym parity')
    require(len(ids) == len(set(ids)) and len(filesystem_ids) == len(set(filesystem_ids)), 'Powtórzona tożsamość')
    require(len(disk_ids) == len(set(disk_ids)) and all(owner == owners[0] for owner in owners), 'Obce owner lub disk_id')
    require(bool(ids) if contract['stage'] == 'postcreate' else not ids, 'Etap niezgodny ze spec')
    require(all(isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value) and value != '0' * 64 for value in hashes), 'Nieprawidłowy SHA')
    return contract


def load_contract(path, digest):
    require(isinstance(digest, str) and re.fullmatch('[0-9a-f]{64}', digest), 'Nieprawidłowy pin kontraktu')
    path = Path(path)
    require(path.is_absolute() and stat.S_IMODE(path.parent.lstat().st_mode) == 0o700, 'Nieprywatny katalog kontraktu')
    raw = small_file(path, mode=0o600)
    require(hashlib.sha256(raw).hexdigest() == digest, 'Niezgodny SHA kontraktu')
    return validate_contract(decode_json(raw))


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


def inventory(value, contract):
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
    expected = {d['serial']: d['bytes'] for d in contract['vm']['disks'].values()}
    require({d['serial']: int(d['size']) for d in whole} == expected, 'Inne seriale lub rozmiary dysków VM')
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


def validate_journal(journal, name, contract, require_ready=True):
    filesystem, roles, *_ = expected_paths(name)
    spec = journal['spec']
    require(contract['stage'] == 'postcreate' and contract['cases'][name]['spec'] is not None and
            spec == contract['cases'][name]['spec'], 'Niezgodna tożsamość spec kontraktu')
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
        expected = contract['vm']['disks'][suffix]
        require(disk['serial'] == expected['serial'] and disk['bytes'] == expected['bytes'], 'Inny dysk roli')
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


def audit(name, report, contract):
    validate_contract(contract)
    require(contract['stage'] == 'postcreate' and contract['cases'][name]['spec'] is not None, 'Brak zatwierdzonego spec przypadku')
    require(os.geteuid() == UID, 'Sonda wymaga root wyłącznie w uzgodnionej VM')
    require(Path('/sys/class/dmi/id/product_uuid').read_text().strip().lower() == VM_UUID, 'Inna VM')
    report['boot_id'] = canonical_uuid(Path('/proc/sys/kernel/random/boot_id').read_text().strip())
    safe_parents(ROOT / 'plik')
    require(stat.S_IMODE(ROOT.stat().st_mode) == 0o700, 'Katalog journala nie jest prywatny')
    with open_read(ROOT / '.storage.lock', 0o600) as lock:
        fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
        result = command(['/usr/bin/lsblk', '--json', '--bytes', '--paths', '--output',
                          'NAME,TYPE,SIZE,WWN,SERIAL,MAJ:MIN,RO,FSTYPE,UUID'])
        require(result.returncode == 0 and not result.stderr, 'Nieudany lsblk')
        whole, nodes = inventory(decode_json(result.stdout), contract)
        report['inventory'] = [{key: d.get(key) for key in ['name', 'serial', 'wwn', 'size', 'maj:min', 'fstype', 'uuid']} for d in whole]
        mounts = mount_rows(Path('/proc/self/mountinfo').read_text())
        raw = small_file(ROOT / f'{contract["cases"][name]["spec"]["array_id"]}.json', mode=0o600)
        journal = decode_json(raw)
        report['journal'] = {key: journal[key] for key in ['schema', 'stage', 'formatted', 'pending', 'boot_id', 'sync_completed_at']}
        report['journal']['sha256'] = hashlib.sha256(raw).hexdigest()
        spec = validate_journal(journal, name, contract, require_ready=False)
        try:
            validate_journal(journal, name, contract)
        except ValueError as error:
            report['violations'].append(str(error))
        report['identity'] = {key: spec[key] for key in ['array_id', 'operation_id', 'name', 'filesystem']}
        report['owner_sha256'] = hashlib.sha256(json.dumps(spec['owner'], sort_keys=True).encode()).hexdigest()
        root_dev = os.stat('/').st_dev
        root_mm = f'{os.major(root_dev)}:{os.minor(root_dev)}'
        ancestor = next(d for d in nodes if d['maj:min'] == root_mm)
        while ancestor['parent']:
            ancestor = next(d for d in nodes if d['name'] == ancestor['parent'])
        require(ancestor['serial'] == contract['vm']['disks']['os']['serial'], 'Root nie pochodzi z dysku OS')
        cache = next(d for d in whole if d['serial'] == contract['vm']['disks']['cache']['serial'])
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
            if report['files'][0].get('sha256') != contract['cases'][name]['pins']['configSha256']:
                report['violations'].append('Niezgodny pin config')
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


def candidate(name, report, contract, array_id, operation_id):
    validate_contract(contract)
    require(contract['stage'] in ('create', 'postcreate') and contract['cases'][name]['spec'] is None, 'Przypadek już przypięty lub niegotowy')
    canonical_uuid(array_id)
    canonical_uuid(operation_id)
    require(array_id != operation_id and os.geteuid() == UID, 'Nieprawidłowy cel/użytkownik')
    require(Path('/sys/class/dmi/id/product_uuid').read_text().strip().lower() == VM_UUID, 'Inna VM')
    safe_parents(ROOT / 'plik')
    require(stat.S_IMODE(ROOT.stat().st_mode) == 0o700, 'Nieprywatny root journal')
    with open_read(ROOT / '.storage.lock', 0o600) as lock:
        fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
        response = command(['/usr/bin/lsblk', '--json', '--bytes', '--paths', '--output',
                            'NAME,TYPE,SIZE,WWN,SERIAL,MAJ:MIN,RO,FSTYPE,UUID'])
        require(response.returncode == 0 and not response.stderr, 'Nieudany lsblk')
        inventory(decode_json(response.stdout), contract)
        path = ROOT / f'{array_id}.json'
        raw = small_file(path, mode=0o600)
        journal = decode_json(raw)
        spec = validate_spec(journal['spec'], name, contract)
        require(spec['array_id'] == array_id and spec['operation_id'] == operation_id and
                spec['owner'] == {'org_id': 'org-default', 'addon_id': 'tentanas-07ec21cd'}, 'Obca próba lub owner')
        report.update(spec=spec, journal_sha256=hashlib.sha256(raw).hexdigest(),
                      journal_bytes=len(raw), journal={key: journal.get(key) for key in
                      ('schema', 'stage', 'pending', 'formatted', 'boot_id', 'sync_completed_at', 'detail')})
        config = Path(expected_paths(name)[2])
        try:
            config_raw = small_file(config)
            config_sha = hashlib.sha256(config_raw).hexdigest()
        except FileNotFoundError:
            config_sha = None
        report['pins'] = {'ownerSha256': hashlib.sha256(json.dumps(spec['owner'], sort_keys=True).encode()).hexdigest(),
                          'specSha256': hashlib.sha256(json.dumps(spec, sort_keys=True).encode()).hexdigest(),
                          'configSha256': config_sha}
        require(small_file(path, mode=0o600) == raw, 'Journal zmienił się podczas eksportu')
        report.update(status='candidate', limits=['To odczyt kandydata, nie zaliczenie Ready/FS/danych ani aktualizacja zaufanego kontraktu.',
                                                  'Operator musi porównać wynik UI i zatwierdzić nowy kontrakt przed pełnym audytem.'])


def main(argv):
    report = {'schema': 1, 'vm_uuid': VM_UUID, 'status': 'refused', 'violations': []}
    try:
        if len(argv) == 6 and argv[0] == 'candidate':
            require(argv[1] in CASES, 'Nieznany przypadek')
            contract = load_contract(argv[2], argv[3])
            report.update(case=argv[1], station_sha256=argv[3])
            candidate(argv[1], report, contract, argv[4], argv[5])
        else:
            require(len(argv) == 3 and argv[0] in CASES, 'Wymagane: przypadek, plik kontraktu, SHA256')
            contract = load_contract(argv[1], argv[2])
            report.update(case=argv[0], station_sha256=argv[2])
            audit(argv[0], report, contract)
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
    print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    return 0 if report['status'] in ('measured', 'candidate') else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
