# =============================================================================
# Plik: guest_cache_probe.py
# Opis: Trzy jednorazowe pomiary cache RW / data NC w prywatnej VM E2-03.
# Przykład: sudo python3 - nc < guest_cache_probe.py; kolejne fazy: race, enospc.
# =============================================================================

import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import select
import stat
import subprocess
import sys
import time
import types
import uuid
import base64

VM_UUID = '5185b7bd-0460-4af9-b103-c4d198134889'
SIZES = {'os': 12, 'data1': 32, 'data2': 32, 'parity': 40, 'cache': 1, 'spare': 40}
ROLES = {'cache': 'data1', 'data': 'parity'}
BASE = Path('/var/lib/tentanas-cache-probe')
MOUNTS = Path('/mnt/tentanas-cache-probe')
FIXTURE = Path('/root/tentanas-e2-audit/guest_storage.py')
FIXTURE_SHA = '9c2298169682b3f7e8170ce1aa736f18eeb49063d368c81faa1546dbf4cd164e'
BINARY_SHA = '773e6df3cbbcdd0a81eb3e294e5293d9920da4996921af53897824cf2940aa60'
MKFS_SHA = 'dccf0d4012f8483701bdfd41894945b97ea3dc486cf1937b05496837fc34da27'
UID = 0
GIB = 1024**3
CHUNK = 1024**2
MAX_FILL = 32 * GIB
MAX_SECONDS = 1200
MIN_FREE = 20 * GIB
OS_RESERVE = 2 * GIB
PHASES = ('nc', 'race', 'enospc')
THRESHOLD_MARKER = b'OPEN_BELOW_MINFREE\n'


def require(value, message):
    if not value:
        raise ValueError(message)


def parents(path):
    for parent in reversed(Path(path).parents):
        value = parent.lstat()
        require(stat.S_ISDIR(value.st_mode) and value.st_uid == UID and not value.st_mode & 0o022,
                'Niebezpieczny katalog nadrzędny')


def read_bytes(path, limit=128 * 1024):
    parents(path)
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NOATIME), 'rb') as stream:
        value = os.fstat(stream.fileno())
        require(stat.S_ISREG(value.st_mode) and value.st_uid == UID and value.st_nlink == 1
                and not value.st_mode & 0o022, 'Niebezpieczny plik')
        data = stream.read(limit + 1)
        require(len(data) <= limit, 'Przekroczony limit odczytu')
        return data


def load_storage():
    require(stat.S_IMODE(FIXTURE.parent.lstat().st_mode) == 0o700
            and stat.S_IMODE(FIXTURE.lstat().st_mode) == 0o600, 'Tryb fixture')
    source = read_bytes(FIXTURE)
    require(hashlib.sha256(source).hexdigest() == FIXTURE_SHA, 'SHA fixture')
    module = types.ModuleType('cache_probe_storage')
    exec(compile(source, str(FIXTURE), 'exec'), module.__dict__)
    return module


def exclusive(path):
    parents(path)
    return os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)


def write_all(fd, data):
    remaining = memoryview(data)
    while remaining:
        count = os.write(fd, remaining)
        require(0 < count <= len(remaining), 'Niepełny zapis')
        remaining = remaining[count:]


def persist(state, storage):
    fd = exclusive(BASE / 'state.next')
    try:
        write_all(fd, json.dumps(state, sort_keys=True).encode())
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(BASE / 'state.next', BASE / 'state.json')
    storage.flush_directory(BASE)


def command(args, lock=None, timeout=60):
    result = subprocess.run(args, capture_output=True, timeout=timeout,
                            pass_fds=() if lock is None else (lock.fileno(),),
                            env={'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL': 'C'})
    require(len(result.stdout) + len(result.stderr) <= 128 * 1024, 'Za duży wynik narzędzia')
    require(result.returncode == 0 and not result.stderr, 'Błąd narzędzia: ' + args[0])
    return result.stdout.decode()


