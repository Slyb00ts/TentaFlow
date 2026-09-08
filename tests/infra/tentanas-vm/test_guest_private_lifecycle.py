# =============================================================================
# Plik: test_guest_private_lifecycle.py
# Opis: Rzeczywiste testy odmów, trwałych dowodów i protokołu odbioru prywatnej macierzy.
# Przykład: python3 -m unittest discover -s tests/infra/tentanas-vm -p test_guest_private_lifecycle.py
# =============================================================================

import base64
import copy
from contextlib import ExitStack
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid

import guest_private_lifecycle as probe
import guest_writer_gate_probe as a0
import guest_storage as storage
import guest_branch_isolation_probe as a1

# Bajty odpowiedzi lsblk z pierwszego odczytowego preflight stanowiska L.
LIVE_LSBLK = r'''{
   "blockdevices": [
      {
         "name": "sr0",
         "size": 378880,
         "type": "rom",
         "serial": "seed",
         "fstype": "iso9660",
         "uuid": "2026-09-08-14-40-20-00",
         "mountpoints": [],
         "maj:min": "11:0",
         "ro": true
      },{
         "name": "vda",
         "size": 12884901888,
         "type": "disk",
         "serial": "tn-c4a36ec37a-os",
         "fstype": null,
         "uuid": null,
         "mountpoints": [],
         "maj:min": "254:0",
         "ro": false,
         "children": [
            {
               "name": "vda1",
               "size": 12750667264,
               "type": "part",
               "serial": null,
               "fstype": "ext4",
               "uuid": "df4df5bb-613a-4382-8cf9-653304605a1f",
               "mountpoints": [
                   "/"
               ],
               "maj:min": "254:1",
               "ro": false
            },{
               "name": "vda14",
               "size": 3145728,
               "type": "part",
               "serial": null,
               "fstype": null,
               "uuid": null,
               "mountpoints": [],
               "maj:min": "254:14",
               "ro": false
            },{
               "name": "vda15",
               "size": 130023424,
               "type": "part",
               "serial": null,
               "fstype": "vfat",
               "uuid": "6EA5-B51D",
               "mountpoints": [
                   "/boot/efi"
               ],
               "maj:min": "254:15",
               "ro": false
            }
         ]
      },{
         "name": "vdb",
         "size": 34359738368,
         "type": "disk",
         "serial": "tn-c4a36ec37a-data1",
         "fstype": null,
         "uuid": null,
         "mountpoints": [],
         "maj:min": "254:16",
         "ro": false
      },{
         "name": "vdc",
         "size": 34359738368,
         "type": "disk",
         "serial": "tn-c4a36ec37a-data2",
         "fstype": null,
         "uuid": null,
         "mountpoints": [],
         "maj:min": "254:32",
         "ro": false
      },{
         "name": "vdd",
         "size": 42949672960,
         "type": "disk",
         "serial": "tn-c4a36ec37a-parity",
         "fstype": null,
         "uuid": null,
         "mountpoints": [],
         "maj:min": "254:48",
         "ro": false
      },{
         "name": "vde",
         "size": 42949672960,
         "type": "disk",
         "serial": "tn-c4a36ec37a-spare",
         "fstype": null,
         "uuid": null,
         "mountpoints": [],
         "maj:min": "254:64",
         "ro": false
      },{
         "name": "nvme0n1",
         "size": 1073741824,
         "type": "disk",
         "serial": "tn-c4a36ec37a-cache",
         "fstype": null,
         "uuid": null,
         "mountpoints": [],
         "maj:min": "259:0",
         "ro": false
      }
   ]
}
'''


def station(name='L'):
    vm, data, parity = probe.STATIONS[name]
    prefix = 'tn-' + vm.replace('-', '')[:10] + '-'
    disks = {role: {'serial': prefix + role, 'bytes': size * 1024**3}
             for role, size in probe.SIZES.items()}
    identifiers = iter(str(uuid.UUID(int=i)) for i in range(101, 120))
    spec = {'array_id': next(identifiers), 'operation_id': next(identifiers),
            'owner': {'org_id': 'org-default', 'addon_id': 'tentanas-a21'},
            'name': 'a21-' + name.lower(), 'filesystem': 'xfs', 'data': [], 'parity': []}
    for kind, roles in [('data', data), ('parity', parity)]:
        spec[kind] = [dict(disks[role], disk_id='disk-' + role, wwn=None,
                           expected_uuid=next(identifiers)) for role in roles]
    return {'schema': 1, 'station': name, 'vm_uuid': vm,
            'initial_boot': str(uuid.UUID(int=500)), 'source_commit': 'a' * 40,
            'helper_sha256': 'b' * 64, 'tools': {path: 'c' * 64 for path in probe.TOOLS},
            'disks': disks, 'spec': spec,
            'maintenance_ids': {kind: next(identifiers) for kind in ('sync', 'scrub', 'nochange')}
            if parity else {}}


def state_result(spec):
    rows = []
    for kind, role in [('data', 'data'), ('parity', 'parity')]:
        for index, disk in enumerate(spec[kind], start=1):
            kernel = 'vd' + chr(97 + list(probe.SIZES).index(disk['serial'].rsplit('-', 1)[1]))
            rows.append({'role': {role: index}, 'kernel_name': kernel, 'device': '/dev/' + kernel,
                         'observed_uuid': disk['expected_uuid'], 'filesystem': 'xfs',
                         'device_present': True, 'mounted': True, 'size_bytes': disk['bytes'],
                         'used_bytes': 0, 'free_bytes': disk['bytes'], 'detail': None})
    return {'array_id': spec['array_id'], 'operation_id': spec['operation_id'], 'owner': spec['owner'],
            'stage': 'ready', 'disks': rows, 'union_mounted': True, 'sync_completed_at': None,
            'detail': None, 'last_run': None}


def observation(value, created=False):
    disks = []
    expected = {d['serial']: d['expected_uuid'] for kind in ('data', 'parity') for d in value['spec'][kind]}
    for index, (role, disk) in enumerate(value['disks'].items()):
        fs_uuid = expected.get(disk['serial']) if created else None
        disks.append({'serial': disk['serial'], 'size': disk['bytes'], 'type': 'disk', 'ro': False,
                      'name': 'nvme0n1' if role == 'cache' else 'vd' + chr(97 + index),
                      'maj:min': '259:0' if role == 'cache' else f'252:{index * 16}',
                      'holders': [], 'children': [{'maj:min': '252:1'}] if role == 'os' else [],
                      'mountpoints': [], 'fstype': 'xfs' if fs_uuid else None, 'uuid': fs_uuid,
                      'signatures': [{'type': 'xfs'}] if fs_uuid else []})
    disks.append(copy.deepcopy(json.loads(LIVE_LSBLK)['blockdevices'][0]))
    return {'uuid': value['vm_uuid'], 'boot_id': value['initial_boot'], 'disks': disks,
            'mounts': [{'target': '/', 'maj:min': '252:1'}], 'swaps': []}


def device_stat(value):
    rows = {('/dev/' + disk['name']): disk for disk in value['disks']}
    def read(path):
        disk = rows[str(path)]
        major, minor = map(int, disk['maj:min'].split(':'))
        return SimpleNamespace(st_mode=stat.S_IFBLK | 0o600, st_rdev=os.makedev(major, minor))
    return read


