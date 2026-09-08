# =============================================================================
# Plik: guest_branch_isolation_probe.py
# Opis: Jednorazowy pomiar prywatnych gałęzi i opublikowanego FUSE w osobnej VM.
# Przykład: sudo python3 - preflight < guest_branch_isolation_probe.py
# =============================================================================

import array
import base64
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import resource
import select
import socket
import stat
import subprocess
import sys
import time
import types

VM_UUID = '77b5de74-031a-4d95-9e36-9d52b9e8ee96'
BOOT_ID = 'c32a49c0-a9b1-46a6-94a5-bc96242b9af5'
PREFIX = 'tn-77b5de7403-'
BASE = Path('/var/lib/tentanas-branch-isolation')
PRIVATE = Path('/mnt/tentanas-branch-private')
PUBLIC = Path('/mnt/tentanas-branch-union')
ALIAS = Path('/mnt/tentanas-branch-bind')
A0_PATH = Path('/root/tentanas-e2-audit/guest_writer_gate_probe.py')
A0_SHA = 'e20611d9dc12661155d5660e49f31c40f3ab7cb5e3c6c07279c512705561dedb'
LIMIT = 65536
FUSE_MAGIC = 0x65735546
a0 = None


def require(value, message):
    if not value:
        raise ValueError(message)