def validate_observation(observation, state):
    require(observation['uuid'] == VM_UUID and str(uuid.UUID(observation['boot_id'])) == observation['boot_id'], 'Obca VM/boot')
    require(state is None or state['boot_id'] == observation['boot_id'], 'Restart poza zakresem sondy')
    disks = [item for item in observation['disks'] if item['type'] != 'rom']
    require(len(disks) == 6 and all(d['type'] == 'disk' for d in disks), 'Niepełne sześć dysków')
    require(len({d['serial'] for d in disks}) == 6 and len({d['maj:min'] for d in disks}) == 6, 'Alias dysku')
    root = [m for m in observation['mounts'] if m['target'] == '/']
    require(len(root) == 1, 'Nieznany root')
    found = {}
    filesystems = {} if state is None else state['filesystems']
    for role, size in SIZES.items():
        rows = [d for d in disks if d['serial'] == 'tn-5185b7bd04-' + role]
        require(len(rows) == 1, 'Obcy serial')
        disk = rows[0]
        require(type(disk['size']) is int and disk['size'] == size * GIB and not disk['ro'], 'Obcy rozmiar/RO')
        require(re.fullmatch(r'nvme\d+n\d+' if role == 'cache' else r'vd[a-z]+', disk['name']), 'Obca magistrala')
        numbers = {disk['maj:min']} | {c['maj:min'] for c in disk.get('children', [])}
        if role == 'os':
            require(root[0]['maj:min'] in numbers, 'Root poza OS')
            continue
        require(not disk.get('children') and not disk['holders'] and not numbers.intersection(observation['swaps'])
                and root[0]['maj:min'] not in numbers, 'Partycje/holder/swap/system')
        logical = next((key for key, value in ROLES.items() if value == role), None)
        mounted = [m for m in observation['mounts'] if m['maj:min'] == disk['maj:min']]
        if logical in filesystems:
            require(disk['fstype'] == 'xfs' and disk['uuid'] == filesystems[logical]
                    and disk['signatures'] and all(s['type'] == 'xfs' for s in disk['signatures']), 'Obcy FS/UUID')
            require(len(mounted) <= 1 and all(m['target'] == str(MOUNTS / logical) for m in mounted), 'Obcy mount')
        else:
            require(disk['fstype'] is None and disk['uuid'] is None and not disk['signatures'] and not mounted, 'Dysk nie jest pusty')
        if logical:
            found[logical] = disk
    require(len(set(filesystems.values())) == len(filesystems), 'Powtórzone UUID FS')
    return found


def mount_rows():
    rows = []
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        left, right = line.split(' - ', 1)
        fields, source = left.split(), right.split()
        rows.append({'target': fields[4], 'number': fields[2], 'root': fields[3],
                     'options': fields[5].split(','), 'filesystem': source[0], 'source': source[1]})
    return rows


def options():
    return {'branches': f'{MOUNTS}/cache=RW:{MOUNTS}/data=NC', 'category.create': 'mfs',
            'cache.files': 'off', 'minfreespace': str(MIN_FREE), 'moveonenospc': 'mfs', 'func.getattr': 'newest',
            'nullrw': 'false', 'cache.writeback': 'false'}


def observed_options():
    return {key: os.getxattr(MOUNTS / 'union' / '.mergerfs', 'user.mergerfs.' + key).decode() for key in options()}


def directory_identity(path):
    parents(path)
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == UID and not info.st_mode & 0o022, 'Obcy katalog pomiaru')
    return {'device': info.st_dev, 'inode': info.st_ino}


def lock_identity(fd=None):
    parents(BASE / '.lock')
    info = (BASE / '.lock').lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == UID and info.st_nlink == 1
            and stat.S_IMODE(info.st_mode) == 0o600, 'Niebezpieczny lock')
    if fd is not None:
        opened = os.fstat(fd)
        require((opened.st_dev, opened.st_ino) == (info.st_dev, info.st_ino), 'Podmieniony lock')
    return {'device': info.st_dev, 'inode': info.st_ino}


def acquire_lock(fd):
    try:
        lock_identity(fd)
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        lock_identity(fd)
        return os.fdopen(fd, 'rb')
    except Exception:
        os.close(fd)
        raise