def journal(value):
    return {'schema': 2, 'spec': copy.deepcopy(value['spec']), 'stage': 'ready',
            'formatted': [{kind: index} for kind in ('data', 'parity')
                          for index in range(1, len(value['spec'][kind]) + 1)],
            'pending': None, 'boot_id': value['initial_boot'], 'sync_completed_at': None,
            'detail': None, 'last_run': None,
            'private': {'published': True, 'anchor': {
                'boot_id': value['initial_boot'], 'pid': 100, 'start_ticks': 200,
                'mount_ns_inode': 300, 'exe_device': 400, 'exe_inode': 500,
                'exe_sha256': value['tools']['/usr/bin/mergerfs'], 'union_device': 600,
                'union_source': 'd1:d2'}}}


class PrivateFixture:
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.directory = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.stack.enter_context(patch.object(probe, 'UID', os.geteuid()))
        self.stack.enter_context(patch.object(probe, 'private_parents'))
        self.seed_reader = self.stack.enter_context(patch.object(probe, 'read_seed'))

    def write(self, name, raw, mode=0o600):
        path = self.directory / name
        fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, mode)
        try:
            os.write(fd, raw)
            os.fsync(fd)
        finally:
            os.close(fd)
        return path


class PrivateFiles(PrivateFixture, unittest.TestCase):
    def test_event_limit_preserves_last_allowed_record_and_refuses_next(self):
        proof = probe.Proof(self.directory / 'bounded', 'a' * 64)
        proof.state['events'] = [f'{index:03d}-prior.json' for index in range(399)]
        proof.record('last', {'measured': True})
        before = (proof.path / 'state.json').read_bytes()
        with self.assertRaisesRegex(ValueError, 'Limit zdarzeń'):
            proof.record('overflow', {})
        self.assertEqual((proof.path / 'state.json').read_bytes(), before)
        self.assertEqual(json.loads((proof.path / '399-last.json').read_bytes()), {'measured': True})
        self.assertFalse((proof.path / '400-overflow.json').exists())

    def test_read_and_hash_actual_private_file(self):
        raw = b'first\rsecond\r\n'
        path = self.write('raw', raw)
        self.assertEqual(probe.read_private(path, hashlib.sha256(raw).hexdigest()), raw)
        with self.assertRaises(ValueError):
            probe.read_private(path, 'e' * 64)

    def test_symlink_hardlink_mode_and_oversize_are_refused(self):
        path = self.write('raw', b'payload')
        link = self.directory / 'symlink'
        link.symlink_to(path)
        with self.assertRaises(OSError):
            probe.read_private(link)
        hardlink = self.directory / 'hardlink'
        os.link(path, hardlink)
        with self.assertRaises(ValueError):
            probe.read_private(hardlink)
        public = self.write('public', b'x', 0o644)
        with self.assertRaises(ValueError):
            probe.read_private(public)
        large = self.write('large', b'x' * 65)
        with patch.object(probe, 'LIMIT', 64), self.assertRaises(ValueError):
            probe.read_private(large)

    def test_station_loader_checks_actual_bytes_before_decode(self):
        value = station()
        raw = json.dumps(value).encode()
        path = self.write('station.json', raw)
        self.assertEqual(probe.load_station(path, hashlib.sha256(raw).hexdigest()), value)
        duplicate = self.write('duplicate.json', b'{"schema":1,"schema":1}')
        with self.assertRaises(ValueError):
            probe.load_station(duplicate, hashlib.sha256(duplicate.read_bytes()).hexdigest())

    def test_durable_writer_handles_real_short_writes_and_refuses_overwrite(self):
        path = self.directory / 'proof.json'
        actual_write = os.write
        with patch.object(probe.os, 'write', side_effect=lambda fd, raw: actual_write(fd, raw[:3])):
            probe.durable_new(path, {'message': 'complete despite short writes'})
        before = path.read_bytes()
        with self.assertRaises(FileExistsError):
            probe.durable_new(path, {'message': 'replacement'})
        self.assertEqual(path.read_bytes(), before)

    def test_real_proof_reopen_keeps_pending_and_refuses_retry(self):
        proof = probe.Proof(self.directory / 'proof', 'a' * 64)
        proof.begin('create')
        proof.record('observation', {'errno': 13})
        reopened = probe.Proof(proof.path, 'a' * 64)
        with self.assertRaises(ValueError):
            reopened.begin('create')
        self.assertEqual(reopened.state['pending'], 'create')
        self.assertEqual(json.loads((proof.path / reopened.state['events'][0]).read_bytes()), {'errno': 13})

    def test_failed_completion_keeps_durable_pending(self):
        proof = probe.Proof(self.directory / 'proof', 'a' * 64)
        proof.begin('create')
        with patch.object(probe.os, 'replace', side_effect=OSError(errno.EIO, 'replace')), self.assertRaises(OSError):
            proof.complete('create')
        self.assertEqual(json.loads((proof.path / 'state.json').read_bytes())['pending'], 'create')
        self.assertEqual(proof.state['pending'], 'create')


class IsolationRows(unittest.TestCase):
    def test_exact_denials_require_positive_public_control_and_complete_routes(self):
        probes = [{'target': name, 'operation': 'setns' if name.endswith('_ns') else 'open'}
                  for name in ('host_path', 'mergerfs_root', 'mergerfs_fd', 'mergerfs_ns', 'public_fuse')]
        rows = [dict(row, errno=errno.EACCES, value=None) for row in probes]
        rows[-1].update(errno=None, value=0)
        probe.validate_isolation({'operation': 'probe', 'rows': rows}, probes, a1)
        negatives = []
        for index in range(4):
            changed = copy.deepcopy(rows)
            changed[index].update(errno=None, value=0)
            negatives.append(changed)
        changed = copy.deepcopy(rows)
        changed[-1].update(errno=errno.EACCES, value=None)
        foreign = copy.deepcopy(rows)
        foreign[1]['target'] = 'foreign'
        negatives.extend([changed, foreign, rows[:-1], rows + [rows[0]], [rows[0], rows[0], *rows[2:]]])
        for bad in negatives:
            with self.subTest(rows=bad), self.assertRaises(ValueError):
                probe.validate_isolation({'operation': 'probe', 'rows': bad}, probes, a1)


