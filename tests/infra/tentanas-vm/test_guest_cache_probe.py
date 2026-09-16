# =============================================================================
# Plik: tests/infra/tentanas-vm/test_guest_cache_probe.py
# Opis: Niezależne regresje odmowy i dowodów sondy E2-03 bez VM i storage tools.
# Przykład: python3 -m unittest discover -s tests/infra/tentanas-vm -p test_guest_cache_probe.py -v
# =============================================================================

import copy
import errno
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import guest_storage as storage_fixture
import socket
from concurrent.futures import ThreadPoolExecutor
import base64


SOURCE = Path(__file__).with_name('guest_cache_probe.py')
spec = importlib.util.spec_from_file_location('e203_actual_probe', SOURCE)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
BOOT = '66933281-2e3d-498a-9bfc-38af18a6128c'


def observation():
    result = {'uuid': probe.VM_UUID, 'boot_id': BOOT, 'swaps': [], 'disks': [],
              'mounts': [{'target': '/', 'source': '/dev/vda1', 'fstype': 'ext4', 'maj:min': '252:1'}]}
    for index, (role, size) in enumerate(probe.SIZES.items()):
        result['disks'].append({'name': 'nvme0n1' if role == 'cache' else 'vd' + chr(97 + index),
                               'serial': 'tn-49efc20d4b-' + role, 'size': size * probe.GIB,
                               'type': 'disk', 'ro': False, 'maj:min': f'252:{index * 16}',
                               'fstype': None, 'uuid': None, 'holders': [], 'signatures': [],
                               'children': [{'maj:min': '252:1'}] if role == 'os' else []})
    return result