def load_helpers():
    for path in [A0_PATH, *A0_PATH.parents]:
        info = path.lstat()
        require(info.st_uid == 0 and not info.st_mode & 0o022 and not stat.S_ISLNK(info.st_mode),
                'Obcy moduł lub przodek importu')
    require(stat.S_ISREG(A0_PATH.lstat().st_mode) and A0_PATH.lstat().st_nlink == 1,
            'Typ modułu importu')
    fd = os.open(A0_PATH, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        raw = stream.read(1024**2 + 1)
    require(hashlib.sha256(raw).hexdigest() == A0_SHA, 'SHA modułu A0')
    module = types.ModuleType('branch_probe_a0')
    exec(compile(raw, str(A0_PATH), 'exec'), module.__dict__)
    return module


def guard(observation, storage):
    require(observation['uuid'] == VM_UUID and observation['boot_id'] == BOOT_ID, 'Obca VM/boot A1')
    sizes = {'os': 12, 'data1': 32, 'data2': 32, 'parity': 40, 'cache': 1, 'spare': 40}
    expected = {'uuid': VM_UUID, 'disks': {role: {'serial': PREFIX + role, 'bytes': size * 1024**3}
                                         for role, size in sizes.items()}}
    storage.validate(expected, observation, {}, BASE)
    storage.automation_guard(VM_UUID)
    require(os.uname().machine == 'x86_64' and storage.space(Path('/'))['available'] >= 2 * 1024**3,
            'Architektura lub rezerwa OS')
    require(hashlib.sha256(Path('/usr/bin/mergerfs').read_bytes()).hexdigest() == a0.MERGERFS_SHA,
            'SHA mergerfs')
    for path in (BASE, PRIVATE, PUBLIC, ALIAS):
        a0.private_parents(path)
        require(not os.path.lexists(path), 'Stanowisko A1 już istnieje')
    return expected


class Proof:
    def __init__(self):
        self.deadline = time.monotonic() + 180
        self.state = {'uuid': VM_UUID, 'boot_id': BOOT_ID, 'pending': None, 'events': [],
                      'tmpfs_volatile': True, 'mergerfs_preserved': False}

    def save(self):
        a0.durable_new(BASE / 'state.next', self.state)
        os.replace(BASE / 'state.next', BASE / 'state.json')
        a0.flush_directory(BASE)

    def begin(self, phase):
        self.check_deadline()
        self.state['pending'] = phase
        self.save()

    def check_deadline(self):
        require(time.monotonic() < self.deadline, 'Przekroczono budżet 180 s A1')

    def record(self, label, value):
        require(len(self.state['events']) < 160, 'Limit zdarzeń A1')
        name = f'{len(self.state["events"]):03d}-{label}.json'
        a0.durable_new(BASE / name, value)
        self.state['events'].append(name)
        self.save()


def close_other_fds(allowed):
    keep = {0, 1, 2, *allowed}
    for name in os.listdir('/proc/self/fd'):
        fd = int(name)
        if fd not in keep:
            try:
                os.close(fd)
            except OSError as error:
                if error.errno != errno.EBADF:
                    raise
    return {name: os.readlink('/proc/self/fd/' + name) for name in os.listdir('/proc/self/fd')
            if os.path.exists('/proc/self/fd/' + name)}


class Child:
    def __init__(self, entry, args=(), allowed_fds=()):
        parent, child = socket.socketpair()
        self.child_fd = child.fileno()
        self.pid = os.fork()
        if self.pid == 0:
            parent.close()
            try:
                null = os.open('/dev/null', os.O_RDWR)
                for fd in (0, 1, 2):
                    os.dup2(null, fd)
                if null > 2:
                    os.close(null)
                os.chdir('/')
                descriptors = close_other_fds((child.fileno(), *allowed_fds))
                resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
                entry(child, descriptors, *args)
                os._exit(0)
            except BaseException as error:
                try:
                    send(child, {'fatal': type(error).__name__, 'detail': str(error),
                                 'errno': getattr(error, 'errno', None)})
                finally:
                    os._exit(1)
        child.close()
        self.channel, self.start, self.reaped = parent, a0.start_time(self.pid), False

    def receive(self):
        return a0.Actor.receive(self)

    def send(self, operation):
        a0.Actor.send(self, operation)

    def stop(self):
        return a0.Actor.stop(self)


def send(channel, value):
    raw = json.dumps(value).encode() + b'\n'
    require(len(raw) <= LIMIT, 'Limit komunikatu A1')
    channel.sendall(raw)


def receive_mount_fd(channel, expected_device, record):
    require(select.select([channel], [], [], 30)[0], 'Timeout deskryptora FUSE')
    raw, ancillary, flags, _ = channel.recvmsg(1, socket.CMSG_SPACE(4 * array.array('i').itemsize))
    descriptors = []
    try:
        for level, kind, payload in ancillary:
            if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                values = array.array('i')
                values.frombytes(payload[:len(payload) - len(payload) % values.itemsize])
                descriptors.extend(values)
        record({'raw': base64.b64encode(raw).decode(), 'flags': flags, 'fds': descriptors,
                'ancillary': [(level, kind, len(payload)) for level, kind, payload in ancillary]})
        observed = [{'fd': fd, 'device': os.fstat(fd).st_dev, 'type': filesystem_type(fd)}
                    for fd in descriptors]
        record({'raw': base64.b64encode(raw).decode(), 'flags': flags, 'descriptors': observed,
                'ancillary': [(level, kind, len(payload)) for level, kind, payload in ancillary]})
        require(all(level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS
                    and len(payload) % array.array('i').itemsize == 0 for level, kind, payload in ancillary),
                'Obce ancillary')
        require(raw == b'M' and not flags & (socket.MSG_CTRUNC | socket.MSG_TRUNC)
                and len(descriptors) == 1, 'Niepełny lub wielokrotny FD mounta')
        fd = descriptors[0]
        require(observed[0]['type'] == FUSE_MAGIC and observed[0]['device'] == expected_device,
                'Odebrany FD nie jest przypiętym FUSE')
        os.set_inheritable(fd, False)
        return descriptors.pop()
    finally:
        for fd in descriptors:
            os.close(fd)


def filesystem_type(fd):
    value = (ctypes.c_long * 32)()
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.fstatfs(fd, ctypes.byref(value)) != 0:
        raise OSError(ctypes.get_errno(), 'fstatfs')
    return value[0]


def isolation_result(rows, expected):
    wanted = {(row['target'], row['operation']) for row in expected}
    require(bool(wanted) and len(wanted) == len(expected) and len(rows) == len(wanted)
            and {(row['target'], row['operation']) for row in rows} == wanted, 'Niepełna macierz izolacji')
    return all(row['errno'] in ((errno.EACCES, errno.ENOENT) if row['target'] == 'host_path'
                               else (errno.EACCES, errno.EPERM))
               and row['value'] is None for row in rows)


def operation(channel):
    raw = bytearray()
    deadline = time.monotonic() + 30
    while not raw.endswith(b'\n'):
        remaining = deadline - time.monotonic()
        require(remaining > 0 and select.select([channel], [], [], remaining)[0], 'Timeout polecenia')
        chunk = channel.recv(1)
        require(chunk and len(raw) < LIMIT, 'Niepełne polecenie')
        raw.extend(chunk)
    return json.loads(raw)['operation']


def event(channel, label, value):
    send(channel, {'event': label, 'value': value})
    require(operation(channel) == 'ack', 'Brak trwałego potwierdzenia dowodu')


def collect(child, proof):
    while True:
        response = child.receive()
        proof.record('child-raw', {'pid': child.pid, 'start': child.start, 'response': response})
        proof.check_deadline()
        require(response['error'] is None, 'Brak odpowiedzi dziecka A1')
        value = json.loads(base64.b64decode(response['raw']))
        require('fatal' not in value, f'Błąd dziecka A1: {value}')
        if 'event' not in value:
            return value
        proof.record(value['event'], value['value'])
        if value['event'] == 'mergerfs-start':
            proof.state.update(mergerfs_preserved=True, mergerfs=value['value'])
            proof.save()
        child.send('ack')


def exchange(child, proof, command):
    proof.begin(command)
    child.send(command)
    return collect(child, proof)


def syscall(channel, label, function):
    ctypes.set_errno(0)
    result = function()
    observed = {'return': result, 'errno': ctypes.get_errno() if result < 0 else None}
    event(channel, label, observed)
    require(result >= 0, 'Odmowa ' + label)
    return result


def metric(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NOATIME)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_size <= 65536,
                'Obcy plik pomiaru')
        raw = stream.read(65537)
    return {'device': info.st_dev, 'inode': info.st_ino, 'bytes': info.st_size,
            'sha256': hashlib.sha256(raw).hexdigest(), 'uid': info.st_uid, 'mtime_ns': info.st_mtime_ns,
            'mode': stat.S_IMODE(info.st_mode), 'nlink': info.st_nlink}