class RestoreDispatch(PrivateFixture, unittest.TestCase):
    def test_private_boot_authorization_binds_receipt_and_preserves_manifest(self):
        value = station()
        original = copy.deepcopy(value)
        proof = probe.Proof(self.directory / 'reboot', 'a' * 64)
        proof.state['payload_baseline'] = '001-payload-baseline.json'
        proof.record('reboot-ready', {'station_sha256': 'a' * 64, 'initial_boot': value['initial_boot'],
                     'journal': journal(value), 'journal_sha256': 'b' * 64,
                     'payload_baseline': proof.state['payload_baseline']})
        proof.state['reboot_checkpoint'] = proof.state['events'][-1]
        proof.save()
        _, receipt_sha = probe.reboot_receipt(proof)
        authorization = {'schema': 1, 'station_sha256': 'a' * 64, 'initial_boot': value['initial_boot'],
                         'boot_id': str(uuid.uuid4()), 'reboot_receipt_sha256': receipt_sha}
        effective = probe.validate_reboot_authorization(value, proof, authorization, 'c' * 64)
        self.assertEqual(effective['initial_boot'], authorization['boot_id'])
        self.assertEqual(value, original)
        for field, replacement in (('schema', True), ('station_sha256', 'd' * 64),
                ('initial_boot', str(uuid.uuid4())), ('boot_id', value['initial_boot']),
                ('boot_id', 'unknown'), ('reboot_receipt_sha256', 'd' * 64)):
            with self.subTest(field=field, replacement=replacement), self.assertRaises(ValueError):
                probe.validate_reboot_authorization(value, proof, dict(authorization, **{field: replacement}), 'c' * 64)
        proof.state['reboot_authorization_sha256'] = 'e' * 64
        proof.save()
        reopened = probe.Proof(proof.path, 'a' * 64)
        with self.assertRaisesRegex(ValueError, 'Inne zatwierdzenie'):
            probe.validate_reboot_authorization(value, reopened, authorization, 'c' * 64)

    def test_real_restore_process_observes_pending_and_second_call_refuses_after_reopen(self):
        value = station()
        proof = probe.Proof(self.directory / 'restore', 'a' * 64)
        proof.begin('restore-live')
        request = {'cmd': 'elastic_restore', 'array_id': value['spec']['array_id'], 'owner': value['spec']['owner']}
        response = state_result(value['spec'])
        calls = []
        def run(argv, **kwargs):
            calls.append(argv)
            script = ('import sys,json; s=json.load(open(sys.argv[1])); '
                      'assert s["pending"]=="restore-live" and s["dispatched"]==["restore-live"]; '
                      'assert json.load(sys.stdin)==json.loads(sys.argv[2]); print(sys.argv[3])')
            return subprocess.run([sys.executable, '-c', script, str(proof.path / 'state.json'),
                                   json.dumps(request), json.dumps(response)], **kwargs)
        self.assertEqual(probe.invoke_helper(value, request, proof, run), response)
        reopened = probe.Proof(proof.path, 'a' * 64)
        with self.assertRaisesRegex(ValueError, 'już wywołany'):
            probe.invoke_helper(value, request, reopened, run)
        foreign = dict(request, cmd='elastic_create')
        with self.assertRaises(ValueError):
            probe.invoke_helper(value, foreign, reopened, run)
        self.assertEqual(len(calls), 1)
        raw = json.loads(next(proof.path.glob('*helper-response.json')).read_bytes())
        self.assertEqual(json.loads(base64.b64decode(raw['stdout'])), response)
        self.assertEqual(reopened.state['pending'], 'restore-live')


class ParentTests(unittest.TestCase):
    def test_actual_parent_metadata_rejects_writable_and_symlink_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory) / 'parent'
            parent.mkdir(mode=0o700)
            alias = Path(directory) / 'alias'
            alias.symlink_to(parent, target_is_directory=True)
            with patch.object(probe, 'UID', os.geteuid()), patch.object(probe, 'Path', return_value=SimpleNamespace(parents=[parent])):
                probe.private_parents('unused')
                parent.chmod(0o777)
                with self.assertRaises(ValueError):
                    probe.private_parents('unused')
            with patch.object(probe, 'UID', os.geteuid()), patch.object(probe, 'Path', return_value=SimpleNamespace(parents=[alias])), self.assertRaises(ValueError):
                probe.private_parents('unused')


class StationTests(unittest.TestCase):
    def test_both_closed_station_contracts(self):
        for name in ('L', 'P'):
            with self.subTest(station=name):
                self.assertEqual(probe.validate_station(station(name)), station(name))

    def test_wrong_vm_role_spec_hash_and_duplicate_identity_refused(self):
        mutations = [lambda s: s.update(vm_uuid=str(uuid.UUID(int=1))),
                     lambda s: s.update(schema=True),
                     lambda s: s.update(helper_sha256='0' * 64),
                     lambda s: s['tools'].pop('/usr/bin/mergerfs'),
                     lambda s: s['disks']['data1'].update(bytes=1),
                     lambda s: s['spec']['data'][0].update(serial='foreign'),
                     lambda s: s['spec'].update(filesystem='ext4'),
                     lambda s: s['spec'].update(operation_id=s['spec']['array_id']),
                     lambda s: s['spec']['data'][1].update(disk_id=s['spec']['data'][0]['disk_id']),
                     lambda s: s['spec']['data'][1].update(expected_uuid=s['spec']['data'][0]['expected_uuid']),
                     lambda s: s['maintenance_ids'].update(sync=str(uuid.UUID(int=700)))]
        for mutate in mutations:
            value = station()
            mutate(value)
            with self.subTest(value=value), self.assertRaises(ValueError):
                probe.validate_station(value)


