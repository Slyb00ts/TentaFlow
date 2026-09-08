# =============================================================================
# Plik: test_guest_writer_gate_probe.py
# Opis: Niezależne testy granic dowodu readonly bez VM, mount i narzędzi storage.
# Przykład: python3 -m unittest discover -s tests/infra/tentanas-vm -p test_guest_writer_gate_probe.py
# =============================================================================

import base64
import copy
from contextlib import ExitStack
import errno
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import Mock, patch
import guest_writer_gate_probe as probe
import io
import sys
from types import SimpleNamespace


def observation():
    value = {'uuid': probe.VM_UUID, 'boot_id': probe.BOOT_ID, 'swaps': [],
             'mounts': [{'target': '/', 'maj:min': '252:1'}], 'disks': []}
    for index, (role, size) in enumerate(probe.SIZES.items()):
        value['disks'].append({'serial': probe.PREFIX + role, 'type': 'disk',
                              'size': size * 1024**3, 'ro': False, 'maj:min': f'252:{index * 16}',
                              'holders': [], 'signatures': [], 'fstype': None, 'uuid': None,
                              'children': [{'maj:min': '252:1'}] if role == 'os' else []})
    return value


def actor_identity(namespace=False):
    before = {'uid': probe.USER_UID, 'euid': probe.USER_UID, 'caps': '0000000000000000',
              'uid_map': '0 0 4294967295', 'gid_map': '0 0 4294967295',
              'user_ns': 11, 'mnt_ns': 12}
    after = copy.deepcopy(before)
    if namespace:
        after.update(uid=0, euid=0, uid_map=f'0 {probe.USER_UID} 1',
                     gid_map=f'0 {probe.USER_UID} 1', user_ns=21, mnt_ns=22)
    return {'before': before, 'after': after, 'namespace_errno': None,
            'setup': {'stage': 'ready', 'dumpable_before_unshare': 1, 'core_limit': [0, 0]}}


