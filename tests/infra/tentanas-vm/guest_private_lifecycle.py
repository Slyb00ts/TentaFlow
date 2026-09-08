# =============================================================================
# Plik: guest_private_lifecycle.py
# Opis: Odbiór produkcyjnego lifecycle prywatnych macierzy na dwóch przypiętych VM.
# Przykład: python3 guest_private_lifecycle.py preflight station.json SHA256
# =============================================================================

import base64
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
import types

UID = 0
HELPER = Path('/usr/local/libexec/tentanas-helper')
ROOT = Path('/var/lib/tentanas/elastic')
BASE = Path('/var/lib/tentanas-private-lifecycle')
LIMIT = 2 * 1024**2
STATIONS = {'L': ('c4a36ec3-7a92-4c61-96e3-77208b628721', ['data1', 'data2'], []),
            'P': ('ca4b0b07-e238-4c5e-85a7-f049cfe9bfa2', ['data1'], ['parity'])}
SIZES = {'os': 12, 'data1': 32, 'data2': 32, 'parity': 40, 'cache': 1, 'spare': 40}
SEEDS = {'L': '001c8be46194314c65665e9fb6d6e3036335e2a9ae66d6584976d1d1c295839b',
         'P': '2ad52a4cde8f7e23765d941c5a9b35e354c2b97f85c61c44bc78529272f1d81a'}
SEED_BYTES = 378880
TOOLS = {'/usr/bin/mergerfs', '/usr/bin/snapraid', '/usr/sbin/mkfs.xfs', '/usr/sbin/blkid'}
SUPPORT = {'guest_storage.py': '9c2298169682b3f7e8170ce1aa736f18eeb49063d368c81faa1546dbf4cd164e',
           'guest_writer_gate_probe.py': 'e20611d9dc12661155d5660e49f31c40f3ab7cb5e3c6c07279c512705561dedb',
           'guest_branch_isolation_probe.py': 'fcbc73b14fb64773283fefa635075d1717e325f054b674a432670a1ef9923356'}
PHASE_COMMANDS = {'create': 'elastic_create', 'inspect-created': 'elastic_inspect',
                  'sync': 'elastic_sync', 'scrub': 'elastic_scrub', 'nochange': 'elastic_sync',
                  'restore-live': 'elastic_restore', 'inspect-live': 'elastic_inspect',
                  'restore-reboot': 'elastic_restore', 'inspect-reboot': 'elastic_inspect'}


def require(value, message):
    if not value:
        raise ValueError(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'Powtórzony klucz JSON: ' + key)
        result[key] = value
    return result


def decode(raw):
    return json.loads(raw, object_pairs_hook=unique_object)


def canonical_uuid(value):
    require(isinstance(value, str) and str(uuid.UUID(value)) == value and uuid.UUID(value).int != 0,
            'Niekanoniczny UUID')
    return value


def digest(value):
    require(isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value) and value != '0' * 64,
            'Nieprawidłowy SHA256')
    return value


def private_parents(path):
    for parent in Path(path).parents:
        info = parent.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == UID and not info.st_mode & 0o022,
                'Nieprywatny przodek pliku')


def read_private(path, expected_sha=None):
    path = Path(path)
    private_parents(path)
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC), 'rb') as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == UID and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) == 0o600, 'Nieprywatny plik')
        raw = stream.read(LIMIT + 1)
        require(len(raw) <= LIMIT, 'Limit pliku')
    if expected_sha is not None:
        require(hashlib.sha256(raw).hexdigest() == digest(expected_sha), 'Obcy SHA pliku')
    return raw


def flush_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def durable_new(path, value):
    path = Path(path)
    private_parents(path)
    raw = json.dumps(value, sort_keys=True).encode()
    require(len(raw) <= LIMIT, 'Limit dowodu')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        remaining = memoryview(raw)
        while remaining:
            written = os.write(fd, remaining)
            require(written > 0, 'Niepełny zapis dowodu')
            remaining = remaining[written:]
        os.fsync(fd)
    finally:
        os.close(fd)
    flush_directory(path.parent)


def validate_station(value):
    require(isinstance(value, dict) and set(value) == {'schema', 'station', 'vm_uuid', 'initial_boot',
            'helper_sha256', 'source_commit', 'tools', 'disks', 'spec', 'maintenance_ids'}, 'Pola stanowiska')
    require(type(value['schema']) is int and value['schema'] == 1 and value['station'] in STATIONS,
            'Schemat stanowiska')
    vm_uuid, data, parity = STATIONS[value['station']]
    require(value['vm_uuid'] == vm_uuid, 'Obca VM')
    canonical_uuid(value['initial_boot'])
    digest(value['helper_sha256'])
    require(isinstance(value['source_commit'], str) and re.fullmatch('[0-9a-f]{40}', value['source_commit'])
            and value['source_commit'] != '0' * 40, 'Nieprawidłowy commit')
    require(isinstance(value['tools'], dict) and set(value['tools']) == TOOLS, 'Zestaw narzędzi')
    for sha in value['tools'].values():
        digest(sha)
    prefix = 'tn-' + vm_uuid.replace('-', '')[:10] + '-'
    expected_disks = {role: {'serial': prefix + role, 'bytes': size * 1024**3} for role, size in SIZES.items()}
    require(value['disks'] == expected_disks, 'Obca mapa sześciu dysków')
    spec = value['spec']
    require(isinstance(spec, dict) and set(spec) == {'array_id', 'operation_id', 'owner', 'name',
            'filesystem', 'data', 'parity'}, 'Pola spec')
    identifiers = [canonical_uuid(spec['array_id']), canonical_uuid(spec['operation_id'])]
    require(spec['filesystem'] == 'xfs' and spec['name'] == 'a21-' + value['station'].lower(), 'Obca topologia')
    require(isinstance(spec['owner'], dict) and set(spec['owner']) == {'org_id', 'addon_id'} and
            all(isinstance(v, str) and re.fullmatch('[A-Za-z0-9_-]{1,128}', v)
                for v in spec['owner'].values()), 'Obcy owner')
    disk_ids = []
    for kind, roles in [('data', data), ('parity', parity)]:
        require(isinstance(spec[kind], list) and len(spec[kind]) == len(roles), 'Liczba ról')
        for disk, role in zip(spec[kind], roles):
            require(isinstance(disk, dict) and set(disk) == {'disk_id', 'wwn', 'serial', 'bytes',
                    'expected_uuid'} and disk['serial'] == expected_disks[role]['serial'] and
                    type(disk['bytes']) is int and disk['bytes'] == expected_disks[role]['bytes'], 'Obcy dysk spec')
            require(isinstance(disk['disk_id'], str) and 0 < len(disk['disk_id']) <= 256 and
                    not re.search('[\x00-\x1f\x7f]', disk['disk_id']), 'Nieprawidłowy disk_id')
            require(disk['wwn'] is None or isinstance(disk['wwn'], str) and 0 < len(disk['wwn']) <= 256
                    and not re.search('[\x00-\x1f\x7f]', disk['wwn']), 'Nieprawidłowy WWN')
            disk_ids.append(disk['disk_id'])
            identifiers.append(canonical_uuid(disk['expected_uuid']))
    wanted = {'sync', 'scrub', 'nochange'} if parity else set()
    require(isinstance(value['maintenance_ids'], dict) and set(value['maintenance_ids']) == wanted,
            'Operacje ochrony niezgodne ze stanowiskiem')
    identifiers.extend(canonical_uuid(v) for v in value['maintenance_ids'].values())
    require(len(set(identifiers)) == len(identifiers) and len(set(disk_ids)) == len(disk_ids), 'Powtórzone ID')
    return value


def load_station(path, sha):
    return validate_station(decode(read_private(path, sha)))


class Proof:
    def __init__(self, path, station_sha):
        self.path = Path(path)
        private_parents(self.path)
        if not self.path.exists():
            self.path.mkdir(mode=0o700)
            flush_directory(self.path.parent)
        info = self.path.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == UID and stat.S_IMODE(info.st_mode) == 0o700,
                'Nieprywatny katalog dowodów')
        self.state = {'station_sha256': digest(station_sha), 'pending': None, 'completed': [], 'events': [], 'dispatched': []}
        if (self.path / 'state.json').exists():
            self.state = decode(read_private(self.path / 'state.json'))
            require(self.state['station_sha256'] == station_sha, 'Stan obcego manifestu')

    def save(self):
        durable_new(self.path / 'state.next', self.state)
        os.replace(self.path / 'state.next', self.path / 'state.json')
        flush_directory(self.path)

    def begin(self, phase):
        require(self.state['pending'] is None and phase not in self.state['completed'], 'Brak zgody na ponowienie')
        self.state['pending'] = phase
        self.save()

    def record(self, label, value):
        require(re.fullmatch('[a-z0-9-]{1,48}', label) and len(self.state['events']) < 400, 'Limit zdarzeń')
        name = f'{len(self.state["events"]):03d}-{label}.json'
        durable_new(self.path / name, value)
        self.state['events'].append(name)
        self.save()

    def complete(self, phase):
        require(self.state['pending'] == phase, 'Inna faza zakończenia')
        completed = {**self.state, 'pending': None, 'completed': [*self.state['completed'], phase]}
        durable_new(self.path / 'state.next', completed)
        os.replace(self.path / 'state.next', self.path / 'state.json')
        flush_directory(self.path)
        self.state = completed