class HelperCalls(PrivateFixture, unittest.TestCase):
    def setUp(self):
        super().setUp()
        self.station = station()
        self.request = {'cmd': 'elastic_create', 'operation': self.station['spec']}
        self.proof = probe.Proof(self.directory / 'proof', 'a' * 64)
        self.proof.begin('create')

    def actual_process(self, stdout, stderr=b'', code=0):
        def run(argv, **kwargs):
            self.assertEqual(argv, [str(probe.HELPER)])
            script = ('import os,sys,json,base64; '
                      'state=json.load(open(sys.argv[1])); '
                      'assert state["pending"]=="create"; '
                      'assert json.loads(sys.stdin.buffer.read())==json.loads(sys.argv[2]); '
                      'os.write(1,base64.b64decode(sys.argv[3])); '
                      'os.write(2,base64.b64decode(sys.argv[4])); sys.exit(int(sys.argv[5]))')
            return subprocess.run([sys.executable, '-c', script, str(self.proof.path / 'state.json'),
                                   json.dumps(self.request), base64.b64encode(stdout).decode(),
                                   base64.b64encode(stderr).decode(), str(code)], **kwargs)
        return run

    def response(self):
        name = next(name for name in self.proof.state['events'] if 'helper-response' in name)
        return json.loads((self.proof.path / name).read_bytes())

    def test_real_process_reads_pending_and_returns_typed_result(self):
        value = state_result(self.station['spec'])
        raw = json.dumps(value).encode()
        actual = probe.invoke_helper(self.station, self.request, self.proof, self.actual_process(raw))
        self.assertEqual(actual, value)
        self.assertEqual(base64.b64decode(self.response()['stdout']), raw)
        self.assertEqual(self.proof.state['pending'], 'create')

    def test_real_failed_process_and_malformed_json_preserve_raw(self):
        for index, (raw, code) in enumerate([(b'not JSON\r\n', 0), (b'partial\r', 69)]):
            self.proof = probe.Proof(self.directory / f'failure-{index}', 'a' * 64)
            self.proof.begin('create')
            with self.subTest(code=code), self.assertRaises(ValueError):
                probe.invoke_helper(self.station, self.request, self.proof,
                                    self.actual_process(raw, b'actual stderr\r', code))
            self.assertEqual(base64.b64decode(self.response()['stdout']), raw)
            self.assertEqual(base64.b64decode(self.response()['stderr']), b'actual stderr\r')
            self.assertEqual(self.response()['exit_code'], code)

    def test_request_fsync_failure_dispatches_zero_processes(self):
        calls = []
        with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'fsync')), self.assertRaises(OSError):
            probe.invoke_helper(self.station, self.request, self.proof, lambda *a, **k: calls.append(a))
        self.assertEqual(calls, [])
        self.assertEqual(json.loads((self.proof.path / 'state.json').read_bytes())['pending'], 'create')

    def test_wrong_phase_or_foreign_spec_dispatches_zero_processes(self):
        for index, kind in enumerate(('phase', 'spec')):
            self.proof = probe.Proof(self.directory / f'wrong-{index}', 'a' * 64)
            self.proof.begin('inspect' if kind == 'phase' else 'create')
            request = copy.deepcopy(self.request)
            if kind == 'spec':
                request['operation']['array_id'] = str(uuid.UUID(int=999))
            calls = []
            def run(*args, **kwargs):
                calls.append(args)
                return subprocess.CompletedProcess(args, 0, json.dumps(state_result(request['operation'])).encode(), b'')
            with self.subTest(kind=kind):
                with self.assertRaises(ValueError):
                    probe.invoke_helper(self.station, request, self.proof, run)
                self.assertEqual(calls, [])

    def test_second_create_refused_even_after_proof_reopen(self):
        value = state_result(self.station['spec'])
        probe.invoke_helper(self.station, self.request, self.proof,
                            self.actual_process(json.dumps(value).encode()))
        self.proof = probe.Proof(self.proof.path, 'a' * 64)
        calls = []
        def run(*args, **kwargs):
            calls.append(args)
            return subprocess.CompletedProcess(args, 0, json.dumps(value).encode(), b'')
        with self.assertRaises(ValueError):
            probe.invoke_helper(self.station, self.request, self.proof, run)
        self.assertEqual(calls, [])

    def test_incomplete_typed_create_result_refused_with_raw_preserved(self):
        value = state_result(self.station['spec'])
        del value['disks']
        raw = json.dumps(value).encode()
        with self.assertRaises(ValueError):
            probe.invoke_helper(self.station, self.request, self.proof, self.actual_process(raw))
        self.assertEqual(base64.b64decode(self.response()['stdout']), raw)

    def test_missing_duplicate_foreign_role_or_uuid_refused(self):
        mutations = [lambda v: v['disks'].pop(),
                     lambda v: v['disks'].__setitem__(1, copy.deepcopy(v['disks'][0])),
                     lambda v: v['disks'][0].update(role={'data': 0}),
                     lambda v: v['disks'][0].update(role={'parity': 1}),
                     lambda v: v['disks'][0].update(observed_uuid=str(uuid.UUID(int=900))),
                     lambda v: v['disks'][0].update(mounted=False),
                     lambda v: v['disks'][1].update(device=v['disks'][0]['device'], kernel_name=v['disks'][0]['kernel_name'])]
        for index, mutate in enumerate(mutations):
            value = state_result(self.station['spec'])
            mutate(value)
            self.proof = probe.Proof(self.directory / f'bad-role-{index}', 'a' * 64)
            self.proof.begin('create')
            raw = json.dumps(value).encode()
            with self.subTest(index=index):
                with self.assertRaises(ValueError):
                    probe.invoke_helper(self.station, self.request, self.proof, self.actual_process(raw))
                self.assertEqual(base64.b64decode(self.response()['stdout']), raw)


class StorageGuards(PrivateFixture, unittest.TestCase):
    def live_observation(self):
        value = station()
        value['initial_boot'] = 'cd3290d4-2486-4158-aa47-e06b97107ce6'
        raw = json.loads(LIVE_LSBLK)
        view = {'disks': raw['blockdevices']}
        def command(args, **kwargs):
            if args[0] == '/usr/bin/lsblk':
                return LIVE_LSBLK
            if args[0] == '/usr/bin/findmnt':
                return json.dumps({'filesystems': [{'source': '/dev/vda1', 'target': '/', 'fstype': 'ext4', 'maj:min': '254:1'}]})
            if args[0] == '/usr/sbin/blkid':
                return 'DEVNAME=/dev/vda\nPTUUID=97af9c2e-ca04-4a3c-bce2-24a66efb4208\nPTTYPE=gpt\n' if args[-1] == '/dev/vda' else None
            if args[0] == '/usr/sbin/wipefs':
                return json.dumps({'signatures': [{'type': 'gpt'}] if args[-1] == '/dev/vda' else []})
            raise AssertionError(args)
        reads = {'/proc/swaps': 'Filename Type Size Used Priority\n',
                 '/sys/class/dmi/id/product_uuid': value['vm_uuid'],
                 '/proc/sys/kernel/random/boot_id': value['initial_boot']}
        with patch.object(storage, 'command', side_effect=command), \
                patch.object(Path, 'stat', lambda path, **kwargs: device_stat(view)(path)), \
                patch.object(Path, 'iterdir', lambda path: iter(())), \
                patch.object(Path, 'read_text', lambda path, **kwargs: reads[str(path)]):
            result = storage.observe()
        return value, result

    def test_actual_lsblk_seed_survives_real_observe_and_storage_guard(self):
        value, observed = self.live_observation()
        self.assertEqual(len(observed['disks']), 7)
        self.assertEqual(observed['disks'][0], json.loads(LIVE_LSBLK)['blockdevices'][0])
        with patch.object(probe.os, 'stat', side_effect=device_stat(observed)), \
                patch.object(probe.os, 'open', side_effect=AssertionError('Brak hostowego raw I/O w unit')):
            self.assertEqual(set(probe.guard_observation(value, observed)), set(probe.SIZES))
        self.seed_reader.assert_called_once_with(observed['disks'][0], probe.SEEDS['L'])

    def test_seed_metadata_and_extra_entries_refused_before_seed_read(self):
        value, observed = self.live_observation()
        mutations = [lambda o: o['disks'].pop(0),
                     lambda o: o['disks'].append(copy.deepcopy(o['disks'][0])),
                     lambda o: o['disks'].append({'type': 'loop'}),
                     lambda o: o['disks'][0].update(name='sr1'),
                     lambda o: o['disks'][0].update(serial='foreign'),
                     lambda o: o['disks'][0].update(ro=False),
                     lambda o: o['disks'][0].update(ro=1),
                     lambda o: o['disks'][0].update(size=probe.SEED_BYTES + 1),
                     lambda o: o['disks'][0].update(fstype='ext4'),
                     lambda o: o['disks'][0].update(uuid='foreign'),
                     lambda o: o['disks'][0].update(**{'maj:min': '11:1'}),
                     lambda o: o['disks'][0].update(children=[{'maj:min': '11:1'}]),
                     lambda o: o['disks'][0].update(holders=['dm-0']),
                     lambda o: o['disks'][0].update(mountpoints=['/seed']),
                     lambda o: o['swaps'].append('11:0'),
                     lambda o: o['mounts'].append({'target': '/seed', 'maj:min': '11:0'})]
        for index, mutate in enumerate(mutations):
            item = copy.deepcopy(observed)
            mutate(item)
            self.seed_reader.reset_mock()
            with self.subTest(index=index):
                with self.assertRaises(ValueError):
                    probe.guard_observation(value, item)
                self.seed_reader.assert_not_called()

    def test_actual_validator_accepts_both_empty_and_created_six_disk_maps(self):
        for name in ('L', 'P'):
            value = station(name)
            for created in (False, True):
                observed = observation(value, created)
                with self.subTest(station=name, created=created), patch.object(probe.os, 'stat', side_effect=device_stat(observed)):
                    self.assertEqual(set(probe.guard_observation(value, observed, created)), set(probe.SIZES))

    def test_guard_denials_prevent_helper_dispatch(self):
        mutations = [lambda o: o.update(uuid=str(uuid.UUID(int=2))),
                     lambda o: o.update(boot_id=str(uuid.UUID(int=3))),
                     lambda o: o['disks'][1].update(size=1),
                     lambda o: o['disks'][1].update(type='part'),
                     lambda o: o['disks'][1].update(ro=True),
                     lambda o: o['disks'][1].update(children=[{'maj:min': '252:17'}]),
                     lambda o: o['disks'][1].update(holders=['dm-1']),
                     lambda o: o['disks'][1].update(fstype='xfs'),
                     lambda o: o['disks'][1].update(signatures=[{'type': 'xfs'}]),
                     lambda o: o['swaps'].append(o['disks'][1]['maj:min']),
                     lambda o: o['mounts'].append({'target': '/foreign', 'maj:min': o['disks'][1]['maj:min']}),
                     lambda o: o['mounts'][0].update(**{'maj:min': o['disks'][1]['maj:min']}),
                     lambda o: o['disks'].append(copy.deepcopy(o['disks'][1]))]
        value = station()
        for index, mutate in enumerate(mutations):
            observed = observation(value)
            mutate(observed)
            proof = probe.Proof(self.directory / f'guard-{index}', 'a' * 64)
            storage = SimpleNamespace(observe=lambda: observed)
            with self.subTest(index=index), patch.object(probe.os, 'stat', side_effect=device_stat(observed)), \
                    patch.object(probe, 'invoke_helper') as dispatch:
                with self.assertRaises(ValueError):
                    probe.execute('preflight', value, proof, {'guest_storage.py': storage})
                dispatch.assert_not_called()
                self.assertIsNone(proof.state['pending'])

    def test_created_uuid_and_block_device_identity_are_not_adopted(self):
        value = station()
        observed = observation(value, True)
        for kind in ('uuid', 'device'):
            item = copy.deepcopy(observed)
            read = device_stat(item)
            if kind == 'uuid':
                item['disks'][1]['uuid'] = str(uuid.UUID(int=1))
            else:
                read = lambda path: SimpleNamespace(st_mode=stat.S_IFREG | 0o600, st_rdev=0)
            with self.subTest(kind=kind), patch.object(probe.os, 'stat', side_effect=read), self.assertRaises(ValueError):
                probe.guard_observation(value, item, True)