def worker(channel, descriptors):
    libc = ctypes.CDLL(None, use_errno=True)
    before = a0.identity()
    syscall(channel, 'worker-unshare', lambda: libc.unshare(0x00020000))
    syscall(channel, 'worker-private', lambda: libc.mount(None, b'/', None, ctypes.c_ulong((1 << 18) | 16384), None))
    event(channel, 'worker-namespace', {'before': before, 'after': a0.identity(), 'fds': descriptors,
                                      'mountinfo': Path('/proc/self/mountinfo').read_text()})
    require(before['mnt_ns'] != a0.identity()['mnt_ns']
            and all(not row['propagation'] for row in a0.mount_rows()), 'Namespace workera nie jest prywatna')
    syscall(channel, 'worker-tmpfs', lambda: libc.mount(b'tmpfs', str(PRIVATE).encode(), b'tmpfs',
                                                       ctypes.c_ulong(2 | 4), b'size=8388608,mode=0755'))
    tmpfs = {'mounts': a0.mount_rows(), 'device': PRIVATE.stat().st_dev,
             'bytes': os.statvfs(PRIVATE).f_blocks * os.statvfs(PRIVATE).f_frsize}
    event(channel, 'worker-tmpfs-state', tmpfs)
    require(tmpfs['bytes'] <= 8388608 and any(row['target'] == str(PRIVATE)
            and row['filesystem'] == 'tmpfs' for row in tmpfs['mounts'])
            and all(not row['propagation'] for row in tmpfs['mounts']), 'Obcy tmpfs/propagacja')
    for name in ('cache', 'data', 'union'):
        (PRIVATE / name).mkdir(mode=0o755)
        if name != 'union':
            os.chown(PRIVATE / name, 1000, 1000)
    for name in ('held.bin', 'mapping.bin', 'truncate.bin', 'direct.bin'):
        fd = os.open(PRIVATE / 'cache' / name, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
        try:
            os.fchown(fd, 1000, 1000)
            a0.write_all(fd, b'B' * 4096)
            os.fsync(fd)
        finally:
            os.close(fd)
    raw_fd = os.open(PRIVATE / 'cache' / 'direct.bin', os.O_RDWR | os.O_NOFOLLOW)
    output_fd = os.open(BASE / 'mergerfs.log', os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
    args = ['/usr/bin/mergerfs', '-f', '-o',
            'allow_other,category.create=mfs,cache.files=off,minfreespace=20G,moveonenospc=true,func.getattr=newest',
            f'{PRIVATE}/cache=RW:{PRIVATE}/data=NC', str(PRIVATE / 'union')]
    process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=output_fd, stderr=output_fd,
                               cwd='/', close_fds=True,
                               preexec_fn=lambda: resource.setrlimit(resource.RLIMIT_FSIZE, (1024**2, 1024**2)))
    os.close(output_fd)
    daemon = {'pid': process.pid, 'start': a0.start_time(process.pid),
              'mnt_ns': os.stat(f'/proc/{process.pid}/ns/mnt').st_ino,
              'expected_binary_sha256': a0.MERGERFS_SHA, 'argv': args}
    event(channel, 'mergerfs-start', daemon)
    deadline = time.monotonic() + 15
    while not any(row['target'] == str(PRIVATE / 'union') for row in a0.mount_rows()):
        require(process.poll() is None and time.monotonic() < deadline, 'Mergerfs nie zamontował unii')
        select.select([], [], [], 0.02)
    daemon['binary_sha256'] = hashlib.sha256(Path(f'/proc/{process.pid}/exe').read_bytes()).hexdigest()
    event(channel, 'mergerfs-ready', daemon)
    require(process.poll() is None and a0.start_time(process.pid) == daemon['start']
            and os.stat(f'/proc/{process.pid}/ns/mnt').st_ino == daemon['mnt_ns']
            and daemon['binary_sha256'] == a0.MERGERFS_SHA, 'Niepotwierdzony proces mergerfs')
    expected_options = {'branches': f'{PRIVATE}/cache=RW:{PRIVATE}/data=NC', 'category.create': 'mfs',
                        'cache.files': 'off', 'minfreespace': str(20 * 1024**3), 'moveonenospc': 'mfs',
                        'func.getattr': 'newest', 'nullrw': 'false', 'cache.writeback': 'false'}
    actual_options = {key: os.getxattr(PRIVATE / 'union' / '.mergerfs', 'user.mergerfs.' + key).decode()
                      for key in expected_options}
    event(channel, 'mergerfs-options', actual_options)
    require(actual_options == expected_options, 'Inne opcje mergerfs')
    clone_fd = syscall(channel, 'clone-fuse', lambda: libc.open_tree(-100, str(PRIVATE / 'union').encode(),
                                                                   ctypes.c_uint(1 | os.O_CLOEXEC)))
    fuse_device = os.fstat(clone_fd).st_dev
    require(filesystem_type(clone_fd) == FUSE_MAGIC, 'Klon nie jest FUSE')
    ready = {'operation': 'ready', 'worker': {'pid': os.getpid(), 'start': a0.start_time(os.getpid()),
             'mnt_ns': a0.identity()['mnt_ns'], 'raw_fd': raw_fd}, 'mergerfs': daemon,
             'fuse_device': fuse_device, 'tmpfs_device': PRIVATE.stat().st_dev,
             'payload': {name: metric(PRIVATE / 'cache' / name) for name in
                         ('held.bin', 'mapping.bin', 'truncate.bin', 'direct.bin')}}
    send(channel, ready)
    require(operation(channel) == 'request_fd', 'Nieoczekiwany eksport')
    channel.sendmsg([b'M'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [clone_fd]))])
    require(operation(channel) == 'fd_received', 'Brak odbioru FD')
    os.close(clone_fd)
    send(channel, {'operation': 'exported'})
    while True:
        command = operation(channel)
        if command == 'root_write':
            result = a0.attempt(lambda: a0.write_marker(raw_fd, b'TRUSTED_ROOT\n'))
            send(channel, {'operation': command, 'write': result, 'metric': metric(PRIVATE / 'cache/direct.bin')})
        elif command == 'inspect':
            send(channel, {'operation': command, 'payload': {name: metric(PRIVATE / 'cache' / name)
                  for name in ('held.bin', 'mapping.bin', 'truncate.bin', 'direct.bin')},
                  'data_empty': not any((PRIVATE / 'data').iterdir()), 'mergerfs_alive': process.poll() is None,
                  'names': sorted(path.name for path in (PRIVATE / 'cache').iterdir()),
                  'mountinfo': Path('/proc/self/mountinfo').read_text()})
        elif command == 'stop':
            os.close(raw_fd)
            return
        else:
            raise ValueError('Obce polecenie workera')


