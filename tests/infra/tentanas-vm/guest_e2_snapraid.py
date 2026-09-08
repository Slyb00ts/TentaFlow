# =============================================================================
# Plik: guest_e2_snapraid.py
# Opis: Jednorazowy korpus i odbiór SnapRAID na istniejącej macierzy VM E2.
# Przykład: sudo python3 - protect < guest_e2_snapraid.py (wyłącznie operator VM).
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
import types

CASE = 'e2-xfs-two'
VM_UUID = '16e0a47b-f61a-4a9c-8407-e2b1ce554d59'
ARRAY_ID = '01a07d8f-faf6-7651-bdb8-86be5572f28e'
OPERATION_ID = '01a07d8f-faf6-7651-bdb8-86cb20fa4d33'
FS_UUIDS = ['27cb314c-2120-4e95-8308-748c49464afc', '664d6dc0-ae24-4241-a8cc-d2f0d9f62a31', '23387aa6-ec97-407d-b436-0b8b2d5f8f62']
JOURNAL_SHA = 'a4dccf084f4b50de354eb9bfd05420886b5f85e4e4e7967f2bf42b253e30d837'
BOOT_ID = '5bab6ff1-29f0-4762-904d-5946ddd07e1e'
BINARY_SHA = 'a01b01325546cdd7058ced642ef9765154efce3392f46fe50ad032e503ff90ee'
CONFIG_SHA = '32318c8474428daef1b67f05d5ddf093805d42bcd4e2afc9bc66619f60a169d7'
EMPTY_CONTENT_SHA = '47ab56bcb07ca4ee54fcfbdcb326429bbc1598e65c2ae3b2a0864b0fd48d19ed'
FIXTURES = Path('/root/tentanas-e2-audit')
PINS = {'readonly-elastic-audit.py': '2aefaf14bcdc3f8ec6fc280b75f38e12ca95585826147986ecd79da20ccb6d84',
        'guest_storage.py': '9c2298169682b3f7e8170ce1aa736f18eeb49063d368c81faa1546dbf4cd164e'}
BASE = Path('/var/lib/tentanas-e2-payload/e2-xfs-two')
UNION = Path('/mnt/e2-xfs-two')
DATA = Path('/mnt/tentanas-branches/e2-xfs-two/data/d1')
FOLDER = 'tentanas-e2-corpus'
FILES = {'restore.bin': 8 * 1024**2, 'control.bin': 9 * 1024**2}
PHASES = ('preflight', 'corpus', 'protect', 'verify')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def load_fixture(name):
    for parent in reversed((FIXTURES / name).parents):
        info = parent.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022, 'Obcy katalog fixture')
    require(stat.S_IMODE(FIXTURES.stat().st_mode) == 0o700, 'Nieprywatny katalog fixture')
    fd = os.open(FIXTURES / name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NOATIME | os.O_CLOEXEC)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) == 0o600, 'Obcy plik fixture')
        source = stream.read(128 * 1024 + 1)
    require(hashlib.sha256(source).hexdigest() == PINS[name], 'Niezgodny SHA fixture')
    module = types.ModuleType(name.removesuffix('.py'))
    exec(compile(source, name, 'exec'), module.__dict__)
    return module


def exclusive(path):
    return os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)


def write_all(fd, data):
    view = memoryview(data)
    while view:
        written = os.write(fd, view)
        require(written > 0, 'Niepełny zapis')
        view = view[written:]


def write_json(path, value, storage):
    fd = exclusive(path)
    try:
        write_all(fd, json.dumps(value, sort_keys=True).encode())
        os.fsync(fd)
    finally:
        os.close(fd)
    storage.flush_directory(path.parent)


def persist(state, storage):
    write_json(BASE / 'state.next', state, storage)
    os.replace(BASE / 'state.next', BASE / 'state.json')
    storage.flush_directory(BASE)