class JournalGuards(unittest.TestCase):
    def test_exact_schema_two_journal_and_anchor(self):
        value = station()
        result = journal(value)
        self.assertEqual(probe.validate_journal(value, result), result['private']['anchor'])

    def test_pending_legacy_foreign_anchor_or_missing_role_refused(self):
        value = station()
        mutations = [lambda j: j.update(schema=1), lambda j: j.update(pending='union'),
                     lambda j: j.update(stage='needs_attention'), lambda j: j['formatted'].pop(),
                     lambda j: j['spec']['data'][0].update(expected_uuid=str(uuid.UUID(int=1))),
                     lambda j: j.update(boot_id=str(uuid.UUID(int=2))),
                     lambda j: j['private'].update(published=False),
                     lambda j: j['private']['anchor'].update(pid=1),
                     lambda j: j['private']['anchor'].update(start_ticks='200'),
                     lambda j: j['private']['anchor'].update(exe_sha256='f' * 64),
                     lambda j: j['private']['anchor'].update(union_source='')]
        for index, mutate in enumerate(mutations):
            item = journal(value)
            mutate(item)
            with self.subTest(index=index), self.assertRaises(ValueError):
                probe.validate_journal(value, item)

    def test_anchor_reuses_actual_proc_start_time_string(self):
        child = subprocess.Popen([sys.executable, '-c', 'import sys;sys.stdin.buffer.read()'],
                                 stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            path = Path(f'/proc/{child.pid}/exe')
            with path.open('rb') as stream:
                info = os.fstat(stream.fileno())
                sha = hashlib.file_digest(stream, 'sha256').hexdigest()
            ns = os.stat(f'/proc/{child.pid}/ns/mnt').st_ino
            ticks = a0.start_time(child.pid)
            self.assertIsInstance(ticks, str)
            anchor = {'pid': child.pid, 'start_ticks': int(ticks), 'mount_ns_inode': ns,
                      'exe_device': info.st_dev, 'exe_inode': info.st_ino, 'exe_sha256': sha}
            actual_stat = os.stat
            def read(path, *args, **kwargs):
                if str(path) == '/proc/self/ns/mnt':
                    return SimpleNamespace(st_ino=ns + 1)
                return actual_stat(path, *args, **kwargs)
            with patch.object(probe.os, 'stat', side_effect=read):
                self.assertEqual(probe.anchor_identity(anchor, a0)['start_ticks'], int(ticks))
                for key, bad in [('start_ticks', int(ticks) + 1), ('exe_sha256', 'f' * 64), ('mount_ns_inode', ns + 1)]:
                    changed = dict(anchor, **{key: bad})
                    with self.subTest(key=key), self.assertRaises(ValueError):
                        probe.anchor_identity(changed, a0)
            with self.assertRaises(ValueError):
                probe.anchor_identity(anchor, a0)
        finally:
            child.communicate(timeout=5)


class SeedBytesTests(unittest.TestCase):
    def test_read_seed_hashes_exact_real_bytes_and_rejects_size_hash_or_device(self):
        rom = json.loads(LIVE_LSBLK)['blockdevices'][0]
        payload = b'S' * probe.SEED_BYTES
        expected = hashlib.sha256(payload).hexdigest()
        for case in ('valid', 'hash', 'short', 'long', 'regular', 'device'):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / 'seed'
                raw = payload[:-1] if case == 'short' else payload + b'x' if case == 'long' else payload
                with path.open('xb') as stream:
                    stream.write(raw)
                    stream.flush()
                    os.fsync(stream.fileno())
                actual_open = os.open
                def open_seed(target, flags):
                    self.assertEqual(target, '/dev/sr0')
                    self.assertEqual(flags, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
                    return actual_open(path, flags)
                info = SimpleNamespace(st_mode=(stat.S_IFREG if case == 'regular' else stat.S_IFBLK) | 0o600,
                                       st_rdev=os.makedev(11, 1 if case == 'device' else 0))
                with patch.object(probe.os, 'open', side_effect=open_seed), patch.object(probe.os, 'fstat', return_value=info):
                    if case == 'valid':
                        result = probe.read_seed(rom, expected)
                        self.assertEqual(result, {'bytes': len(payload), 'sha256': expected, 'device': info.st_rdev})
                    else:
                        with self.assertRaises(ValueError):
                            probe.read_seed(rom, 'f' * 64 if case == 'hash' else expected)


class PayloadFiles(PrivateFixture, unittest.TestCase):
    def test_real_seventeen_megabyte_write_hash_and_exact_mtime(self):
        path = self.directory / 'large.bin'
        size = 17 * 1024**2
        metric = probe.write_payload_file(path, size, b'A')
        self.assertEqual((metric['bytes'], metric['sha256'], metric['mode'], metric['nlink']),
                         (size, hashlib.sha256(b'A' * size).hexdigest(), 0o600, 1))
        ns = 1900000000123456789
        os.utime(path, ns=(ns, ns))
        self.assertEqual(probe.file_metric(path)['mtime_ns'], str(ns))

    def test_real_short_writes_and_exclusive_payload(self):
        path = self.directory / 'short.bin'
        actual = os.write
        with patch.object(probe.os, 'write', side_effect=lambda fd, raw: actual(fd, raw[:127])):
            result = probe.write_payload_file(path, 4096, b'B')
        self.assertEqual(result['sha256'], hashlib.sha256(b'B' * 4096).hexdigest())
        with self.assertRaises(FileExistsError):
            probe.write_payload_file(path, 4096, b'C')
        self.assertEqual(path.read_bytes(), b'B' * 4096)

    def test_payload_fsync_failure_preserves_file_and_does_not_report_metric(self):
        path = self.directory / 'failed.bin'
        with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'fsync')), self.assertRaises(OSError):
            probe.write_payload_file(path, 4096, b'A')
        self.assertEqual(path.read_bytes(), b'A' * 4096)

    def test_zero_write_and_invalid_plan_refuse_without_reporting_success(self):
        path = self.directory / 'zero.bin'
        with patch.object(probe.os, 'write', return_value=0), self.assertRaises(ValueError):
            probe.write_payload_file(path, 4096, b'A')
        self.assertEqual(path.read_bytes(), b'')
        for size, byte in [(0, b'A'), (True, b'A'), (17 * 1024**2 + 1, b'A'), (1, b'AB')]:
            with self.subTest(size=size, byte=byte), patch.object(probe.os, 'open') as opened:
                with self.assertRaises(ValueError):
                    probe.write_payload_file(self.directory / 'never', size, byte)
                opened.assert_not_called()

    def test_payload_symlink_hardlink_oversize_and_changed_read_refuse(self):
        path = self.write('original', b'A' * 4096)
        symlink = self.directory / 'link'
        symlink.symlink_to(path)
        with self.assertRaises(OSError):
            probe.file_metric(symlink)
        with self.assertRaises(ValueError):
            probe.file_metric(path, limit=4095)
        actual = hashlib.file_digest
        def change_after_hash(stream, algorithm):
            result = actual(stream, algorithm)
            with path.open('ab') as output:
                output.write(b'changed')
                output.flush()
                os.fsync(output.fileno())
            return result
        with patch.object(probe.hashlib, 'file_digest', side_effect=change_after_hash), self.assertRaises(ValueError):
            probe.file_metric(path)
        os.link(path, self.directory / 'hardlink')
        with self.assertRaises(ValueError):
            probe.file_metric(path)

    def measured_payload(self):
        value = station('P')
        branch = probe.role_paths(value)[0]
        folder = str(branch[3] / 'a21-fixture')
        measured = {'files': {}, 'union_names': [], 'branch_names': {folder: []}}
        for name, (size, byte, uid) in probe.payload_layout(value).items():
            metric = probe.write_payload_file(self.directory / name, size, byte)
            if os.geteuid() == 0:
                os.chown(self.directory / name, uid, uid)
                metric = probe.file_metric(self.directory / name)
            public = dict(metric, device=os.makedev(0, 77), inode=metric['inode'] + 1000)
            measured['files'][name] = {'union': public, 'backing': metric,
                                       'path': folder + '/' + name, 'fs_uuid': branch[2]['expected_uuid']}
            measured['union_names'].append(name)
            measured['branch_names'][folder].append(name)
        return value, measured, {branch[2]['expected_uuid']: metric['device']}, public['device']

    def test_payload_manifest_uses_real_bytes_and_separate_fuse_identity(self):
        value, measured, devices, fuse = self.measured_payload()
        self.assertEqual(probe.validate_payload(value, measured, devices=devices, union_device=fuse), measured)
        for name, row in measured['files'].items():
            self.assertNotEqual(row['backing']['inode'], row['union']['inode'])

    def test_payload_reboot_allows_only_backing_device_projection(self):
        value, measured, devices, fuse = self.measured_payload()
        changed = copy.deepcopy(measured)
        new_devices = {key: device + 1 for key, device in devices.items()}
        for row in changed['files'].values():
            row['backing']['device'] += 1
            row['union']['device'] = fuse + 1
            row['union']['inode'] += 20
        with self.assertRaises(ValueError):
            probe.validate_payload(value, changed, baseline=measured, devices=new_devices, union_device=fuse + 1)
        self.assertEqual(probe.validate_payload(value, changed, baseline=measured, reboot=True,
                                               devices=new_devices, union_device=fuse + 1), changed)
        for key, replacement in [('inode', 1), ('mtime_ns', '1900000000123456789'), ('sha256', 'f' * 64),
                                 ('uid', 1), ('mode', 0o644), ('nlink', 2)]:
            item = copy.deepcopy(changed)
            item['files']['payload-a.bin']['backing'][key] = replacement
            with self.subTest(key=key), self.assertRaises(ValueError):
                probe.validate_payload(value, item, baseline=measured, reboot=True, devices=new_devices, union_device=fuse + 1)

    def test_payload_exact_names_placement_uuid_and_devices_refuse(self):
        value, measured, devices, fuse = self.measured_payload()
        branch = next(iter(measured['branch_names']))
        mutations = [lambda v: v['union_names'].append('foreign'),
                     lambda v: v['union_names'].pop(),
                     lambda v: v['branch_names'][branch].append('payload-a.bin'),
                     lambda v: v['files']['payload-a.bin'].update(fs_uuid=str(uuid.UUID(int=1))),
                     lambda v: v['files']['payload-a.bin'].update(path='/foreign/payload-a.bin'),
                     lambda v: v['files']['payload-a.bin']['backing'].update(device=0),
                     lambda v: v['files']['payload-a.bin']['union'].update(device=0),
                     lambda v: v['files']['payload-a.bin']['backing'].update(mtime_ns=1900000000123456789)]
        for index, mutate in enumerate(mutations):
            item = copy.deepcopy(measured)
            mutate(item)
            with self.subTest(index=index), self.assertRaises((ValueError, KeyError)):
                probe.validate_payload(value, item, devices=devices, union_device=fuse)