def validate_result(value, request, station=None):
    require(isinstance(value, dict), 'Wynik nie jest obiektem')
    maintenance = request['cmd'] in ('elastic_sync', 'elastic_scrub')
    state = value.get('state') if maintenance else value
    spec = station['spec'] if station else request.get('operation')
    owner = spec['owner'] if spec else request['owner']
    array_id = spec['array_id'] if spec else request['array_id']
    require(isinstance(state, dict) and state.get('owner') == owner and state.get('array_id') == array_id
            and state.get('stage') == 'ready' and state.get('union_mounted') is True, 'Niezgodny wynik macierzy')
    if spec:
        require(state.get('operation_id') == spec['operation_id'], 'Obca operacja Create')
        wanted = [({kind: index}, disk) for kind in ('data', 'parity')
                  for index, disk in enumerate(spec[kind], 1)]
        rows = state.get('disks')
        require(isinstance(rows, list) and len(rows) == len(wanted), 'Niepełne role wyniku')
        require(all(isinstance(row, dict) for row in rows) and
                len({row.get('device') for row in rows}) == len(rows), 'Powtórzone urządzenie wyniku')
        for row, (role, disk) in zip(rows, wanted):
            require(isinstance(row, dict) and row.get('role') == role and
                    row.get('observed_uuid') == disk['expected_uuid'] and row.get('filesystem') == 'xfs'
                    and row.get('device_present') is True and row.get('mounted') is True and
                    isinstance(row.get('device'), str) and row['device'].startswith('/dev/') and
                    row.get('kernel_name') == row['device'].removeprefix('/dev/') and
                    all(type(row.get(key)) is int and row[key] >= 0 for key in
                        ('size_bytes', 'used_bytes', 'free_bytes')) and row.get('detail') is None,
                    'Niepotwierdzona rola wyniku')
    else:
        require(isinstance(state.get('disks'), list) and bool(state['disks']), 'Brak ról wyniku')
    if maintenance:
        run = value.get('run')
        require(isinstance(run, dict) and run.get('operation_id') == request['operation_id'] and
                run.get('kind') == request['cmd'].removeprefix('elastic_') and run.get('outcome') == 'succeeded'
                and run.get('finished_at') and run.get('exit_code') == 0, 'Niezgodny wynik ochrony')
        require(all(type(run.get(key)) is int and run[key] == 0 for key in
                    ('errors_file', 'errors_io', 'errors_data')), 'Niepotwierdzone liczniki ochrony')
        if request['cmd'] == 'elastic_scrub':
            require(type(run.get('checked_blocks')) is int and type(run.get('total_blocks')) is int
                    and 0 < run['checked_blocks'] <= run['total_blocks'], 'Niepełny Scrub')
    return value


def invoke_helper(station, request, proof, run=subprocess.run):
    validate_station(station)
    phase = proof.state['pending']
    require(phase in PHASE_COMMANDS, 'Brak obsługiwanej trwałej fazy przed helperem')
    expected = {'cmd': PHASE_COMMANDS[phase]}
    dispatch_key = phase
    if phase in ('sync', 'scrub', 'nochange'):
        require(station['station'] == 'P', 'Ochrona poza stanowiskiem P')
        dispatch_key = phase + ':' + request.get('cmd', '')
        if request.get('cmd') == 'elastic_inspect':
            require(phase + ':' + PHASE_COMMANDS[phase] in proof.state['dispatched'], 'Inspect przed ochroną')
            expected['cmd'] = 'elastic_inspect'
        else:
            expected['operation_id'] = station['maintenance_ids'][phase]
    if phase == 'create':
        expected['operation'] = station['spec']
    else:
        expected.update(array_id=station['spec']['array_id'], owner=station['spec']['owner'])
    require(request == expected, 'Obcy request lub faza')
    require(dispatch_key not in proof.state['dispatched'], 'Helper tej fazy został już wywołany')
    raw = json.dumps(request, separators=(',', ':')).encode() + b'\n'
    require(len(raw) <= 16 * 1024, 'Limit requestu')
    proof.record('helper-request', {'raw': base64.b64encode(raw).decode(), 'sha256': hashlib.sha256(raw).hexdigest()})
    proof.state['dispatched'].append(dispatch_key)
    proof.save()
    try:
        result = run([str(HELPER)], input=raw, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                     timeout=1200, check=False, env={'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL': 'C'})
    except subprocess.TimeoutExpired as error:
        proof.record('helper-timeout', {'stdout': base64.b64encode(error.stdout or b'').decode(),
                                     'stderr': base64.b64encode(error.stderr or b'').decode()})
        raise
    proof.record('helper-response', {'stdout': base64.b64encode(result.stdout).decode(),
                 'stderr': base64.b64encode(result.stderr).decode(), 'exit_code': result.returncode})
    require(result.returncode == 0, 'Helper zakończony błędem')
    return validate_result(decode(result.stdout), request, station)


def load_support():
    modules = {}
    for name, sha in SUPPORT.items():
        path = Path(__file__).resolve().parent / name
        raw = read_private(path, sha)
        module = types.ModuleType(name.removesuffix('.py'))
        exec(compile(raw, str(path), 'exec'), module.__dict__)
        modules[name] = module
    modules['guest_branch_isolation_probe.py'].a0 = modules['guest_writer_gate_probe.py']
    return modules


def binary_identity(path, expected_sha):
    path = Path(path)
    private_parents(path)
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC), 'rb') as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_uid == UID and not before.st_mode & 0o022,
                'Obca binarka')
        sha = hashlib.file_digest(stream, 'sha256').hexdigest()
        after = os.fstat(stream.fileno())
        require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) ==
                (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns), 'Binarka zmieniła się podczas odczytu')
        require(sha == expected_sha, 'Obcy SHA binarki')
        return {'device': before.st_dev, 'inode': before.st_ino, 'sha256': sha}


def read_seed(rom, expected_sha):
    fd = os.open('/dev/sr0', os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISBLK(info.st_mode) and
                f'{os.major(info.st_rdev)}:{os.minor(info.st_rdev)}' == rom['maj:min'], 'Obce urządzenie seed')
        raw = stream.read(SEED_BYTES + 1)
        sha = hashlib.sha256(raw).hexdigest()
        require(len(raw) == SEED_BYTES and sha == expected_sha,
                'Obce bajty immutable seed')
        return {'bytes': len(raw), 'sha256': sha, 'device': info.st_rdev}