def entries(audit, path, device):
    audit.safe_parents(path / 'odczyt')
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_NOATIME | os.O_CLOEXEC)
    try:
        require(os.fstat(fd).st_dev == device, 'Katalog na innym filesystemie')
        with os.scandir(fd) as scanned:
            return sorted(entry.name for entry in scanned)
    finally:
        os.close(fd)


def guard(audit):
    require(os.geteuid() == 0 and Path('/sys/class/dmi/id/product_uuid').read_text().strip().lower() == VM_UUID, 'Obca VM/użytkownik')
    require(Path('/proc/sys/kernel/random/boot_id').read_text().strip() == BOOT_ID, 'Ten checkpoint nie obejmuje nowego bootu')
    raw = audit.small_file(audit.ROOT / f'{ARRAY_ID}.json', 128 * 1024, 0o600)
    require(hashlib.sha256(raw).hexdigest() == JOURNAL_SHA, 'Zmiana journala aplikacji')
    spec = audit.validate_journal(audit.decode_json(raw), CASE)
    require(spec['array_id'] == ARRAY_ID and spec['operation_id'] == OPERATION_ID
            and [d['expected_uuid'] for d in spec['data'] + spec['parity']] == FS_UUIDS, 'Obca tożsamość macierzy')
    response = audit.command(['/usr/bin/lsblk', '--json', '--bytes', '--paths', '--output', 'NAME,TYPE,SIZE,WWN,SERIAL,MAJ:MIN,RO,FSTYPE,UUID'])
    require(response.returncode == 0 and not response.stderr, 'Błąd inventory')
    whole, nodes = audit.inventory(audit.decode_json(response.stdout))
    root_dev = os.stat('/').st_dev
    root = next(row for row in nodes if row['maj:min'] == f'{os.major(root_dev)}:{os.minor(root_dev)}')
    while root['parent']:
        root = next(row for row in nodes if row['name'] == root['parent'])
    require(root['serial'] == 'tn-16e0a47bf6-os', 'Obcy OS')
    _, roles, config, content, parity = audit.expected_paths(CASE)
    mounts = audit.mount_rows(Path('/proc/self/mountinfo').read_text())
    devices = {}
    for role, index, _, target in roles:
        wanted = spec[role][index - 1]
        disk = next(row for row in whole if row['serial'] == wanted['serial'])
        info = os.stat(disk['name'], follow_symlinks=False)
        require(not disk.get('children') and stat.S_ISBLK(info.st_mode)
                and f'{os.major(info.st_rdev)}:{os.minor(info.st_rdev)}' == disk['maj:min']
                and (disk.get('wwn') or None) == (wanted['wwn'] or None), 'Obcy dysk roli')
        fields = audit.parse_blkid(audit.command(['/usr/sbin/blkid', '-p', '-o', 'export', disk['name']]))
        require(fields.get('UUID') == wanted['expected_uuid'] and fields.get('TYPE') == 'xfs', 'Obcy UUID/FS')
        found = [m for m in mounts if m['target'] == target or m['major_minor'] == disk['maj:min']]
        require(len(found) == 1 and found[0]['target'] == target and found[0]['source'] == disk['name']
                and found[0]['filesystem'] == 'xfs' and found[0]['root'] == '/' and 'rw' in found[0]['options'].split(','), 'Obcy mount roli')
        audit.safe_parents(Path(target) / 'odczyt')
        require(os.stat(target).st_dev == info.st_rdev and info.st_rdev != root_dev, 'Branch poza właściwym FS')
        devices[target] = info.st_rdev
    require(not any(Path(m['target']) != Path(target) and Path(m['target']).is_relative_to(target)
                    for m in mounts for target in [*devices, str(UNION)]), 'Mount potomny')
    unions = [m for m in mounts if m['target'] == str(UNION)]
    require(len(unions) == 1 and unions[0]['filesystem'] == 'fuse.mergerfs' and unions[0]['source'] == str(DATA)
            and unions[0]['root'] == '/' and 'rw' in unions[0]['options'].split(','), 'Obca unia')
    options = {'branches': str(DATA) + '=RW', 'category.create': 'mfs', 'cache.files': 'off',
               'minfreespace': '21474836480', 'moveonenospc': 'mfs', 'func.getattr': 'newest'}
    require({key: os.getxattr(str(UNION / '.mergerfs'), 'user.mergerfs.' + key).decode() for key in options} == options, 'Inne opcje unii')
    expected = [f'parity {parity[0]}', f'2-parity {parity[1]}'] + ['content ' + p for p in content]
    expected += [f'data d1 {DATA}'] + ['exclude ' + p for p in ['/lost+found/', '/tmp/', '*.unrecoverable', '.AppleDouble', '._AppleDouble', '.DS_Store']]
    expected += ['blocksize 256', 'autosave 500']
    require(audit.config_directives(audit.small_file(config).decode()) == expected, 'Inny config')
    metrics = []
    for path in [config] + content + parity:
        device = next((dev for target, dev in devices.items() if Path(path).is_relative_to(target)), root_dev)
        require(not Path(path).is_relative_to(DATA), 'Metadane w unii')
        value = audit.file_metric(path, device)
        require(value['present'], 'Brak metadanych macierzy')
        metrics.append(value)
    for path in ['/', *devices]:
        value = os.statvfs(path)
        require(value.f_bavail * value.f_frsize >= 1024**3, 'Za mało wolnego miejsca')
    versions = audit.command(['/usr/bin/dpkg-query', '-W', '-f=${Package}=${Version}\n', 'snapraid', 'mergerfs'])
    require(versions.returncode == 0 and not versions.stderr
            and set(versions.stdout.splitlines()) == {'snapraid=12.4-1', 'mergerfs=2.40.2-5'}, 'Inne pakiety')
    binary = audit.file_metric('/usr/bin/snapraid', root_dev)
    require(binary['sha256'] == BINARY_SHA and metrics[0]['sha256'] == CONFIG_SHA, 'Obca binarka/config')
    require(audit.small_file(audit.ROOT / f'{ARRAY_ID}.json', 128 * 1024, 0o600) == raw, 'Journal zmienił się podczas odczytu')
    return {'devices': devices, 'union_device': os.makedev(*map(int, unions[0]['major_minor'].split(':'))),
            'files': metrics, 'config': config, 'binary_sha256': binary['sha256'], 'journal_sha256': JOURNAL_SHA}