class PayloadChild(PrivateFixture, unittest.TestCase):
    def test_real_unprivileged_writer_waits_for_durable_ack(self):
        value = station()
        public = self.directory / 'public'
        directory = public / value['spec']['name'] / 'a21-fixture'
        directory.mkdir(parents=True)
        proof = probe.Proof(self.directory / 'proof', 'a' * 64)
        proof.begin('payload')
        actual_path = Path
        def relocated(path):
            return public if str(path) == '/mnt' else actual_path(path)
        def current_identity(namespace):
            identity = a0.identity()
            probe.require(identity['uid'] == 1000 and identity['euid'] == 1000 and int(identity['caps'], 16) == 0,
                          'Test wymaga rzeczywistego nieuprzywilejowanego UID1000')
            probe.require(json.loads((proof.path / 'state.json').read_bytes())['pending'] == 'payload', 'Brak pending')
            return {'before': identity, 'after': identity, 'namespace_errno': None}
        actor_api = SimpleNamespace(enter_actor=current_identity, send=a1.send, operation=a1.operation)
        with patch.object(a1, 'a0', a0, create=True), patch.object(probe, 'Path', side_effect=relocated):
            child = a1.Child(probe.payload_writer, (value, actor_api))
            try:
                ready = probe.child_value(child, proof, 'writer-ready-test')
                a0.validate_actor(ready['identity'], False, a1.parent_uid(child))
                self.assertEqual(list(directory.iterdir()), [])
                self.assertEqual(set(ready['descriptors']), {'0', '1', '2', str(child.child_fd)})
                child.send('write')
                written = probe.child_value(child, proof, 'writer-result-test')
                self.assertEqual(set(written['files']), {'payload-a.bin', 'payload-b.bin'})
                self.assertTrue(all(row['uid'] == 1000 and row['bytes'] == 2 * 1024**2 for row in written['files'].values()))
            finally:
                probe.stop_child(child, proof)
        self.assertEqual(proof.state['pending'], 'payload')