def guard_observation(station, observation, created=False):
    require(observation['uuid'] == station['vm_uuid'] and observation['boot_id'] == station['initial_boot'],
            'Obca VM lub boot')
    entries = observation['disks']
    roms = [entry for entry in entries if entry['type'] == 'rom']
    disks = [entry for entry in entries if entry['type'] == 'disk']
    require(len(entries) == 7 and len(roms) == 1 and len(disks) == 6, 'Inny zestaw dysków i seedROM')
    rom = roms[0]
    require(rom['name'] == 'sr0' and rom['serial'] == 'seed' and rom['ro'] is True and
            type(rom['size']) is int and rom['size'] == SEED_BYTES and rom['fstype'] == 'iso9660' and
            isinstance(rom['uuid'], str) and re.fullmatch(r'\d{4}(?:-\d{2}){6}', rom['uuid']) and
            rom['maj:min'] == '11:0' and not rom.get('children') and not rom.get('holders') and
            not any(rom['mountpoints'] or []) and rom['maj:min'] not in observation['swaps'] and
            not any(m['maj:min'] == rom['maj:min'] for m in observation['mounts']) and
            all(d['maj:min'] != rom['maj:min'] for d in disks), 'Obcy lub zajęty seedROM')
    read_seed(rom, SEEDS[station['station']])
    require(len(disks) == 6 and all(d['type'] == 'disk' for d in disks) and
            len({d['serial'] for d in disks}) == 6 and len({d['maj:min'] for d in disks}) == 6,
            'Inny zestaw całych dysków')
    roots = [m for m in observation['mounts'] if m['target'] == '/']
    require(len(roots) == 1, 'Nieznany root OS')
    wanted_fs = {d['serial']: d['expected_uuid'] for kind in ('data', 'parity') for d in station['spec'][kind]}
    result = {}
    for role, wanted in station['disks'].items():
        disk = next(d for d in disks if d['serial'] == wanted['serial'])
        require(type(disk['size']) is int and disk['size'] == wanted['bytes'] and disk['ro'] in (0, False),
                'Rozmiar lub readonly dysku')
        require(re.fullmatch(r'nvme\d+n\d+' if role == 'cache' else r'vd[a-z]+', disk['name']), 'Magistrala dysku')
        numbers = {disk['maj:min'], *(child['maj:min'] for child in disk.get('children', []))}
        if role == 'os':
            require(roots[0]['maj:min'] in numbers, 'Root nie należy do OS')
        else:
            require(not disk.get('children') and not disk['holders'] and
                    not numbers.intersection(observation['swaps']) and
                    not any(m['maj:min'] in numbers for m in observation['mounts']) and
                    not any(disk['mountpoints'] or []), 'Raw dysk opublikowany lub zajęty')
            expected_uuid = wanted_fs.get(disk['serial']) if created else None
            if expected_uuid:
                require(disk['fstype'] == 'xfs' and disk['uuid'] == expected_uuid and
                        bool(disk['signatures']) and all(s['type'] == 'xfs' for s in disk['signatures']),
                        'Obcy FSUUID lub podpis')
            else:
                require(not disk['fstype'] and not disk['uuid'] and not disk['signatures'], 'Dysk nie jest pusty')
        info = os.stat('/dev/' + disk['name'])
        require(stat.S_ISBLK(info.st_mode) and f'{os.major(info.st_rdev)}:{os.minor(info.st_rdev)}' == disk['maj:min'],
                'Obce urządzenie blokowe')
        result[role] = {**disk, 'device': info.st_rdev}
    return result


def validate_journal(station, journal):
    require(isinstance(journal, dict) and set(journal) == {'schema', 'spec', 'stage', 'formatted', 'pending',
            'boot_id', 'sync_completed_at', 'detail', 'last_run', 'private'}, 'Pola journala')
    require(type(journal['schema']) is int and journal['schema'] == 2 and journal['spec'] == station['spec']
            and journal['boot_id'] == station['initial_boot'] and journal['stage'] == 'ready'
            and journal['pending'] is None, 'Niepotwierdzony prywatny journal')
    roles = [{kind: index} for kind in ('data', 'parity') for index in range(1, len(station['spec'][kind]) + 1)]
    require(journal['formatted'] == roles, 'Niepełny format journal')
    private = journal['private']
    require(isinstance(private, dict) and set(private) == {'anchor', 'published'} and private['published'] is True,
            'Niepełna topologia prywatna')
    anchor = private['anchor']
    require(isinstance(anchor, dict) and set(anchor) == {'boot_id', 'pid', 'start_ticks', 'mount_ns_inode',
            'exe_device', 'exe_inode', 'exe_sha256', 'union_device', 'union_source'}, 'Pola kotwicy')
    require(anchor['boot_id'] == journal['boot_id'] and anchor['exe_sha256'] == station['tools']['/usr/bin/mergerfs']
            and all(type(anchor[k]) is int and anchor[k] > 0 for k in
                    ('pid', 'start_ticks', 'mount_ns_inode', 'exe_device', 'exe_inode', 'union_device'))
            and anchor['pid'] > 1 and isinstance(anchor['union_source'], str) and anchor['union_source'], 'Obca kotwica')
    return anchor


def anchor_identity(anchor, a0):
    pid = anchor['pid']
    ns = os.stat(f'/proc/{pid}/ns/mnt').st_ino
    exe_path = Path(f'/proc/{pid}/exe')
    with exe_path.open('rb') as stream:
        info = os.fstat(stream.fileno())
        exe_sha = hashlib.file_digest(stream, 'sha256').hexdigest()
    measured = {'start_ticks': int(a0.start_time(pid)), 'mount_ns_inode': ns,
                'exe_device': info.st_dev, 'exe_inode': info.st_ino, 'exe_sha256': exe_sha}
    require(all(measured[key] == anchor[key] for key in measured) and
            ns != os.stat('/proc/self/ns/mnt').st_ino, 'Nieżywa lub obca kotwica')
    return measured


def role_paths(station):
    root = Path('/mnt/tentanas-branches') / station['spec']['name']
    return [(kind, index, disk, root / kind / (f'd{index}' if kind == 'data' else str(index)))
            for kind in ('data', 'parity') for index, disk in enumerate(station['spec'][kind], 1)]


def union_snapshot(station, anchor, a0):
    path = '/mnt/' + station['spec']['name']
    return {'raw_mountinfo': a0.attempt(lambda: Path('/proc/self/mountinfo').read_text()),
            'device': a0.attempt(lambda: os.stat(path).st_dev),
            'options': {key: a0.attempt(lambda key=key: os.getxattr(path + '/.mergerfs',
                        'user.mergerfs.' + key).decode()) for key in
                        ('branches', 'category.create', 'cache.files', 'minfreespace', 'moveonenospc')}}


def validate_union(station, anchor, snapshot, a0):
    require(snapshot['raw_mountinfo']['errno'] is None, 'Brak mountinfo')
    all_rows = a0.parse_mounts(snapshot['raw_mountinfo']['value'])
    rows = [r for r in all_rows if r['target'] == '/mnt/' + station['spec']['name']]
    require(len(rows) == 1, 'Brak jednoznacznej publikacji FUSE')
    row = rows[0]
    number = f'{os.major(anchor["union_device"])}:{os.minor(anchor["union_device"])}'
    require(row['filesystem'] == 'fuse.mergerfs' and row['root'] == '/' and row['source'] == anchor['union_source']
            and row['number'] == number and snapshot['device'] == {'errno': None, 'value': anchor['union_device']} and
            'rw' in row['mount_options'] and 'rw' in row['super_options'], 'Obca publikacja FUSE')
    wanted = {'branches': ':'.join(str(p) + '=RW' for kind, _, _, p in role_paths(station) if kind == 'data'),
              'category.create': 'mfs', 'cache.files': 'off', 'minfreespace': str(20 * 1024**3), 'moveonenospc': 'mfs'}
    require(snapshot['options'] == {key: {'errno': None, 'value': value} for key, value in wanted.items()},
            'Obce opcje FUSE')
    return all_rows


def namespace_audit(channel, descriptors, ns_fd, station, anchor, disks, a0, a1):
    os.setns(ns_fd, 0)
    os.close(ns_fd)
    os.chdir('/')
    measured = []
    for kind, index, disk, path in role_paths(station):
        measured.append({'role': {kind: index}, 'path': str(path),
                         'device': a0.attempt(lambda path=path: os.stat(path).st_dev)})
    a1.send(channel, {'namespace': os.stat('/proc/self/ns/mnt').st_ino, 'roles': measured,
                      'union': union_snapshot(station, anchor, a0), 'descriptors': descriptors})


def validate_inside(station, anchor, disks, inside, a0):
    require('fatal' not in inside and inside.get('namespace') == anchor['mount_ns_inode'], 'Odmowa namespace')
    rows = validate_union(station, anchor, inside['union'], a0)
    wanted = role_paths(station)
    require(isinstance(inside.get('roles'), list) and len(inside['roles']) == len(wanted), 'Niepełna obserwacja branchy')
    for observed, (kind, index, disk, path) in zip(inside['roles'], wanted):
        device = next(d for d in disks.values() if d['serial'] == disk['serial'])
        mounts = [r for r in rows if r['target'] == str(path)]
        require(observed == {'role': {kind: index}, 'path': str(path),
                             'device': {'errno': None, 'value': device['device']}} and
                len(mounts) == 1 and mounts[0]['filesystem'] == 'xfs' and mounts[0]['root'] == '/' and
                mounts[0]['number'] == device['maj:min'] and 'rw' in mounts[0]['mount_options'] and
                'rw' in mounts[0]['super_options'], 'Obcy prywatny mount')