def target_evidence(targets):
    rows = []
    for target in targets:
        pid = target['pid']
        require(a0.start_time(pid) == target['start'], 'Obcy lub zakończony PID celu')
        namespace = os.stat(f'/proc/{pid}/ns/mnt').st_ino
        require(namespace == target['mnt_ns'], 'Obca namespace celu')
        info = Path(target['path']).stat()
        rows.append({'target': target['target'], 'pid': pid, 'start': target['start'], 'mnt_ns': namespace,
                     'device': info.st_dev, 'inode': info.st_ino})
    return rows


def enter_actor(namespace):
    os.setgroups([])
    os.setresgid(1000, 1000, 1000)
    os.setresuid(1000, 1000, 1000)
    before = a0.identity()
    require(before['uid'] == 1000 and before['euid'] == 1000 and int(before['caps'], 16) == 0,
            'Aktor nie jest nieuprzywilejowany')
    setup = {'core_limit': list(resource.getrlimit(resource.RLIMIT_CORE))}
    if namespace:
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(4, ctypes.c_ulong(1), 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), 'PR_SET_DUMPABLE aktora')
        setup['dumpable_before_unshare'] = libc.prctl(3, 0, 0, 0, 0)
        require(setup['dumpable_before_unshare'] == 1, 'PR_GET_DUMPABLE aktora')
        if libc.unshare(0x10000000 | 0x00020000) != 0:
            raise OSError(ctypes.get_errno(), 'Unshare aktora')
        Path('/proc/self/setgroups').write_text('deny')
        Path('/proc/self/uid_map').write_text('0 1000 1\n')
        Path('/proc/self/gid_map').write_text('0 1000 1\n')
    return {'before': before, 'after': a0.identity(), 'namespace_errno': None, 'setup': setup}