class AuditTests(PrivateFixture, unittest.TestCase):
    def setUp(self):
        super().setUp()
        self.value = station()
        self.root = self.directory / 'root'
        self.root.mkdir(mode=0o700)
        self.stack.enter_context(patch.object(probe, 'ROOT', self.root))
        self.proof = probe.Proof(self.directory / 'proof', 'a' * 64)
        self.proof.begin('inspect-created')
        self.process = subprocess.Popen([sys.executable, '-c', 'import sys;sys.stdin.buffer.read()'],
                                        stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(lambda: self.process.communicate(timeout=5))
        self.record = journal(self.value)
        with Path(f'/proc/{self.process.pid}/exe').open('rb') as stream:
            info = os.fstat(stream.fileno())
            sha = hashlib.file_digest(stream, 'sha256').hexdigest()
        self.value['tools']['/usr/bin/mergerfs'] = sha
        self.record['spec'] = copy.deepcopy(self.value['spec'])
        self.anchor = self.record['private']['anchor']
        self.anchor.update(pid=self.process.pid, start_ticks=int(a0.start_time(self.process.pid)),
                           mount_ns_inode=os.stat(f'/proc/{self.process.pid}/ns/mnt').st_ino,
                           exe_device=info.st_dev, exe_inode=info.st_ino, exe_sha256=sha, union_device=os.makedev(0, 77))
        self.lock = self.write('root/.storage.lock', b'')
        self.journal_path = self.write('root/' + self.value['spec']['array_id'] + '.json', json.dumps(self.record).encode())
        observed = observation(self.value, True)
        with patch.object(probe.os, 'stat', side_effect=device_stat(observed)):
            self.disks = probe.guard_observation(self.value, observed, True)
        self.mountinfo = f'40 1 0:77 / /mnt/a21-l rw - fuse.mergerfs {self.anchor["union_source"]} rw\n'
        self.options = {'branches': ':'.join(str(path) + '=RW' for kind, _, _, path in probe.role_paths(self.value) if kind == 'data'),
                        'category.create': 'mfs', 'cache.files': 'off', 'minfreespace': str(20 * 1024**3), 'moveonenospc': 'mfs'}
        inside_rows, inside_info = [], self.mountinfo
        for kind, index, disk, path in probe.role_paths(self.value):
            device = next(d for d in self.disks.values() if d['serial'] == disk['serial'])
            inside_info += f'{50 + index} 1 {device["maj:min"]} / {path} rw - xfs /dev/{device["name"]} rw\n'
            inside_rows.append({'role': {kind: index}, 'path': str(path), 'device': {'errno': None, 'value': device['device']}})
        self.inside = {'namespace': self.anchor['mount_ns_inode'], 'roles': inside_rows,
                       'union': {'raw_mountinfo': {'errno': None, 'value': inside_info},
                                 'device': {'errno': None, 'value': self.anchor['union_device']},
                                 'options': {key: {'errno': None, 'value': value} for key, value in self.options.items()}}}
        actual_stat, actual_read = os.stat, Path.read_text
        def stat_adapter(path, *args, **kwargs):
            if str(path) == '/proc/self/ns/mnt':
                return SimpleNamespace(st_ino=self.anchor['mount_ns_inode'] + 1)
            if str(path) == '/mnt/a21-l':
                return SimpleNamespace(st_dev=self.anchor['union_device'])
            return actual_stat(path, *args, **kwargs)
        def read_adapter(path, *args, **kwargs):
            return self.mountinfo if str(path) == '/proc/self/mountinfo' else actual_read(path, *args, **kwargs)
        self.stack.enter_context(patch.object(probe.os, 'stat', side_effect=stat_adapter))
        self.stack.enter_context(patch.object(Path, 'read_text', read_adapter))
        self.stack.enter_context(patch.object(probe.os, 'getxattr', side_effect=lambda path, key: self.options[key.removeprefix('user.mergerfs.')].encode()))
        parent = self
        class ChildTransport:
            def __init__(self, function, args, descriptors):
                script = ('import sys,fcntl,json; f=open(sys.argv[1],"rb"); '
                          '\ntry: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)'
                          '\nexcept BlockingIOError: print(sys.argv[2])'
                          '\nelse: sys.exit(44)')
                self.child = subprocess.Popen([sys.executable, '-c', script, str(parent.lock), json.dumps(parent.inside)],
                                              stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            def receive(self):
                stdout, stderr = self.child.communicate(timeout=5)
                parent.assertEqual((self.child.returncode, stderr), (0, b''))
                return {'raw': base64.b64encode(stdout).decode(), 'error': None}
            def stop(self):
                self.child.communicate(timeout=5)
                return {'alive': self.child.poll() is None, 'exit_code': self.child.returncode}
        self.modules = {'guest_writer_gate_probe.py': a0, 'guest_branch_isolation_probe.py': SimpleNamespace(Child=ChildTransport)}

    def test_real_shared_lock_raw_audit_and_release_without_journal_change(self):
        before = self.journal_path.read_bytes()
        result = probe.audit(self.value, self.proof, self.modules, self.disks)
        self.assertEqual(result['journal_sha256'], hashlib.sha256(before).hexdigest())
        self.assertEqual(self.journal_path.read_bytes(), before)
        with self.lock.open('rb') as stream:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_shared_namespace_reader_uses_real_lock_and_durable_raw_before_return(self):
        before = self.journal_path.read_bytes()
        result = probe.read_namespace(self.value, self.proof, self.modules, self.disks,
                                      probe.payload_reader, label='shared-test')
        self.assertEqual(result['value'], self.inside)
        raw = json.loads(next(self.proof.path.glob('*-shared-test.json')).read_bytes())
        self.assertEqual(json.loads(base64.b64decode(raw['raw'])), self.inside)
        self.assertEqual(self.journal_path.read_bytes(), before)
        self.assertEqual(len([n for n in self.proof.state['events'] if n.endswith('-audit.json')]), 2)
        with self.lock.open('rb') as stream:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_new_l_phases_refuse_missing_pending_before_any_lock(self):
        self.proof.state['pending'] = None
        with patch.object(probe, 'open_product_lock') as lock:
            with self.assertRaises(ValueError):
                probe.isolation(self.value, self.proof, self.modules, self.disks)
            with self.assertRaises(ValueError):
                probe.restore_live(self.value, 'restore-live', self.proof, self.modules, self.disks)
            lock.assert_not_called()

    def test_reboot_changed_journal_is_durable_and_refuses_before_helper(self):
        original = self.journal_path.read_bytes()
        self.proof.record('reboot-ready', {'initial_boot': self.value['initial_boot'],
                          'journal': self.record, 'journal_sha256': hashlib.sha256(original).hexdigest()})
        self.proof.state.update(reboot_checkpoint=self.proof.state['events'][-1],
                                reboot_authorization_sha256='b' * 64, pending='restore-reboot')
        self.proof.save()
        effective = dict(self.value, initial_boot=str(uuid.uuid4()))
        self.journal_path.write_bytes(original + b'\n')
        with patch.object(probe, 'invoke_helper') as invoke:
            with self.assertRaisesRegex(ValueError, 'Journal zmienił się'):
                probe.restore_reboot(effective, 'restore-reboot', self.proof, self.modules, self.disks)
            invoke.assert_not_called()
        saved = json.loads(next(self.proof.path.glob('*reboot-journal-before.json')).read_bytes())
        self.assertEqual(base64.b64decode(saved['raw']), original + b'\n')
        self.assertEqual(probe.Proof(self.proof.path, 'a' * 64).state['pending'], 'restore-reboot')
        with self.lock.open('rb') as stream:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_reboot_restore_releases_read_lock_for_process_and_accepts_reused_numeric_anchor(self):
        original = self.journal_path.read_bytes()
        self.proof.record('reboot-ready', {'initial_boot': self.value['initial_boot'],
                          'journal': self.record, 'journal_sha256': hashlib.sha256(original).hexdigest()})
        self.proof.state.update(reboot_checkpoint=self.proof.state['events'][-1],
                                reboot_authorization_sha256='b' * 64, pending='restore-reboot')
        self.proof.save()
        effective = dict(self.value, initial_boot=str(uuid.uuid4()))
        updated = copy.deepcopy(self.record)
        updated['boot_id'] = effective['initial_boot']
        updated['private']['anchor']['boot_id'] = effective['initial_boot']
        response = state_result(effective['spec'])
        actual_invoke = probe.invoke_helper
        def run(argv, **kwargs):
            script = ('import sys,json,fcntl; lock=open(sys.argv[1],"rb"); '
                      'fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB); '
                      'state=json.load(open(sys.argv[2])); assert state["dispatched"]==["restore-reboot"]; '
                      'assert json.load(sys.stdin)["cmd"]=="elastic_restore"; '
                      'open(sys.argv[3],"w").write(sys.argv[4]); print(sys.argv[5])')
            return subprocess.run([sys.executable, '-c', script, str(self.lock), str(self.proof.path / 'state.json'),
                                   str(self.journal_path), json.dumps(updated), json.dumps(response)], **kwargs)
        with patch.object(probe, 'invoke_helper', side_effect=lambda s, r, p: actual_invoke(s, r, p, run)), \
                patch.object(probe, 'read_payload') as payload:
            probe.restore_reboot(effective, 'restore-reboot', self.proof, self.modules, self.disks)
            payload.assert_called_once_with(effective, self.proof, self.modules, self.disks, reboot=True)
        self.assertEqual(json.loads(self.journal_path.read_bytes()), updated)
        self.assertEqual(updated['private']['anchor']['pid'], self.record['private']['anchor']['pid'])
        self.assertTrue(any(name.endswith('-reboot-accepted.json') for name in self.proof.state['events']))

    def test_wrong_host_option_preserved_before_refusal(self):
        self.options['cache.files'] = 'partial'
        with self.assertRaises(ValueError):
            probe.audit(self.value, self.proof, self.modules, self.disks)
        path = next(self.proof.path / name for name in self.proof.state['events'] if 'host-publication' in name)
        self.assertEqual(json.loads(path.read_bytes())['union']['options']['cache.files']['value'], 'partial')

    def test_wrong_private_mount_preserved_before_refusal(self):
        self.inside['roles'][0]['device']['value'] += 1
        with self.assertRaises(ValueError):
            probe.audit(self.value, self.proof, self.modules, self.disks)
        path = next(self.proof.path / name for name in self.proof.state['events'] if 'namespace-raw' in name)
        raw = json.loads(path.read_bytes())['raw']
        self.assertEqual(json.loads(base64.b64decode(raw))['roles'], self.inside['roles'])

    def test_payload_without_pending_or_already_completed_refuses_before_lock_and_child(self):
        for pending, completed in ((None, []), ('inspect-created', []), ('payload', ['payload'])):
            with self.subTest(pending=pending, completed=completed):
                self.proof.state.update(pending=pending, completed=completed)
                with patch.object(probe, 'open_product_lock') as lock, \
                        patch.object(self.modules['guest_branch_isolation_probe.py'], 'Child') as child:
                    with self.assertRaisesRegex(ValueError, 'Brak trwałej fazy danych'):
                        probe.payload(self.value, self.proof, self.modules, self.disks)
                    lock.assert_not_called()
                    child.assert_not_called()

    def test_payload_prepare_refusal_or_raw_fsync_failure_sends_no_ack(self):
        original_child = self.modules['guest_branch_isolation_probe.py'].Child
        initial_journal = self.journal_path.read_bytes()
        for failure in ('identity', 'fsync'):
            self.proof = probe.Proof(self.directory / ('payload-' + failure), 'a' * 64)
            self.proof.begin('payload')
            sent = []
            parent = self
            class GateChild(original_child):
                def __init__(self, function, args, descriptors):
                    previous = parent.inside
                    try:
                        if function is probe.payload_prepare and failure == 'identity':
                            parent.inside = copy.deepcopy(previous)
                            parent.inside['roles'][0]['device']['value'] += 1
                        super().__init__(function, args, descriptors)
                    finally:
                        parent.inside = previous
                def send(self, command):
                    sent.append(command)
                    raise AssertionError('ACK nie jest dozwolony w tym scenariuszu')
            self.modules['guest_branch_isolation_probe.py'] = SimpleNamespace(Child=GateChild)
            actual_record = self.proof.record
            def record(label, value):
                if label == 'payload-prepare-before' and failure == 'fsync':
                    with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'fsync')):
                        return actual_record(label, value)
                return actual_record(label, value)
            with self.subTest(failure=failure), patch.object(self.proof, 'record', side_effect=record):
                with self.assertRaises((ValueError, OSError)):
                    probe.payload(self.value, self.proof, self.modules, self.disks)
                self.assertEqual(sent, [])
                self.assertEqual(self.proof.state['pending'], 'payload')
                self.assertEqual(self.journal_path.read_bytes(), initial_journal)
                files = list(self.proof.path.glob('*payload-prepare-before.json'))
                self.assertEqual(len(files), 1)
                measured = json.loads(base64.b64decode(json.loads(files[0].read_bytes())['raw']))
                expected_device = self.inside['roles'][0]['device']['value'] + (failure == 'identity')
                self.assertEqual(measured['roles'][0]['device']['value'], expected_device)
                with self.lock.open('rb') as stream:
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
