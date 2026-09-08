# =============================================================================
# Plik: guest_writer_gate_probe.py
# Opis: Jednorazowy pomiar zakresu readonly FUSE w prywatnej VM, bez transferu.
# Przykład: sudo python3 - prepare < guest_writer_gate_probe.py
# =============================================================================

import base64
import ctypes
import errno
import fcntl
import hashlib
import json
import mmap
import os
from pathlib import Path
import select
import signal
import socket
import stat
import subprocess
import sys
import time
import types
import resource

VM_UUID = 'b1fa1b6e-bd25-41c9-aa6f-a8f2140544aa'
BOOT_ID = 'e26f6d28-942d-41e4-8c92-285c4d540d24'
PREFIX = 'tn-b1fa1b6ebd-'
SIZES = {'os': 12, 'data1': 32, 'data2': 32, 'parity': 40, 'cache': 1, 'spare': 40}
BASE = Path('/var/lib/tentanas-writer-gate')
TREE = Path('/mnt/tentanas-writer-gate')
FIXTURE = Path('/root/tentanas-e2-audit/guest_storage.py')
FIXTURE_SHA = '9c2298169682b3f7e8170ce1aa736f18eeb49063d368c81faa1546dbf4cd164e'
MERGERFS_SHA = '773e6df3cbbcdd0a81eb3e294e5293d9920da4996921af53897824cf2940aa60'
USER_UID = 1000
LIMIT = 65536
MS_RDONLY, MS_REMOUNT, MS_BIND, MS_PRIVATE = 1, 32, 4096, 1 << 18
PHASES = ('prepare', 'local', 'global')


def require(value, reason):
    if not value:
        raise ValueError(reason)


def private_parents(path):
    for parent in reversed(Path(path).parents):
        info = parent.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022,
                'Niebezpieczny rodzic')


def read_private(path, limit=128 * 1024):
    private_parents(path)
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NOATIME), 'rb') as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) == 0o600, 'Nieprywatny plik')
        data = stream.read(limit + 1)
        require(len(data) <= limit, 'Limit pliku')
        return data


def write_all(fd, data):
    while data:
        count = os.write(fd, data)
        require(0 < count <= len(data), 'Niepełny zapis')
        data = data[count:]


def flush_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def durable_new(path, value):
    private_parents(path)
    raw = json.dumps(value, sort_keys=True).encode()
    require(len(raw) <= 2 * 1024**2, 'Limit dowodu')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        write_all(fd, raw)
        os.fsync(fd)
    finally:
        os.close(fd)
    flush_directory(path.parent)


def persist(state):
    durable_new(BASE / 'state.next', state)
    os.replace(BASE / 'state.next', BASE / 'state.json')
    flush_directory(BASE)


def pending(state, label):
    state['pending'] = label
    persist(state)


def acquire_lock(create=False):
    path = BASE / '.lock'
    private_parents(path)
    require(stat.S_IMODE(BASE.lstat().st_mode) == 0o700, 'Tryb katalogu blokady')
    flags = os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC
    fd = os.open(path, flags | (os.O_CREAT | os.O_EXCL if create else 0), 0o600)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) == 0o600, 'Obcy lock')
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        current = path.lstat()
        require((info.st_dev, info.st_ino) == (current.st_dev, current.st_ino), 'Podmieniony lock')
        return fd
    except BaseException:
        os.close(fd)
        raise


def load_storage():
    require(stat.S_IMODE(FIXTURE.parent.lstat().st_mode) == 0o700, 'Tryb fixture')
    raw = read_private(FIXTURE)
    require(hashlib.sha256(raw).hexdigest() == FIXTURE_SHA, 'SHA fixture')
    module = types.ModuleType('writer_gate_storage')
    exec(compile(raw, str(FIXTURE), 'exec'), module.__dict__)
    return module


def validate_observation(observation):
    require(observation['uuid'] == VM_UUID and observation['boot_id'] == BOOT_ID, 'Obca VM/boot')
    disks = [disk for disk in observation['disks'] if disk['type'] != 'rom']
    require(len(disks) == 6 and len({disk['maj:min'] for disk in disks}) == 6, 'Niepełna szóstka')
    roots = [row for row in observation['mounts'] if row['target'] == '/']
    require(len(roots) == 1, 'Nieznany root')
    for role, size in SIZES.items():
        rows = [disk for disk in disks if disk['serial'] == PREFIX + role]
        require(len(rows) == 1, 'Obcy serial')
        disk = rows[0]
        require(disk['type'] == 'disk' and type(disk['size']) is int
                and disk['size'] == size * 1024**3 and not disk['ro'], 'Obcy typ/rozmiar')
        numbers = {disk['maj:min']} | {child['maj:min'] for child in disk.get('children', [])}
        if role == 'os':
            require(roots[0]['maj:min'] in numbers, 'Root poza OS')
        else:
            require(not disk.get('children') and not disk['holders'] and not disk['signatures']
                    and disk['fstype'] is None and disk['uuid'] is None
                    and not numbers.intersection(observation['swaps'])
                    and not any(row['maj:min'] in numbers for row in observation['mounts']), 'Dysk danych używany')
    return roots[0]['maj:min']