def empty_checkpoint(measured):
    require(all(value['bytes'] == 133 and value['sha256'] == EMPTY_CONTENT_SHA for value in measured['files'][1:4])
            and all(value['bytes'] == 0 and value['sha256'] == hashlib.sha256(b'').hexdigest() for value in measured['files'][4:]),
            'Metadane nie odpowiadają odebranemu pustemu checkpointowi')


def corpus_metrics(audit, measured):
    require(entries(audit, DATA, measured['devices'][str(DATA)]) == [FOLDER], 'Obce dane na branchu')
    require(entries(audit, UNION, measured['union_device']) == [FOLDER], 'Obce dane w unii')
    require(entries(audit, DATA / FOLDER, measured['devices'][str(DATA)]) == sorted(FILES), 'Obcy korpus')
    result = {}
    for name, size in FILES.items():
        path = DATA / FOLDER / name
        value = audit.file_metric(str(path), measured['devices'][str(DATA)])
        info = path.stat(follow_symlinks=False)
        require(value['present'] and value['bytes'] == size and value['allocated'] >= size, 'Niepełny lub sparse korpus')
        union_value = audit.file_metric(str(UNION / FOLDER / name), measured['union_device'])
        require(union_value['sha256'] == value['sha256'], 'Inne dane przez unię')
        result[name] = {key: value[key] for key in ['sha256', 'bytes', 'inode', 'device']}
        result[name]['mtime_ns'] = info.st_mtime_ns
    return result