class ObservationTests(unittest.TestCase):
    def test_exact_vm_and_six_unused_devices(self):
        self.assertEqual(probe.validate_observation(observation()), '252:1')

    def test_foreign_identity_and_used_devices_refused(self):
        cases = []
        for field, value in [('uuid', '49efc20d-4b22-44e5-ac25-2ad5063b19eb'),
                             ('uuid', '04bb9cc8-63b2-4562-b6ef-46a5c0663733'),
                             ('boot_id', '66933281-2e3d-498a-9bfc-38af18a6128c')]:
            item = observation()
            item[field] = value
            cases.append((field, item))
        for field, value in [('serial', 'foreign'), ('type', 'part'), ('size', True),
                             ('ro', True), ('maj:min', '252:0'), ('holders', ['dm-0']),
                             ('children', [{'maj:min': '252:17'}]), ('signatures', ['xfs']),
                             ('fstype', 'xfs'), ('uuid', probe.BOOT_ID)]:
            item = observation()
            item['disks'][1][field] = value
            cases.append((field, item))
        item = observation()
        item['swaps'] = ['252:16']
        cases.append(('swap', item))
        item = observation()
        item['mounts'].append({'target': '/foreign', 'maj:min': '252:16'})
        cases.append(('mounted', item))
        item = observation()
        item['mounts'][0]['maj:min'] = '252:16'
        cases.append(('foreign_root', item))
        for label, item in cases:
            with self.subTest(label=label), self.assertRaises(ValueError):
                probe.validate_observation(item)

    def test_mount_parser_keeps_per_mount_and_superblock_flags_separate(self):
        rows = probe.parse_mounts('41 1 0:77 / /union ro,nosuid shared:2 - fuse.mergerfs cache:data rw,user_id=0\n')
        self.assertEqual((rows[0]['mount_options'], rows[0]['super_options']),
                         (['ro', 'nosuid'], ['rw', 'user_id=0']))

    def test_actor_uid_capability_and_namespace_validation(self):
        for namespace in [False, True]:
            probe.validate_actor(actor_identity(namespace), namespace, probe.USER_UID)
        for field, value in [('uid', 0), ('euid', 0), ('caps', '0000000000000001')]:
            item = actor_identity()
            item['before'][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                probe.validate_actor(item, False, probe.USER_UID)
        for field, value in [('uid_map', '0 0 1'), ('gid_map', '0 0 1'),
                             ('user_ns', 11), ('mnt_ns', 12)]:
            item = actor_identity(True)
            item['after'][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                probe.validate_actor(item, True, probe.USER_UID)
        for field, value in [('dumpable_before_unshare', 0), ('core_limit', [-1, -1])]:
            item = actor_identity(True)
            item['setup'][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                probe.validate_actor(item, True, probe.USER_UID)
        with self.assertRaises(ValueError):
            probe.validate_actor(actor_identity(), False, 0)

    def test_global_readonly_requires_superblock_statvfs_and_all_four_erofs(self):
        expected = []
        for namespace, path, mount_id in [(11, '/union', '41'), (11, '/alias', '42'), (21, '/union', '43')]:
            expected.append({'path': path, 'mnt_ns': namespace,
                             'mounts': [{'id': mount_id, 'number': '0:77', 'root': '/', 'target': path,
                                         'filesystem': 'fuse.mergerfs', 'source': 'cache:data',
                                         'super_options': ['rw'], 'mount_options': ['rw']}],
                             'stat': {'errno': None, 'value': {'readonly': False, 'device': 77}}})
        snapshots = copy.deepcopy(expected)
        for row in snapshots:
            row['mounts'][0]['super_options'] = ['ro']
            row['stat']['value']['readonly'] = True
        opens = [{'path': row['path'], 'mnt_ns': row['mnt_ns'],
                  'outcomes': {name: {'errno': errno.EROFS, 'value': None}
                               for name in ['wronly', 'rdwr', 'create', 'truncate']}} for row in expected]
        self.assertTrue(probe.evaluate_global({'errno': None}, snapshots, opens, expected))
        for code in [errno.EACCES, errno.ENODEV, None]:
            changed = copy.deepcopy(opens)
            changed[0]['outcomes']['rdwr']['errno'] = code
            self.assertFalse(probe.evaluate_global({'errno': None}, snapshots, changed, expected))
        changed = copy.deepcopy(snapshots)
        changed[0]['mounts'][0]['super_options'] = ['rw']
        changed[0]['mounts'][0]['mount_options'] = ['ro']
        self.assertFalse(probe.evaluate_global({'errno': None}, changed, opens, expected))
        changed = copy.deepcopy(snapshots)
        changed[0]['stat']['value']['readonly'] = False
        self.assertFalse(probe.evaluate_global({'errno': None}, changed, opens, expected))
        self.assertFalse(probe.evaluate_global({'errno': errno.EBUSY}, snapshots, opens, expected))
        self.assertFalse(probe.evaluate_global({'errno': None}, [], opens, expected))
        self.assertFalse(probe.evaluate_global({'errno': None}, snapshots, [], expected))
        for field, value in [('path', '/foreign'), ('mnt_ns', 999)]:
            for side in ['snapshots', 'opens']:
                changed = copy.deepcopy(snapshots if side == 'snapshots' else opens)
                changed[0][field] = value
                with self.subTest(field=field, side=side):
                    self.assertFalse(probe.evaluate_global({'errno': None},
                        changed if side == 'snapshots' else snapshots,
                        changed if side == 'opens' else opens, expected))
        for field, value in [('id', 'foreign'), ('number', '0:99'), ('root', '/subdir'),
                             ('target', '/foreign'), ('filesystem', 'tmpfs'), ('source', 'foreign')]:
            changed = copy.deepcopy(snapshots)
            changed[0]['mounts'][0][field] = value
            with self.subTest(field=field):
                self.assertFalse(probe.evaluate_global({'errno': None}, changed, opens, expected))
        changed = copy.deepcopy(snapshots)
        changed[0]['stat']['value']['device'] = 99
        self.assertFalse(probe.evaluate_global({'errno': None}, changed, opens, expected))
        for changed in [snapshots[:1], snapshots[:2], snapshots + snapshots[:1], [snapshots[0]] * 3]:
            self.assertFalse(probe.evaluate_global({'errno': None}, changed, opens, expected))
        for changed in [opens[:1], opens[:2], opens + opens[:1], [opens[0]] * 3]:
            self.assertFalse(probe.evaluate_global({'errno': None}, snapshots, changed, expected))


class PrivateFilesTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.base = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.base.chmod(0o700)
        actual_lstat, actual_fstat = Path.lstat, os.fstat

        def owner(info, mode=None):
            values = list(info)
            values[4] = 0
            if mode is not None:
                values[0] = mode
            return os.stat_result(values)

        def lstat(path):
            info = actual_lstat(path)
            # Podmieniamy wyłącznie właściciela VM i publicznych rodziców katalogu testu.
            mode = stat.S_IFDIR | 0o755 if path in self.base.parents else None
            return owner(info, mode)

        self.stack.enter_context(patch.object(probe, 'BASE', self.base))
        self.stack.enter_context(patch.object(Path, 'lstat', lstat))
        self.stack.enter_context(patch.object(probe.os, 'fstat', side_effect=lambda fd: owner(actual_fstat(fd))))

    def runner(self):
        state = {'phase': 'global', 'stage': 'local', 'pending': None, 'events': [],
                 'proof_bytes': 0, 'mounts': []}
        probe.persist(state)
        self.stack.enter_context(patch.object(probe, 'own_mounts', return_value=[]))
        return probe.Runner(state, None)

    def test_raw_actor_response_survives_payload_failure(self):
        runner = self.runner()
        response = {'error': None, 'raw': base64.b64encode(b'{"operation":"held","errno":30}\n').decode()}
        actor = SimpleNamespace(pid=123, send=Mock(), receive=lambda: response)
        with patch.object(probe, 'payload', side_effect=OSError(errno.EIO, 'payload read')), self.assertRaises(OSError):
            runner.request(actor, 'held')
        records = [json.loads(probe.read_private(path)) for path in self.base.glob('global-*.json')]
        self.assertTrue(any(record.get('response') == response for record in records), records)
        self.assertEqual(json.loads(probe.read_private(self.base / 'state.json'))['pending'], 'held')

    def test_fatal_handshake_is_durable_and_reports_original_permission_error(self):
        runner = self.runner()

        def denied(channel, roots, backing, namespace):
            raise PermissionError(errno.EACCES, 'Permission denied', '/proc/self/setgroups')

        caught = None
        with patch.object(probe, 'actor_loop', denied):
            try:
                runner.actor('global', True, [self.base])
            except BaseException as error:
                caught = error
            finally:
                runner.close()
        records = [json.loads(probe.read_private(path)) for path in self.base.glob('global-*.json')]
        responses = [json.loads(base64.b64decode(row['response']['raw'])) for row in records if 'response' in row]
        self.assertTrue(any(row.get('fatal') == 'PermissionError' and '/proc/self/setgroups' in row['detail']
                            for row in responses))
        self.assertEqual(json.loads(probe.read_private(self.base / 'state.json'))['pending'], 'actor-start')
        self.assertIsInstance(caught, ValueError)
        self.assertIn('PermissionError', str(caught))

    def test_pending_precedes_send_and_malformed_response_is_durable(self):
        runner = self.runner()
        observed = []

        def send(operation):
            observed.append(json.loads(probe.read_private(self.base / 'state.json'))['pending'])

        response = {'error': None, 'raw': base64.b64encode(b'not-json\n').decode()}
        actor = SimpleNamespace(pid=123, send=send, receive=lambda: response)
        with patch.object(probe, 'payload', return_value={}), self.assertRaises(json.JSONDecodeError):
            runner.request(actor, 'held')
        self.assertEqual(observed, ['held'])
        records = [json.loads(probe.read_private(path)) for path in self.base.glob('global-*.json')]
        self.assertTrue(any(record.get('response') == response for record in records))

    def test_eof_fatal_and_foreign_operation_remain_raw_refusals(self):
        runner = self.runner()
        cases = [({'operation': 'held'}, 'eof'),
                 ({'fatal': 'PermissionError', 'operation': 'held'}, None),
                 ({'operation': 'foreign'}, None)]
        for value, error in cases:
            response = {'raw': base64.b64encode(json.dumps(value).encode()).decode(), 'error': error}
            actor = SimpleNamespace(pid=123, send=Mock(), receive=lambda: response)
            with self.subTest(value=value), patch.object(probe, 'payload', return_value={}), self.assertRaises(ValueError):
                runner.request(actor, 'held')
            records = [json.loads(probe.read_private(path)) for path in self.base.glob('global-*.json')]
            self.assertTrue(any(record.get('response') == response for record in records))
            self.assertEqual(json.loads(probe.read_private(self.base / 'state.json'))['pending'], 'held')

    def test_pending_fsync_failure_prevents_actor_send(self):
        runner = self.runner()
        actor = SimpleNamespace(pid=123, send=Mock(), receive=Mock())
        with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'persist')), self.assertRaises(OSError):
            runner.request(actor, 'held')
        actor.send.assert_not_called()
        actor.receive.assert_not_called()
        self.assertTrue((self.base / 'state.next').exists())

    def test_real_command_failure_preserves_errno_stdout_stderr_and_pending(self):
        runner = self.runner()
        fd = probe.acquire_lock(create=True)
        runner.lock = fd
        try:
            with self.assertRaises(ValueError):
                runner.command([sys.executable, '-c',
                    'import json,sys; print(json.dumps({"errno":30})); print("denied",file=sys.stderr); sys.exit(7)'], 'controlled')
        finally:
            os.close(fd)
        record = json.loads(probe.read_private(self.base / runner.state['events'][0]))
        self.assertEqual((record['rc'], json.loads(base64.b64decode(record['stdout']))['errno']), (7, 30))
        self.assertEqual(base64.b64decode(record['stderr']), b'denied\n')
        self.assertEqual(json.loads(probe.read_private(self.base / 'state.json'))['pending'], 'controlled')

    def test_main_refuses_durable_pending_before_measurement_or_guard(self):
        probe.persist({'stage': 'local', 'pending': 'held', 'phase': 'global'})
        lock = probe.acquire_lock(create=True)
        os.close(lock)
        output = io.StringIO()
        with patch.object(probe.os, 'geteuid', return_value=0), \
                patch.object(probe, 'load_storage', return_value=object()), \
                patch.object(probe, 'guard') as guard, patch.object(probe, 'run_measurement') as operation, \
                patch.object(sys, 'stdout', output):
            self.assertEqual(probe.main(['global']), 1)
        guard.assert_not_called()
        operation.assert_not_called()
        self.assertEqual(json.loads(output.getvalue())['status'], 'refused')
        self.assertEqual(json.loads(probe.read_private(self.base / 'state.json'))['pending'], 'held')

    def measurement(self, kind, failure=None):
        runner = self.runner()
        runner.state['fuse_devices'] = {kind: 77}
        tree = probe.BASE / 'fixtures'
        self.stack.enter_context(patch.object(probe, 'TREE', tree))
        self.stack.enter_context(patch.object(probe, 'USER_UID', 0))
        actual_stat = Path.stat
        device = self.base.stat().st_dev

        def root_stat(path, *args, **kwargs):
            info = actual_stat(path, *args, **kwargs)
            if path == Path('/'):
                fields = list(info)
                fields[2] = device
                return os.stat_result(fields)
            return info

        self.stack.enter_context(patch.object(Path, 'stat', root_stat))
        for variant in ['local', 'global']:
            for branch in ['cache', 'data']:
                (tree / variant / branch).mkdir(parents=True, mode=0o700)
            for name in ['held.bin', 'mapping.bin', 'truncate.bin', 'direct.bin']:
                path = tree / variant / 'cache' / name
                path.write_bytes(b'B' * 4096)
                path.chmod(0o600)
        runner.state['expected'] = {path: {'identity': metric, 'content': base64.b64encode(Path(path).read_bytes()).decode()}
                                    for path, metric in probe.payload().items()}
        calls = []

        def actor_factory(kind, namespace, roots):
            actor = SimpleNamespace(pid=123, roots=roots, mnt_ns=21 if namespace else 11,
                                    backing=probe.backing(kind), mapping_count=0)
            actor.send = lambda operation: setattr(actor, 'operation', operation)

            def receive():
                operation = actor.operation
                readonly = kind == 'global' and operation == 'reopen'
                mounts = [{'path': str(path), 'mnt_ns': actor.mnt_ns,
                           'mounts': [{'id': str(index + actor.mnt_ns), 'number': '0:77',
                                       'root': '/', 'target': str(path), 'filesystem': 'fuse.mergerfs',
                                       'source': 'cache:data', 'mount_options': ['rw'],
                                       'super_options': ['ro' if readonly else 'rw']}],
                           'stat': {'errno': None, 'value': {'device': 77, 'readonly': readonly}}}
                          for index, path in enumerate(roots)]
                value = {'operation': operation, 'snapshot': mounts}
                if operation in ('baseline', 'held'):
                    value['writes'] = [{'errno': errno.EIO if failure == 'held_eio' and operation == 'held' else None,
                                        'value': 9 if operation == 'baseline' else 5} for _ in roots]
                    if failure == 'held_empty' and operation == 'held':
                        value['writes'] = []
                    if operation == 'baseline':
                        value['creates'] = [{'errno': errno.EROFS, 'value': None} for _ in roots]
                        value['mmap'] = [{'errno': errno.ENODEV, 'value': None} for _ in roots]
                elif operation == 'close_fd':
                    value.update(open_fds=0, actual_fds=[], mappings=0)
                elif operation == 'unmap':
                    value['mappings'] = 0
                elif operation == 'reopen':
                    value['opens'] = [{'path': str(path), 'mnt_ns': actor.mnt_ns,
                                      'outcomes': {name: {'errno': errno.EROFS if readonly or name == 'create' else None,
                                                          'value': None if readonly or name == 'create' else 7}
                                                   for name in ['wronly', 'rdwr', 'create', 'truncate']}}
                                     for path in roots]
                    if failure == 'alias_eio':
                        value['opens'][0]['outcomes']['rdwr']['errno'] = errno.EIO
                elif operation == 'direct':
                    value['write'] = {'errno': None, 'value': 15}
                else:
                    raise AssertionError(operation)
                if operation in ('baseline', 'held'):
                    for write in value['writes']:
                        if write['errno'] is None:
                            with (actor.backing / 'held.bin').open('ab') as stream:
                                stream.write(b'BASELINE\n' if operation == 'baseline' else b'HELD\n')
                elif operation == 'reopen':
                    for row in value['opens']:
                        for name, outcome in row['outcomes'].items():
                            if outcome['errno'] is None:
                                path = actor.backing / ('truncate.bin' if name == 'truncate' else 'held.bin')
                                with path.open('wb' if name == 'truncate' else 'ab') as stream:
                                    stream.write(b'REOPEN\n')
                elif operation == 'direct':
                    with (actor.backing / 'direct.bin').open('ab') as stream:
                        stream.write(b'DIRECT_BACKING\n')
                if operation == 'held' and failure in ['backing_missing', 'backing_extra', 'backing_inode',
                                                       'backing_sha', 'backing_mode', 'data_object']:
                    path = actor.backing / 'held.bin'
                    if failure == 'backing_missing':
                        path.rename(tree / 'preserved-held.bin')
                    elif failure == 'backing_extra':
                        (actor.backing / 'foreign').write_bytes(b'foreign')
                    elif failure == 'backing_inode':
                        original = path.read_bytes()
                        path.rename(tree / 'preserved-held.bin')
                        path.write_bytes(original)
                        path.chmod(0o600)
                    elif failure == 'backing_sha':
                        with path.open('r+b') as stream:
                            stream.write(b'WRONG')
                    elif failure == 'backing_mode':
                        path.chmod(0o644)
                    else:
                        (tree / kind / 'data' / 'foreign').write_bytes(b'foreign')
                return {'error': None, 'raw': base64.b64encode(json.dumps(value).encode()).decode()}

            actor.receive = receive
            return actor

        def mount(kind, action):
            runner.begin(action)
            calls.append(action)
            code = errno.EBUSY if kind == 'global' and len(calls) == 1 else None
            if failure == action:
                code = errno.EINVAL
            result = {'errno': code, 'return': -1 if code else 0}
            runner.record(action, result)
            return result

        with patch.object(runner, 'actor', side_effect=actor_factory), \
                patch.object(runner, 'mount', side_effect=mount):
            return probe.run_measurement(runner, kind)

    def test_controlled_global_positive_uses_real_runner_without_claiming_mmap(self):
        result = self.measurement('global')
        self.assertTrue(result['gate_supported'])
        self.assertFalse(result['mmap_tested'])
        self.assertTrue(list(self.base.glob('global-*.json')))

    def test_global_busy_does_not_hide_failed_or_missing_held_write(self):
        for failure in ['held_eio', 'held_empty']:
            child = self.base / failure
            child.mkdir(mode=0o700)
            with self.subTest(failure=failure), patch.object(probe, 'BASE', child), self.assertRaises(ValueError):
                self.measurement('global', failure)
            raw = [json.loads(probe.read_private(path)) for path in child.glob('global-*.json')]
            self.assertTrue(any('response' in item for item in raw))

    def test_local_failures_do_not_become_measured(self):
        for failure in ['local_ro', 'unmount', 'alias_eio']:
            child = self.base / failure
            child.mkdir(mode=0o700)
            with self.subTest(failure=failure), patch.object(probe, 'BASE', child), self.assertRaises(ValueError):
                self.measurement('local', failure)
            state = json.loads(probe.read_private(child / 'state.json'))
            self.assertIsNotNone(state['pending'])

    def test_real_backing_set_identity_and_marker_failures_keep_raw_before_refusal(self):
        for failure in ['backing_missing', 'backing_extra', 'backing_inode',
                        'backing_sha', 'backing_mode', 'data_object']:
            child = self.base / failure
            child.mkdir(mode=0o700)
            with self.subTest(failure=failure), patch.object(probe, 'BASE', child), self.assertRaises(ValueError):
                self.measurement('global', failure)
            raw = [json.loads(probe.read_private(path)) for path in child.glob('global-*.json')]
            held = [item for item in raw if 'response' in item
                    and json.loads(base64.b64decode(item['response']['raw'])).get('operation') == 'held']
            self.assertTrue(held)
            self.assertEqual(json.loads(probe.read_private(child / 'state.json'))['pending'], 'held')

    def test_durable_new_roundtrip_and_exclusive_no_overwrite(self):
        path = self.base / 'evidence.json'
        probe.durable_new(path, {'errno': errno.EROFS, 'raw': 'retained'})
        self.assertEqual(json.loads(probe.read_private(path)), {'errno': 30, 'raw': 'retained'})
        with self.assertRaises(FileExistsError):
            probe.durable_new(path, {'errno': 0})
        self.assertEqual(json.loads(probe.read_private(path))['errno'], 30)

    def test_private_file_mode_hardlink_symlink_type_and_size_refused(self):
        path = self.base / 'evidence.json'
        probe.durable_new(path, {'raw': 'retained'})
        path.chmod(0o644)
        with self.assertRaises(ValueError):
            probe.read_private(path)
        path.chmod(0o600)
        os.link(path, self.base / 'hardlink')
        with self.assertRaises(ValueError):
            probe.read_private(path)
        symlink = self.base / 'symlink'
        symlink.symlink_to(path)
        with self.assertRaises(OSError):
            probe.read_private(symlink)
        with self.assertRaises((OSError, ValueError)):
            probe.read_private(self.base)
        other = self.base / 'large'
        probe.durable_new(other, {'long': 'x' * 20})
        with self.assertRaisesRegex(ValueError, 'Limit'):
            probe.read_private(other, limit=4)

    def test_writable_or_symlink_parent_refuses_before_create(self):
        parent = self.base / 'unsafe'
        parent.mkdir(mode=0o700)
        parent.chmod(0o770)
        with self.assertRaises(ValueError):
            probe.durable_new(parent / 'raw', {})
        self.assertFalse((parent / 'raw').exists())
        alias = self.base / 'alias'
        alias.symlink_to(parent, target_is_directory=True)
        with self.assertRaises(ValueError):
            probe.durable_new(alias / 'raw', {})

    def test_pending_is_durable_and_failed_fsync_never_replaces_old_state(self):
        state = {'stage': 'prepared', 'pending': None}
        probe.persist(state)
        probe.pending(state, 'global')
        self.assertEqual(json.loads(probe.read_private(self.base / 'state.json'))['pending'], 'global')
        with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'test')), self.assertRaises(OSError):
            probe.persist({'stage': 'measured', 'pending': None})
        self.assertEqual(json.loads(probe.read_private(self.base / 'state.json'))['pending'], 'global')
        self.assertTrue((self.base / 'state.next').exists())
        with self.assertRaises(FileExistsError):
            probe.persist({'stage': 'measured'})

    def test_zero_write_refuses_with_evidence_file_retained(self):
        path = self.base / 'partial'
        with patch.object(probe.os, 'write', return_value=0), self.assertRaises(ValueError):
            probe.durable_new(path, {'raw': 'never persisted'})
        self.assertEqual(path.read_bytes(), b'')

    def test_short_writes_are_completed_and_foreign_fixture_never_executes(self):
        actual_write = os.write
        with patch.object(probe.os, 'write', side_effect=lambda fd, data: actual_write(fd, data[:3])):
            probe.durable_new(self.base / 'short', {'raw': 'complete'})
        self.assertEqual(json.loads(probe.read_private(self.base / 'short')), {'raw': 'complete'})
        fixture = self.base / 'fixture.py'
        marker = self.base / 'executed'
        fixture.write_text(f'from pathlib import Path\nPath({str(marker)!r}).touch()\n')
        fixture.chmod(0o600)
        with patch.object(probe, 'FIXTURE', fixture), self.assertRaisesRegex(ValueError, 'SHA fixture'):
            probe.load_storage()
        self.assertFalse(marker.exists())

    def test_lock_busy_and_invalid_existing_lock_refused(self):
        fd = probe.acquire_lock(create=True)
        try:
            with self.assertRaises(BlockingIOError):
                probe.acquire_lock()
            with self.assertRaises(FileExistsError):
                probe.acquire_lock(create=True)
        finally:
            os.close(fd)
        fd = probe.acquire_lock()
        os.close(fd)
        (self.base / '.lock').chmod(0o644)
        with self.assertRaises(ValueError):
            probe.acquire_lock()

    def test_foreign_owner_symlink_and_hardlinked_lock_refused(self):
        fd = probe.acquire_lock(create=True)
        os.close(fd)
        actual_fstat = os.fstat

        def foreign(fd):
            value = list(actual_fstat(fd))
            value[4] = 1234
            return os.stat_result(value)

        with patch.object(probe.os, 'fstat', side_effect=foreign), self.assertRaises(ValueError):
            probe.acquire_lock()
        os.link(self.base / '.lock', self.base / 'alias')
        with self.assertRaises(ValueError):
            probe.acquire_lock()
        other = self.base / 'other'
        other.mkdir(mode=0o700)
        (other / '.lock').symlink_to(self.base / '.lock')
        with patch.object(probe, 'BASE', other), self.assertRaises(OSError):
            probe.acquire_lock()

    def test_real_actor_inherits_lock_until_child_exit(self):
        fd = probe.acquire_lock(create=True)
        with ExitStack() as stack:
            stack.enter_context(patch.object(probe, 'USER_UID', os.getuid()))
            if os.geteuid() != 0:
                stack.enter_context(patch.object(probe.os, 'setgroups'))
            actor = probe.Actor([], self.base, False, fd)
            try:
                self.assertIsNone(actor.receive()['error'])
                os.close(fd)
                fd = None
                with self.assertRaises(BlockingIOError):
                    probe.acquire_lock()
            finally:
                actor.stop()
                if fd is not None:
                    os.close(fd)
        released = probe.acquire_lock()
        os.close(released)