def parse_mounts(text):
    rows = []
    for line in text.splitlines():
        left, right = line.split(' - ', 1)
        fields, source = left.split(), right.split()
        rows.append({'id': fields[0], 'number': fields[2], 'root': fields[3], 'target': fields[4],
                     'mount_options': fields[5].split(','), 'propagation': fields[6:],
                     'filesystem': source[0], 'source': source[1], 'super_options': source[2].split(',')})
    return rows


def mount_rows():
    return parse_mounts(Path('/proc/self/mountinfo').read_text())


def identity():
    status = dict(line.split(':', 1) for line in Path('/proc/self/status').read_text().splitlines())
    return {'uid': os.getuid(), 'euid': os.geteuid(), 'gid': os.getgid(),
            'uid_map': Path('/proc/self/uid_map').read_text(), 'gid_map': Path('/proc/self/gid_map').read_text(),
            'caps': status['CapEff'].strip(), 'mnt_ns': os.stat('/proc/self/ns/mnt').st_ino,
            'user_ns': os.stat('/proc/self/ns/user').st_ino}


def attempt(operation):
    try:
        return {'errno': None, 'value': operation()}
    except OSError as error:
        return {'errno': error.errno, 'value': None}


def write_marker(fd, marker):
    os.lseek(fd, 0, os.SEEK_END)
    write_all(fd, marker)
    os.fsync(fd)
    return len(marker)


def snapshot(roots):
    rows = mount_rows()
    return [{'path': str(root), 'mnt_ns': os.stat('/proc/self/ns/mnt').st_ino,
             'mounts': [row for row in rows if row['target'] == str(root)],
             'stat': attempt(lambda root=root: {'device': root.stat().st_dev,
                       'readonly': bool(os.statvfs(root).f_flag & os.ST_RDONLY)})} for root in roots]