def guard_storage(storage, state, union_required=True):
    observation = storage.observe()
    found = validate_observation(observation, state)
    if state is not None:
        require(state.get('lock') == lock_identity(), 'Zmieniony inode blokady')
        for path, expected in state.get('directories', {}).items():
            require(path in [str(BASE), str(MOUNTS)] + [str(MOUNTS / role) for role in ('cache', 'data', 'union')]
                    and directory_identity(Path(path)) == expected, 'Podmieniony katalog/inode')
    rows = mount_rows()
    targets = [str(MOUNTS / role) for role in ('cache', 'data', 'union')]
    require(not any(Path(m['target']).is_relative_to(MOUNTS) and m['target'] not in targets for m in rows), 'Mount potomny/obcy')
    for role, disk in found.items():
        device = Path('/dev') / disk['name']
        info = device.lstat()
        require(stat.S_ISBLK(info.st_mode) and f'{os.major(info.st_rdev)}:{os.minor(info.st_rdev)}' == disk['maj:min'], 'Podmienione urządzenie')
        mounts = [m for m in rows if m['target'] == str(MOUNTS / role) or m['number'] == disk['maj:min']]
        if state is not None and role in state['mounted']:
            require(len(mounts) == 1 and mounts[0]['target'] == str(MOUNTS / role)
                    and mounts[0]['source'] == str(device) and mounts[0]['filesystem'] == 'xfs'
                    and mounts[0]['root'] == '/' and 'rw' in mounts[0]['options'], 'Obcy/niepełny mount brancha')
            require((MOUNTS / role).stat().st_dev == info.st_rdev, 'Branch poza FS')
        else:
            require(not mounts, 'Przedwczesny mount')
    union = [m for m in rows if m['target'] == str(MOUNTS / 'union')]
    if union_required:
        require(len(union) == 1 and union[0]['filesystem'] == 'fuse.mergerfs'
                and union[0]['source'] == 'cache:data'
                and union[0]['root'] == '/' and 'rw' in union[0]['options'], 'Obca unia')
        observed = observed_options()
        require(observed == options(), 'Inne opcje mergerfs')
    else:
        require(not union, 'Przedwczesna unia')
    require(storage.space(Path('/'))['available'] >= OS_RESERVE, 'Rezerwa OS')
    return observation, found


def metric(path):
    parents(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NOATIME)
    try:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and before.st_uid == UID, 'Obcy plik pomiaru')
        digest = hashlib.sha256()
        while block := os.read(fd, CHUNK):
            digest.update(block)
        after = os.fstat(fd)
        require((before.st_ino, before.st_size, before.st_mtime_ns) == (after.st_ino, after.st_size, after.st_mtime_ns), 'Plik zmieniony podczas odczytu')
        return {'bytes': after.st_size, 'allocated': after.st_blocks * 512, 'inode': after.st_ino,
                'device': after.st_dev, 'mtime_ns': after.st_mtime_ns, 'sha256': digest.hexdigest()}
    finally:
        os.close(fd)