def validate_targets(rows, targets):
    require(len(rows) == len(targets) and len({row['target'] for row in rows}) == len(targets),
            'Niepełne tożsamości celów')
    for row, target in zip(rows, targets):
        require(row['target'] == target['target'] and row['inode'] == target['expected_inode']
                and row['device'] == target['expected_device'], 'Obcy inode/device istniejącego celu')


def validate_payload(initial, final, mapping_counts):
    names = {'held.bin', 'mapping.bin', 'truncate.bin', 'direct.bin'}
    require(set(initial) == set(final['payload']) == names and set(final['names']) == names
            and len(final['names']) == 4 and final['data_empty'], 'Obcy zestaw końcowych plików')
    contents = {'held.bin': b'B' * 4096 + b'BASELINE\n' * 3 + b'HELD\n' * 3,
                'mapping.bin': (b'POST' + b'B' * 4092) if sum(mapping_counts) else b'B' * 4096,
                'truncate.bin': b'B' * 4096, 'direct.bin': b'B' * 4096 + b'TRUSTED_ROOT\n'}
    for name in names:
        before, after = initial[name], final['payload'][name]
        require(before['bytes'] == 4096 and before['sha256'] == hashlib.sha256(b'B' * 4096).hexdigest()
                and before['uid'] == 1000 and before['mode'] == 0o600 and before['nlink'] == 1,
                'Obcy początkowy payload')
        require(all(after[key] == before[key] for key in ('device', 'inode', 'uid', 'mode', 'nlink'))
                and after['bytes'] == len(contents[name])
                and after['sha256'] == hashlib.sha256(contents[name]).hexdigest(),
                'Zmieniona tożsamość lub treść pliku ' + name)
        if name == 'truncate.bin' or (name == 'mapping.bin' and not sum(mapping_counts)):
            require(after['mtime_ns'] == before['mtime_ns'], 'Zmieniony mtime niepisanego pliku')
    return True


def isolation_actor(channel, descriptors, namespace, targets):
    identity = enter_actor(namespace)
    send(channel, {'operation': 'ready', 'identity': identity, 'fds': descriptors, 'cwd': os.getcwd()})
    require(operation(channel) == 'probe', 'Brak polecenia próby izolacji')
    rows = []
    for target in targets:
        def probe(target=target):
            fd = os.open(target['path'], os.O_RDONLY if target['operation'] == 'setns' else os.O_WRONLY)
            try:
                if target['operation'] == 'setns':
                    libc = ctypes.CDLL(None, use_errno=True)
                    if libc.setns(fd, 0x00020000) != 0:
                        raise OSError(ctypes.get_errno(), 'setns')
                return 0
            finally:
                os.close(fd)
        rows.append({'target': target['target'], 'operation': target['operation'], **a0.attempt(probe)})
    send(channel, {'operation': 'probe', 'rows': rows})
    require(operation(channel) == 'stop', 'Obce polecenie końcowe aktora')


def public_actor(channel, descriptors, roots, namespace):
    event(channel, 'public-actor-fds', {'fds': descriptors, 'cwd': os.getcwd()})
    a0.actor_loop(channel, roots, PRIVATE / 'cache', namespace)