def actor_loop(channel, roots, backing, namespace):
    before, setup = None, {'stage': 'drop_identity'}
    try:
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        os.setgroups([])
        os.setresgid(USER_UID, USER_UID, USER_UID)
        os.setresuid(USER_UID, USER_UID, USER_UID)
        before = identity()
        require(before['uid'] == USER_UID and before['euid'] == USER_UID
                and int(before['caps'], 16) == 0, 'Niepotwierdzony UID przed userns')
        libc = ctypes.CDLL(None, use_errno=True)
        setup['dumpable_after_drop'] = libc.prctl(3, 0, 0, 0, 0)
        require(setup['dumpable_after_drop'] in (0, 1, 2), 'Brak odczytu dumpability')
        setup['core_limit'] = list(resource.getrlimit(resource.RLIMIT_CORE))
        if namespace:
            setup['stage'] = 'dumpability'
            # Własność plików proc zależy od dumpability po zmianie UID.
            if libc.prctl(4, ctypes.c_ulong(1), 0, 0, 0) != 0:
                raise OSError(ctypes.get_errno(), 'PR_SET_DUMPABLE')
            setup['dumpable_before_unshare'] = libc.prctl(3, 0, 0, 0, 0)
            require(setup['dumpable_before_unshare'] == 1, 'Niepotwierdzona dumpability')
            setup['stage'] = 'unshare'
            if libc.unshare(ctypes.c_int(0x10000000 | 0x00020000)) != 0:
                raise OSError(ctypes.get_errno(), 'unshare')
            setup['proc_controls_before_mapping'] = {}
            for name in ('setgroups', 'uid_map', 'gid_map'):
                value = Path('/proc/self', name).stat()
                setup['proc_controls_before_mapping'][name] = {
                    'uid': value.st_uid, 'gid': value.st_gid, 'mode': stat.S_IMODE(value.st_mode)}
            for name, value in (('setgroups', 'deny'), ('uid_map', f'0 {USER_UID} 1\n'),
                                ('gid_map', f'0 {USER_UID} 1\n')):
                setup['stage'] = name
                Path('/proc/self', name).write_text(value)
        setup['stage'] = 'ready'
        channel.sendall(json.dumps({'before': before, 'after': identity(),
                                   'namespace_errno': None, 'setup': setup}).encode() + b'\n')
    except Exception as error:
        channel.sendall(json.dumps({'fatal': type(error).__name__, 'detail': str(error),
                                   'errno': getattr(error, 'errno', None), 'before': before,
                                   'setup': setup}).encode() + b'\n')
        return
    files, mappings = [], []
    reader = channel.makefile('rb')
    while raw := reader.readline(LIMIT + 1):
        require(len(raw) <= LIMIT and raw.endswith(b'\n'), 'Limit polecenia aktora')
        operation = json.loads(raw)['operation']
        result = {'operation': operation}
        if operation == 'baseline':
            result['writes'], result['mmap'], result['creates'] = [], [], []
            for root in roots:
                fd = os.open(root / 'held.bin', os.O_RDWR | os.O_NOFOLLOW)
                files.append(fd)
                result['writes'].append(attempt(lambda fd=fd: write_marker(fd, b'BASELINE\n')))
                fd = os.open(root / 'mapping.bin', os.O_RDWR | os.O_NOFOLLOW)
                try:
                    mapped = attempt(lambda: mmap.mmap(fd, 4096, flags=mmap.MAP_SHARED,
                                                       prot=mmap.PROT_READ | mmap.PROT_WRITE, trackfd=False))
                    if mapped['errno'] is None:
                        view = mapped['value']
                        mappings.append(view)
                        view[0:4] = b'BASE'
                        view.flush()
                        mapped['value'] = 'mapped'
                    result['mmap'].append(mapped)
                finally:
                    os.close(fd)
                def created(root=root):
                    fd = os.open(root / 'new.bin', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
                    os.close(fd)
                    return 0
                result['creates'].append(attempt(created))
        elif operation == 'held':
            result['writes'] = [attempt(lambda fd=fd: write_marker(fd, b'HELD\n')) for fd in files]
        elif operation == 'close_fd':
            for fd in files:
                os.close(fd)
            files.clear()
            result['open_fds'] = 0
            result['mappings'] = len(mappings)
            result['actual_fds'] = [os.readlink(path) for path in Path('/proc/self/fd').iterdir()
                                   if path.exists() and any(str(root) in os.readlink(path) for root in roots)]
        elif operation == 'map':
            def change(view):
                view[0:4] = b'POST'
                view.flush()
                return 4
            result['writes'] = [attempt(lambda view=view: change(view)) for view in mappings]
        elif operation == 'unmap':
            for view in mappings:
                view.close()
            mappings.clear()
            result['mappings'] = 0
        elif operation == 'reopen':
            result['opens'] = []
            for root in roots:
                outcomes = {}
                for name, flags, file in [('wronly', os.O_WRONLY, 'held.bin'),
                                          ('rdwr', os.O_RDWR, 'held.bin'),
                                          ('create', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 'new.bin'),
                                          ('truncate', os.O_WRONLY | os.O_TRUNC, 'truncate.bin')]:
                    def opened(root=root, flags=flags, file=file):
                        fd = os.open(root / file, flags | os.O_NOFOLLOW, 0o600)
                        try:
                            return write_marker(fd, b'REOPEN\n')
                        finally:
                            os.close(fd)
                    outcomes[name] = attempt(opened)
                result['opens'].append({'path': str(root), 'mnt_ns': os.stat('/proc/self/ns/mnt').st_ino,
                                        'outcomes': outcomes})
        elif operation == 'direct':
            fd = os.open(backing / 'direct.bin', os.O_RDWR | os.O_NOFOLLOW)
            try:
                result['write'] = attempt(lambda: write_marker(fd, b'DIRECT_BACKING\n'))
            finally:
                os.close(fd)
        elif operation == 'stop':
            return
        else:
            raise ValueError('Obca operacja aktora')
        result['snapshot'] = snapshot(roots)
        channel.sendall(json.dumps(result).encode() + b'\n')


def start_time(pid):
    return Path(f'/proc/{pid}/stat').read_text().rsplit(') ', 1)[1].split()[19]


class Actor:
    def __init__(self, roots, backing, namespace, lock):
        parent, child = socket.socketpair()
        self.pid = os.fork()
        if self.pid == 0:
            parent.close()
            os.dup2(child.fileno(), 1)
            os.dup2(child.fileno(), 2)
            try:
                actor_loop(child, roots, backing, namespace)
                os._exit(0)
            except BaseException as error:
                child.sendall(json.dumps({'fatal': type(error).__name__, 'detail': str(error)}).encode() + b'\n')
                os._exit(1)
        child.close()
        self.channel, self.start, self.reaped = parent, start_time(self.pid), False

    def receive(self):
        data = bytearray()
        deadline = time.monotonic() + 30
        while not data.endswith(b'\n'):
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.channel], [], [], remaining)[0]:
                return {'raw': base64.b64encode(data).decode(), 'error': 'timeout'}
            block = self.channel.recv(min(4096, LIMIT + 1 - len(data)))
            if not block:
                pid, status = os.waitpid(self.pid, os.WNOHANG)
                self.reaped = bool(pid)
                return {'raw': base64.b64encode(data).decode(), 'error': 'eof',
                        'wait_status': status if pid else None,
                        'exit_code': os.waitstatus_to_exitcode(status) if pid else None, 'alive': not bool(pid)}
            data.extend(block)
            if len(data) > LIMIT:
                return {'raw': base64.b64encode(data).decode(), 'error': 'limit'}
        return {'raw': base64.b64encode(data).decode(), 'error': None}

    def send(self, operation):
        self.channel.sendall(json.dumps({'operation': operation}).encode() + b'\n')

    def stop(self):
        if not self.reaped:
            require(start_time(self.pid) == self.start, 'Podmieniony proces aktora')
            os.kill(self.pid, signal.SIGTERM)
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                pid, status = os.waitpid(self.pid, os.WNOHANG)
                if pid:
                    self.reaped = True
                    break
                select.select([], [], [], 0.02)
            if not self.reaped:
                require(start_time(self.pid) == self.start, 'Podmieniony proces aktora')
                os.kill(self.pid, signal.SIGKILL)
                deadline = time.monotonic() + 2
                while time.monotonic() < deadline:
                    pid, status = os.waitpid(self.pid, os.WNOHANG)
                    if pid:
                        self.reaped = True
                        break
                    select.select([], [], [], 0.02)
        self.channel.close()
        return {'pid': self.pid, 'start': self.start, 'alive': not self.reaped}


