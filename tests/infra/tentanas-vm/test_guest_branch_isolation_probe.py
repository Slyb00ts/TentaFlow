# =============================================================================
# Plik: test_guest_branch_isolation_probe.py
# Opis: Rzeczywiste testy granic dowodu izolacji branchy bez mount, VM i sudo.
# Przykład: python3 -m unittest discover -s tests/infra/tentanas-vm -p test_guest_branch_isolation_probe.py
# =============================================================================

import array
import base64
import copy
from contextlib import ExitStack
import errno
import json
import os
from pathlib import Path
import socket
import stat
import tempfile
import unittest
from unittest.mock import patch
import guest_branch_isolation_probe as probe
import guest_writer_gate_probe as a0
import guest_storage as storage


def observation():
    result = {'uuid': probe.VM_UUID, 'boot_id': probe.BOOT_ID, 'swaps': [],
              'mounts': [{'target': '/', 'maj:min': '252:1'}], 'disks': []}
    for index, (role, size) in enumerate({'os': 12, 'data1': 32, 'data2': 32, 'parity': 40,
                                        'cache': 1, 'spare': 40}.items()):
        result['disks'].append({'serial': probe.PREFIX + role, 'type': 'disk',
            'name': 'nvme0n1' if role == 'cache' else 'vd' + chr(97 + index),
            'size': size * 1024**3, 'ro': False, 'maj:min': f'252:{index * 16}',
            'holders': [], 'signatures': [], 'fstype': None, 'uuid': None, 'mountpoints': [],
            'children': [{'maj:min': '252:1'}] if role == 'os' else []})
    return result


class GuardTests(unittest.TestCase):
    def test_foreign_vm_and_actual_inventory_validator_refuse_before_automation(self):
        cases = []
        for key, value in [('uuid', '5a6b69ca-df2a-4083-b90d-7bbd3f486a3f'),
                           ('boot_id', '95a783e6-9609-4d4b-a0cf-4725d87987f4')]:
            item = observation()
            item[key] = value
            cases.append(item)
        for key, value in [('type', 'part'), ('serial', 'foreign'), ('size', 1),
                           ('ro', True), ('holders', ['dm-0']), ('fstype', 'xfs'),
                           ('uuid', probe.BOOT_ID), ('signatures', [{'type': 'xfs'}])]:
            item = observation()
            item['disks'][1][key] = value
            cases.append(item)
        item = observation()
        item['disks'].append(copy.deepcopy(item['disks'][1]))
        cases.append(item)
        for item in cases:
            with self.subTest(observation=item), patch.object(storage, 'automation_guard') as automation, \
                    self.assertRaises((ValueError, RuntimeError)):
                probe.guard(item, storage)
            automation.assert_not_called()