def open_product_lock():
    lock_path = ROOT / '.storage.lock'
    private_parents(lock_path)
    lock = os.open(lock_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        info = os.fstat(lock)
        linked = lock_path.lstat()
        require(stat.S_ISREG(info.st_mode) and info.st_uid == UID and info.st_nlink == 1 and
                stat.S_IMODE(info.st_mode) == 0o600 and
                (info.st_dev, info.st_ino) == (linked.st_dev, linked.st_ino), 'Obcy lock produktu')
        fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
        return lock
    except BaseException:
        os.close(lock)
        raise


def audit(station, proof, modules, disks):
    a0 = modules['guest_writer_gate_probe.py']
    a1 = modules['guest_branch_isolation_probe.py']
    lock = open_product_lock()
    try:
        journal_path = ROOT / (station['spec']['array_id'] + '.json')
        raw = read_private(journal_path)
        proof.record('journal-before', {'raw': base64.b64encode(raw).decode(), 'sha256': hashlib.sha256(raw).hexdigest()})
        journal = decode(raw)
        anchor = validate_journal(station, journal)
        before = anchor_identity(anchor, a0)
        ns_fd = os.open(f'/proc/{anchor["pid"]}/ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
        try:
            require(os.fstat(ns_fd).st_ino == anchor['mount_ns_inode'], 'Inny deskryptor namespace')
            host = union_snapshot(station, anchor, a0)
            proof.record('host-publication', {'anchor': before, 'union': host})
            host_rows = validate_union(station, anchor, host, a0)
            selected = {d['maj:min'] for role, d in disks.items() if role != 'os'}
            require(not any(r['number'] in selected for r in host_rows), 'Raw mount lub alias na hoście')
            child = a1.Child(namespace_audit, (ns_fd, station, anchor, disks, a0, a1), (ns_fd, lock))
            try:
                response = child.receive()
                proof.record('namespace-raw', response)
                require(response.get('error') is None, 'Błąd odczytu namespace')
                inside = decode(base64.b64decode(response['raw']))
                validate_inside(station, anchor, disks, inside, a0)
            finally:
                stopped = child.stop()
                proof.record('namespace-child', stopped)
                require(not stopped['alive'], 'Dziecko odczytu nadal żyje')
            require(anchor_identity(anchor, a0) == before and os.fstat(ns_fd).st_ino == anchor['mount_ns_inode'],
                    'Kotwica zmieniła się podczas odczytu')
            after = union_snapshot(station, anchor, a0)
            proof.record('host-publication-after', after)
            require(after == host, 'Publikacja zmieniła się podczas odczytu')
            require(read_private(journal_path) == raw, 'Journal zmienił się podczas odczytu')
            result = {'journal': journal, 'journal_sha256': hashlib.sha256(raw).hexdigest(), 'host': host,
                      'inside': inside, 'anchor': before}
            proof.record('audit', result)
            return result
        finally:
            os.close(ns_fd)
    finally:
        os.close(lock)


def execute(phase, station, proof, modules, boot_authorization=None):
    phases = ['preflight', 'create', 'inspect-created', 'payload'] + (
        ['isolation', 'restore-live', 'inspect-live', 'reboot-checkpoint', 'restore-reboot', 'inspect-reboot']
        if station['station'] == 'L' else ['sync', 'scrub', 'nochange'])
    require(phase in phases, 'Niezaimplementowana faza')
    expected_previous = phases[:phases.index(phase)]
    require(proof.state['completed'] == expected_previous and proof.state['pending'] is None, 'Inna kolejność faz')
    reboot = phase in ('restore-reboot', 'inspect-reboot')
    require(reboot == (boot_authorization is not None), 'Nieoczekiwany lub brakujący checkpoint boot')
    if reboot:
        station = validate_reboot_authorization(station, proof, *boot_authorization)
    storage = modules['guest_storage.py']
    observation = storage.observe()
    proof.record('storage-before', observation)
    disks = guard_observation(station, observation, phase not in ('preflight', 'create'))
    storage.automation_guard(station['vm_uuid'])
    binary_identity(HELPER, station['helper_sha256'])
    for path, sha in station['tools'].items():
        binary_identity(path, sha)
    require(storage.space(Path('/'))['available'] >= 2 * 1024**3, 'Brak rezerwy OS')
    if phase in ('preflight', 'create'):
        require(not os.path.lexists('/mnt/' + station['spec']['name']) and
                not os.path.lexists('/mnt/tentanas-branches/' + station['spec']['name']) and
                (not ROOT.exists() or not list(ROOT.glob('*.json'))), 'Istniejąca macierz lub obca przestrzeń')
    proof.begin(phase)
    if phase == 'preflight':
        proof.record('preflight', {'storage': observation, 'format_authority': False,
                                  'capability_probe': 'wewnątrz Create przed reserve/mkfs'})
    elif phase == 'payload':
        payload(station, proof, modules, disks)
    elif phase == 'isolation':
        isolation(station, proof, modules, disks)
    elif phase in ('restore-live', 'inspect-live'):
        restore_live(station, phase, proof, modules, disks)
    elif phase == 'reboot-checkpoint':
        prepare_reboot(station, proof, modules, disks)
    elif reboot:
        proof.state['reboot_authorization_sha256'] = boot_authorization[1]
        proof.save()
        restore_reboot(station, phase, proof, modules, disks)
    elif phase in ('sync', 'scrub', 'nochange'):
        p_maintenance(station, phase, proof, modules, disks, read_payload)
    else:
        request = {'cmd': PHASE_COMMANDS[phase]}
        if phase == 'create':
            request['operation'] = station['spec']
        else:
            request.update(array_id=station['spec']['array_id'], owner=station['spec']['owner'])
        value = invoke_helper(station, request, proof)
        proof.record('typed-result', value)
        observation = storage.observe()
        proof.record('storage-after', observation)
        disks = guard_observation(station, observation, True)
        audit(station, proof, modules, disks)
    proof.complete(phase)
    return {'phase': phase, 'completed': True, 'pending': proof.state['pending']}


def payload_layout(station):
    if station['station'] == 'L':
        return {'payload-a.bin': (2 * 1024**2, b'A', 1000),
                'payload-b.bin': (2 * 1024**2, b'B', 1000), 'root-marker.bin': (4096, b'R', 0)}
    return {'payload-a.bin': (17 * 1024**2, b'A', 1000), 'payload-b.bin': (4096, b'B', 1000)}


def file_metric(path, limit=18 * 1024**2):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NOATIME)
    with os.fdopen(fd, 'rb') as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and before.st_size <= limit,
                'Obcy typ, dowiązanie lub rozmiar danych')
        sha = hashlib.file_digest(stream, 'sha256').hexdigest()
        after = os.fstat(stream.fileno())
        fields = ('st_dev', 'st_ino', 'st_size', 'st_uid', 'st_mode', 'st_nlink', 'st_mtime_ns')
        require(all(getattr(before, key) == getattr(after, key) for key in fields), 'Plik zmienił się podczas odczytu')
        return {'device': before.st_dev, 'inode': before.st_ino, 'bytes': before.st_size,
                'uid': before.st_uid, 'mode': stat.S_IMODE(before.st_mode), 'nlink': before.st_nlink,
                'mtime_ns': str(before.st_mtime_ns), 'sha256': sha}


def write_payload_file(path, size, byte):
    require(type(size) is int and 0 < size <= 17 * 1024**2 and isinstance(byte, bytes) and len(byte) == 1,
            'Obcy plan pliku danych')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        remaining = size
        chunk = byte * min(size, 1024**2)
        while remaining:
            written = os.write(fd, chunk[:min(remaining, len(chunk))])
            require(0 < written <= min(remaining, len(chunk)), 'Niepełny zapis danych')
            remaining -= written
        os.fsync(fd)
    finally:
        os.close(fd)
    flush_directory(Path(path).parent)
    return file_metric(path)