def validate_log(label, stdout, log, storage):
    stdout = stdout.replace('\r\n', '\n').replace('\r', '\n')
    log = log.replace('\r\n', '\n').replace('\r', '\n')
    require(re.findall(r'(?m)^blocksize:(\d+)$', log) == ['262144'], 'Inny blocksize')
    adding = label in ('01-diff', '02-sync')
    expected = {'exit': 'diff' if label == '01-diff' else 'equal' if label == '03-diff' else 'ok'}
    if label in ('01-diff', '02-sync', '03-diff'):
        expected.update({key: '0' for key in ('removed', 'updated', 'moved', 'copied', 'restored')})
        expected.update({'added': '2' if adding else '0', 'equal': '0' if adding else '2'})
        require(sorted(re.findall(r'(?m)^scan:(.*)$', log)) ==
                (sorted('add:d1:' + FOLDER + '/' + name for name in FILES) if adding else []), 'Obce różnice')
    elif label == '04-check':
        expected.update(error='0', error_unrecoverable='0')
        require(re.search(r'100% completed, [1-9][0-9]* MB accessed', stdout), 'Brak pełnego check')
    if label in ('02-sync', '05-scrub'):
        expected.update(error_file='0', error_io='0', error_data='0')
    for key, value in expected.items():
        values = ['diff', 'ok'] if label == '02-sync' and key == 'exit' else [value]
        require(re.findall(r'(?m)^summary:' + key + r':(\w+)$', log) == values, 'Inny wynik narzędzia: ' + key)
    if label == '05-scrub':
        require(storage.scrub_blocks(stdout, log) == 68, 'Scrub nie obejmuje 68 bloków korpusu')


def run_snap(audit, storage, lock, state, label, args, code):
    measured = guard(audit)
    require(measured['binary_sha256'] == state['initial']['binary_sha256'] and measured['files'][0] == state['initial']['files'][0], 'Zmiana config/narzędzia')
    require(corpus_metrics(audit, measured) == state['original'], 'Zmieniony korpus')
    state['pending'] = label
    persist(state, storage)
    descriptors = []
    try:
        for suffix in ('log', 'stdout', 'stderr'):
            descriptors.append(exclusive(BASE / 'logs' / f'{label}.{suffix}'))
        storage.flush_directory(BASE / 'logs')
        log_fd, out_fd, error_fd = descriptors
        args = ['/usr/bin/snapraid', '-c', measured['config'], '-l', f'/proc/self/fd/{log_fd}'] + args
        result = subprocess.run(args, cwd='/', stdin=subprocess.DEVNULL, stdout=out_fd, stderr=error_fd,
                                timeout=300, check=False, pass_fds=(lock.fileno(), log_fd),
                                env={'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL': 'C'})
        print(json.dumps({'step': label, 'exit': result.returncode}), flush=True)
        require(result.returncode == code, 'Nieoczekiwany exit narzędzia')
    finally:
        try:
            for fd in descriptors:
                os.fsync(fd)
        finally:
            for fd in descriptors:
                os.close(fd)
    stdout = audit.small_file(BASE / 'logs' / f'{label}.stdout').decode()
    log = audit.small_file(BASE / 'logs' / f'{label}.log').decode()
    require(not audit.small_file(BASE / 'logs' / f'{label}.stderr'), 'Diagnostyka narzędzia')
    validate_log(label, stdout, log, storage)