def validate_actor(value, namespace, parent_uid):
    require(isinstance(value, dict), 'Nieprawidłowy handshake aktora')
    require(not value.get('fatal'), f"Błąd inicjalizacji aktora: {value.get('fatal')}: {value.get('detail')}")
    require(isinstance(value.get('before'), dict) and isinstance(value.get('after'), dict)
            and 'namespace_errno' in value, 'Niepełny handshake aktora')
    before, after = value['before'], value['after']
    require(value['namespace_errno'] is None and before['uid'] == USER_UID
            and before['euid'] == USER_UID and int(before['caps'], 16) == 0
            and parent_uid == USER_UID, 'Niepotwierdzony UID aktora')
    if namespace:
        require(value.get('setup', {}).get('dumpable_before_unshare') == 1
                and value['setup'].get('core_limit') == [0, 0], 'Niepotwierdzone przygotowanie userns')
        require(after['uid'] == 0 and after['uid_map'].split() == ['0', str(USER_UID), '1']
                and after['gid_map'].split() == ['0', str(USER_UID), '1']
                and before['user_ns'] != after['user_ns'] and before['mnt_ns'] != after['mnt_ns'],
                'Niepotwierdzony userns/mountns')
    else:
        require(before == after, 'Nieoczekiwana namespace')


def evaluate_global(remount, snapshots, opens, expected):
    if remount['errno'] is not None:
        return False
    key = lambda row: (row['mnt_ns'], row['path'])
    if len(expected) != 3 or len(snapshots) != 3 or len(opens) != 3:
        return False
    wanted = {key(row): row for row in expected}
    if len(wanted) != 3 or {key(row) for row in snapshots} != set(wanted) or {key(row) for row in opens} != set(wanted):
        return False
    def same_mount(row):
        before = wanted[key(row)]
        fields = ('id', 'number', 'root', 'target', 'filesystem', 'source')
        return len(before['mounts']) == 1 and len(row['mounts']) == 1 and all(
            row['mounts'][0][field] == before['mounts'][0][field] for field in fields
        ) and row['stat']['value']['device'] == before['stat']['value']['device']
    return all(
        len(row['mounts']) == 1 and 'ro' in row['mounts'][0]['super_options']
        and row['stat']['errno'] is None and row['stat']['value']['readonly'] and same_mount(row) for row in snapshots
    ) and all(set(row['outcomes']) == {'wronly', 'rdwr', 'create', 'truncate'}
              and all(item['errno'] == errno.EROFS for item in row['outcomes'].values()) for row in opens)


def backing(kind):
    return TREE / kind / 'cache'


def options(kind):
    return {'branches': f'{TREE / kind / "cache"}=RW:{TREE / kind / "data"}=NC',
            'category.create': 'mfs', 'cache.files': 'off', 'minfreespace': str(20 * 1024**3),
            'moveonenospc': 'mfs', 'func.getattr': 'newest', 'nullrw': 'false', 'cache.writeback': 'false'}


def own_mounts():
    return [row for row in mount_rows() if Path(row['target']).is_relative_to(TREE)]


def guard(storage, state=None):
    private_parents(BASE)
    private_parents(TREE)
    observation = storage.observe()
    root = validate_observation(observation)
    storage.automation_guard(VM_UUID)
    info = Path('/usr/bin/mergerfs').lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022, 'Obca binarka')
    require(hashlib.sha256(Path('/usr/bin/mergerfs').read_bytes()).hexdigest() == MERGERFS_SHA, 'SHA mergerfs')
    require(storage.space(Path('/'))['available'] >= 2 * 1024**3, 'Rezerwa OS')
    if state is None:
        require(not os.path.lexists(BASE) and not os.path.lexists(TREE) and not own_mounts(), 'Stanowisko już istnieje')
    else:
        require(state['uuid'] == VM_UUID and state['boot_id'] == BOOT_ID and state['root'] == root,
                'Obcy zapisany stan')
        require(own_mounts() == state['mounts'], 'Zmienione montowania')
        require(stat.S_IMODE(BASE.lstat().st_mode) == 0o700, 'Tryb katalogu stanu')
        for path, wanted in state['directories'].items():
            require(Path(path) in (BASE, TREE), 'Obcy katalog stanu')
            private_parents(Path(path))
            value = Path(path).lstat()
            require(stat.S_ISDIR(value.st_mode) and value.st_uid == 0
                    and [value.st_dev, value.st_ino] == wanted and not value.st_mode & 0o022, 'Zmieniony katalog')
    return observation