def validate_payload(station, measured, baseline=None, reboot=False, devices=None, union_device=None):
    layout = payload_layout(station)
    require(isinstance(measured, dict) and set(measured) == {'files', 'union_names', 'branch_names'} and
            set(measured['files']) == set(layout) and sorted(measured['union_names']) == sorted(layout),
            'Obcy zestaw danych unii')
    branches = {str(path / 'a21-fixture'): disk['expected_uuid'] for kind, _, disk, path in role_paths(station)
                if kind == 'data'}
    require(set(measured['branch_names']) == set(branches), 'Niepełny zbiór prywatnych katalogów')
    located = []
    for parent, names in measured['branch_names'].items():
        require(isinstance(names, list) and len(names) == len(set(names)) and set(names) <= set(layout),
                'Obcy plik prywatnego katalogu')
        located.extend((parent, name) for name in names)
    require(len(located) == len(layout) and {name for _, name in located} == set(layout), 'Duplikaty lub brak danych')
    for name, (size, byte, uid) in layout.items():
        row = measured['files'][name]
        require(set(row) == {'union', 'backing', 'path', 'fs_uuid'}, 'Pola pomiaru danych')
        parent = str(Path(row['path']).parent)
        require(parent in branches and Path(row['path']).name == name and
                row['fs_uuid'] == branches[parent] and (parent, name) in located, 'Obce rozmieszczenie danych')
        if devices is not None:
            require(row['backing'].get('device') == devices[row['fs_uuid']], 'Obcy device danych')
        if union_device is not None:
            require(row['union'].get('device') == union_device, 'Obcy device FUSE danych')
        expected_sha = hashlib.sha256(byte * size).hexdigest()
        for metric in (row['union'], row['backing']):
            require(metric['bytes'] == size and metric['sha256'] == expected_sha and metric['uid'] == uid and
                    metric['mode'] == 0o600 and metric['nlink'] == 1 and type(metric['device']) is int and
                    type(metric['inode']) is int and metric['inode'] > 0 and
                    isinstance(metric['mtime_ns'], str) and re.fullmatch('[0-9]+', metric['mtime_ns']),
                    'Niezgodna treść lub tożsamość danych')
        if name == 'root-marker.bin':
            require(parent == str(role_paths(station)[1][3] / 'a21-fixture'), 'Kontrola root nie jest na data2')
        if baseline is not None:
            previous = baseline['files'][name]
            require(row['path'] == previous['path'] and row['fs_uuid'] == previous['fs_uuid'], 'Przemieszczenie danych')
            ignored = {'device'} if reboot else set()
            require({key: value for key, value in row['backing'].items() if key not in ignored} ==
                    {key: value for key, value in previous['backing'].items() if key not in ignored},
                    'Zmiana trwałych danych lub metadanych')
    return measured


def child_value(child, proof, label):
    raw = child.receive()
    proof.record(label, raw)
    require(raw.get('error') is None, 'Niepełny wynik procesu')
    value = decode(base64.b64decode(raw['raw']))
    require(isinstance(value, dict) and 'fatal' not in value, 'Odmowa procesu: ' + str(value))
    return value


def stop_child(child, proof):
    result = child.stop()
    proof.record('payload-child-exit', result)
    require(not result['alive'], 'Proces payload nadal żyje')


def payload_prepare(channel, descriptors, ns_fd, station, anchor, disks, a0, a1):
    namespace_audit(channel, descriptors, ns_fd, station, anchor, disks, a0, a1)
    require(a1.operation(channel) == 'prepare', 'Brak zgody na własne katalogi')
    directories = {}
    for kind, index, _, path in role_paths(station):
        if kind != 'data':
            continue
        directory = path / 'a21-fixture'
        directory.mkdir(mode=0o700)
        fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            os.fchown(fd, 1000, 1000)
            os.fsync(fd)
            info = os.fstat(fd)
            directories[str(directory)] = {'uid': info.st_uid, 'mode': stat.S_IMODE(info.st_mode),
                                            'device': info.st_dev, 'inode': info.st_ino}
        finally:
            os.close(fd)
        flush_directory(path)
        if station['station'] == 'L' and index == 2:
            write_payload_file(directory / 'root-marker.bin', 4096, b'R')
    a1.send(channel, {'directories': directories})


def payload_writer(channel, descriptors, station, a1):
    identity = a1.enter_actor(False)
    a1.send(channel, {'identity': identity, 'descriptors': descriptors})
    require(a1.operation(channel) == 'write', 'Brak zgody na dane przez FUSE')
    directory = Path('/mnt') / station['spec']['name'] / 'a21-fixture'
    files = {name: write_payload_file(directory / name, size, byte)
             for name, (size, byte, uid) in payload_layout(station).items() if uid == 1000}
    a1.send(channel, {'files': files})


def payload_reader(channel, descriptors, ns_fd, station, anchor, disks, a0, a1):
    os.setns(ns_fd, 0)
    os.close(ns_fd)
    os.chdir('/')
    require(os.stat('/proc/self/ns/mnt').st_ino == anchor['mount_ns_inode'], 'Inna namespace odczytu danych')
    union = Path('/mnt') / station['spec']['name'] / 'a21-fixture'
    names = sorted(os.listdir(union))
    branch_names, files = {}, {}
    for kind, _, disk, path in role_paths(station):
        if kind != 'data':
            continue
        directory = path / 'a21-fixture'
        branch_names[str(directory)] = sorted(os.listdir(directory))
        for name in branch_names[str(directory)]:
            if name not in payload_layout(station) or name in files:
                continue
            try:
                backing, public = file_metric(directory / name), file_metric(union / name)
            except (OSError, ValueError) as error:
                backing = public = {'error': type(error).__name__, 'errno': getattr(error, 'errno', None),
                                    'detail': str(error)}
            files[name] = {'path': str(directory / name), 'fs_uuid': disk['expected_uuid'],
                           'backing': backing, 'union': public}
    a1.send(channel, {'files': files, 'union_names': names, 'branch_names': branch_names})