class ActorTests(unittest.TestCase):
    def test_real_userns_dumpability_zero_refuses_and_production_setup_one_succeeds(self):
        self.assertEqual(os.getuid(), probe.USER_UID, 'Test wymaga rzeczywistego nieuprzywilejowanego UID 1000')
        self.assertEqual(int(probe.identity()['caps'], 16), 0)
        original_loop = probe.actor_loop

        def no_dumpable(channel, roots, backing, namespace):
            libc = probe.ctypes.CDLL(None, use_errno=True)
            probe.resource.setrlimit(probe.resource.RLIMIT_CORE, (0, 0))
            changed = libc.prctl(4, 0, 0, 0, 0)
            unshared = libc.unshare(probe.ctypes.c_int(0x10000000 | 0x00020000))
            unshare_errno = probe.ctypes.get_errno() if unshared else None
            denied = probe.attempt(lambda: Path('/proc/self/setgroups').write_text('deny')) if unshared == 0 else None
            channel.sendall(json.dumps({'prctl': changed, 'unshare': unshared,
                                        'unshare_errno': unshare_errno, 'setgroups': denied}).encode() + b'\n')

        with patch.object(probe, 'actor_loop', no_dumpable):
            actor = probe.Actor([], Path('/unused'), True, None)
            try:
                response = actor.receive()
                self.assertIsNone(response['error'])
                value = json.loads(base64.b64decode(response['raw']))
                self.assertEqual((value['prctl'], value['unshare']), (0, 0), value)
                self.assertEqual(value['setgroups']['errno'], errno.EACCES)
            finally:
                actor.stop()

        def production_from_zero(channel, roots, backing, namespace):
            libc = probe.ctypes.CDLL(None, use_errno=True)
            if libc.prctl(4, 0, 0, 0, 0) != 0:
                raise OSError(probe.ctypes.get_errno(), 'PR_SET_DUMPABLE test')
            original_loop(channel, roots, backing, namespace)

        with patch.object(probe, 'actor_loop', production_from_zero), patch.object(probe.os, 'setgroups'):
            actor = probe.Actor([], Path('/unused'), True, None)
            try:
                response = actor.receive()
                self.assertIsNone(response['error'])
                value = json.loads(base64.b64decode(response['raw']))
                probe.validate_actor(value, True, os.getuid())
                self.assertEqual((value['setup']['dumpable_after_drop'],
                                  value['setup']['dumpable_before_unshare'], value['setup']['stage']), (0, 1, 'ready'))
                self.assertEqual(value['setup']['core_limit'], [0, 0])
                self.assertEqual(set(value['setup']['proc_controls_before_mapping']), {'setgroups', 'uid_map', 'gid_map'})
                self.assertEqual(value['after']['uid'], 0)
            finally:
                actor.stop()

    def test_real_child_socket_preserves_malformed_raw_and_nonzero_eof(self):
        def malformed(channel, roots, backing, namespace):
            channel.sendall(b'not-json\n')
            os._exit(7)

        with patch.object(probe, 'actor_loop', malformed):
            actor = probe.Actor([], Path('/unused'), False, None)
            try:
                raw = actor.receive()
                self.assertEqual(base64.b64decode(raw['raw']), b'not-json\n')
                with self.assertRaises(json.JSONDecodeError):
                    json.loads(base64.b64decode(raw['raw']))
                pidfd = os.pidfd_open(actor.pid)
                try:
                    self.assertTrue(probe.select.select([pidfd], [], [], 3)[0])
                finally:
                    os.close(pidfd)
                ended = actor.receive()
                self.assertEqual((ended['error'], ended['exit_code']), ('eof', 7))
            finally:
                actor.stop()

    def test_real_actor_socket_held_fd_mapping_and_direct_backing_write(self):
        with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
            base = Path(directory)
            for name in ['held.bin', 'mapping.bin', 'truncate.bin', 'direct.bin']:
                (base / name).write_bytes(b'x' * 4096)
            stack.enter_context(patch.object(probe, 'USER_UID', os.getuid()))
            if os.geteuid() != 0:
                stack.enter_context(patch.object(probe.os, 'setgroups'))
            actor = probe.Actor([base], base, False, None)
            try:
                handshake = actor.receive()
                self.assertIsNone(handshake['error'])
                value = json.loads(base64.b64decode(handshake['raw']))
                probe.validate_actor(value, False, os.getuid())
                for operation in ['baseline', 'held', 'close_fd', 'map', 'unmap', 'reopen', 'direct']:
                    actor.send(operation)
                    raw = actor.receive()
                    self.assertIsNone(raw['error'], raw)
                    value = json.loads(base64.b64decode(raw['raw']))
                    self.assertEqual(value['operation'], operation)
                    if operation == 'baseline':
                        self.assertEqual(value['writes'][0], {'errno': None, 'value': 9})
                    if operation == 'close_fd':
                        self.assertEqual((value['open_fds'], value['mappings']), (0, 1))
                        self.assertEqual(value['actual_fds'], [])
                    if operation == 'map':
                        self.assertEqual(value['writes'], [{'errno': None, 'value': 4}])
                self.assertTrue((base / 'direct.bin').read_bytes().endswith(b'DIRECT_BACKING\n'))
            finally:
                actor.stop()

    def test_attempt_preserves_mmap_unavailable_errno(self):
        def unavailable():
            raise OSError(errno.ENODEV, 'mmap unavailable')
        self.assertEqual(probe.attempt(unavailable), {'errno': errno.ENODEV, 'value': None})

    def test_actor_reports_mmap_enodev_without_claiming_a_live_mapping(self):
        with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
            base = Path(directory)
            for name in ['held.bin', 'mapping.bin']:
                (base / name).write_bytes(b'x' * 4096)
            stack.enter_context(patch.object(probe, 'USER_UID', os.getuid()))
            if os.geteuid() != 0:
                stack.enter_context(patch.object(probe.os, 'setgroups'))
            stack.enter_context(patch.object(probe.mmap, 'mmap', side_effect=OSError(errno.ENODEV, 'unavailable')))
            actor = probe.Actor([base], base, False, None)
            try:
                self.assertIsNone(actor.receive()['error'])
                actor.send('baseline')
                value = json.loads(base64.b64decode(actor.receive()['raw']))
                self.assertEqual(value['mmap'], [{'errno': errno.ENODEV, 'value': None}])
                actor.send('close_fd')
                value = json.loads(base64.b64decode(actor.receive()['raw']))
                self.assertEqual((value['mappings'], value['actual_fds']), (0, []))
                actor.send('map')
                value = json.loads(base64.b64decode(actor.receive()['raw']))
                self.assertEqual(value['writes'], [])
            finally:
                actor.stop()


if __name__ == '__main__':
    unittest.main()