def payload():
    result = {}
    for kind in ('local', 'global'):
        require(not any((TREE / kind / 'data').iterdir()), 'Nieoczekiwany obiekt data NC')
        names = ('held.bin', 'mapping.bin', 'truncate.bin', 'direct.bin')
        require({path.name for path in backing(kind).iterdir()} == set(names), 'Inny zestaw payload')
        for name in names:
            path = backing(kind) / name
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NOATIME)
            try:
                info = os.fstat(fd)
                require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_uid == USER_UID
                        and info.st_size <= 1024**2 and info.st_dev == Path('/').stat().st_dev, 'Obcy payload')
                raw = os.read(fd, 1024**2 + 1)
                require(len(raw) == info.st_size, 'Niepełny payload')
                result[str(path)] = {'bytes': info.st_size, 'device': info.st_dev, 'inode': info.st_ino,
                                     'uid': info.st_uid, 'mode': stat.S_IMODE(info.st_mode), 'nlink': info.st_nlink,
                                     'sha256': hashlib.sha256(raw).hexdigest()}
            finally:
                os.close(fd)
    return result


def verify_payload(measured, expected):
    require(set(measured) == set(expected), 'Inny zestaw zachowanych plików')
    for path, row in measured.items():
        wanted = expected[path]
        content = base64.b64decode(wanted['content'])
        require(all(row[field] == wanted['identity'][field] for field in ('device', 'inode', 'uid', 'mode', 'nlink')),
                'Inna tożsamość backing')
        require(row['bytes'] == len(content) and row['sha256'] == hashlib.sha256(content).hexdigest(), 'Inna treść backing')


def expected_markers(expected, actor, operation, value):
    result = json.loads(json.dumps(expected))
    def change(name, marker, replace=False):
        row = result[str(actor.backing / name)]
        content = base64.b64decode(row['content'])
        row['content'] = base64.b64encode(marker if replace else content + marker).decode()
    if operation in ('baseline', 'held', 'map'):
        marker = {'baseline': b'BASELINE\n', 'held': b'HELD\n', 'map': b'POST'}[operation]
        count = actor.mapping_count if operation == 'map' else len(actor.roots)
        require(len(value['writes']) == count, 'Niepełny wynik write')
        for row in value['writes']:
            if row['errno'] is None:
                require(row['value'] == len(marker), 'Inna liczba bajtów markera')
                if operation == 'map':
                    wanted = result[str(actor.backing / 'mapping.bin')]
                    wanted['content'] = base64.b64encode(marker + base64.b64decode(wanted['content'])[4:]).decode()
                else:
                    change('held.bin', marker)
        if operation == 'baseline':
            require(len(value['mmap']) == len(actor.roots) and len(value['creates']) == len(actor.roots), 'Niepełny baseline')
            for row in value['mmap']:
                if row['errno'] is None:
                    require(row['value'] == 'mapped', 'Brak mapowania')
                    wanted = result[str(actor.backing / 'mapping.bin')]
                    wanted['content'] = base64.b64encode(b'BASE' + base64.b64decode(wanted['content'])[4:]).decode()
    elif operation == 'reopen':
        require(len(value['opens']) == len(actor.roots)
                and {(row['mnt_ns'], row['path']) for row in value['opens']}
                == {(actor.mnt_ns, str(path)) for path in actor.roots}, 'Niepełny wynik reopen')
        for row in value['opens']:
            require(set(row['outcomes']) == {'wronly', 'rdwr', 'create', 'truncate'}, 'Niepełne tryby reopen')
            for name, outcome in row['outcomes'].items():
                if outcome['errno'] is None:
                    require(name != 'create' and outcome['value'] == 7, 'Nieoczekiwany create lub zapis reopen')
                    change('truncate.bin' if name == 'truncate' else 'held.bin', b'REOPEN\n', name == 'truncate')
    elif operation == 'direct' and value['write']['errno'] is None:
        require(value['write']['value'] == len(b'DIRECT_BACKING\n'), 'Niepełny direct marker')
        change('direct.bin', b'DIRECT_BACKING\n')
    return result