def parent_uid(child):
    require(a0.start_time(child.pid) == child.start, 'Obcy PID aktora')
    return int(next(line for line in Path(f'/proc/{child.pid}/status').read_text().splitlines()
                    if line.startswith('Uid:')).split()[1])


def mount_call(proof, label, function):
    proof.begin(label)
    before = Path('/proc/self/mountinfo').read_text()
    proof.record(label + '-before', {'mountinfo': before, 'mnt_ns': a0.identity()['mnt_ns']})
    ctypes.set_errno(0)
    result = function()
    raw = {'return': result, 'errno': ctypes.get_errno() if result else None}
    proof.record(label, raw)
    return raw


def require_markers(result, count, marker):
    require(len(result['writes']) == count
            and all(row == {'errno': None, 'value': marker} for row in result['writes']),
            'Niepotwierdzony zapis przez FUSE')


def validate_global(result):
    require(a0.evaluate_global(result['remount'], result['snapshots'], result['opens'], result['expected']),
            'Niepotwierdzone globalne RO FUSE')
    require(result['root_write'] == {'errno': None, 'value': len(b'TRUSTED_ROOT\n')},
            'Brak dodatniej kontroli root workera')
    return True


def actor_fds(child, reported):
    actual = {name: os.readlink(f'/proc/{child.pid}/fd/{name}')
              for name in os.listdir(f'/proc/{child.pid}/fd')}
    require(actual == reported and set(actual) == {'0', '1', '2', str(child.child_fd)}
            and all(actual[str(fd)] == '/dev/null' for fd in (0, 1, 2))
            and actual[str(child.child_fd)].startswith('socket:[')
            and os.readlink(f'/proc/{child.pid}/cwd') == '/', 'Niedozwolony FD/cwd aktora')
    return actual


def host_view(proof, tmpfs_device):
    raw = Path('/proc/self/mountinfo').read_text()
    proof.record('host-mountinfo', {'raw': raw, 'mnt_ns': a0.identity()['mnt_ns']})
    rows = a0.parse_mounts(raw)
    require(all(os.makedev(*(int(part) for part in row['number'].split(':'))) != tmpfs_device
                for row in rows) and PRIVATE.stat().st_dev != tmpfs_device, 'Raw tmpfs ujawniony na hoście')