def payload(station, proof, modules, disks):
    require(proof.state['pending'] == 'payload' and 'payload' not in proof.state['completed'],
            'Brak trwałej fazy danych')
    a0, a1 = modules['guest_writer_gate_probe.py'], modules['guest_branch_isolation_probe.py']
    lock = open_product_lock()
    ns_fd = None
    try:
        initial = audit(station, proof, modules, disks)
        anchor = initial['journal']['private']['anchor']
        ns_fd = os.open(f'/proc/{anchor["pid"]}/ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
        require(os.fstat(ns_fd).st_ino == anchor['mount_ns_inode'], 'Obcy FD namespace danych')
        require(anchor_identity(anchor, a0) == initial['anchor'], 'Kotwica danych zmieniła się')
        args = (ns_fd, station, anchor, disks, a0, a1)
        child = a1.Child(payload_prepare, args, (ns_fd, lock))
        try:
            inside = child_value(child, proof, 'payload-prepare-before')
            validate_inside(station, anchor, disks, inside, a0)
            child.send('prepare')
            prepared = child_value(child, proof, 'payload-prepared')
            wanted = {str(path / 'a21-fixture'): next(d['device'] for d in disks.values()
                      if d['serial'] == disk['serial']) for kind, _, disk, path in role_paths(station) if kind == 'data'}
            require(set(prepared['directories']) == set(wanted), 'Niepełne katalogi testowe')
            for path, device in wanted.items():
                metric = prepared['directories'][path]
                require(metric['uid'] == 1000 and metric['mode'] == 0o700 and metric['device'] == device
                        and metric['inode'] > 0, 'Obcy katalog testowy')
        finally:
            stop_child(child, proof)
        writer = a1.Child(payload_writer, (station, a1))
        try:
            ready = child_value(writer, proof, 'payload-writer-ready')
            a0.validate_actor(ready['identity'], False, a1.parent_uid(writer))
            require(set(ready['descriptors']) == {'0', '1', '2', str(writer.child_fd)} and
                    ready['identity']['before']['mnt_ns'] == os.stat('/proc/self/ns/mnt').st_ino,
                    'Obce FD lub namespace pisarza')
            writer.send('write')
            written = child_value(writer, proof, 'payload-writer-result')
            wanted = {name: (size, byte) for name, (size, byte, uid) in payload_layout(station).items() if uid == 1000}
            require(set(written['files']) == set(wanted), 'Niepełny zapis UID1000')
            for name, (size, byte) in wanted.items():
                metric = written['files'][name]
                require(metric['uid'] == 1000 and metric['bytes'] == size and
                        metric['sha256'] == hashlib.sha256(byte * size).hexdigest(), 'Niepotwierdzony zapis UID1000')
        finally:
            stop_child(writer, proof)
        reader = a1.Child(payload_reader, args, (ns_fd, lock))
        try:
            measured = child_value(reader, proof, 'payload-measured')
            devices = {disk['expected_uuid']: next(d['device'] for d in disks.values() if d['serial'] == disk['serial'])
                       for disk in station['spec']['data']}
            validate_payload(station, measured, devices=devices, union_device=anchor['union_device'])
        finally:
            stop_child(reader, proof)
        final = audit(station, proof, modules, disks)
        require(final['journal_sha256'] == initial['journal_sha256'] and final['anchor'] == initial['anchor'],
                'Zmiana journala lub kotwicy podczas danych')
        proof.record('payload-baseline', measured)
        proof.state['payload_baseline'] = proof.state['events'][-1]
        proof.save()
        return measured
    finally:
        if ns_fd is not None:
            os.close(ns_fd)
        os.close(lock)


def read_namespace(station, proof, modules, disks, entry, extra=(), label='private-read'):
    a0 = modules['guest_writer_gate_probe.py']
    a1 = modules['guest_branch_isolation_probe.py']
    lock = open_product_lock()
    try:
        before = audit(station, proof, modules, disks)
        anchor = before['journal']['private']['anchor']
        ns_fd = os.open(f'/proc/{anchor["pid"]}/ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
        try:
            require(os.fstat(ns_fd).st_ino == anchor['mount_ns_inode'], 'Inny namespace odczytu')
            require(anchor_identity(anchor, a0) == before['anchor'], 'Inna kotwica odczytu')
            child = a1.Child(entry, (ns_fd, station, anchor, disks, a0, a1, *extra), (ns_fd, lock))
            try:
                value = child_value(child, proof, label)
            finally:
                stop_child(child, proof)
            require(os.fstat(ns_fd).st_ino == anchor['mount_ns_inode'], 'Namespace zmienił się po odczycie')
            after = audit(station, proof, modules, disks)
            require(after['journal_sha256'] == before['journal_sha256'] and
                    after['anchor'] == before['anchor'] and after['host'] == before['host'],
                    'Tożsamość zmieniła się podczas odczytu')
            return {'audit': before, 'value': value}
        finally:
            os.close(ns_fd)
    finally:
        os.close(lock)


def payload_baseline(proof):
    name = proof.state.get('payload_baseline')
    require(isinstance(name, str) and re.fullmatch(r'[0-9]{3}-payload-baseline\.json', name)
            and name in proof.state['events'], 'Brak zatwierdzonego baseline payloadu')
    return decode(read_private(proof.path / name))


def read_payload(station, proof, modules, disks, baseline=None, reboot=False):
    baseline = payload_baseline(proof) if baseline is None else baseline
    result = read_namespace(station, proof, modules, disks, payload_reader, label='payload-readback')
    devices = {disk['expected_uuid']: disks[next(role for role, value in station['disks'].items()
               if value['serial'] == disk['serial'])]['device'] for disk in station['spec']['data']}
    validate_payload(station, result['value'], baseline, reboot, devices,
                     result['audit']['journal']['private']['anchor']['union_device'])
    proof.record('payload-verified', {'baseline': proof.state.get('payload_baseline'), 'reboot': reboot})
    return result['value']


def isolation(station, proof, modules, disks):
    require(station['station'] == 'L' and proof.state['pending'] == 'isolation'
            and 'isolation' not in proof.state['completed'], 'Brak fazy izolacji')
    a0, a1 = modules['guest_writer_gate_probe.py'], modules['guest_branch_isolation_probe.py']
    lock = open_product_lock()
    try:
        measured = read_payload(station, proof, modules, disks)
        target = measured['files']['payload-a.bin']
        require(target['backing']['uid'] == 1000, 'Cel izolacji nie należy do aktora')
        before = audit(station, proof, modules, disks)
        anchor = before['journal']['private']['anchor']
        public = '/mnt/' + station['spec']['name'] + '/a21-fixture/payload-a.bin'
        held = os.open(public, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
        try:
            candidates = list(Path(f'/proc/{anchor["pid"]}/fd').iterdir())
            require(len(candidates) <= 4096, 'Limit deskryptorów kotwicy')
            matches = []
            for path in candidates:
                try:
                    info = path.stat()
                except FileNotFoundError:
                    continue
                if (info.st_dev, info.st_ino) == (target['backing']['device'], target['backing']['inode']):
                    matches.append(str(path))
            proof.record('isolation-fd-candidates', {'matches': matches, 'anchor': anchor, 'target': target})
            require(bool(matches), 'Brak rzeczywistego FD payloadu w mergerfs')
            common = {'pid': anchor['pid'], 'start': str(anchor['start_ticks']), 'mnt_ns': anchor['mount_ns_inode']}
            targets = [dict(common, target='mergerfs_root', operation='open',
                            path=f'/proc/{anchor["pid"]}/root' + target['path'],
                            expected_device=target['backing']['device'], expected_inode=target['backing']['inode']),
                       dict(common, target='mergerfs_fd', operation='open', path=matches[0],
                            expected_device=target['backing']['device'], expected_inode=target['backing']['inode'])]
            ns_info = os.stat(f'/proc/{anchor["pid"]}/ns/mnt')
            targets.append(dict(common, target='mergerfs_ns', operation='setns',
                                path=f'/proc/{anchor["pid"]}/ns/mnt', expected_device=ns_info.st_dev,
                                expected_inode=anchor['mount_ns_inode']))
            raw_targets = a1.target_evidence(targets)
            proof.record('isolation-targets-before', raw_targets)
            a1.validate_targets(raw_targets, targets)
            probes = [{'target': 'host_path', 'operation': 'open', 'path': target['path']}, *targets,
                      {'target': 'public_fuse', 'operation': 'open', 'path': public}]
            for namespace in (False, True):
                child = a1.Child(a1.isolation_actor, (namespace, probes))
                try:
                    ready = child_value(child, proof, 'isolation-ready')
                    a0.validate_actor(ready['identity'], namespace, a1.parent_uid(child))
                    proof.record('isolation-actor-fds', a1.actor_fds(child, ready['fds']))
                    require(ready['identity']['before']['mnt_ns'] == os.stat('/proc/self/ns/mnt').st_ino,
                            'Aktor odziedziczył prywatną namespace')
                    child.send('probe')
                    result = child_value(child, proof, 'isolation-probe')
                    validate_isolation(result, probes, a1)
                    child.send('stop')
                finally:
                    stop_child(child, proof)
            after_targets = a1.target_evidence(targets)
            proof.record('isolation-targets-after', after_targets)
            a1.validate_targets(after_targets, targets)
            require(after_targets == raw_targets, 'Cel izolacji zmienił tożsamość')
        finally:
            os.close(held)
        read_payload(station, proof, modules, disks)
        after = audit(station, proof, modules, disks)
        require(after['journal_sha256'] == before['journal_sha256'] and after['anchor'] == before['anchor'],
                'Izolacja zmieniła journal lub kotwicę')
        proof.record('isolation-accepted', {'actors': 2, 'denials': 8, 'public_open': 2,
                                         'setns_measurement': 'próba open→setns; odmowa open nie dowodzi wywołania setns'})
    finally:
        os.close(lock)


def validate_isolation(result, probes, a1):
    rows = result.get('rows')
    require(result.get('operation') == 'probe' and isinstance(rows, list) and len(rows) == len(probes),
            'Niepełny wynik izolacji')
    public = [row for row in rows if row['target'] == 'public_fuse']
    require(len(public) == 1 and public[0] == {'target': 'public_fuse', 'operation': 'open',
                                            'errno': None, 'value': 0}, 'Brak dodatniej kontroli FUSE')
    require(a1.isolation_result([row for row in rows if row['target'] != 'public_fuse'],
                                [row for row in probes if row['target'] != 'public_fuse']),
            'Bezpośrednia droga do danych nie jest odizolowana')


def restore_live(station, phase, proof, modules, disks):
    require(station['station'] == 'L' and phase in ('restore-live', 'inspect-live')
            and proof.state['pending'] == phase and phase not in proof.state['completed'], 'Brak fazy Restore/Inspect')
    read_payload(station, proof, modules, disks)
    before = audit(station, proof, modules, disks)
    value = invoke_helper(station, {'cmd': PHASE_COMMANDS[phase], 'array_id': station['spec']['array_id'],
                                   'owner': station['spec']['owner']}, proof)
    proof.record('lifecycle-typed', value)
    after = audit(station, proof, modules, disks)
    require(before['journal'] == after['journal'] and before['anchor'] == after['anchor']
            and before['host'] == after['host'], 'Restore żywej macierzy zmienił trwałą tożsamość lub publikację')
    read_payload(station, proof, modules, disks)
    proof.record('lifecycle-accepted', {'phase': phase, 'journal_sha256': after['journal_sha256']})


def reboot_receipt(proof):
    name = proof.state.get('reboot_checkpoint')
    require(isinstance(name, str) and re.fullmatch(r'[0-9]{3}-reboot-ready\.json', name)
            and name in proof.state['events'], 'Brak trwałego checkpointu restartu')
    raw = read_private(proof.path / name)
    return decode(raw), hashlib.sha256(raw).hexdigest()


def prepare_reboot(station, proof, modules, disks):
    require(station['station'] == 'L' and proof.state['pending'] == 'reboot-checkpoint'
            and 'reboot-checkpoint' not in proof.state['completed'] and
            'reboot_checkpoint' not in proof.state, 'Brak fazy przygotowania restartu')
    read_payload(station, proof, modules, disks)
    measured = audit(station, proof, modules, disks)
    proof.record('reboot-ready', {'station_sha256': proof.state['station_sha256'],
                 'initial_boot': station['initial_boot'], 'journal': measured['journal'],
                 'journal_sha256': measured['journal_sha256'], 'anchor': measured['anchor'],
                 'payload_baseline': proof.state['payload_baseline']})
    proof.state['reboot_checkpoint'] = proof.state['events'][-1]
    proof.save()


def validate_reboot_authorization(station, proof, value, sha):
    digest(sha)
    require(station['station'] == 'L' and isinstance(value, dict) and set(value) ==
            {'schema', 'station_sha256', 'initial_boot', 'boot_id', 'reboot_receipt_sha256'},
            'Pola zatwierdzenia restartu')
    receipt, receipt_sha = reboot_receipt(proof)
    require(type(value['schema']) is int and value['schema'] == 1 and
            value['station_sha256'] == proof.state['station_sha256'] == receipt['station_sha256'] and
            value['initial_boot'] == station['initial_boot'] == receipt['initial_boot'] and
            value['reboot_receipt_sha256'] == receipt_sha and
            receipt['payload_baseline'] == proof.state['payload_baseline'], 'Obcy checkpoint restartu')
    canonical_uuid(value['boot_id'])
    require(value['boot_id'] != station['initial_boot'], 'Boot nie zmienił się po restarcie')
    require(proof.state.get('reboot_authorization_sha256', sha) == sha, 'Inne zatwierdzenie restartu')
    validate_journal(station, receipt['journal'])
    return {**station, 'initial_boot': value['boot_id']}


def restore_reboot(station, phase, proof, modules, disks):
    require(station['station'] == 'L' and phase in ('restore-reboot', 'inspect-reboot') and
            proof.state['pending'] == phase and phase not in proof.state['completed'], 'Brak fazy po restarcie')
    receipt, _ = reboot_receipt(proof)
    require(station['initial_boot'] != receipt['initial_boot'] and
            proof.state.get('reboot_authorization_sha256'), 'Brak zatwierdzenia nowego boot')
    if phase == 'restore-reboot':
        lock = open_product_lock()
        try:
            raw = read_private(ROOT / (station['spec']['array_id'] + '.json'))
            proof.record('reboot-journal-before', {'raw': base64.b64encode(raw).decode(),
                         'sha256': hashlib.sha256(raw).hexdigest()})
            require(hashlib.sha256(raw).hexdigest() == receipt['journal_sha256'] and
                    decode(raw) == receipt['journal'], 'Journal zmienił się przed jedynym Restore')
        finally:
            os.close(lock)
    else:
        before = audit(station, proof, modules, disks)
        read_payload(station, proof, modules, disks, reboot=True)
    result = invoke_helper(station, {'cmd': PHASE_COMMANDS[phase], 'array_id': station['spec']['array_id'],
                                    'owner': station['spec']['owner']}, proof)
    proof.record('reboot-typed', result)
    after = audit(station, proof, modules, disks)
    stable = lambda journal: {key: value for key, value in journal.items() if key not in ('boot_id', 'private')}
    require(stable(after['journal']) == stable(receipt['journal']), 'Trwałe dane journala zmieniły się po restarcie')
    if phase == 'inspect-reboot':
        require(after['journal'] == before['journal'] and after['anchor'] == before['anchor'] and
                after['host'] == before['host'], 'Inspect zmienił kotwicę po restarcie')
    read_payload(station, proof, modules, disks, reboot=True)
    proof.record('reboot-accepted', {'phase': phase, 'boot_id': station['initial_boot'],
                                   'journal_sha256': after['journal_sha256']})


def p_paths(station):
    require(station['station'] == 'P', 'Ochrona wyłącznie stanowiska P')
    name = station['spec']['name']
    parity = next(path for kind, _, _, path in role_paths(station) if kind == 'parity')
    return {'config': Path('/etc/tentanas') / f'snapraid-{name}.conf',
            'content_system': Path('/etc/tentanas') / f'{name}-snapraid.content',
            'content_parity': parity / 'snapraid.content', 'parity': parity / 'snapraid.parity'}


def p_config(station):
    paths = p_paths(station)
    name = station['spec']['name']
    data = next(path for kind, _, _, path in role_paths(station) if kind == 'data')
    lines = [f"# Managed by TentaNas — Elastic Array '{name}'. Do not edit: the app",
             '# rewrites this file from tentanas.db on every change (plan-02 §3.4).',
             f'parity {paths["parity"]}', f'content {paths["content_system"]}',
             f'content {paths["content_parity"]}', f'data d1 {data}']
    lines.extend('exclude ' + value for value in
                 ('/lost+found/', '/tmp/', '*.unrecoverable', '.AppleDouble', '._AppleDouble', '.DS_Store'))
    return ('\n'.join([*lines, 'blocksize 256', 'autosave 500']) + '\n').encode()


def p_files_reader(channel, descriptors, ns_fd, station, anchor, disks, a0, a1):
    os.setns(ns_fd, 0)
    os.close(ns_fd)
    os.chdir('/')
    require(os.stat('/proc/self/ns/mnt').st_ino == anchor['mount_ns_inode'], 'Obca namespace ochrony')
    measured = {}
    for key, path in p_paths(station).items():
        try:
            private_parents(path)
            value = file_metric(path, 32 * 1024**2 if key == 'parity' else LIMIT)
            if key == 'config':
                value['raw'] = base64.b64encode(read_private(path)).decode()
            measured[key] = value
        except (OSError, ValueError) as error:
            measured[key] = {'error': type(error).__name__, 'errno': getattr(error, 'errno', None),
                             'detail': str(error)}
    a1.send(channel, measured)


def p_validate_files(station, measured, disks, protected, baseline=None):
    require(set(measured) == set(p_paths(station)), 'Niepełne metadane ochrony')
    parity_serial = station['spec']['parity'][0]['serial']
    parity_device = next(d['device'] for d in disks.values() if d['serial'] == parity_serial)
    for key, value in measured.items():
        require('error' not in value and value['uid'] == 0 and value['mode'] == 0o600
                and value['nlink'] == 1 and value['inode'] > 0, 'Obcy plik ochrony: ' + key)
        if key in ('parity', 'content_parity'):
            require(value['device'] == parity_device, 'Obcy filesystem ochrony')
        if protected:
            require(value['bytes'] > 0, 'Pusty plik ochrony: ' + key)
    config = measured['config']
    raw = base64.b64decode(config['raw'], validate=True)
    require(raw == p_config(station) and len(raw) == config['bytes'] and
            hashlib.sha256(raw).hexdigest() == config['sha256'], 'Obcy config')
    require(measured['content_system']['device'] == config['device'], 'Obcy systemowy content')
    if protected:
        require(measured['content_system']['sha256'] == measured['content_parity']['sha256']
                and measured['content_system']['bytes'] == measured['content_parity']['bytes'],
                'Nierówne kopie content')
    if baseline is not None:
        require(config == baseline['config'], 'Config zmienił się podczas ochrony')
    return measured


def p_packed(raw):
    return {'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest(),
            'base64': base64.b64encode(raw).decode()}


def p_receipt(station, phase, proof):
    validate_station(station)
    require(station['station'] == 'P' and phase in ('sync', 'scrub', 'nochange'), 'Obcy receipt')
    operation = station['maintenance_ids'][phase]
    lock = open_product_lock()
    try:
        path = ROOT / (station['spec']['array_id'] + '.json')
        raw = read_private(path)
        result = {'journal': p_packed(raw), 'logs': {}}
        proof.record('receipt-journal-raw', result['journal'])
        directory = ROOT / (station['spec']['array_id'] + '.runs') / operation
        private_parents(directory / 'run.log')
        require(all(stat.S_IMODE(p.lstat().st_mode) == 0o700 for p in (directory, directory.parent)),
                'Nieprywatny katalog logów')
        names = sorted(os.listdir(directory))
        result['names'] = names
        proof.record('receipt-names', names)
        expected = {f'{label}.{suffix}' for label in (('run', 'diff') if phase == 'scrub' else ('run',))
                    for suffix in ('log', 'stdout', 'stderr')}
        for name in sorted(expected):
            try:
                data = read_private(directory / name)
                require(len(data) <= 128 * 1024, 'Limit logu 128 KiB')
                result['logs'][name] = p_packed(data)
            except (OSError, ValueError) as error:
                result['logs'][name] = {'error': type(error).__name__, 'detail': str(error)}
            proof.record('receipt-' + name.replace('.', '-'), result['logs'][name])
        after = read_private(path)
        result['journal_after'] = p_packed(after)
        proof.record('receipt-raw', result)
        require(raw == after and set(names) == expected, 'Zmieniony journal lub obce logi')
        return result
    finally:
        os.close(lock)


def p_log_fields(raw):
    fields = {}
    for line in raw.decode('utf-8').replace('\r', '\n').splitlines():
        require(not line.startswith(('error:', 'parity_error:', 'msg:error:', 'msg:fatal:')), 'Błąd narzędzia')
        prefix = 'summary:' if line.startswith('summary:') else 'conf:' if line.startswith('conf:file:') else ''
        key, separator, value = line[len(prefix):].partition(':')
        if separator:
            fields.setdefault(prefix + key, []).append(value)
    return fields


def p_validate_receipt(station, phase, result, typed, before_journal):
    from datetime import datetime
    journal = decode(base64.b64decode(result['journal']['base64'], validate=True))
    validate_journal(station, journal)
    run = journal['last_run']
    require(run == typed['run'] and run['operation_id'] == station['maintenance_ids'][phase], 'Obcy last_run')
    require(type(run['exit_code']) is int and type(typed['run']['exit_code']) is int
            and run['exit_code'] == 0, 'Nieprawidłowy kod narzędzia')
    require(all(isinstance(run[key], str) and run[key].endswith('Z') for key in ('started_at', 'finished_at'))
            and datetime.fromisoformat(run['started_at']) <= datetime.fromisoformat(run['finished_at']), 'Daty receipt')
    request = {'cmd': 'elastic_scrub' if phase == 'scrub' else 'elastic_sync',
               'array_id': station['spec']['array_id'], 'owner': station['spec']['owner'],
               'operation_id': station['maintenance_ids'][phase]}
    validate_result(typed, request, station)
    if phase == 'scrub':
        require(journal['sync_completed_at'] == before_journal['sync_completed_at'], 'Scrub zmienił datę Sync')
    else:
        require(journal['sync_completed_at'] == run['finished_at'], 'Brak daty Sync')
    for label in (('run', 'diff') if phase == 'scrub' else ('run',)):
        logs = {}
        for suffix in ('log', 'stdout', 'stderr'):
            packed = result['logs'][label + '.' + suffix]
            require('error' not in packed, 'Niepełny eksport logów')
            raw = base64.b64decode(packed['base64'], validate=True)
            require(len(raw) == packed['bytes'] and hashlib.sha256(raw).hexdigest() == packed['sha256'], 'SHA logu')
            logs[suffix] = raw
        require(not logs['stderr'] and logs['log'] and logs['stdout'], 'Niepełne wyjście narzędzia')
        fields = p_log_fields(logs['log'])
        allowed_summary = {'exit', 'error_file', 'error_io', 'error_data', 'equal', 'added', 'removed',
                           'updated', 'moved', 'copied', 'restored'}
        require(all(not key.startswith('summary:') or key[8:] in allowed_summary for key in fields),
                'Nieznane podsumowanie narzędzia')
        command = 'diff' if label == 'diff' else ('scrub' if phase == 'scrub' else 'sync')
        require(all(fields.get(key) == [expected] for key, expected in
                    (('command', command), ('conf:file', str(p_paths(station)['config'])),
                     ('blocksize', '262144'), ('mode', 'par1'))), 'Obca tożsamość logu')
        if label == 'diff' or phase != 'scrub':
            scan = ('equal', 'added', 'removed', 'updated', 'moved', 'copied', 'restored')
            require(all(len(fields.get('summary:' + key, [])) == 1 and
                        fields['summary:' + key][0].isdigit() for key in scan), 'Niepełny scan')
            changed = any(int(fields['summary:' + key][0]) != 0 for key in scan[1:])
            require(changed == (phase == 'sync'), 'Niezgodny scan danych')
        exits = ['equal'] if label == 'diff' else ['ok'] if phase == 'scrub' else ['diff' if phase == 'sync' else 'equal', 'ok']
        require(fields.get('summary:exit') == exits, 'Niepełny terminalny log')
        if label == 'diff':
            continue
        require(all(fields.get('summary:' + key) == ['0'] for key in ('error_file', 'error_io', 'error_data')),
                'Niepotwierdzone błędy logu')
        stdout = [line.strip() for line in logs['stdout'].decode().replace('\r', '\n').splitlines()]
        progress = [line for line in stdout if '% completed, ' in line]
        if phase == 'nochange':
            require(stdout.count('Nothing to do') == 1 and stdout.count('Everything OK') == 0 and not progress
                    and logs['log'].splitlines().count(b'msg:status: Nothing to do') == 1,
                    'Brak semantyki Nothing to do')
        else:
            require(stdout.count('Everything OK') == 1 and 'Nothing to do' not in stdout and len(progress) == 1
                    and progress[0].startswith('100% completed, '), 'Niepełna praca narzędzia')
            accessed = re.fullmatch(r'100% completed, ([0-9]+) MB accessed.*', progress[0])
            require(accessed is not None and int(accessed[1]) == run['accessed_mb'], 'Inny odczyt MB')
        require(fields.get('block_count', []) == ([] if run['total_blocks'] is None else [str(run['total_blocks'])]),
                'Inna liczba bloków')
        if phase == 'scrub':
            require(fields.get('block_count') == [str(run['total_blocks'])] and
                    fields.get('info_count') == [str(run['checked_blocks'])], 'Inne bloki Scrub')
    return journal


def p_maintenance(station, phase, proof, modules, disks, read_payload):
    validate_station(station)
    require(station['station'] == 'P' and phase in ('sync', 'scrub', 'nochange') and
            proof.state['pending'] == phase and phase not in proof.state['completed'], 'Brak fazy ochrony')
    read_payload(station, proof, modules, disks)
    before = read_namespace(station, proof, modules, disks, p_files_reader, label='protection-before')
    p_validate_files(station, before['value'], disks, phase != 'sync')
    request = {'cmd': 'elastic_scrub' if phase == 'scrub' else 'elastic_sync',
               'array_id': station['spec']['array_id'], 'owner': station['spec']['owner'],
               'operation_id': station['maintenance_ids'][phase]}
    try:
        typed = invoke_helper(station, request, proof)
    except Exception:
        p_receipt(station, phase, proof)
        raise
    proof.record('maintenance-typed', typed)
    receipt = p_receipt(station, phase, proof)
    journal = p_validate_receipt(station, phase, receipt, typed, before['audit']['journal'])
    invoke_helper(station, {'cmd': 'elastic_inspect', 'array_id': station['spec']['array_id'],
                           'owner': station['spec']['owner']}, proof)
    after = read_namespace(station, proof, modules, disks, p_files_reader, label='protection-after')
    p_validate_files(station, after['value'], disks, True, before['value'])
    require(after['audit']['journal'] == journal, 'Inspect zmienił journal ochrony')
    if phase != 'sync':
        require(after['value']['parity'] == before['value']['parity'], 'Parzystość zmieniła się bez Sync danych')
    read_payload(station, proof, modules, disks)
    result = {'phase': phase, 'run': typed['run'], 'files': after['value'], 'journal': journal}
    proof.record('maintenance-accepted', result)
    return result


def main(argv):
    if argv == ['--help']:
        print('guest_private_lifecycle.py PHASE STATION_JSON SHA256 [BOOT_JSON BOOT_SHA256]; '
              'PHASE: preflight/create/inspect-created/payload/isolation/restore-live/inspect-live/'
              'reboot-checkpoint/restore-reboot/inspect-reboot/sync/scrub/nochange')
        return 0
    require(len(argv) in (3, 5) and argv[0] in ('preflight', 'create', 'inspect-created', 'payload', 'isolation',
            'restore-live', 'inspect-live', 'reboot-checkpoint', 'restore-reboot', 'inspect-reboot',
            'sync', 'scrub', 'nochange') and (len(argv) == 5) ==
            (argv[0] in ('restore-reboot', 'inspect-reboot')), 'Argumenty fazy')
    require(os.geteuid() == 0 and os.getuid() == 0, 'Wymagany operator root w przypiętej VM')
    station = load_station(argv[1], argv[2])
    boot_authorization = (decode(read_private(argv[3], digest(argv[4]))), argv[4]) if len(argv) == 5 else None
    modules = load_support()
    private_parents(BASE)
    if not BASE.exists():
        BASE.mkdir(mode=0o700)
        flush_directory(BASE.parent)
    proof = Proof(BASE / station['station'], argv[2])
    lock = os.open(proof.path / '.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        info = os.fstat(lock)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == UID and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) == 0o600, 'Obcy lock harnessu')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        proof = Proof(BASE / station['station'], argv[2])
        print(json.dumps(execute(argv[0], station, proof, modules, boot_authorization), sort_keys=True))
        return 0
    finally:
        os.close(lock)


if __name__ == '__main__':
    try:
        sys.exit(main(sys.argv[1:]))
    except Exception as error:
        print(json.dumps({'status': 'refused', 'error': type(error).__name__, 'detail': str(error)}))
        sys.exit(1)
