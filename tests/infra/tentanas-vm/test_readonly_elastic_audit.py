# =============================================================================
# Plik: test_readonly_elastic_audit.py
# Opis: Lokalne testy parserów i odczytowych granic sondy E2, bez dostępu do VM.
# Przykład: python3 -B -m unittest discover -s tests/infra/tentanas-vm -p test_readonly_elastic_audit.py
# =============================================================================

import hashlib
import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

loader = importlib.util.spec_from_file_location('audit', Path(__file__).with_name('readonly-elastic-audit.py'))
audit = importlib.util.module_from_spec(loader)
loader.loader.exec_module(audit)


def station(specs=()):
    cases = {name: {'filesystem': fs, 'dataRoles': data, 'parityRoles': parity,
                   'cacheRoles': [], 'spec': None, 'pins': None}
             for name, (fs, data, parity) in audit.CASES.items()}
    for spec in specs:
        cases[spec['name']].update(spec=copy.deepcopy(spec), pins={
            'ownerSha256': hashlib.sha256(json.dumps(spec['owner'], sort_keys=True).encode()).hexdigest(),
            'specSha256': hashlib.sha256(json.dumps(spec, sort_keys=True).encode()).hexdigest(),
            'configSha256': 'c' * 64 if spec['parity'] else None})
    return {'schema': 1, 'stage': 'postcreate' if specs else 'create',
            'vm': {'uuid': audit.VM_UUID, 'baseUrl': 'https://127.0.0.1:34961',
                   'manifestSha256': 'a' * 64,
                   'disks': {role: {'serial': 'tn-561fce56b0-' + role, 'bytes': size * audit.GIB}
                             for role, size in audit.DISK_SIZES.items()}},
            'deployment': {'coreSha256': 'a' * 64, 'helperSha256': 'b' * 64, 'wasmSha256': 'c' * 64,
                           'coreVersion': '0.8.0', 'helperVersion': '0.8.0'},
            'nodeId': 'd' * 64, 'cases': cases}


def journal(name='e2-xfs-two'):
    filesystem, roles, *_ = audit.expected_paths(name)
    spec = {'array_id': '11111111-1111-4111-8111-111111111111',
            'operation_id': '22222222-2222-4222-8222-222222222222',
            'owner': {'org_id': 'private-org', 'addon_id': 'private-addon'},
            'name': name, 'filesystem': filesystem, 'data': [], 'parity': []}
    for role, index, suffix, _ in roles:
        serial = 'tn-561fce56b0-' + suffix
        spec[role].append({'disk_id': suffix, 'serial': serial, 'wwn': None,
                           'bytes': audit.DISK_SIZES[suffix] * audit.GIB, 'expected_uuid': f'33333333-3333-4333-8333-3333333333{0 if role == "data" else 1}{index}'})
    if name == 'e2-ext4-zero':
        spec['array_id'] = '44444444-4444-4444-8444-444444444444'
        spec['operation_id'] = '55555555-5555-4555-8555-555555555555'
        spec['data'][0]['expected_uuid'] = '66666666-6666-4666-8666-666666666666'
    return {'schema': 1, 'spec': spec, 'stage': 'ready', 'pending': None,
            'formatted': [{role: index} for role, index, _, _ in roles], 'boot_id': audit.VM_UUID,
            'sync_completed_at': '2026-09-07T12:00:00Z' if spec['parity'] else None, 'detail': None}