class Runner:
    def __init__(self, state, lock):
        self.state, self.lock, self.deadline, self.actors = state, lock, time.monotonic() + 180, []

    def record(self, label, value):
        name = f'{self.state["phase"]}-{len(self.state["events"]):03d}-{label}.json'
        self.state['proof_bytes'] += len(json.dumps(value).encode())
        require(self.state['proof_bytes'] <= 2 * 1024**2, 'Limit dowodów')
        durable_new(BASE / name, value)
        self.state['events'].append(name)
        persist(self.state)

    def begin(self, label):
        require(time.monotonic() < self.deadline, 'Limit fazy')
        require(own_mounts() == self.state['mounts'], 'Zmieniony mount przed operacją')
        pending(self.state, label)

    def command(self, args, label, daemon=False):
        self.begin(label)
        process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd='/',
                                   env={'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL': 'C'},
                                   pass_fds=() if daemon else (self.lock,))
        error = None
        try:
            output, errors = process.communicate(timeout=min(30, max(0.1, self.deadline - time.monotonic())))
        except subprocess.TimeoutExpired as timeout:
            process.kill()
            try:
                output, errors = process.communicate(timeout=2)
            except subprocess.TimeoutExpired:
                output, errors = timeout.output or b'', timeout.stderr or b''
            error = 'timeout'
        self.state['mounts'] = own_mounts()
        self.record(label, {'argv': args, 'stdout': base64.b64encode(output[:LIMIT]).decode(),
                            'stderr': base64.b64encode(errors[:LIMIT]).decode(), 'rc': process.returncode,
                            'error': error, 'truncated': len(output) + len(errors) > LIMIT,
                            'mounts': self.state['mounts']})
        require(not error and len(output) + len(errors) <= LIMIT and process.returncode == 0
                and not errors, 'Odmowa narzędzia ' + label)
        return output

    def mount(self, kind, action):
        target = TREE / kind / 'union'
        row = next(row for row in own_mounts() if row['target'] == str(target))
        preserve = sum(bit for name, bit in [('nosuid', 2), ('nodev', 4), ('noexec', 8),
                                            ('noatime', 1024), ('nodiratime', 2048), ('relatime', 1 << 21)]
                       if name in row['mount_options'])
        flags = preserve | MS_REMOUNT | MS_RDONLY | (MS_BIND if action == 'local_ro' else 0)
        code = ('import ctypes,json,sys; c=ctypes.CDLL(None,use_errno=True); '
                'r=c.umount2(sys.argv[1].encode(),0) if sys.argv[3]=="unmount" else '
                'c.mount(None,sys.argv[1].encode(),None,ctypes.c_ulong(int(sys.argv[2])),None); '
                'print(json.dumps({"errno":ctypes.get_errno() if r else None,"return":r}))')
        raw = self.command(['/usr/bin/python3', '-c', code, str(target), str(flags), action], action)
        return json.loads(raw)

    def actor(self, kind, namespace, roots):
        self.begin('actor-start')
        actor = Actor(roots, backing(kind), namespace, self.lock)
        self.actors.append(actor)
        raw = actor.receive()
        self.record('actor-raw', {'pid': actor.pid, 'start': actor.start, 'response': raw})
        status = Path(f'/proc/{actor.pid}/status').read_text() if not actor.reaped else ''
        self.record('actor-start', {'pid': actor.pid, 'start': actor.start, 'response': raw, 'status': status})
        require(raw['error'] is None, 'Aktor nie gotowy')
        value = json.loads(base64.b64decode(raw['raw']))
        parent_uid = int(next(line for line in status.splitlines() if line.startswith('Uid:')).split()[1])
        validate_actor(value, namespace, parent_uid)
        actor.mnt_ns, actor.roots = value['after']['mnt_ns'], roots
        actor.backing, actor.mapping_count = backing(kind), 0
        return actor

    def request(self, actor, operation):
        self.begin(operation)
        actor.send(operation)
        raw = actor.receive()
        self.record(operation, {'pid': actor.pid, 'response': raw})
        measured = payload()
        self.record(operation + '-payload', measured)
        require(raw['error'] is None, 'Brak wyniku aktora')
        value = json.loads(base64.b64decode(raw['raw']))
        require(value.get('operation') == operation and 'fatal' not in value, 'Błąd aktora')
        expected = expected_markers(self.state['expected'], actor, operation, value)
        verify_payload(measured, expected)
        self.state['expected'] = expected
        if operation == 'baseline':
            actor.mapping_count = sum(row['errno'] is None for row in value['mmap'])
        elif operation == 'unmap':
            require(value['mappings'] == 0, 'Pozostało mapowanie')
            actor.mapping_count = 0
        persist(self.state)
        return value

    def close(self):
        outcomes = []
        for actor in self.actors:
            try:
                outcomes.append(actor.stop())
            except BaseException as error:
                outcomes.append({'pid': actor.pid, 'alive': True, 'error': str(error)})
        self.record('actors-stop', outcomes)
        require(not any(row['alive'] for row in outcomes), 'Niepotwierdzone zatrzymanie dzieci')