def run(proof, lock):
    children = []
    libc = ctypes.CDLL(None, use_errno=True)
    try:
        proof.begin('private-publish')
        host_identity = a0.identity()
        proof.record('host-before-worker', {'identity': host_identity,
                                          'mountinfo': Path('/proc/self/mountinfo').read_text()})
        for path, mode in ((PRIVATE, 0o700), (PUBLIC, 0o755), (ALIAS, 0o755)):
            path.mkdir(mode=mode)
        control = Child(worker, allowed_fds=(lock,))
        children.append(control)
        ready = collect(control, proof)
        require(ready['operation'] == 'ready' and ready['worker']['pid'] == control.pid
                and ready['worker']['start'] == control.start, 'Obcy worker')
        require(ready['worker']['mnt_ns'] != host_identity['mnt_ns']
                and ready['tmpfs_device'] != ready['fuse_device'], 'Brak rozdzielenia namespace/FS')
        proof.state.update(worker=ready['worker'], mergerfs=ready['mergerfs'],
                           tmpfs_device=ready['tmpfs_device'], fuse_device=ready['fuse_device'])
        proof.save()
        host_view(proof, ready['tmpfs_device'])
        proof.begin('request-fuse-fd')
        control.send('request_fd')
        mount_fd = receive_mount_fd(control.channel, ready['fuse_device'], lambda value: proof.record('fuse-fd', value))
        try:
            control.send('fd_received')
            require(collect(control, proof)['operation'] == 'exported', 'Brak potwierdzenia eksportu')
            attached = mount_call(proof, 'publish-fuse', lambda: libc.move_mount(
                mount_fd, b'', -100, str(PUBLIC).encode(), ctypes.c_uint(4)))
            require(attached == {'return': 0, 'errno': None}, 'Odmowa publikacji FUSE')
        finally:
            os.close(mount_fd)
        bound = mount_call(proof, 'bind-fuse', lambda: libc.mount(str(PUBLIC).encode(), str(ALIAS).encode(),
                                                                None, ctypes.c_ulong(4096), None))
        require(bound == {'return': 0, 'errno': None}, 'Odmowa aliasu FUSE')
        require(PUBLIC.stat().st_dev == ALIAS.stat().st_dev == ready['fuse_device'], 'Obcy opublikowany FUSE')
        host_view(proof, ready['tmpfs_device'])
        public, baselines, expected = [], [], []
        for namespace, roots in ((False, [PUBLIC, ALIAS]), (True, [PUBLIC])):
            proof.begin('public-actor')
            child = Child(public_actor, (roots, namespace))
            children.append(child)
            identity = collect(child, proof)
            a0.validate_actor(identity, namespace, parent_uid(child))
            require(identity['before']['mnt_ns'] == host_identity['mnt_ns']
                    and identity['before']['user_ns'] == host_identity['user_ns'], 'Aktor nie powstał na hoście')
            descriptors = {name: os.readlink(f'/proc/{child.pid}/fd/{name}')
                           for name in os.listdir(f'/proc/{child.pid}/fd')}
            proof.record('public-actor-live-fds', descriptors)
            actor_fds(child, descriptors)
            baseline = exchange(child, proof, 'baseline')
            require_markers(baseline, len(roots), 9)
            require(len(baseline['mmap']) == len(roots) and len(baseline['creates']) == len(roots)
                    and all(row['errno'] in (None, errno.ENODEV) for row in baseline['mmap'])
                    and all(row['errno'] in (errno.ENOSPC, errno.EROFS) for row in baseline['creates']),
                    'Niepotwierdzony mmap/create baseline')
            public.append((child, roots))
            baselines.append(baseline)
            expected.extend(baseline['snapshot'])
        require(len(expected) == 3 and all(row['stat']['errno'] is None
                and row['stat']['value'] == {'device': ready['fuse_device'], 'readonly': False}
                for row in expected), 'Obce aliasy baseline')
        daemon = ready['mergerfs']
        entries = os.listdir(f'/proc/{daemon["pid"]}/fd')
        require(len(entries) <= 256, 'Limit FD mergerfs')
        matches = []
        for name in entries:
            try:
                value = Path(f'/proc/{daemon["pid"]}/fd/{name}').stat()
            except FileNotFoundError:
                continue
            if (value.st_dev, value.st_ino) == (ready['payload']['held.bin']['device'], ready['payload']['held.bin']['inode']):
                matches.append(int(name))
        require(matches, 'Brak istniejącego raw FD mergerfs')
        targets = [{'target': 'host_path', 'operation': 'open', 'path': str(PRIVATE / 'cache/held.bin')}]
        for name, process, descriptor in (('worker', ready['worker'], ready['worker']['raw_fd']),
                                           ('mergerfs', daemon, min(matches))):
            for kind, op, suffix in (('root', 'open', f'root{PRIVATE}/cache/held.bin'),
                                     ('fd', 'open', f'fd/{descriptor}'), ('ns', 'setns', 'ns/mnt')):
                targets.append({'target': name + '_' + kind, 'operation': op,
                                'path': f'/proc/{process["pid"]}/{suffix}', 'pid': process['pid'],
                                'start': process['start'], 'mnt_ns': process['mnt_ns']})
                pinned = (ready['payload']['direct.bin'] if name == 'worker' and kind == 'fd'
                          else ready['payload']['held.bin'])
                targets[-1].update(expected_inode=process['mnt_ns'] if kind == 'ns' else pinned['inode'],
                                   expected_device=os.stat(f'/proc/{process["pid"]}/ns/mnt').st_dev
                                   if kind == 'ns' else pinned['device'])
        isolation = []
        for namespace in (False, True):
            proof.begin('isolation-actor')
            child = Child(isolation_actor, (namespace, targets))
            children.append(child)
            handshake = collect(child, proof)
            proof.record('isolation-fd-identity', handshake)
            actor_fds(child, handshake['fds'])
            a0.validate_actor(handshake['identity'], namespace, parent_uid(child))
            require(handshake['identity']['before']['mnt_ns'] == host_identity['mnt_ns']
                    and handshake['identity']['before']['user_ns'] == host_identity['user_ns'],
                    'Aktor izolacji nie powstał na hoście')
            before = target_evidence(targets[1:])
            proof.record('isolation-targets-before', before)
            validate_targets(before, targets[1:])
            measured = exchange(child, proof, 'probe')
            after = target_evidence(targets[1:])
            proof.record('isolation-targets-after', after)
            validate_targets(after, targets[1:])
            require(before == after and isolation_result(measured['rows'], targets), 'Direct branch isolation niepotwierdzone')
            isolation.append(measured)
            proof.record('isolation-actor-stop', child.stop())
        flags = ctypes.c_ulong(32 | 1 | 2 | 4 | (1 << 21))
        first = mount_call(proof, 'global-ro-held', lambda: libc.mount(None, str(PUBLIC).encode(), None, flags, None))
        for child, roots in public:
            held = exchange(child, proof, 'held')
            require_markers(held, len(roots), 5)
        require(first == {'return': -1, 'errno': errno.EBUSY}, 'Brak odmowy RO przy writerach')
        mapping_counts = []
        for index, (child, _) in enumerate(public):
            closed = exchange(child, proof, 'close_fd')
            require(closed['open_fds'] == 0 and not closed['actual_fds'], 'Pozostał zapisujący FD')
            require(closed['mappings'] == sum(row['errno'] is None for row in baselines[index]['mmap']),
                    'Inna liczba istniejących mapowań')
            mapping_counts.append(closed['mappings'])
        if sum(mapping_counts):
            mapped = mount_call(proof, 'global-ro-mmap', lambda: libc.mount(None, str(PUBLIC).encode(), None, flags, None))
            require(mapped == {'return': -1, 'errno': errno.EBUSY}, 'Brak odmowy przy mmap')
            for index, (child, _) in enumerate(public):
                result = exchange(child, proof, 'map')
                require_markers(result, mapping_counts[index], 4)
        for child, _ in public:
            require(exchange(child, proof, 'unmap')['mappings'] == 0, 'Pozostało mmap')
        last = mount_call(proof, 'global-ro-closed', lambda: libc.mount(None, str(PUBLIC).encode(), None, flags, None))
        reopened = [exchange(child, proof, 'reopen') for child, _ in public]
        trusted = exchange(control, proof, 'root_write')
        final = exchange(control, proof, 'inspect')
        result = {'scope': 'private_tmpfs_and_FUSE_only', 'isolation': isolation, 'remount': last,
                  'snapshots': [row for item in reopened for row in item['snapshot']],
                  'opens': [row for item in reopened for row in item['opens']], 'expected': expected,
                  'root_write': trusted['write'], 'final': final, 'tmpfs_volatile': True,
                  'initial_payload': ready['payload'], 'mapping_counts': mapping_counts,
                  'mergerfs_preserved': True, 'mergerfs': daemon,
                  'mmap_tested': all(row['errno'] is None for item in baselines for row in item['mmap'])}
        proof.record('result', result)
        validate_global(result)
        validate_payload(ready['payload'], final, mapping_counts)
        require(final['data_empty'] and final['mergerfs_alive'], 'Brak końcowego stanu workera')
        host_view(proof, ready['tmpfs_device'])
        return result
    finally:
        stopped = []
        for child in children:
            stopped.append(child.stop())
        proof.record('children-stop', stopped)
        require(not any(item['alive'] for item in stopped), 'Dziecko kontrolne nadal działa')