class AuditTests(unittest.TestCase):
    def test_candidate_reads_only_explicit_attempt_without_accepting_ready(self):
        value = journal()
        value['spec']['owner'] = {'org_id': 'org-default', 'addon_id': 'tentanas-07ec21cd'}
        value.update(stage='needs_attention', pending={'format': {'data': 1}}, formatted=[])
        contract = station()
        disks = [{'type': 'disk', 'name': f'/dev/test{i}', 'maj:min': f'8:{i}', 'ro': False,
                  'serial': disk['serial'], 'size': disk['bytes']}
                 for i, disk in enumerate(contract['vm']['disks'].values())]
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(audit, 'UID', os.getuid()), \
                mock.patch.object(audit, 'ROOT', Path(directory)), mock.patch.object(audit, 'safe_parents'), \
                mock.patch.object(Path, 'read_text', return_value=audit.VM_UUID), \
                mock.patch.object(audit, 'command', return_value=subprocess.CompletedProcess([], 0, json.dumps({'blockdevices': disks}), '')):
            root = Path(directory)
            path = root / (value['spec']['array_id'] + '.json')
            path.write_text(json.dumps(value))
            path.chmod(0o600)
            (root / '.storage.lock').touch(mode=0o600)
            config = root / 'config'
            config.write_bytes(b'config fixture')
            config.chmod(0o600)
            paths = audit.expected_paths('e2-xfs-two')
            before = path.read_bytes()
            with mock.patch.object(audit, 'expected_paths', return_value=(*paths[:2], str(config), *paths[3:])):
                report = {}
                audit.candidate('e2-xfs-two', report, contract, value['spec']['array_id'], value['spec']['operation_id'])
                self.assertEqual(report['status'], 'candidate')
                self.assertEqual(report['journal']['stage'], 'needs_attention')
                self.assertEqual(report['journal_sha256'], hashlib.sha256(before).hexdigest())
                self.assertEqual(report['pins']['configSha256'], hashlib.sha256(b'config fixture').hexdigest())
                self.assertEqual(path.read_bytes(), before)
                with self.assertRaisesRegex(ValueError, 'Obca próba'):
                    audit.candidate('e2-xfs-two', {}, contract, value['spec']['array_id'], '77777777-7777-4777-8777-777777777777')
                with (root / '.storage.lock').open('rb') as lock:
                    audit.fcntl.flock(lock, audit.fcntl.LOCK_EX | audit.fcntl.LOCK_NB)
                    with self.assertRaises(BlockingIOError):
                        audit.candidate('e2-xfs-two', {}, contract, value['spec']['array_id'], value['spec']['operation_id'])
                (root / '.storage.lock').unlink()
                with self.assertRaises(FileNotFoundError):
                    audit.candidate('e2-xfs-two', {}, contract, value['spec']['array_id'], value['spec']['operation_id'])
                self.assertFalse((root / '.storage.lock').exists())

    def test_shared_fixture_has_two_exact_specs(self):
        fixture = Path(__file__).with_name('fixtures') / 'e2-station-contract.json'
        value = audit.decode_json(fixture.read_bytes())
        self.assertEqual(audit.validate_contract(value), station([journal(name)['spec'] for name in audit.CASES]))

    def test_contract_stages_and_partial_postcreate_are_closed(self):
        create = station()
        self.assertEqual(audit.validate_contract(create), create)
        seed = dict(create, stage='seed', nodeId=None)
        self.assertEqual(audit.validate_contract(seed), seed)
        post = station([journal()['spec']])
        self.assertEqual(audit.validate_contract(post), post)
        for mutation in [lambda x: x.update(extra=True), lambda x: x.update(schema=True),
                         lambda x: x['vm'].update(uuid='16e0a47b-f61a-4a9c-8407-e2b1ce554d59'),
                         lambda x: x['vm'].update(baseUrl='https://127.0.0.1:42171'),
                         lambda x: x['vm']['disks']['data1'].update(bytes=1),
                         lambda x: x['vm']['disks']['cache'].update(serial='foreign'),
                         lambda x: x.update(nodeId=None), lambda x: x.update(stage='create'),
                         lambda x: x['cases']['e2-xfs-two'].update(dataRoles=['data2']),
                         lambda x: x['cases']['e2-xfs-two']['spec']['data'][0].update(expected_uuid='00000000-0000-0000-0000-000000000000'),
                         lambda x: x['cases']['e2-xfs-two']['pins'].update(specSha256='a' * 64),
                         lambda x: x['deployment'].update(helperVersion='0.7.0')]:
            wrong = copy.deepcopy(post)
            mutation(wrong)
            with self.subTest(wrong=wrong), self.assertRaises((ValueError, TypeError, KeyError)):
                audit.validate_contract(wrong)
        with self.assertRaisesRegex(ValueError, 'spec przypadku'), mock.patch.object(audit, 'command') as command:
            audit.audit('e2-ext4-zero', {}, post)
        command.assert_not_called()

    def test_contract_reads_exact_private_bytes_before_validation(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(audit, 'UID', os.getuid()), \
                mock.patch.object(audit, 'safe_parents'):
            path = Path(directory) / 'station.json'
            raw = json.dumps(station([journal()['spec']])).encode()
            path.write_bytes(raw)
            path.chmod(0o600)
            digest = hashlib.sha256(raw).hexdigest()
            before = path.stat()
            self.assertEqual(audit.load_contract(path, digest)['stage'], 'postcreate')
            self.assertEqual(path.stat().st_mtime_ns, before.st_mtime_ns)
            for mode in [0o644, 0o660]:
                path.chmod(mode)
                with self.assertRaises(ValueError):
                    audit.load_contract(path, digest)
            path.chmod(0o600)
            with self.assertRaisesRegex(ValueError, 'SHA kontraktu'):
                audit.load_contract(path, 'a' * 64)
            path.write_bytes(b'{"schema":1,"schema":1}')
            with self.assertRaisesRegex(ValueError, 'Powtórzony'):
                audit.load_contract(path, hashlib.sha256(path.read_bytes()).hexdigest())
            path.write_bytes(raw)
            link = path.with_name('symlink')
            link.symlink_to(path)
            with self.assertRaises(OSError):
                audit.load_contract(link, digest)
            link.unlink()
            os.link(path, link)
            with self.assertRaises(ValueError):
                audit.load_contract(path, digest)

    def test_json_rejects_duplicate_keys(self):
        with self.assertRaises(ValueError):
            audit.decode_json('{"pending": null, "pending": "sync"}')

    def test_blkid_blank_requires_both_streams_empty(self):
        self.assertEqual(audit.parse_blkid(subprocess.CompletedProcess([], 2, '', '')), {})
        for rc, out, err in [(2, '', 'read error'), (8, '', ''), (0, 'UUID=a\nUUID=b\n', ''), (0, '', '')]:
            with self.subTest(rc=rc, out=out, err=err), self.assertRaises(ValueError):
                audit.parse_blkid(subprocess.CompletedProcess([], rc, out, err))
        self.assertEqual(audit.parse_blkid(subprocess.CompletedProcess([], 0, 'TYPE=xfs\nUUID=a\n', '')),
                         {'TYPE': 'xfs', 'UUID': 'a'})

    def test_inventory_exact_six_and_root_ancestry(self):
        disks = [{'type': 'disk', 'serial': serial, 'size': size, 'name': f'/dev/test{i}',
                  'maj:min': f'8:{i}', 'ro': False} for i, (serial, size) in enumerate((d['serial'], d['bytes']) for d in station()['vm']['disks'].values())]
        disks[0]['children'] = [{'type': 'part', 'name': '/dev/test0p1', 'maj:min': '8:10'}]
        whole, nodes = audit.inventory({'blockdevices': disks}, station())
        self.assertEqual((len(whole), len(nodes), nodes[1]['parent']), (6, 7, '/dev/test0'))
        for mutate in [lambda rows: rows.pop(), lambda rows: rows[1].update(serial=rows[0]['serial']),
                       lambda rows: rows[1].update(size=1), lambda rows: rows[2].update(ro=True)]:
            wrong = copy.deepcopy(disks)
            mutate(wrong)
            with self.assertRaises(ValueError):
                audit.inventory({'blockdevices': wrong}, station())

    def test_mountinfo_decodes_component_escapes(self):
        rows = audit.mount_rows('36 25 8:1 / /mnt/a\\040b rw,relatime shared:1 - xfs /dev/vdb rw\n')
        self.assertEqual(rows[0]['target'], '/mnt/a b')
        self.assertEqual(rows[0]['major_minor'], '8:1')
        with self.assertRaises(ValueError):
            audit.mount_rows('brak separatora')

    def test_journal_both_cases_and_partial_is_not_ready(self):
        for name in audit.CASES:
            value = journal(name)
            contract = station([value['spec']])
            self.assertEqual(audit.validate_journal(value, name, contract)['name'], name)
            value.update(stage='needs_attention', pending={'format': {'data': 1}}, formatted=[])
            with self.assertRaises(ValueError):
                audit.validate_journal(value, name, contract)
            self.assertEqual(audit.validate_journal(value, name, contract, require_ready=False)['name'], name)
            value['spec']['data'][0]['serial'] = 'foreign'
            with self.assertRaises(ValueError):
                audit.validate_journal(value, name, contract, require_ready=False)

    def test_metadata_paths_outside_union_and_zero_parity(self):
        _, roles, config, content, parity = audit.expected_paths('e2-xfs-two')
        self.assertEqual(len(content), 3)
        self.assertTrue(parity[1].endswith('/parity/2/snapraid.2-parity'))
        for path in [config] + content + parity:
            self.assertFalse(Path(path).is_relative_to(roles[0][3]))
        self.assertFalse(Path('/mnt/data10/file').is_relative_to('/mnt/data1'))
        self.assertEqual(audit.expected_paths('e2-ext4-zero')[-1], [])

    def test_hash_reads_file_and_rejects_symlink_hardlink(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(audit, 'UID', os.getuid()), mock.patch.object(audit, 'safe_parents'):
            path = Path(directory) / 'content'
            path.write_bytes(b'known bytes')
            path.chmod(0o600)
            before = path.stat()
            result = audit.file_metric(str(path), before.st_dev)
            self.assertEqual(result['bytes'], 11)
            self.assertEqual(path.read_bytes(), b'known bytes')
            self.assertEqual(before.st_mtime_ns, path.stat().st_mtime_ns)
            with self.assertRaises(ValueError):
                audit.file_metric(str(path), before.st_dev + 1)
            link = Path(directory) / 'link'
            link.symlink_to(path)
            with self.assertRaises(OSError):
                audit.open_read(link)
            link.unlink()
            os.link(path, link)
            with self.assertRaises(ValueError):
                audit.open_read(path)

    def test_busy_lock_is_kernel_read_only_refusal(self):
        with tempfile.TemporaryFile() as stream:
            audit.fcntl.flock(stream, audit.fcntl.LOCK_EX | audit.fcntl.LOCK_NB)
            with open(f'/proc/self/fd/{stream.fileno()}', 'rb') as second:
                with self.assertRaises(BlockingIOError):
                    audit.fcntl.flock(second, audit.fcntl.LOCK_SH | audit.fcntl.LOCK_NB)

    def test_command_uses_no_stdin_no_shell_and_bounded_wait(self):
        with mock.patch.object(audit.subprocess, 'run', return_value='result') as run:
            self.assertEqual(audit.command(['/usr/bin/lsblk', '--json']), 'result')
            self.assertEqual(run.call_args.kwargs['stdin'], subprocess.DEVNULL)
            self.assertNotIn('shell', run.call_args.kwargs)
            self.assertEqual(run.call_args.kwargs['timeout'], 20)

    def test_failure_keeps_partial_report_and_exit_one(self):
        def partial(name, report, contract):
            report['journal'] = {'stage': 'needs_attention', 'pending': 'sync'}
            report['violations'].append('brak ready')
            raise ValueError('Brak unii')
        output = io.StringIO()
        with mock.patch.object(audit, 'audit', side_effect=partial), mock.patch.object(audit, 'load_contract', return_value=station()), contextlib.redirect_stdout(output):
            self.assertEqual(audit.main(['e2-xfs-two', '/fixture', 'a' * 64]), 1)
        report = json.loads(output.getvalue())
        self.assertEqual(report['journal']['pending'], 'sync')
        self.assertEqual(report['violations'], ['brak ready'])

    def test_restart_compare_ignores_mutable_metrics_but_not_identity(self):
        before = {'case': 'e2-xfs-two', 'status': 'measured', 'boot_id': 'before', 'stable': {'uuid': 'a'}}
        after = dict(before, boot_id='after', capacity=123)
        self.assertTrue(audit.compare(before, after)['same_checkpoint'])
        with self.assertRaises(ValueError):
            audit.compare(before, before)
        with self.assertRaises(ValueError):
            audit.compare(before, dict(after, stable={'uuid': 'b'}))
        with self.assertRaises(ValueError):
            audit.compare(before, dict(after, status='refused'))


if __name__ == '__main__':
    unittest.main()