def create_file(path, data, storage):
    fd = exclusive(path)
    try:
        write_all(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    storage.flush_directory(path.parent)


def pending(state, label, storage):
    state['pending'] = label
    persist(state, storage)


def nc(storage, state, lock):
    for role in ('cache', 'data'):
        pending(state, 'format_' + role, storage)
        _, found = guard_storage(storage, state, False)
        expected_uuid = str(uuid.uuid4())
        state['expected_format'] = {'role': role, 'uuid': expected_uuid}
        persist(state, storage)
        command(['/usr/sbin/mkfs.xfs', '-m', 'uuid=' + expected_uuid, '/dev/' + found[role]['name']], lock)
        state['filesystems'][role] = expected_uuid
        persist(state, storage)
        pending(state, 'mount_' + role, storage)
        _, found = guard_storage(storage, state, False)
        (MOUNTS / role).mkdir(mode=0o700)
        storage.flush_directory(MOUNTS)
        command(['/usr/bin/mount', '-t', 'xfs', '-o', 'noatime', '/dev/' + found[role]['name'], str(MOUNTS / role)], lock)
        state['mounted'].append(role)
        state['directories'][str(MOUNTS / role)] = directory_identity(MOUNTS / role)
        persist(state, storage)
    pending(state, 'union', storage)
    guard_storage(storage, state, False)
    (MOUNTS / 'union').mkdir(mode=0o700)
    storage.flush_directory(MOUNTS)
    command(['/usr/bin/mergerfs', f'{MOUNTS}/cache=RW:{MOUNTS}/data=NC', str(MOUNTS / 'union'), '-o',
             'category.create=mfs,minfreespace=20G,cache.files=off,moveonenospc=true,allow_other,func.getattr=newest'])
    state['directories'][str(MOUNTS / 'union')] = directory_identity(MOUNTS / 'union')
    persist(state, storage)
    guard_storage(storage, state)
    spaces = {role: storage.space(MOUNTS / role) for role in ROLES}
    require(all(value['available'] > MIN_FREE for value in spaces.values())
            and spaces['data']['available'] > spaces['cache']['available'], 'Brak zapasu/większego data NC')
    pending(state, 'create_probe', storage)
    data = bytes(range(256)) * 4096
    create_file(MOUNTS / 'union' / 'new.bin', data, storage)
    require(not os.path.lexists(MOUNTS / 'data' / 'new.bin'), 'Nowy plik ominął cache')
    require(metric(MOUNTS / 'cache' / 'new.bin')['sha256'] == hashlib.sha256(data).hexdigest(), 'Inne nowe dane')
    create_file(MOUNTS / 'data' / 'existing.bin', data, storage)
    fd = os.open(MOUNTS / 'union' / 'existing.bin', os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        write_all(fd, b'NC_APPEND')
        os.fsync(fd)
    finally:
        os.close(fd)
    require(not os.path.lexists(MOUNTS / 'cache' / 'existing.bin') and
            metric(MOUNTS / 'data' / 'existing.bin')['sha256'] == hashlib.sha256(data + b'NC_APPEND').hexdigest(), 'NC append zmienił branch/dane')
    return {'options': observed_options(), 'spaces': spaces, 'new': metric(MOUNTS / 'cache' / 'new.bin'),
            'existing': metric(MOUNTS / 'data' / 'existing.bin')}


def wait_byte(fd):
    require(select.select([fd], [], [], 10)[0] and os.read(fd, 1) == b'1', 'Brak bariery pisarza')


def race_one(source, target, writer_path, lock, storage, union_device):
    ready_read, ready_write = os.pipe()
    continue_read, continue_write = os.pipe()
    script = ('import os,sys,json; f=os.open(sys.argv[1],os.O_WRONLY|os.O_APPEND|os.O_NOFOLLOW); '
              's=os.fstat(f); before={"inode":s.st_ino,"device":s.st_dev,"bytes":s.st_size}; '
              'os.write(int(sys.argv[2]),b"1"); assert os.read(int(sys.argv[3]),1)==b"1"; '
              'assert os.write(f,b"AFTER_COPY_MARKER")==17; os.fsync(f); '
              's=os.fstat(f); after={"inode":s.st_ino,"device":s.st_dev,"bytes":s.st_size}; '
              'os.write(int(sys.argv[2]),b"1"); assert os.read(int(sys.argv[3]),1)==b"1"; '
              'print(json.dumps({"before":before,"after":after,"marker":"AFTER_COPY_MARKER"})); os.close(f)')
    child = subprocess.Popen([sys.executable, '-c', script, str(writer_path), str(ready_write), str(continue_read)],
                             pass_fds=(ready_write, continue_read, lock.fileno()), cwd='/', stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, env={'LC_ALL': 'C'})
    os.close(ready_write)
    os.close(continue_read)
    try:
        wait_byte(ready_read)
        before = metric(source)
        expected = hashlib.sha256()
        fd = exclusive(target)
        try:
            with os.fdopen(os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NOATIME | os.O_CLOEXEC), 'rb') as stream:
                while block := stream.read(CHUNK):
                    write_all(fd, block)
                    expected.update(block)
            os.fsync(fd)
        finally:
            os.close(fd)
        require(metric(target)['sha256'] == before['sha256'], 'Niepełna kopia przed barierą')
        os.write(continue_write, b'1')
        wait_byte(ready_read)
        expected.update(b'AFTER_COPY_MARKER')
        after, copied = metric(source), metric(target)
        os.write(continue_write, b'1')
        output, errors = child.communicate(timeout=10)
        require(len(output) + len(errors) <= 64 * 1024, 'Za duży zapis obserwacji pisarza')
        observation = {'status': 'observation', 'writer_path': str(writer_path), 'union_device': union_device,
                       'writer_stdout_b64': base64.b64encode(output).decode('ascii'),
                       'writer_stderr_b64': base64.b64encode(errors).decode('ascii'),
                       'writer_exit_code': child.returncode, 'before': before, 'source_after': after,
                       'target': copied, 'expected_source_sha256': expected.hexdigest()}
        mode = 'cache' if writer_path == source else 'union'
        create_file(BASE / ('race-' + mode + '-observation.json'),
                    json.dumps(observation, sort_keys=True).encode(), storage)
        require(after['device'] == before['device'] and after['inode'] == before['inode'] and after['bytes'] == before['bytes'] + 17
                and after['sha256'] == expected.hexdigest() and copied['sha256'] == before['sha256'], 'Brak dowodu zapisu po kopii')
        require(child.returncode == 0 and len(output) < 1024, 'Błąd pisarza')
        writer = json.loads(output)
        require(writer['marker'] == 'AFTER_COPY_MARKER', 'Inny marker pisarza')
        differences = [{'moment': moment, 'field': field, 'writer': writer[moment][field], 'backing': backing[field]}
                       for moment, backing in (('before', before), ('after', after))
                       for field in ('device', 'inode', 'bytes') if writer[moment][field] != backing[field]]
        if writer_path == source:
            require(writer['before']['bytes'] == before['bytes'], 'Inny rozmiar FD pisarza przed kopią')
            require(writer['after']['bytes'] == after['bytes'], 'Inny rozmiar FD pisarza po markerze')
            require(writer['before']['inode'] == writer['after']['inode'], 'Inny inode FD pisarza')
            require(writer['before']['device'] == writer['after']['device'], 'Inne urządzenie FD pisarza')
            require(writer['before']['inode'] == before['inode'] and writer['before']['device'] == before['device'], 'Obcy backing FD')
        else:
            require(type(union_device) is int and union_device > 0
                    and writer['before']['device'] == writer['after']['device'] == union_device, 'Obce urządzenie FUSE pisarza')
        return {'before': before, 'source_after': after, 'target': copied, 'writer_fd': writer,
                'writer_differences': differences, 'source_unlinked': False}
    finally:
        os.close(ready_read)
        os.close(continue_write)
        if child.poll() is None:
            child.kill()
            child.wait()


def race(storage, state, lock):
    result = {}
    for mode in ('union', 'cache'):
        guard_storage(storage, state)
        name = 'race-' + mode + '.bin'
        source, target = MOUNTS / 'cache' / name, MOUNTS / 'data' / name
        create_file(source, os.urandom(8 * CHUNK), storage)
        result[mode] = race_one(source, target, MOUNTS / mode / name, lock, storage,
                                state['directories'][str(MOUNTS / 'union')]['device'])
        storage.flush_directory(target.parent)
    return result


def append_result(fd, payload, limit):
    accepted = 0
    while accepted < limit:
        operation = 'write'
        requested = min(len(payload), limit - accepted)
        try:
            count = os.write(fd, payload[:requested])
            require(0 < count <= requested, 'Niepoprawny shortwrite')
            accepted += count
            operation = 'fsync'
            os.fsync(fd)
        except OSError as error:
            require(error.errno == errno.ENOSPC, 'Nieoczekiwany błąd append')
            return {'accepted': accepted, 'errno': error.errno, 'operation': operation}
    return {'accepted': accepted, 'errno': None, 'operation': 'completed'}


def fill(fd, storage, state, progress, held_fd, allocation_paths):
    started, accepted = time.monotonic(), 0
    payload = bytes(range(256)) * (CHUNK // 256)
    allocation_before = {str(path): metric(path)['allocated'] for path in allocation_paths}
    before = storage.space(MOUNTS / 'cache')
    while accepted < MAX_FILL:
        require(time.monotonic() - started < MAX_SECONDS, 'Limit czasu fill')
        require(storage.space(Path('/'))['available'] >= OS_RESERVE, 'Rezerwa OS podczas fill')
        require(storage.space(MOUNTS / 'data')['available'] > MIN_FREE, 'Data bez zapasu20G')
        available = storage.space(MOUNTS / 'cache')['available']
        if available < MIN_FREE and 'below_minfree' not in progress:
            requested = MOUNTS / 'union' / 'below-minfree.bin'
            try:
                opened = exclusive(requested)
            except OSError as error:
                progress['below_minfree'] = {'errno': error.errno, 'available': available}
            else:
                os.close(opened)
                raise ValueError('Nowy create poniżej20G nie odmówił')
            require(progress['below_minfree']['errno'] == errno.ENOSPC and
                    all(not os.path.lexists(MOUNTS / role / 'below-minfree.bin') for role in ROLES), 'Obiekt po odmowie create')
            progress['below_minfree']['held_append'] = append_result(held_fd, THRESHOLD_MARKER, len(THRESHOLD_MARKER))
            require(progress['below_minfree']['held_append']['errno'] is None, 'Otwarty FD odmówił już na progu20G')
            persist(state, storage)
        result = append_result(fd, payload, min(CHUNK, MAX_FILL - accepted))
        accepted += result['accepted']
        if result['errno'] is not None:
            require(accepted > 0 and 'below_minfree' in progress, 'Fill bez dodatniego zapisu/progu')
            after = storage.space(MOUNTS / 'cache')
            allocated = os.fstat(fd).st_blocks * 512
            allocation_after = {str(path): metric(path)['allocated'] for path in allocation_paths}
            allocation_delta = sum(allocation_after.values()) - sum(allocation_before.values())
            consumed = before['free'] - after['free']
            progress['fill'] = {'accepted': accepted, 'errno': result['errno'], 'operation': result['operation'],
                                'before': before, 'after': after, 'allocated': allocated, 'uid': os.geteuid(),
                                'other_cache_allocation_before': allocation_before, 'other_cache_allocation_after': allocation_after,
                                'other_cache_allocation_delta': allocation_delta, 'observed_consumed': consumed,
                                'expected_consumed': allocated + allocation_delta,
                                'available_zero_allocation': storage.enospc_allocation(before, after, allocated + allocation_delta)}
            persist(state, storage)
            require(abs(consumed - allocated - allocation_delta) <= 16 * CHUNK, 'Brak niezależnego bilansu fill')
            return progress['fill']
    raise ValueError('Limit bajtów bez ENOSPC')


def verify_preserved(preserved):
    actual = {path: metric(Path(path)) for path in preserved}
    require(all({key: value for key, value in actual[path].items() if key != 'allocated'} ==
                {key: value for key, value in expected.items() if key != 'allocated'}
                for path, expected in preserved.items()), 'Dane poprzedniej fazy zmienione')
    return actual


def enospc(storage, state, lock):
    guard_storage(storage, state)
    require(all(storage.space(MOUNTS / role)['available'] > MIN_FREE for role in ROLES), 'Brak początkowego zapasu20G')
    prefix = b'OPEN_BEFORE_FILL\n'
    path = MOUNTS / 'union' / 'append.bin'
    result = {}
    state['measurements']['enospc'] = result
    preserved = {str(MOUNTS / 'cache' / 'new.bin'): state['measurements']['nc']['new'],
                 str(MOUNTS / 'data' / 'existing.bin'): state['measurements']['nc']['existing']}
    for mode in ('union', 'cache'):
        preserved[str(MOUNTS / 'cache' / ('race-' + mode + '.bin'))] = state['measurements']['race'][mode]['source_after']
        preserved[str(MOUNTS / 'data' / ('race-' + mode + '.bin'))] = state['measurements']['race'][mode]['target']
    result['preserved_before'] = verify_preserved(preserved)
    create_file(path, prefix, storage)
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW | os.O_CLOEXEC)
    filler = None
    try:
        filler = exclusive(MOUNTS / 'cache' / 'ballast.bin')
        result['prefix'] = metric(MOUNTS / 'cache' / 'append.bin')
        allocation_paths = [Path(path) for path in preserved if Path(path).parent == MOUNTS / 'cache']
        allocation_paths.append(MOUNTS / 'cache' / 'append.bin')
        result['fill'] = fill(filler, storage, state, result, fd, allocation_paths)
        persist(state, storage)
        guard_storage(storage, state)
        result['append'] = append_result(fd, b'A' * CHUNK, 16 * CHUNK)
    finally:
        if filler is not None:
            os.close(filler)
        os.close(fd)
    locations = [role for role in ROLES if os.path.lexists(MOUNTS / role / 'append.bin')]
    require(len(locations) == 1, 'Niejednoznaczne położenie pliku append')
    result['location'] = locations[0]
    result['file'] = metric(MOUNTS / result['location'] / 'append.bin')
    expected = prefix + THRESHOLD_MARKER + b'A' * result['append']['accepted']
    require(result['file']['bytes'] == len(expected) and result['file']['sha256'] == hashlib.sha256(expected).hexdigest()
            and metric(path)['sha256'] == result['file']['sha256'], 'Utrata zaakceptowanych danych append')
    result['outcome'] = ('spill_observed' if result['location'] == 'data' else
                         'no_spill_nc' if result['append']['errno'] == errno.ENOSPC else 'append_completed_on_cache')
    result['preserved_after'] = verify_preserved(preserved)
    result['spaces'] = {role: storage.space(MOUNTS / role) for role in ROLES}
    persist(state, storage)
    require(result['outcome'] != 'append_completed_on_cache', 'Nierozstrzygnięty union ENOSPC: limit append bez błędu, brak ponowienia')
    return result


def execute(phase, storage, lock):
    storage.private(BASE, True)
    if phase == 'nc':
        require(not os.path.lexists(BASE / 'state.json') and not os.path.lexists(MOUNTS), 'Przebieg już rozpoczęty')
        observation, _ = guard_storage(storage, None, False)
        state = {'schema': 1, 'vm_uuid': VM_UUID, 'boot_id': observation['boot_id'], 'stage': 'nc',
                 'pending': 'nc', 'filesystems': {}, 'mounted': [], 'measurements': {},
                 'directories': {str(BASE): directory_identity(BASE)}, 'lock': lock_identity(lock.fileno())}
        persist(state, storage)
        parents(MOUNTS)
        MOUNTS.mkdir(mode=0o700)
        storage.flush_directory(MOUNTS.parent)
        state['directories'][str(MOUNTS)] = directory_identity(MOUNTS)
        persist(state, storage)
    else:
        storage.private(BASE / 'state.json')
        state = json.loads(read_bytes(BASE / 'state.json'))
        require(state['schema'] == 1 and state['vm_uuid'] == VM_UUID and state['pending'] is None
                and state['stage'] == ('nc' if phase == 'race' else 'race'), 'Niepełna lub powtórzona faza; brak retry')
        guard_storage(storage, state)
        pending(state, phase, storage)
    state['measurements'][phase] = {'nc': nc, 'race': race, 'enospc': enospc}[phase](storage, state, lock)
    guard_storage(storage, state)
    state['stage'], state['pending'] = phase, None
    persist(state, storage)
    return {'status': 'measured', 'phase': phase, 'measurement': state['measurements'][phase],
            'limits': ['Sonda semantyki dwóch FS, nie produkcyjny mover/cache/NVMe.', 'Bez cleanup, retry i zmiany innych dysków.']}


def main(argv):
    if argv == ['--help']:
        print('Fazy: preflight (tylko odczyt), nc, race, enospc. Bez retry i cleanup.')
        return 0
    report = {'status': 'refused'}
    try:
        require(len(argv) == 1 and argv[0] in (*PHASES, 'preflight') and os.geteuid() == UID, 'Nieprawidłowa faza/użytkownik')
        require(Path('/sys/class/dmi/id/product_uuid').read_text().strip().lower() == VM_UUID, 'Obca VM')
        storage = load_storage()
        storage.automation_guard(VM_UUID)
        require(command(['/usr/bin/dpkg-query', '-W', '-f=${Version}', 'mergerfs']) == '2.40.2-5', 'Inny pakiet mergerfs')
        require(metric(Path('/usr/bin/mergerfs'))['sha256'] == BINARY_SHA, 'Obca binarka mergerfs')
        require(command(['/usr/bin/dpkg-query', '-W', '-f=${Version}', 'xfsprogs']) == '6.13.0-2+b1', 'Inny pakiet XFS')
        require(metric(Path('/usr/sbin/mkfs.xfs'))['sha256'] == MKFS_SHA, 'Obca binarka mkfs')
        parents(BASE)
        if argv[0] == 'preflight':
            require(not os.path.lexists(BASE) and not os.path.lexists(MOUNTS), 'Przebieg już rozpoczęty')
            observation, found = guard_storage(storage, None, False)
            report.update(status='measured', phase='preflight', vm_uuid=VM_UUID, boot_id=observation['boot_id'],
                          roles={role: {'serial': disk['serial'], 'bytes': disk['size']} for role, disk in found.items()},
                          mutating_authority=False)
            print(json.dumps(report, sort_keys=True), flush=True)
            return 0
        if argv[0] == 'nc':
            require(not os.path.lexists(BASE), 'Przebieg już istnieje')
            BASE.mkdir(mode=0o700)
            storage.flush_directory(BASE.parent)
            fd = exclusive(BASE / '.lock')
            os.fsync(fd)
            storage.flush_directory(BASE)
        else:
            storage.private(BASE, True)
            fd = os.open(BASE / '.lock', os.O_RDONLY | os.O_NOFOLLOW | os.O_NOATIME | os.O_CLOEXEC)
        with acquire_lock(fd) as lock:
            report.update(execute(argv[0], storage, lock))
    except Exception as error:
        report['status'] = 'refused'
        report['error'] = f'{type(error).__name__}: {error}'
    print(json.dumps(report, sort_keys=True), flush=True)
    return 0 if report['status'] == 'measured' else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