def execute(phase, audit, storage, lock):
    measured = guard(audit)
    if phase == 'preflight':
        empty_checkpoint(measured)
        require(not os.path.lexists(BASE), 'Przebieg już rozpoczęty')
        require(not entries(audit, DATA, measured['devices'][str(DATA)])
                and not entries(audit, UNION, measured['union_device']), 'Początkowa macierz nie jest pusta')
        return {'status': 'ready', 'measured': measured}
    if phase == 'corpus':
        empty_checkpoint(measured)
        require(not os.path.lexists(BASE), 'Corpus jest jednokrotny')
        require(not entries(audit, DATA, measured['devices'][str(DATA)])
                and not entries(audit, UNION, measured['union_device']), 'Początkowa macierz nie jest pusta')
        audit.safe_parents(BASE.parent)
        if not BASE.parent.exists():
            BASE.parent.mkdir(mode=0o700)
            storage.flush_directory(BASE.parent.parent)
        storage.private(BASE.parent, True)
        require(BASE.parent.stat().st_dev == os.stat('/').st_dev, 'Journal harnessu poza OS')
        BASE.mkdir(mode=0o700)
        storage.flush_directory(BASE.parent)
        (BASE / 'logs').mkdir(mode=0o700)
        state = {'schema': 1, 'stage': 'corpus', 'pending': 'corpus', 'initial': measured}
        persist(state, storage)
        (UNION / FOLDER).mkdir(mode=0o700)
        storage.flush_directory(UNION)
        for name, size in FILES.items():
            fd = exclusive(UNION / FOLDER / name)
            try:
                for _ in range(size // (1024**2)):
                    write_all(fd, os.urandom(1024**2))
                os.fsync(fd)
            finally:
                os.close(fd)
        storage.flush_directory(UNION / FOLDER)
        state['original'] = corpus_metrics(audit, guard(audit))
        write_json(BASE / 'original.json', state['original'], storage)
        state['stage'], state['pending'] = 'corpus_done', None
        persist(state, storage)
        return {'status': 'corpus_done', 'original': state['original']}
    storage.private(BASE, True)
    storage.private(BASE / 'logs', True)
    state = audit.decode_json(audit.small_file(BASE / 'state.json', mode=0o600))
    require(state['schema'] == 1 and state['pending'] is None, 'Nieukończona faza; brak retry')
    require(state['original'] == audit.decode_json(audit.small_file(BASE / 'original.json', mode=0o600))
            and corpus_metrics(audit, measured) == state['original'], 'Zmieniony manifest/korpus')
    if phase == 'protect':
        require(state['stage'] == 'corpus_done' and measured == state['initial'], 'Protect jednokrotny lub zmieniony baseline')
        state['stage'], state['pending'] = 'protect', 'protect'
        persist(state, storage)
        for label, args, code in [('01-diff', ['diff'], 2), ('02-sync', ['sync'], 0), ('03-diff', ['diff'], 0),
                                   ('04-check', ['check'], 0), ('05-scrub', ['-p', 'full', 'scrub'], 0)]:
            run_snap(audit, storage, lock, state, label, args, code)
        measured = guard(audit)
        require(corpus_metrics(audit, measured) == state['original'], 'Zmienione dane po ochronie')
        require(all(f['bytes'] > 0 for f in measured['files'][4:])
                and len({f['sha256'] for f in measured['files'][1:4]}) == 1, 'Niepełne parity/content')
        state['stage'], state['pending'], state['baseline'] = 'protected', None, measured
        persist(state, storage)
        return {'status': 'protected', 'blocks': 68, 'baseline': measured}
    require(phase == 'verify' and state['stage'] == 'protected' and measured == state['baseline'], 'Nie odebrano kompletnego checkpointu')
    return {'status': 'verified', 'blocks': 68, 'original': state['original']}


def main(argv):
    report = {'status': 'refused'}
    try:
        require(len(argv) == 1 and argv[0] in PHASES, 'Dozwolone: preflight/corpus/protect/verify')
        require(os.geteuid() == 0, 'Wymagany root wyłącznie w VM')
        audit = load_fixture('readonly-elastic-audit.py')
        storage = load_fixture('guest_storage.py')
        storage.private(audit.ROOT, True)
        with audit.open_read(audit.ROOT / '.storage.lock', mode=0o600) as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            report.update(execute(argv[0], audit, storage, lock))
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
    print(json.dumps(report, sort_keys=True, ensure_ascii=False), flush=True)
    return 0 if report['status'] != 'refused' else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