class Guards(unittest.TestCase):
    def setUp(self):
        self.value = observation()

    def test_positive_exact_six_blank_disks(self):
        self.assertEqual(set(probe.validate_observation(self.value, None)), {'cache', 'data'})

    def test_retired_vm_is_refused_with_its_original_serials(self):
        for identity, prefix in [('44fc2cf1-06f3-4135-a26b-220a2d5beba5', 'tn-44fc2cf106-'),
                                 ('5185b7bd-0460-4af9-b103-c4d198134889', 'tn-5185b7bd04-')]:
            value = copy.deepcopy(self.value)
            value['uuid'] = identity
            for disk in value['disks']:
                disk['serial'] = disk['serial'].replace('tn-49efc20d4b-', prefix)
            with self.subTest(identity=identity), patch.object(probe, 'command') as mutation:
                with self.assertRaises(ValueError):
                    probe.guard_storage(SimpleNamespace(observe=lambda: value), None, False)
                mutation.assert_not_called()

    def test_identity_negatives_refuse_before_mutating_commands(self):
        cases = []
        for field, value in [('serial', 'foreign'), ('size', 1), ('size', True), ('ro', True),
                             ('type', 'part'), ('maj:min', '252:0'), ('holders', ['dm-0']),
                             ('children', [{'maj:min': '252:17'}]), ('fstype', 'xfs'),
                             ('uuid', BOOT), ('signatures', [{'type': 'gpt'}]), ('name', 'sdb')]:
            changed = copy.deepcopy(self.value)
            changed['disks'][1][field] = value
            cases.append((field, changed))
        for field, value in [('uuid', BOOT), ('boot_id', 'unknown'), ('swaps', ['252:16'])]:
            changed = copy.deepcopy(self.value)
            changed[field] = value
            cases.append((field, changed))
        changed = copy.deepcopy(self.value)
        changed['mounts'][0]['maj:min'] = '252:16'
        cases.append(('root_on_target', changed))
        changed = copy.deepcopy(self.value)
        changed['mounts'].append({'target': '/foreign', 'maj:min': '252:16'})
        cases.append(('foreign_mount', changed))
        changed = copy.deepcopy(self.value)
        changed['disks'].append(copy.deepcopy(changed['disks'][1]))
        cases.append(('extra_disk', changed))
        for name, value in cases:
            with self.subTest(name=name), patch.object(probe, 'command') as mutation:
                with self.assertRaises(ValueError):
                    probe.guard_storage(SimpleNamespace(observe=lambda: value), None, False)
                mutation.assert_not_called()

    def mounted_fixture(self):
        state = {'boot_id': BOOT, 'filesystems': {'cache': '11111111-1111-4111-8111-111111111111',
                                                'data': '22222222-2222-4222-8222-222222222222'},
                 'mounted': ['cache', 'data'], 'lock': {'device': 7, 'inode': 9}}
        rows = []
        for role, index in [('cache', 1), ('data', 3)]:
            disk = self.value['disks'][index]
            disk.update(fstype='xfs', uuid=state['filesystems'][role], signatures=[{'type': 'xfs'}])
            self.value['mounts'].append({'target': str(probe.MOUNTS / role), 'source': '/dev/' + disk['name'],
                                         'fstype': 'xfs', 'maj:min': disk['maj:min']})
            rows.append({'target': str(probe.MOUNTS / role), 'number': disk['maj:min'], 'root': '/',
                         'options': ['rw'], 'filesystem': 'xfs', 'source': '/dev/' + disk['name']})
        rows.append({'target': str(probe.MOUNTS / 'union'), 'number': '0:100', 'root': '/',
                     'options': ['rw'], 'filesystem': 'fuse.mergerfs',
                     'source': 'cache:data'})
        return state, rows

    def run_guard(self, state, rows, options=None):
        def lstat(path):
            index = 1 if str(path) == '/dev/vdb' else 3
            return SimpleNamespace(st_mode=stat.S_IFBLK | 0o600, st_rdev=os.makedev(252, index * 16))

        def branch_stat(path):
            return SimpleNamespace(st_dev=os.makedev(252, 16 if path.name == 'cache' else 48))

        configured = probe.options() if options is None else options
        with patch.object(probe, 'mount_rows', return_value=rows), \
                patch.object(probe, 'lock_identity', return_value={'device': 7, 'inode': 9}), \
                patch.object(Path, 'lstat', lstat), patch.object(Path, 'stat', branch_stat), \
                patch.object(probe.os, 'getxattr', side_effect=lambda path, key: configured[key[14:]].encode()):
            return probe.guard_storage(SimpleNamespace(observe=lambda: self.value,
                                      space=lambda path: {'available': 3 * probe.GIB}), state)

    def test_positive_mounted_guard_uses_real_validator(self):
        state, rows = self.mounted_fixture()
        self.assertEqual(set(self.run_guard(state, rows)[1]), {'cache', 'data'})

    def test_mount_and_options_negative_boundaries(self):
        state, rows = self.mounted_fixture()
        for index, field, value in [(0, 'root', '/subdir'), (0, 'source', '/dev/vdc'),
                                    (0, 'filesystem', 'ext4'), (0, 'options', ['ro']),
                                    (2, 'source', 'foreign'), (2, 'root', '/subdir')]:
            changed = copy.deepcopy(rows)
            changed[index][field] = value
            with self.subTest(field=field, index=index), self.assertRaises(ValueError):
                self.run_guard(state, changed)
        for key in probe.options():
            changed = probe.options()
            changed[key] = 'foreign'
            with self.subTest(option=key), self.assertRaises(ValueError):
                self.run_guard(state, rows, changed)
        with self.assertRaises(ValueError):
            self.run_guard(state, rows + [{**rows[0], 'target': str(probe.MOUNTS / 'cache' / 'nested')}])
        state['boot_id'] = '33333333-3333-4333-8333-333333333333'
        with self.assertRaises(ValueError):
            self.run_guard(state, rows)

    def test_formatted_filesystem_and_duplicate_uuid_refused(self):
        state, rows = self.mounted_fixture()
        original = copy.deepcopy(self.value)
        for field, value in [('fstype', 'ext4'), ('uuid', BOOT), ('signatures', []),
                             ('signatures', [{'type': 'xfs'}, {'type': 'gpt'}])]:
            self.value = copy.deepcopy(original)
            self.value['disks'][1][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                self.run_guard(state, rows)
        self.value = copy.deepcopy(original)
        state['filesystems']['data'] = state['filesystems']['cache']
        self.value['disks'][3]['uuid'] = state['filesystems']['cache']
        with self.assertRaises(ValueError):
            self.run_guard(state, rows)


class LocalIO(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='e203-independent-')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.mounts = self.root / 'mnt'
        self.base = self.root / 'journal'
        self.base.mkdir(mode=0o700)
        self.mounts.mkdir(mode=0o700)
        for role in ('cache', 'data', 'union'):
            (self.mounts / role).mkdir(mode=0o700)
        self.scoped_patch(probe, 'BASE', self.base)
        self.scoped_patch(probe, 'MOUNTS', self.mounts)
        self.scoped_patch(probe, 'UID', os.getuid())
        self.scoped_patch(probe, 'parents', side_effect=self.parents)
        self.storage = SimpleNamespace(flush_directory=storage_fixture.flush_directory, private=self.private)

    def scoped_patch(self, obj, name, *args, **kwargs):
        handle = patch.object(obj, name, *args, **kwargs)
        self.addCleanup(handle.stop)
        return handle.start()

    def parents(self, path):
        self.assertTrue(Path(path).is_relative_to(self.root))
        self.assertEqual(Path(path).parent.resolve(), Path(path).parent)

    def private(self, path, directory=False):
        value = path.lstat()
        self.assertEqual(value.st_uid, os.getuid())
        self.assertFalse(value.st_mode & 0o077)
        self.assertTrue(stat.S_ISDIR(value.st_mode) if directory else stat.S_ISREG(value.st_mode))

    def state(self):
        return {'schema': 1, 'vm_uuid': probe.VM_UUID, 'boot_id': BOOT, 'pending': None,
                'stage': 'nc', 'filesystems': {}, 'mounted': [], 'measurements': {}}

    def test_journal_fsync_failure_keeps_old_state_and_blocks_overwrite(self):
        state = self.state()
        probe.persist(state, self.storage)
        original = (self.base / 'state.json').read_bytes()
        state['pending'] = 'race'
        with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'fixture')):
            with self.assertRaises(OSError):
                probe.persist(state, self.storage)
        self.assertEqual((self.base / 'state.json').read_bytes(), original)
        with self.assertRaises(FileExistsError):
            probe.persist(state, self.storage)

    def test_pending_and_repeated_stage_deny_before_guard_or_body(self):
        for changes in [{'pending': 'race'}, {'stage': 'race'}, {'vm_uuid': BOOT}]:
            state = self.state()
            state.update(changes)
            probe.persist(state, self.storage)
            with patch.object(probe, 'guard_storage') as guard, patch.object(probe, 'race') as body:
                with self.assertRaises(ValueError):
                    probe.execute('race', self.storage, None)
                guard.assert_not_called()
                body.assert_not_called()

    def test_phase_pending_is_durable_before_real_race_entry(self):
        state = self.state()
        probe.persist(state, self.storage)

        def fail_at_entry(*args):
            self.assertEqual(json.loads((self.base / 'state.json').read_bytes())['pending'], 'race')
            raise RuntimeError('kontrolowane przerwanie przed operacją')

        with patch.object(probe, 'guard_storage'), patch.object(probe, 'race', side_effect=fail_at_entry):
            with self.assertRaisesRegex(RuntimeError, 'kontrolowane'):
                probe.execute('race', self.storage, None)
        self.assertEqual(json.loads((self.base / 'state.json').read_bytes())['pending'], 'race')

    def test_real_pipe_child_writes_after_copy_and_keeps_both_files(self):
        source, target = self.mounts / 'cache' / 'race.bin', self.mounts / 'data' / 'race.bin'
        payload = bytes(range(256)) * 100
        probe.create_file(source, payload, self.storage)
        with tempfile.TemporaryFile(dir=self.root) as lock:
            result = probe.race_one(source, target, source, lock, self.storage, None)
        self.assertEqual(source.read_bytes(), payload + b'AFTER_COPY_MARKER')
        self.assertEqual(target.read_bytes(), payload)
        self.assertFalse(result['source_unlinked'])
        artifact = json.loads((self.base / 'race-cache-observation.json').read_bytes())
        self.assertEqual(json.loads(base64.b64decode(artifact['writer_stdout_b64'])), result['writer_fd'])
        self.assertEqual(artifact['writer_exit_code'], 0)

    def test_writer_metadata_refusal_preserves_raw_observation_before_predicate(self):
        source, target = self.mounts / 'cache' / 'race.bin', self.mounts / 'data' / 'race.bin'
        payload = b'original-data'
        probe.create_file(source, payload, self.storage)
        real_popen = subprocess.Popen
        recorded = {}

        def child_with_changed_report(*args, **kwargs):
            child = real_popen(*args, **kwargs)
            real_communicate = child.communicate

            def communicate(*args, **kwargs):
                output, errors = real_communicate(*args, **kwargs)
                writer = json.loads(output)
                writer['after']['bytes'] = writer['before']['bytes']
                recorded['raw'] = json.dumps(writer).encode()
                return recorded['raw'], errors

            child.communicate = communicate
            return child

        with tempfile.TemporaryFile(dir=self.root) as lock, \
                patch.object(probe.subprocess, 'Popen', side_effect=child_with_changed_report), \
                patch.object(probe.os, 'fsync', wraps=os.fsync) as flush:
            with self.assertRaisesRegex(ValueError, 'Inny rozmiar FD pisarza po markerze'):
                probe.race_one(source, target, source, lock, self.storage, None)
            self.assertGreaterEqual(flush.call_count, 3)
        artifact_path = self.base / 'race-cache-observation.json'
        artifact = json.loads(artifact_path.read_bytes())
        self.assertEqual(base64.b64decode(artifact['writer_stdout_b64']), recorded['raw'])
        self.assertEqual(artifact['status'], 'observation')
        self.assertEqual(artifact['writer_exit_code'], 0)
        self.assertEqual(artifact['source_after']['sha256'], hashlib.sha256(payload + b'AFTER_COPY_MARKER').hexdigest())
        self.assertEqual(artifact['target']['sha256'], hashlib.sha256(payload).hexdigest())
        self.assertEqual(stat.S_IMODE(artifact_path.stat().st_mode), 0o600)
        self.assertEqual(source.read_bytes(), payload + b'AFTER_COPY_MARKER')
        self.assertEqual(target.read_bytes(), payload)

    def test_union_metadata_is_observed_but_backing_marker_and_device_remain_strict(self):
        real_popen, real_metric = subprocess.Popen, probe.metric
        cases = [('stale_metadata', None), ('backing_device', 'Brak dowodu'),
                 ('backing_content', 'Brak dowodu'), ('marker', 'Inny marker'),
                 ('union_device', 'Obce urządzenie FUSE')]
        for name, error in cases:
            with self.subTest(name=name):
                base = self.root / name
                base.mkdir(mode=0o700)
                (base / 'alias').mkdir()
                source, target, writer_path = base / 'source', base / 'target', base / 'alias' / '..' / 'source'
                payload = b'original-data'
                probe.create_file(source, payload, self.storage)
                source_reads = 0

                def metric(path):
                    nonlocal source_reads
                    value = real_metric(path)
                    if path == source:
                        source_reads += 1
                        if source_reads == 2 and name == 'backing_device':
                            value['device'] += 1
                        if source_reads == 2 and name == 'backing_content':
                            value['sha256'] = '0' * 64
                    return value

                def changed_report(*args, **kwargs):
                    child = real_popen(*args, **kwargs)
                    real_communicate = child.communicate

                    def communicate(*args, **kwargs):
                        output, errors = real_communicate(*args, **kwargs)
                        writer = json.loads(output)
                        writer['before']['device'] = 777
                        writer['after'].update(device=778 if name == 'union_device' else 777,
                                               bytes=writer['before']['bytes'],
                                               inode=writer['before']['inode'] + 1)
                        if name == 'marker':
                            writer['marker'] = 'FOREIGN_MARKER'
                        return json.dumps(writer).encode(), errors

                    child.communicate = communicate
                    return child

                # Lokalna ścieżka z .. i kontrolowane metadane nie są montowaniem FUSE.
                with patch.object(probe, 'BASE', base), patch.object(probe, 'metric', side_effect=metric), \
                        patch.object(probe.subprocess, 'Popen', side_effect=changed_report), \
                        tempfile.TemporaryFile(dir=self.root) as lock:
                    if error:
                        with self.assertRaisesRegex(ValueError, error):
                            probe.race_one(source, target, writer_path, lock, self.storage, 777)
                    else:
                        result = probe.race_one(source, target, writer_path, lock, self.storage, 777)
                        differences = {(row['moment'], row['field']) for row in result['writer_differences']}
                        self.assertIn(('after', 'bytes'), differences)
                        self.assertIn(('after', 'inode'), differences)
                artifact = json.loads((base / 'race-union-observation.json').read_bytes())
                raw = json.loads(base64.b64decode(artifact['writer_stdout_b64']))
                self.assertEqual(raw['after']['bytes'], len(payload))
                self.assertEqual(artifact['union_device'], 777)
                self.assertEqual(artifact['writer_exit_code'], 0)
                self.assertEqual(source.read_bytes(), payload + b'AFTER_COPY_MARKER')
                self.assertEqual(target.read_bytes(), payload)

    def test_other_process_lock_blocks_main_before_execute(self):
        lock_path = self.base / '.lock'
        fd = os.open(lock_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(fd)
        child = subprocess.Popen([sys.executable, '-c',
            'import fcntl,sys; f=open(sys.argv[1],"rb"); fcntl.flock(f,fcntl.LOCK_EX); '
            'print("locked",flush=True); sys.stdin.read(1)', str(lock_path)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            self.assertEqual(child.stdout.readline().strip(), 'locked')
            self.storage.automation_guard = Mock()
            output = io.StringIO()
            with patch.object(Path, 'read_text', return_value=probe.VM_UUID), \
                    patch.object(probe, 'load_storage', return_value=self.storage), \
                    patch.object(probe, 'command', side_effect=lambda args: '2.40.2-5'
                                 if args[-1] == 'mergerfs' else '6.13.0-2+b1'), \
                    patch.object(probe, 'metric', side_effect=lambda path: {'sha256': probe.BINARY_SHA
                                 if path.name == 'mergerfs' else probe.MKFS_SHA}), \
                    patch.object(probe, 'execute') as execute, patch.object(probe.sys, 'stdout', output):
                self.assertEqual(probe.main(['race']), 1)
                execute.assert_not_called()
            self.assertIn('BlockingIOError', json.loads(output.getvalue())['error'])
        finally:
            child.communicate('1', timeout=5)
        self.assertEqual(child.returncode, 0)

    def test_fill_reserve_refusal_precedes_create_or_write(self):
        self.storage.space = lambda path: {'available': 0}
        with patch.object(probe, 'exclusive') as create, patch.object(probe, 'append_result') as append:
            with self.assertRaisesRegex(ValueError, 'Rezerwa OS'):
                probe.fill(-1, self.storage, self.state(), {}, -1, [])
            create.assert_not_called()
            append.assert_not_called()

    def test_fill_time_and_byte_limits_never_become_enospc(self):
        self.storage.space = lambda path: {'available': 25 * probe.GIB}
        with patch.object(probe.time, 'monotonic', side_effect=[0, probe.MAX_SECONDS + 1]), \
                patch.object(probe, 'append_result') as append:
            with self.assertRaisesRegex(ValueError, 'Limit czasu'):
                probe.fill(-1, self.storage, self.state(), {}, -1, [])
            append.assert_not_called()
        with patch.object(probe, 'MAX_FILL', 0):
            with self.assertRaisesRegex(ValueError, 'Limit bajtów'):
                probe.fill(-1, self.storage, self.state(), {}, -1, [])

    def test_actual_lock_rejects_replacement_hardlink_and_permissions(self):
        path = self.base / '.lock'
        old = os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
        path.rename(self.base / 'old-lock')
        new = os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
        os.close(new)
        with self.assertRaisesRegex(ValueError, 'Podmieniony lock'):
            probe.acquire_lock(old)
        with self.assertRaises(OSError) as closed:
            os.fstat(old)
        self.assertEqual(closed.exception.errno, errno.EBADF)
        os.link(path, self.base / 'hardlink')
        with self.assertRaisesRegex(ValueError, 'Niebezpieczny lock'):
            probe.lock_identity()
        (self.base / 'hardlink').unlink()
        path.chmod(0o644)
        with self.assertRaisesRegex(ValueError, 'Niebezpieczny lock'):
            probe.lock_identity()
        path.chmod(0o600)
        with probe.acquire_lock(os.open(path, os.O_RDONLY)) as lock:
            self.assertEqual(probe.lock_identity(lock.fileno()), probe.lock_identity())

    def test_format_intent_persisted_before_first_mutating_command(self):
        state = self.state()
        observed, found = {}, {'cache': {'name': 'vdb'}}

        def command(args, lock):
            recorded = json.loads((self.base / 'state.json').read_bytes())
            self.assertEqual(recorded['pending'], 'format_cache')
            self.assertEqual(recorded['expected_format']['role'], 'cache')
            self.assertEqual(args, ['/usr/sbin/mkfs.xfs', '-m',
                                   'uuid=' + recorded['expected_format']['uuid'], '/dev/vdb'])
            raise RuntimeError('odmowa adaptera przed mkfs')

        with patch.object(probe, 'guard_storage', return_value=(observed, found)), \
                patch.object(probe, 'command', side_effect=command) as commands:
            with self.assertRaisesRegex(RuntimeError, 'przed mkfs'):
                probe.nc(self.storage, state, None)
            self.assertEqual(commands.call_count, 1)
        self.assertEqual(json.loads((self.base / 'state.json').read_bytes())['pending'], 'format_cache')

    def test_failed_pending_fsync_or_zero_write_prevents_format(self):
        for operation in ('fsync', 'write'):
            with self.subTest(operation=operation), patch.object(probe, 'command') as command:
                with patch.object(probe.os, operation, side_effect=OSError(errno.EIO, 'fixture')
                                  if operation == 'fsync' else None,
                                  **({'return_value': 0} if operation == 'write' else {})):
                    with self.assertRaises((OSError, ValueError)):
                        probe.nc(self.storage, self.state(), None)
                command.assert_not_called()
                self.assertFalse((self.base / 'state.json').exists())
                (self.base / 'state.next').unlink()

    def test_command_child_keeps_inherited_lock_after_parent_close(self):
        path = self.base / '.lock'
        lock = probe.acquire_lock(os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600))
        listener = socket.socket(socket.AF_UNIX)
        self.addCleanup(listener.close)
        address = str(self.root / 'barrier.sock')
        listener.bind(address)
        listener.listen(1)
        listener.settimeout(5)
        script = ('import os,socket,sys; os.fstat(int(sys.argv[1])); '
                  's=socket.socket(socket.AF_UNIX); s.connect(sys.argv[2]); '
                  's.sendall(b"1"); assert s.recv(1)==b"1"; s.close()')
        with ThreadPoolExecutor(max_workers=1) as worker:
            future = worker.submit(probe.command, [sys.executable, '-c', script,
                                                   str(lock.fileno()), address], lock, 10)
            try:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(5)
                    self.assertEqual(connection.recv(1), b'1')
                    lock.close()
                    try:
                        with self.assertRaises(BlockingIOError):
                            probe.acquire_lock(os.open(path, os.O_RDONLY))
                    finally:
                        connection.sendall(b'1')
                self.assertEqual(future.result(timeout=5), '')
            finally:
                lock.close()
        with probe.acquire_lock(os.open(path, os.O_RDONLY)):
            self.assertTrue(path.exists())

    def test_fill_records_its_own_errno_after_actual_accepted_bytes(self):
        with tempfile.TemporaryFile(dir=self.root) as stream, tempfile.TemporaryFile(dir=self.root) as held:
            fd = stream.fileno()
            real_write, real_exclusive = os.write, probe.exclusive
            writes = 0

            def write(descriptor, data):
                nonlocal writes
                if descriptor == fd:
                    writes += 1
                    if writes > 1:
                        raise OSError(errno.ENOSPC, 'kontrolowany brak miejsca fillera')
                return real_write(descriptor, data)

            def exclusive(path):
                if path.name == 'below-minfree.bin':
                    raise OSError(errno.ENOSPC, 'kontrolowana odmowa progu')
                return real_exclusive(path)

            def space(path):
                if path == self.mounts / 'cache':
                    return {'available': 0, 'free': 2 * probe.GIB - os.fstat(fd).st_blocks * 512}
                return {'available': 25 * probe.GIB}

            self.storage.space = space
            self.storage.enospc_allocation = storage_fixture.enospc_allocation
            progress = {}
            with patch.object(probe.os, 'write', side_effect=write), \
                    patch.object(probe, 'exclusive', side_effect=exclusive), \
                    patch.object(probe, 'observed_options', return_value=probe.options()):
                result = probe.fill(fd, self.storage, self.state(), progress, held.fileno(), [])
            self.assertEqual(result['errno'], errno.ENOSPC)
            self.assertEqual(result['operation'], 'write')
            self.assertEqual(result['accepted'], os.fstat(fd).st_size)
            self.assertGreater(result['accepted'], 0)
            self.assertEqual(progress['below_minfree']['errno'], errno.ENOSPC)
            held.seek(0)
            self.assertEqual(held.read(), probe.THRESHOLD_MARKER)

    def test_threshold_records_errno_objects_and_refuses_before_more_writes(self):
        cases = [(errno.ENOSPC, None), (errno.EROFS, None), (errno.EIO, None),
                 (errno.ENOSPC, 'cache'), (errno.ENOSPC, 'data'),
                 (errno.EROFS, 'cache'), (errno.EROFS, 'data'), (None, None),
                 (errno.ENOSPC, 'unknown'), (errno.EROFS, 'options')]
        for index, (refusal, residue) in enumerate(cases):
            with self.subTest(errno=refusal, residue=residue):
                base = self.root / ('threshold-' + str(index))
                base.mkdir(mode=0o700)
                mounts = base / 'mounts'
                mounts.mkdir(mode=0o700)
                for role in ('cache', 'data', 'union'):
                    (mounts / role).mkdir(mode=0o700)
                real_exclusive, real_append, real_lstat = probe.exclusive, probe.append_result, Path.lstat
                opened = []
                state = self.state()
                state.update(stage='race', pending='enospc')
                progress = {}
                state['measurements']['enospc'] = progress
                allowed = refusal in (errno.ENOSPC, errno.EROFS) and residue is None

                def exclusive(path):
                    if path.name != 'below-minfree.bin':
                        return real_exclusive(path)
                    if residue in ('cache', 'data'):
                        os.close(real_exclusive(mounts / residue / path.name))
                    if refusal is not None:
                        raise OSError(refusal, 'kontrolowany wynik create')
                    result = real_exclusive(path)
                    opened.append(result)
                    return result

                def lstat(path, *args, **kwargs):
                    if residue == 'unknown' and path == mounts / 'cache' / 'below-minfree.bin':
                        raise OSError(errno.EACCES, 'kontrolowany brak odczytu')
                    return real_lstat(path, *args, **kwargs)

                def options():
                    value = probe.options()
                    if residue == 'options':
                        value['category.create'] = 'ff'
                    return value

                with tempfile.TemporaryFile(dir=base) as filler, tempfile.TemporaryFile(dir=base) as held:
                    def append(fd, payload, limit):
                        if fd == filler.fileno():
                            raise RuntimeError('granica przed kolejnym zapisem fillera')
                        return real_append(fd, payload, limit)

                    self.storage.space = lambda path: {'available': 0, 'free': 0} if path == mounts / 'cache' else {'available': 25 * probe.GIB}
                    with patch.object(probe, 'BASE', base), patch.object(probe, 'MOUNTS', mounts), \
                            patch.object(probe, 'exclusive', side_effect=exclusive), \
                            patch.object(probe, 'observed_options', side_effect=options), \
                            patch.object(Path, 'lstat', lstat), \
                            patch.object(probe, 'append_result', side_effect=append) as writes:
                        with self.assertRaisesRegex(RuntimeError if allowed else ValueError,
                                                    'granica przed' if allowed else 'create|Zmiana opcji'):
                            probe.fill(filler.fileno(), self.storage, state, progress, held.fileno(), [])
                        saved = json.loads((base / 'state.json').read_bytes())
                        raw = saved['measurements']['enospc']['below_minfree']
                        self.assertEqual(raw['errno'], refusal)
                        self.assertEqual(raw['created'], refusal is None)
                        self.assertEqual(raw['objects']['cache']['present'], None if residue == 'unknown' else residue == 'cache')
                        self.assertEqual(raw['objects']['data']['present'], residue == 'data')
                        self.assertEqual(raw['options'], options())
                        if residue == 'unknown':
                            self.assertEqual(raw['objects']['cache']['errno'], errno.EACCES)
                        self.assertEqual(raw['filler_accepted'], 0)
                        self.assertEqual(saved['pending'], 'enospc')
                        self.assertEqual(writes.call_count, 2 if allowed else 0)
                        held.seek(0)
                        self.assertEqual(held.read(), probe.THRESHOLD_MARKER if allowed else b'')
                        self.assertEqual(os.fstat(filler.fileno()).st_size, 0)
                        with patch.object(probe, 'guard_storage') as guard, patch.object(probe, 'enospc') as body:
                            with self.assertRaisesRegex(ValueError, 'Brak retry|brak retry'):
                                probe.execute('enospc', self.storage, None)
                            guard.assert_not_called()
                            body.assert_not_called()
                    for fd in opened:
                        with self.assertRaises(OSError):
                            os.fstat(fd)

    def test_append_distinguishes_write_fsync_and_success(self):
        for operation in ('write', 'fsync', 'completed'):
            with self.subTest(operation=operation), tempfile.TemporaryFile(dir=self.root) as stream:
                with patch.object(probe.os, 'write', side_effect=OSError(errno.ENOSPC, 'fixture')
                                  if operation == 'write' else lambda fd, data: len(data)), \
                        patch.object(probe.os, 'fsync', side_effect=OSError(errno.ENOSPC, 'fixture')
                                     if operation == 'fsync' else None):
                    result = probe.append_result(stream.fileno(), b'abcd', 4)
                self.assertEqual(result, {'accepted': 0 if operation == 'write' else 4,
                                         'errno': None if operation == 'completed' else errno.ENOSPC,
                                         'operation': operation})

    def test_preserved_projection_ignores_only_allocation(self):
        path = self.mounts / 'cache' / 'preserved.bin'
        probe.create_file(path, b'original', self.storage)
        actual = probe.metric(path)
        expected = dict(actual, allocated=actual['allocated'] + 32 * probe.CHUNK)
        self.assertEqual(probe.verify_preserved({str(path): expected}), {str(path): actual})
        for field in ('sha256', 'inode', 'mtime_ns', 'bytes', 'device'):
            changed = dict(expected)
            changed[field] = '0' * 64 if field == 'sha256' else changed[field] + 1
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'Dane poprzedniej fazy'):
                probe.verify_preserved({str(path): changed})

    def test_fill_measures_signed_reclaim_and_rejects_wrong_balance(self):
        preserved, held_path = self.mounts / 'cache' / 'preserved.bin', self.mounts / 'cache' / 'held.bin'
        probe.create_file(preserved, b'original', self.storage)
        probe.create_file(held_path, b'held', self.storage)
        real_metric = probe.metric
        delta = -32 * probe.CHUNK + 4096
        for mismatch in (0, 17 * probe.CHUNK):
            with self.subTest(mismatch=mismatch), tempfile.TemporaryFile(dir=self.root) as stream:
                fd = stream.fileno()
                after = False

                def metric(path):
                    value = real_metric(path)
                    value['allocated'] = (4096 if after else 4096 + 32 * probe.CHUNK) if path == preserved else (8192 if after else 4096)
                    return value

                def append(descriptor, payload, limit):
                    nonlocal after
                    accepted = os.write(descriptor, payload[:limit])
                    os.fsync(descriptor)
                    after = True
                    return {'accepted': accepted, 'errno': errno.ENOSPC, 'operation': 'write'}

                def space(path):
                    if path == self.mounts / 'cache':
                        consumed = os.fstat(fd).st_blocks * 512 + delta + mismatch if after else 0
                        return {'available': 0, 'free': 100 * probe.CHUNK - consumed}
                    return {'available': 25 * probe.GIB}

                self.storage.space = space
                self.storage.enospc_allocation = storage_fixture.enospc_allocation
                progress = {'below_minfree': {}}
                state = self.state()
                state['measurements']['enospc'] = progress
                with patch.object(probe, 'metric', side_effect=metric), \
                        patch.object(probe, 'append_result', side_effect=append):
                    if mismatch:
                        with self.assertRaisesRegex(ValueError, 'bilansu fill'):
                            probe.fill(fd, self.storage, state, progress, -1, [preserved, held_path])
                    else:
                        result = probe.fill(fd, self.storage, state, progress, -1, [preserved, held_path])
                        self.assertEqual(result['other_cache_allocation_delta'], delta)
                        self.assertEqual(result['expected_consumed'], result['observed_consumed'])
                        self.assertEqual(result['other_cache_allocation_before'][str(preserved)], 4096 + 32 * probe.CHUNK)
                        self.assertEqual(result['other_cache_allocation_after'][str(preserved)], 4096)
                        self.assertEqual(result['allocated'], os.fstat(fd).st_blocks * 512)
                        self.assertTrue(result['available_zero_allocation'])
                saved = json.loads((self.base / 'state.json').read_bytes())['measurements']['enospc']['fill']
                self.assertEqual(saved['observed_consumed'] - saved['expected_consumed'], mismatch)
                self.assertEqual(preserved.read_bytes(), b'original')

    def test_non_enospc_append_is_not_claimed_as_space_exhaustion(self):
        for error in (errno.EIO, errno.EROFS):
            with self.subTest(errno=error), patch.object(probe.os, 'write', side_effect=OSError(error, 'fixture')):
                with self.assertRaises(ValueError):
                    probe.append_result(-1, b'abc', 3)

    def test_successful_union_append_on_cache_is_not_spill(self):
        prefix = b'OPEN_BEFORE_FILL\n'
        union = self.mounts / 'union' / 'append.bin'
        cache = self.mounts / 'cache' / 'append.bin'
        real_create = probe.create_file

        def create(path, data, storage):
            real_create(cache, data, storage)
            union.symlink_to(cache)

        real_open = os.open

        def open_file(path, flags, *args):
            return real_open(cache if Path(path) == union else path, flags, *args)

        state = self.state()
        state['measurements'] = {'nc': {}, 'race': {}}
        state.update(stage='race', pending='enospc')
        for role, name, key in [('cache', 'new.bin', 'new'), ('data', 'existing.bin', 'existing')]:
            path = self.mounts / role / name
            probe.create_file(path, b'prior', self.storage)
            state['measurements']['nc'][key] = probe.metric(path)
        for mode in ('union', 'cache'):
            state['measurements']['race'][mode] = {}
            for role, key in [('cache', 'source_after'), ('data', 'target')]:
                path = self.mounts / role / ('race-' + mode + '.bin')
                probe.create_file(path, b'prior-race', self.storage)
                state['measurements']['race'][mode][key] = probe.metric(path)

        def filled(filler, storage, state, progress, held_fd, allocation_paths):
            probe.write_all(held_fd, probe.THRESHOLD_MARKER)
            os.fsync(held_fd)
            return {'errno': errno.ENOSPC}


        self.storage.space = lambda path: {'available': 25 * probe.GIB}
        with patch.object(probe, 'guard_storage'), patch.object(probe, 'fill', side_effect=filled), \
                patch.object(probe, 'append_result', wraps=probe.append_result) as append, \
                patch.object(probe, 'create_file', side_effect=create), patch.object(probe.os, 'open', side_effect=open_file):
            with self.assertRaisesRegex(ValueError, 'Nierozstrzygnięty union ENOSPC'):
                probe.enospc(self.storage, state, None)
            self.assertEqual(append.call_count, 1)
        recorded = json.loads((self.base / 'state.json').read_bytes())
        self.assertEqual(recorded['pending'], 'enospc')
        result = recorded['measurements']['enospc']
        self.assertEqual(result['location'], 'cache')
        self.assertEqual(result['append']['operation'], 'completed')
        self.assertEqual(result['append']['accepted'], 16 * probe.CHUNK)
        self.assertNotEqual(result['outcome'], 'spill_observed')
        self.assertTrue(cache.read_bytes().startswith(prefix))


if __name__ == '__main__':
    print('Testowany SHA256:', hashlib.sha256(SOURCE.read_bytes()).hexdigest(), flush=True)
    unittest.main()