def prepare(runner):
    runner.begin('directories')
    TREE.mkdir(mode=0o755)
    runner.state['directories'][str(TREE)] = [TREE.stat().st_dev, TREE.stat().st_ino]
    for kind in ('local', 'global'):
        (TREE / kind).mkdir(mode=0o755)
        for name in ('cache', 'data', 'union', 'bind'):
            path = TREE / kind / name
            path.mkdir(mode=0o755)
            require(path.stat().st_dev == Path('/').stat().st_dev, 'Katalog poza OS')
            if name in ('cache', 'data'):
                os.chown(path, USER_UID, USER_UID)
        for name in ('held.bin', 'mapping.bin', 'truncate.bin', 'direct.bin'):
            fd = os.open(backing(kind) / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            try:
                os.fchown(fd, USER_UID, USER_UID)
                write_all(fd, b'B' * 4096)
                os.fsync(fd)
            finally:
                os.close(fd)
        flush_directory(backing(kind))
        args = ['allow_other', 'category.create=mfs', 'cache.files=off', 'minfreespace=20G',
                'moveonenospc=true', 'func.getattr=newest']
        runner.command(['/usr/bin/mergerfs', '-o', ','.join(args), options(kind)['branches'],
                        str(TREE / kind / 'union')], 'mergerfs-' + kind, daemon=True)
        actual = {key: os.getxattr(TREE / kind / 'union' / '.mergerfs', 'user.mergerfs.' + key).decode()
                  for key in options(kind)}
        runner.record('options-' + kind, actual)
        require(actual == options(kind), 'Inne opcje FUSE')
        runner.command(['/usr/bin/mount', '--make-private', str(TREE / kind / 'union')], 'private-' + kind)
        runner.command(['/usr/bin/mount', '--bind', str(TREE / kind / 'union'), str(TREE / kind / 'bind')], 'bind-' + kind)
        runner.command(['/usr/bin/mount', '--make-private', str(TREE / kind / 'bind')], 'private-bind-' + kind)
    require((TREE / 'local/union').stat().st_dev != (TREE / 'global/union').stat().st_dev, 'Wspólny FUSE device')
    runner.state['fuse_devices'] = {kind: (TREE / kind / 'union').stat().st_dev for kind in ('local', 'global')}
    measured = payload()
    runner.state['expected'] = {path: {'identity': row, 'content': base64.b64encode(b'B' * 4096).decode()}
                                for path, row in measured.items()}
    runner.record('prepared-payload', measured)
    verify_payload(measured, runner.state['expected'])


def run_measurement(runner, kind):
    union, alias = TREE / kind / 'union', TREE / kind / 'bind'
    if kind == 'local':
        initial = runner.actor(kind, False, [union])
        baseline = runner.request(initial, 'baseline')
        require(all(row['errno'] is None for row in baseline['writes']), 'Brak baseline głównej unii')
        runner.request(initial, 'close_fd')
        runner.request(initial, 'unmap')
    actors = [runner.actor(kind, False, [alias] if kind == 'local' else [union, alias]),
              runner.actor(kind, True, [union])]
    baselines = [runner.request(actor, 'baseline') for actor in actors]
    require(all(row['errno'] is None for result in baselines for row in result['writes']), 'Brak baseline aliasu')
    require(all(row['errno'] in (errno.ENOSPC, errno.EROFS) for result in baselines for row in result['creates']),
            'Niepotwierdzony policy create baseline')
    expected = [row for result in baselines for row in result['snapshot']]
    wanted = {(actor.mnt_ns, str(path)) for actor in actors for path in actor.roots}
    require(len(expected) == len(wanted) and {(row['mnt_ns'], row['path']) for row in expected} == wanted,
            'Niepełne aliasy baseline')
    require(all(len(row['mounts']) == 1 and row['mounts'][0]['filesystem'] == 'fuse.mergerfs'
                and row['mounts'][0]['source'] == 'cache:data' and row['mounts'][0]['root'] == '/'
                and 'rw' in row['mounts'][0]['mount_options'] and 'rw' in row['mounts'][0]['super_options']
                and row['stat']['errno'] is None
                and row['stat']['value']['device'] == runner.state['fuse_devices'][kind] for row in expected),
            'Obca instancja FUSE baseline')
    mapped = [row for result in baselines for row in result['mmap']]
    require(all(row['errno'] in (None, errno.ENODEV) for row in mapped), 'Nieznany błąd mmap baseline')
    if kind == 'local':
        remount = runner.mount(kind, 'local_ro')
        observations = [runner.request(actor, 'held') for actor in actors]
        unmount = runner.mount(kind, 'unmount')
        reopened = [runner.request(actor, 'reopen') for actor in actors]
        runner.record('local-result', {'remount': remount, 'unmount': unmount, 'held': observations, 'reopen': reopened})
        require(remount['errno'] is None and unmount['errno'] is None
                and all(row['errno'] is None for result in observations for row in result['writes'])
                and all(row['outcomes'][name]['errno'] is None for result in reopened for row in result['opens']
                        for name in ('wronly', 'rdwr', 'truncate')), 'Niepotwierdzona kontrola per-mount')
        return {'scope': 'per_mount', 'remount': remount, 'unmount': unmount,
                'held': observations, 'reopen': reopened, 'gate_supported': False}
    outcomes = []
    first = runner.mount(kind, 'global_ro')
    held = [runner.request(actor, 'held') for actor in actors]
    outcomes.append({'stage': 'held_fd', 'remount': first, 'writes': held})
    runner.record('held-fd', outcomes[-1])
    require(first['errno'] in (None, errno.EBUSY), 'Nieznana odmowa global RO')
    if first['errno'] == errno.EBUSY:
        require(all(row['errno'] is None for value in held for row in value['writes']), 'Błąd held write po EBUSY')
    closed = [runner.request(actor, 'close_fd') for actor in actors]
    require(all(row['open_fds'] == 0 and not row['actual_fds'] for row in closed), 'Pozostały FD aktora')
    if first['errno'] is None:
        outcomes.append({'stage': 'unexpected_ro_with_writer', 'maps': [runner.request(actor, 'map') for actor in actors]})
        runner.record('unexpected-ro-with-writer', outcomes)
        raise ValueError('RO przy held writer; wymaga odrębnej analizy')
    if any(row['mappings'] for row in closed):
        second = runner.mount(kind, 'global_ro')
        outcomes.append({'stage': 'mmap_only', 'remount': second,
                         'writes': [runner.request(actor, 'map') for actor in actors]})
        runner.record('mmap-only', outcomes[-1])
        require(second['errno'] == errno.EBUSY, 'Niepotwierdzona odmowa przy writable mmap')
        require(all(row['errno'] is None for value in outcomes[-1]['writes'] for row in value['writes']), 'Błąd mmap write po EBUSY')
    for actor in actors:
        runner.request(actor, 'unmap')
    last = runner.mount(kind, 'global_ro')
    reopened = [runner.request(actor, 'reopen') for actor in actors]
    snapshots = [row for result in reopened for row in result['snapshot']]
    opens = [row for result in reopened for row in result['opens']]
    supported = evaluate_global(last, snapshots, opens, expected)
    direct = runner.request(actors[0], 'direct')
    result = {'scope': 'FUSE_only', 'stages': outcomes, 'remount_without_writers': last,
              'reopen': reopened, 'direct_backing': direct, 'gate_supported': supported,
              'mmap_tested': all(row['errno'] is None for row in mapped)}
    runner.record('global-result', result)
    require(supported and direct['write']['errno'] is None, 'Globalna bramka niepotwierdzona')
    return result


def main(argv):
    if argv == ['--help']:
        print('Użycie: sudo python3 - preflight|prepare|local|global < guest_writer_gate_probe.py')
        return 0
    report, runner, lock = {'status': 'refused', 'mutating_authority': False}, None, None
    try:
        require(argv in [[phase] for phase in ('preflight',) + PHASES] and os.geteuid() == 0, 'Argument/UID')
        phase = argv[0]
        storage = load_storage()
        if phase in ('preflight', 'prepare'):
            observation = guard(storage)
            if phase == 'preflight':
                print(json.dumps({'status': 'measured', 'mutating_authority': False, 'observation': observation}))
                return 0
            BASE.mkdir(mode=0o700)
            lock = acquire_lock(True)
            flush_directory(BASE.parent)
            state = {'uuid': VM_UUID, 'boot_id': BOOT_ID, 'root': validate_observation(observation),
                     'stage': None, 'pending': None, 'phase': phase, 'events': [], 'proof_bytes': 0,
                     'directories': {str(BASE): [BASE.stat().st_dev, BASE.stat().st_ino]}, 'mounts': []}
            persist(state)
        else:
            lock = acquire_lock()
            state = json.loads(read_private(BASE / 'state.json', 2 * 1024**2))
            require(state['pending'] is None and state['stage'] == PHASES[PHASES.index(phase) - 1], 'Pending lub powtórzenie fazy')
            guard(storage, state)
            verify_payload(payload(), state['expected'])
            state['phase'] = phase
        runner = Runner(state, lock)
        report['mutating_authority'] = True
        result = prepare(runner) if phase == 'prepare' else run_measurement(runner, phase)
        runner.close()
        guard(storage, state)
        state.update(stage=phase, pending=None)
        persist(state)
        report.update(status='measured', phase=phase, result=result)
    except BaseException as error:
        report['error'] = type(error).__name__ + ': ' + str(error)
        if runner is not None:
            try:
                runner.close()
            except BaseException as cleanup:
                report['children_error'] = str(cleanup)
    finally:
        if lock is not None:
            os.close(lock)
    print(json.dumps(report, sort_keys=True))
    return 0 if report['status'] == 'measured' else 1


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