def main(argv):
    global a0
    if argv == ['--help']:
        print('Użycie: sudo python3 - preflight|run < guest_branch_isolation_probe.py')
        return 0
    proof, lock = None, None
    report = {'status': 'refused', 'mutating_authority': False}
    try:
        require(argv in (['preflight'], ['run']) and os.geteuid() == 0, 'Argument/UID A1')
        a0 = load_helpers()
        storage = a0.load_storage()
        observation = storage.observe()
        guard(observation, storage)
        if argv == ['preflight']:
            print(json.dumps({'status': 'measured', 'mutating_authority': False, 'observation': observation}))
            return 0
        BASE.mkdir(mode=0o700)
        lock = os.open(BASE / '.lock', os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        import fcntl
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        proof = Proof()
        proof.save()
        report['mutating_authority'] = True
        result = run(proof, lock)
        proof.state.update(pending=None, completed=True)
        proof.save()
        report.update(status='measured', result=result)
    except BaseException as error:
        report['error'] = type(error).__name__ + ': ' + str(error)
        if proof is not None:
            try:
                proof.record('failure', report)
            except BaseException as persist_error:
                report['failure_record_error'] = type(persist_error).__name__ + ': ' + str(persist_error)
    finally:
        if lock is not None:
            os.close(lock)
    print(json.dumps(report, sort_keys=True))
    return 0 if report['status'] == 'measured' else 1


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