class IsolationTests(unittest.TestCase):
    def setUp(self):
        self.expected = [{'target': target, 'operation': operation}
                         for target in ('host_path', 'daemon_root', 'daemon_fd', 'worker_fd', 'setns')
                         for operation in ('open', 'write')]
        self.rows = [dict(row, errno=errno.ENOENT if row['target'] == 'host_path' else errno.EACCES,
                          value=None) for row in self.expected]

    def test_exact_denials_without_claiming_target_liveness(self):
        self.assertTrue(probe.isolation_result(self.rows, self.expected))

    def test_incomplete_duplicate_or_foreign_matrix_refused(self):
        for rows in ([], self.rows[:-1], self.rows + self.rows[:1],
                     [self.rows[0]] * len(self.rows),
                     [dict(self.rows[0], target='foreign'), *self.rows[1:]]):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                probe.isolation_result(rows, self.expected)
        with self.assertRaises(ValueError):
            probe.isolation_result(self.rows, self.expected + self.expected[:1])

    def test_empty_expected_does_not_prove_isolation(self):
        with self.assertRaises(ValueError):
            probe.isolation_result([], [])

    def test_success_setup_errors_and_missing_object_are_not_denial_proof(self):
        for code, value in ((None, 15), (errno.EIO, None), (errno.EBADF, None),
                            (errno.ENOENT, None), (errno.EACCES, 0)):
            rows = copy.deepcopy(self.rows)
            rows[-1].update(errno=code, value=value)
            with self.subTest(errno=code, value=value):
                self.assertFalse(probe.isolation_result(rows, self.expected))

    def test_global_requires_three_pinned_aliases_and_positive_root_write(self):
        expected = [{'path': path, 'mnt_ns': namespace,
                     'mounts': [{'id': str(index), 'number': '0:77', 'root': '/', 'target': path,
                                 'filesystem': 'fuse.mergerfs', 'source': 'cache:data',
                                 'mount_options': ['rw'], 'super_options': ['rw']}],
                     'stat': {'errno': None, 'value': {'device': 77, 'readonly': False}}}
                    for index, (namespace, path) in enumerate([(11, '/union'), (11, '/bind'), (21, '/union')])]
        snapshots = copy.deepcopy(expected)
        for row in snapshots:
            row['mounts'][0]['super_options'] = ['ro']
            row['stat']['value']['readonly'] = True
        opens = [{'path': row['path'], 'mnt_ns': row['mnt_ns'],
                  'outcomes': {name: {'errno': errno.EROFS, 'value': None}
                               for name in ('wronly', 'rdwr', 'create', 'truncate')}} for row in expected]
        valid = {'remount': {'return': 0, 'errno': None}, 'snapshots': snapshots,
                 'opens': opens, 'expected': expected,
                 'root_write': {'errno': None, 'value': len(b'TRUSTED_ROOT\n')}}
        with patch.object(probe, 'a0', a0):
            self.assertTrue(probe.validate_global(valid))
            for case in ('missing', 'duplicate', 'foreign', 'rw', 'stat', 'open', 'root', 'busy'):
                result = copy.deepcopy(valid)
                if case == 'missing':
                    result['snapshots'].pop()
                elif case == 'duplicate':
                    result['snapshots'][1] = result['snapshots'][0]
                elif case == 'foreign':
                    result['snapshots'][0]['mounts'][0]['number'] = '0:99'
                elif case == 'rw':
                    result['snapshots'][0]['mounts'][0]['super_options'] = ['rw']
                elif case == 'stat':
                    result['snapshots'][0]['stat']['value']['readonly'] = False
                elif case == 'open':
                    result['opens'][0]['outcomes']['rdwr']['errno'] = errno.EACCES
                elif case == 'root':
                    result['root_write']['value'] = 0
                else:
                    result['remount'] = {'return': -1, 'errno': errno.EBUSY}
                with self.subTest(case=case), self.assertRaises(ValueError):
                    probe.validate_global(result)

    def test_missing_failed_or_short_markers_refused(self):
        valid = {'writes': [{'errno': None, 'value': 5}] * 3}
        probe.require_markers(valid, 3, 5)
        for writes in ([], valid['writes'][:2], [{'errno': errno.EIO, 'value': None}] * 3,
                       [{'errno': None, 'value': 4}] * 3):
            with self.subTest(writes=writes), self.assertRaises(ValueError):
                probe.require_markers({'writes': writes}, 3, 5)
        probe.require_markers({'writes': []}, 0, 4)
        for count in (1, 2):
            with self.subTest(mapping_count=count), self.assertRaises(ValueError):
                probe.require_markers({'writes': []}, count, 4)


class DescriptorTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.base = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.stack.enter_context(patch.object(probe, 'a0', a0))

    def transfer(self, count=1, marker=b'M', device=None, filesystem=None, record_failure=None):
        sender, receiver = socket.socketpair()
        self.stack.callback(sender.close)
        self.stack.callback(receiver.close)
        original = os.open(self.base, os.O_RDONLY | os.O_DIRECTORY)
        self.stack.callback(os.close, original)
        payload = array.array('i', [original] * count)
        sender.sendmsg([marker], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, payload)] if count else [])
        self.records = []
        def record(value):
            self.records.append(copy.deepcopy(value))
            path = self.base / ('record-' + str(len(list(self.base.iterdir()))) + '.json')
            fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            try:
                os.write(fd, json.dumps(value).encode())
                os.fsync(fd)
            finally:
                os.close(fd)
            if len(self.records) == record_failure:
                raise OSError(errno.EIO, 'recorder')
        with patch.object(probe, 'filesystem_type', return_value=probe.FUSE_MAGIC if filesystem is None else filesystem):
            return probe.receive_mount_fd(receiver, self.base.stat().st_dev if device is None else device, record)

    def test_real_scm_rights_device_and_close_on_exec(self):
        received = self.transfer()
        try:
            self.assertEqual(os.fstat(received).st_ino, self.base.stat().st_ino)
            self.assertFalse(os.get_inheritable(received))
        finally:
            os.close(received)

    def test_rejected_received_descriptors_are_closed(self):
        for case in ({'count': 0}, {'count': 2}, {'count': 8}, {'marker': b'X'},
                     {'device': self.base.stat().st_dev + 1}, {'filesystem': 0x01021994}):
            closed = []
            actual_close = os.close
            def close(fd):
                closed.append(fd)
                actual_close(fd)
            with self.subTest(case=case), patch.object(probe.os, 'close', side_effect=close), \
                    self.assertRaises(ValueError):
                self.transfer(**case)
            if case.get('count') != 0:
                self.assertTrue(closed)
            self.assertEqual(base64.b64decode(self.records[0]['raw']), case.get('marker', b'M'))
            for fd in self.records[0]['fds']:
                with self.assertRaises(OSError):
                    os.fstat(fd)

    def test_recorder_failure_keeps_raw_and_closes_received_fd(self):
        for phase in (1, 2):
            with self.subTest(phase=phase), self.assertRaises(OSError):
                self.transfer(record_failure=phase)
            self.assertEqual(len(self.records), phase)
            files = [json.loads(path.read_bytes()) for path in self.base.glob('record-*.json')]
            self.assertIn(json.loads(json.dumps(self.records[0])), files)
            for fd in self.records[0]['fds']:
                with self.assertRaises(OSError):
                    os.fstat(fd)

    def test_actual_fstatfs_rejects_closed_descriptor(self):
        fd = os.open(self.base, os.O_RDONLY | os.O_DIRECTORY)
        os.close(fd)
        with self.assertRaises(OSError) as raised:
            probe.filesystem_type(fd)
        self.assertEqual(raised.exception.errno, errno.EBADF)

    def test_actual_child_closes_foreign_fd_and_keeps_explicit_role_fd(self):
        foreign = os.open(self.base / 'foreign', os.O_CREAT | os.O_RDWR, 0o600)
        allowed = os.open(self.base / 'allowed', os.O_CREAT | os.O_RDWR, 0o600)
        self.stack.callback(os.close, foreign)
        self.stack.callback(os.close, allowed)
        def actor(channel, descriptors):
            probe.send(channel, {'descriptors': descriptors, 'core': list(probe.resource.getrlimit(probe.resource.RLIMIT_CORE))})
            channel.recv(1)
        child = probe.Child(actor, allowed_fds=(allowed,))
        try:
            response = child.receive()
            self.assertIsNone(response['error'])
            value = json.loads(base64.b64decode(response['raw']))
            self.assertNotIn(str(foreign), value['descriptors'])
            self.assertEqual(value['descriptors'][str(allowed)], str(self.base / 'allowed'))
            self.assertEqual(value['core'], [0, 0])
        finally:
            self.assertFalse(child.stop()['alive'])
        self.assertEqual(os.fstat(foreign).st_size, 0)

    def test_target_evidence_reads_actual_live_process_and_fd(self):
        fd = os.open(self.base, os.O_RDONLY | os.O_DIRECTORY)
        other_path = self.base / 'other'
        other_path.mkdir()
        other_fd = os.open(other_path, os.O_RDONLY | os.O_DIRECTORY)
        def actor(channel, descriptors):
            probe.send(channel, {'ready': True})
            channel.recv(1)
        child = probe.Child(actor, allowed_fds=(fd, other_fd))
        try:
            self.assertIsNone(child.receive()['error'])
            target = {'target': 'worker_fd', 'pid': child.pid, 'start': child.start,
                      'mnt_ns': os.stat(f'/proc/{child.pid}/ns/mnt').st_ino,
                      'path': f'/proc/{child.pid}/fd/{fd}',
                      'expected_device': self.base.stat().st_dev, 'expected_inode': self.base.stat().st_ino}
            row = probe.target_evidence([target])[0]
            self.assertEqual((row['device'], row['inode']), (self.base.stat().st_dev, self.base.stat().st_ino))
            probe.validate_targets([row], [target])
            foreign = dict(target, path=f'/proc/{child.pid}/fd/{other_fd}')
            before = probe.target_evidence([foreign])
            self.assertEqual(before, probe.target_evidence([foreign]))
            with self.assertRaises(ValueError):
                probe.validate_targets(before, [foreign])
            for rows in ([], [row, row], [dict(row, device=row['device'] + 1)]):
                with self.subTest(rows=rows), self.assertRaises(ValueError):
                    probe.validate_targets(rows, [target])
            for key, value in [('start', 'foreign'), ('mnt_ns', target['mnt_ns'] + 1),
                               ('path', f'/proc/{child.pid}/fd/999999')]:
                with self.subTest(key=key), self.assertRaises((ValueError, OSError)):
                    probe.target_evidence([dict(target, **{key: value})])
        finally:
            self.assertFalse(child.stop()['alive'])
            os.close(fd)
            os.close(other_fd)
        with self.assertRaises((ValueError, OSError)):
            probe.target_evidence([target])

    def test_actual_actor_fd_guard_requires_null_standard_fds_and_root_cwd(self):
        def actor(channel, descriptors):
            probe.send(channel, {'fds': descriptors})
            channel.recv(1)
        child = probe.Child(actor)
        try:
            reported = json.loads(base64.b64decode(child.receive()['raw']))['fds']
            self.assertEqual(probe.actor_fds(child, reported), reported)
            self.assertEqual([reported[str(fd)] for fd in (0, 1, 2)], ['/dev/null'] * 3)
            for changed in ({}, dict(reported, **{'999': '/foreign'})):
                with self.assertRaises(ValueError):
                    probe.actor_fds(child, changed)
        finally:
            self.assertFalse(child.stop()['alive'])


class ProofTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.base = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.base.chmod(0o700)
        self.stack.enter_context(patch.object(probe, 'BASE', self.base))
        self.stack.enter_context(patch.object(probe, 'a0', a0))
        actual_lstat = Path.lstat
        def root_metadata(path, *args, **kwargs):
            info = actual_lstat(path, *args, **kwargs)
            fields = list(info)
            fields[4] = 0
            if path in self.base.parents:
                fields[0] = stat.S_IFDIR | 0o755
            return os.stat_result(fields)
        self.stack.enter_context(patch.object(Path, 'lstat', root_metadata))

    def test_payload_exact_real_files_markers_and_identity_refusals(self):
        self.assertEqual(os.getuid(), 1000, 'Ten pomiar plików wymaga lokalnego UID 1000')
        names = ['held.bin', 'mapping.bin', 'truncate.bin', 'direct.bin']
        for name in names:
            path = self.base / name
            path.write_bytes(b'B' * 4096)
            path.chmod(0o600)
        initial = {name: probe.metric(self.base / name) for name in names}
        with (self.base / 'held.bin').open('ab') as stream:
            stream.write(b'BASELINE\n' * 3 + b'HELD\n' * 3)
        with (self.base / 'direct.bin').open('ab') as stream:
            stream.write(b'TRUSTED_ROOT\n')
        final = {'payload': {name: probe.metric(self.base / name) for name in names},
                 'names': names, 'data_empty': True}
        self.assertTrue(probe.validate_payload(initial, final, [0, 0]))
        for field in ('device', 'inode', 'uid', 'mode', 'nlink', 'bytes', 'sha256', 'mtime_ns'):
            changed = copy.deepcopy(final)
            row = changed['payload']['truncate.bin']
            row[field] = '0' * 64 if field == 'sha256' else row[field] + 1
            with self.subTest(field=field), self.assertRaises(ValueError):
                probe.validate_payload(initial, changed, [0, 0])
        for variant in ('missing', 'extra', 'duplicate', 'data', 'initial'):
            before, after = copy.deepcopy(initial), copy.deepcopy(final)
            if variant == 'missing':
                del after['payload']['held.bin']
            elif variant == 'extra':
                after['payload']['foreign'] = after['payload']['held.bin']
            elif variant == 'duplicate':
                after['names'] = names + names[:1]
            elif variant == 'data':
                after['data_empty'] = False
            else:
                before['held.bin']['sha256'] = '0' * 64
            with self.subTest(variant=variant), self.assertRaises(ValueError):
                probe.validate_payload(before, after, [0, 0])
        with (self.base / 'mapping.bin').open('r+b') as stream:
            stream.write(b'POST')
        final['payload']['mapping.bin'] = probe.metric(self.base / 'mapping.bin')
        self.assertTrue(probe.validate_payload(initial, final, [2, 1]))
        with (self.base / 'direct.bin').open('r+b') as stream:
            stream.seek(4096)
            stream.write(b'WRONG')
        final['payload']['direct.bin'] = probe.metric(self.base / 'direct.bin')
        with self.assertRaises(ValueError):
            probe.validate_payload(initial, final, [2, 1])

    def test_actual_json_request_scm_ack_boundaries_preserve_following_json(self):
        proof = probe.Proof()
        directory = os.open(self.base, os.O_RDONLY | os.O_DIRECTORY)
        def actor(channel, descriptors):
            probe.send(channel, {'operation': 'ready'})
            if probe.operation(channel) != 'request_fd':
                raise ValueError('Inne polecenie')
            channel.sendmsg([b'M'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [directory]))])
            if probe.operation(channel) != 'fd_received':
                raise ValueError('Brak odbioru')
            probe.send(channel, {'operation': 'exported'})
            channel.recv(1)
        child = probe.Child(actor, allowed_fds=(directory,))
        received = None
        try:
            self.assertEqual(probe.collect(child, proof), {'operation': 'ready'})
            proof.begin('request-fuse-fd')
            child.send('request_fd')
            with patch.object(probe, 'filesystem_type', return_value=probe.FUSE_MAGIC):
                received = probe.receive_mount_fd(child.channel, self.base.stat().st_dev,
                    lambda value: proof.record('fuse-fd', value))
            child.send('fd_received')
            self.assertEqual(probe.collect(child, proof), {'operation': 'exported'})
            self.assertEqual(os.fstat(received).st_ino, self.base.stat().st_ino)
            raw = json.loads((self.base / '001-fuse-fd.json').read_bytes())
            self.assertEqual(base64.b64decode(raw['raw']), b'M')
        finally:
            self.assertFalse(child.stop()['alive'])
            os.close(directory)
            if received is not None:
                os.close(received)

    def test_mount_call_pending_and_context_are_durable_before_syscall_boundary(self):
        proof = probe.Proof()
        def function():
            self.assertEqual(json.loads((self.base / 'state.json').read_bytes())['pending'], 'remount')
            before = json.loads((self.base / '000-remount-before.json').read_bytes())
            self.assertTrue(a0.parse_mounts(before['mountinfo']))
            probe.ctypes.set_errno(errno.EBUSY)
            return -1
        self.assertEqual(probe.mount_call(proof, 'remount', function), {'return': -1, 'errno': errno.EBUSY})
        self.assertEqual(json.loads((self.base / '001-remount.json').read_bytes())['errno'], errno.EBUSY)
        with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'journal')), \
                patch.object(probe.ctypes, 'set_errno') as entered, self.assertRaises(OSError):
            probe.mount_call(proof, 'next', function)
        entered.assert_not_called()

    def test_real_exchange_child_observes_durable_pending_before_command(self):
        proof = probe.Proof()
        def actor(channel, descriptors):
            command = probe.operation(channel)
            state = json.loads((probe.BASE / 'state.json').read_bytes())
            probe.send(channel, {'operation': command, 'pending': state['pending']})
            channel.recv(1)
        child = probe.Child(actor)
        try:
            self.assertEqual(probe.exchange(child, proof, 'inspect'), {'operation': 'inspect', 'pending': 'inspect'})
            self.assertTrue(list(self.base.glob('*-child-raw.json')))
        finally:
            self.assertFalse(child.stop()['alive'])

    def test_deadline_refuses_before_begin_and_keeps_raw_without_ack(self):
        proof = probe.Proof()
        proof.deadline = 0
        with self.assertRaises(ValueError):
            proof.begin('late')
        self.assertFalse((self.base / 'state.json').exists())
        def actor(channel, descriptors):
            probe.event(channel, 'late-result', {'return': -1, 'errno': errno.EBUSY})
        child = probe.Child(actor)
        try:
            with patch.object(child, 'send', wraps=child.send) as sent, self.assertRaises(ValueError):
                probe.collect(child, proof)
            sent.assert_not_called()
            raw = json.loads((self.base / '000-child-raw.json').read_bytes())['response']['raw']
            self.assertEqual(json.loads(base64.b64decode(raw))['event'], 'late-result')
        finally:
            self.assertFalse(child.stop()['alive'])

    def test_real_malformed_and_fatal_child_keep_raw_before_refusal(self):
        for fatal in (False, True):
            proof = probe.Proof()
            def actor(channel, descriptors):
                if fatal:
                    raise PermissionError(errno.EACCES, 'setup')
                channel.sendall(b'{malformed\n')
                channel.recv(1)
            child = probe.Child(actor)
            try:
                with self.subTest(fatal=fatal), self.assertRaises(ValueError):
                    probe.collect(child, proof)
                response = json.loads((self.base / '000-child-raw.json').read_bytes())['response']
                decoded = base64.b64decode(response['raw'])
                if fatal:
                    self.assertEqual(json.loads(decoded)['errno'], errno.EACCES)
                else:
                    self.assertEqual(decoded, b'{malformed\n')
            finally:
                self.assertFalse(child.stop()['alive'])
            (self.base / '000-child-raw.json').rename(self.base / ('preserved-' + str(fatal) + '.json'))

    def test_real_event_journal_failure_prevents_ack(self):
        proof = probe.Proof()
        proof.begin('mount')
        def actor(channel, descriptors):
            probe.event(channel, 'mount-result', {'return': -1, 'errno': errno.EPERM})
        child = probe.Child(actor)
        try:
            actual_replace = os.replace
            replacements = []
            def replace(source, target):
                replacements.append(str(source))
                if len(replacements) == 2:
                    raise OSError(errno.EIO, 'journal')
                return actual_replace(source, target)
            with patch.object(probe.os, 'replace', side_effect=replace), \
                    patch.object(child, 'send', wraps=child.send) as sent, self.assertRaises(OSError):
                probe.collect(child, proof)
            sent.assert_not_called()
            self.assertEqual(json.loads((self.base / '001-mount-result.json').read_bytes())['errno'], errno.EPERM)
            self.assertEqual(json.loads((self.base / 'state.json').read_bytes())['pending'], 'mount')
        finally:
            self.assertFalse(child.stop()['alive'])

    def test_real_failed_syscall_is_recorded_before_child_refusal(self):
        proof = probe.Proof()
        def actor(channel, descriptors):
            libc = probe.ctypes.CDLL(None, use_errno=True)
            probe.syscall(channel, 'close-invalid', lambda: libc.close(-1))
        child = probe.Child(actor)
        try:
            with self.assertRaisesRegex(ValueError, 'Błąd dziecka'):
                probe.collect(child, proof)
            self.assertEqual(json.loads((self.base / '001-close-invalid.json').read_bytes()),
                             {'return': -1, 'errno': errno.EBADF})
        finally:
            self.assertFalse(child.stop()['alive'])

    def test_real_pending_and_raw_record_survive_reopen(self):
        proof = probe.Proof()
        proof.begin('actor')
        raw = {'errno': errno.EACCES, 'raw': base64.b64encode(b'permission denied').decode()}
        proof.record('response', raw)
        state = json.loads((self.base / 'state.json').read_bytes())
        self.assertEqual(state['pending'], 'actor')
        self.assertEqual(json.loads((self.base / state['events'][0]).read_bytes()), raw)
        self.assertTrue(state['tmpfs_volatile'])
        self.assertFalse(state['mergerfs_preserved'])

    def test_raw_file_remains_when_state_persistence_fails(self):
        proof = probe.Proof()
        proof.begin('actor')
        with patch.object(probe.os, 'replace', side_effect=OSError(errno.EIO, 'journal')), self.assertRaises(OSError):
            proof.record('response', {'fatal': 'PermissionError', 'errno': errno.EACCES})
        self.assertEqual(json.loads((self.base / '000-response.json').read_bytes())['errno'], errno.EACCES)
        self.assertEqual(json.loads((self.base / 'state.json').read_bytes())['pending'], 'actor')
        with self.assertRaises(FileExistsError):
            probe.Proof().begin('retry')

    def test_pending_fsync_failure_is_not_durable_success(self):
        proof = probe.Proof()
        with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'journal')), self.assertRaises(OSError):
            proof.begin('mount')
        self.assertFalse((self.base / 'state.json').exists())
        self.assertTrue((self.base / 'state.next').exists())

    def test_record_rejects_existing_symlink_and_event_limit(self):
        proof = probe.Proof()
        target = self.base / 'target'
        target.write_bytes(b'unchanged')
        (self.base / '000-response.json').symlink_to(target)
        with self.assertRaises(FileExistsError):
            proof.record('response', {'errno': 0})
        self.assertEqual(target.read_bytes(), b'unchanged')
        proof.state['events'] = ['existing'] * 160
        with self.assertRaises(ValueError):
            proof.record('other', {'errno': 0})

    def test_real_pinned_helper_import_and_foreign_file_refusals(self):
        original = Path(a0.__file__).read_bytes()
        target = self.base / 'helper.py'
        target.write_bytes(original)
        target.chmod(0o600)
        with patch.object(probe, 'A0_PATH', target):
            loaded = probe.load_helpers()
        self.assertEqual(loaded.VM_UUID, a0.VM_UUID)
        self.assertEqual(loaded.BASE, a0.BASE)
        for variant in ('sha', 'mode', 'symlink', 'hardlink', 'directory'):
            path = self.base / variant
            if variant == 'symlink':
                path.symlink_to(target)
            elif variant == 'hardlink':
                os.link(target, path)
            elif variant == 'directory':
                path.mkdir(mode=0o700)
            else:
                path.write_bytes(b'raise AssertionError("must not execute")' if variant == 'sha' else original)
                path.chmod(0o666 if variant == 'mode' else 0o600)
            with self.subTest(variant=variant), patch.object(probe, 'A0_PATH', path), self.assertRaises(ValueError):
                probe.load_helpers()


if __name__ == '__main__':
    unittest.main()
