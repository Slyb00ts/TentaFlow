# =============================================================================
# Plik: test_guest_private_maintenance.py
# Opis: Testy rzeczywistych plików receipt i odmów odbioru prywatnej ochrony SnapRAID.
# Przykład: python3 -m unittest discover -s tests/infra/tentanas-vm -p test_guest_private_maintenance.py
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
import tempfile
import unittest
from unittest.mock import patch
import subprocess
import sys

import guest_private_lifecycle as probe
import test_guest_private_lifecycle as fixture


def run_result(station, phase):
    return {'state': fixture.state_result(station['spec']), 'run': {
        'operation_id': station['maintenance_ids'][phase], 'kind': 'scrub' if phase == 'scrub' else 'sync',
        'started_at': '2026-09-08T20:00:00Z', 'finished_at': '2026-09-08T20:00:01Z',
        'outcome': 'succeeded', 'exit_code': 0, 'total_blocks': None if phase == 'nochange' else 69,
        'checked_blocks': 69 if phase == 'scrub' else None, 'accessed_mb': None if phase == 'nochange' else 18,
        'errors_file': 0, 'errors_io': 0, 'errors_data': 0, 'detail': None}}


def raw_logs(station, phase):
    result = {}
    for label in (('run', 'diff') if phase == 'scrub' else ('run',)):
        command = 'diff' if label == 'diff' else 'scrub' if phase == 'scrub' else 'sync'
        lines = [f'command:{command}', 'conf:file:' + str(probe.p_paths(station)['config']),
                 'blocksize:262144', 'mode:par1']
        if label == 'diff' or phase != 'scrub':
            lines.extend(f'summary:{key}:{1 if key == "added" and phase == "sync" else 0}' for key in
                         ('equal', 'added', 'removed', 'updated', 'moved', 'copied', 'restored'))
            lines.append('summary:exit:' + ('diff' if phase == 'sync' else 'equal'))
        if label != 'diff':
            if phase != 'nochange':
                lines.append('block_count:69')
            if phase == 'scrub':
                lines.append('info_count:69')
            if phase == 'nochange':
                lines.append('msg:status: Nothing to do')
            lines.extend(('summary:error_file:0', 'summary:error_io:0', 'summary:error_data:0', 'summary:exit:ok'))
        result[label + '.log'] = ('\n'.join(lines) + '\n').encode()
        result[label + '.stdout'] = (b'Equal\n' if label == 'diff' else b'Nothing to do\n' if phase == 'nochange'
                                    else b'100% completed, 18 MB accessed\rEverything OK\n')
        result[label + '.stderr'] = b''
    return result


class MaintenanceTests(fixture.PrivateFixture, unittest.TestCase):
    def setUp(self):
        super().setUp()
        self.station = fixture.station('P')
        self.root = self.directory / 'root'
        self.root.mkdir(mode=0o700)
        self.stack.enter_context(patch.object(probe, 'ROOT', self.root))
        self.write('root/.storage.lock', b'')
        self.proof = probe.Proof(self.directory / 'proof', 'a' * 64)

    def receipt_files(self, phase):
        typed = run_result(self.station, phase)
        journal = fixture.journal(self.station)
        journal.update(last_run=typed['run'], sync_completed_at=typed['run']['finished_at'])
        path = self.root / (self.station['spec']['array_id'] + '.json')
        self.write(str(path.relative_to(self.directory)), json.dumps(journal).encode())
        run_root = self.root / (self.station['spec']['array_id'] + '.runs')
        run_root.mkdir(mode=0o700)
        directory = run_root / self.station['maintenance_ids'][phase]
        directory.mkdir(mode=0o700)
        for name, raw in raw_logs(self.station, phase).items():
            self.write(str((directory / name).relative_to(self.directory)), raw)
        return typed, journal, directory

    def test_real_receipt_three_or_six_files_preserves_cr_and_releases_shared_lock(self):
        typed, journal, directory = self.receipt_files('scrub')
        self.proof.begin('scrub')
        actual_read = probe.read_private
        def read(path, *args):
            with (self.root / '.storage.lock').open('rb') as lock:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return actual_read(path, *args)
        with patch.object(probe, 'read_private', side_effect=read):
            receipt = probe.p_receipt(self.station, 'scrub', self.proof)
        self.assertEqual(len(receipt['logs']), 6)
        self.assertEqual(base64.b64decode(receipt['logs']['run.stdout']['base64']),
                         (directory / 'run.stdout').read_bytes())
        self.assertEqual(probe.p_validate_receipt(self.station, 'scrub', receipt, typed, journal), journal)
        with (self.root / '.storage.lock').open('rb') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_nochange_without_progress_or_everything_ok_is_valid(self):
        typed, journal, _ = self.receipt_files('nochange')
        self.proof.begin('nochange')
        receipt = probe.p_receipt(self.station, 'nochange', self.proof)
        self.assertEqual(probe.p_validate_receipt(self.station, 'nochange', receipt, typed, journal), journal)

    def test_missing_or_oversized_log_remains_durable_and_never_validates(self):
        typed, journal, directory = self.receipt_files('sync')
        self.proof.begin('sync')
        with (directory / 'run.stdout').open('ab') as stream:
            stream.write(b'x' * (128 * 1024))
        receipt = probe.p_receipt(self.station, 'sync', self.proof)
        self.assertIn('error', receipt['logs']['run.stdout'])
        self.assertTrue(any('receipt-raw' in name for name in self.proof.state['events']))
        with self.assertRaises(ValueError):
            probe.p_validate_receipt(self.station, 'sync', receipt, typed, journal)

    def test_receipt_refuses_foreign_entries_after_preserving_observation(self):
        _, _, directory = self.receipt_files('sync')
        self.write(str((directory / 'foreign').relative_to(self.directory)), b'not a log')
        self.proof.begin('sync')
        with self.assertRaises(ValueError):
            probe.p_receipt(self.station, 'sync', self.proof)
        raw = json.loads(next(self.proof.path.glob('*receipt-raw.json')).read_bytes())
        self.assertIn('foreign', raw['names'])
        self.assertEqual(self.proof.state['pending'], 'sync')

    def test_receipt_fsync_failure_releases_lock_and_preserves_pending(self):
        self.receipt_files('sync')
        self.proof.begin('sync')
        with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'fsync')):
            with self.assertRaises(OSError):
                probe.p_receipt(self.station, 'sync', self.proof)
        with (self.root / '.storage.lock').open('rb') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertEqual(probe.Proof(self.proof.path, 'a' * 64).state['pending'], 'sync')

    def test_log_and_receipt_contradictions_refuse(self):
        typed, journal, _ = self.receipt_files('sync')
        self.proof.begin('sync')
        original = probe.p_receipt(self.station, 'sync', self.proof)
        changes = (lambda raw: raw.replace(b'summary:error_io:0', b'summary:error_io:1'),
                   lambda raw: raw.replace(b'summary:error_io:0\n', b''),
                   lambda raw: raw + b'summary:exit:ok\n',
                   lambda raw: raw.replace(b'command:sync', b'command:scrub'),
                   lambda raw: raw.replace(b'mode:par1', b'mode:par2'),
                   lambda raw: raw.replace(b'block_count:69', b'block_count:70'),
                   lambda raw: raw + b'msg:error: bad\n',
                   lambda raw: raw + b'summary:unknown:0\n')
        for change in changes:
            changed = copy.deepcopy(original)
            changed['logs']['run.log'] = probe.p_packed(change(base64.b64decode(original['logs']['run.log']['base64'])))
            with self.subTest(change=change), self.assertRaises(ValueError):
                probe.p_validate_receipt(self.station, 'sync', changed, typed, journal)
        for raw in (b'', b'Everything OK\n', b'100% completed, 19 MB accessed\nEverything OK\n'):
            changed = copy.deepcopy(original)
            changed['logs']['run.stdout'] = probe.p_packed(raw)
            with self.subTest(stdout=raw), self.assertRaises(ValueError):
                probe.p_validate_receipt(self.station, 'sync', changed, typed, journal)

    def test_scrub_requires_unchanged_last_sync_and_equal_diff(self):
        typed, journal, _ = self.receipt_files('scrub')
        self.proof.begin('scrub')
        receipt = probe.p_receipt(self.station, 'scrub', self.proof)
        before = {**journal, 'sync_completed_at': '2026-09-08T19:59:59Z'}
        with self.assertRaises(ValueError):
            probe.p_validate_receipt(self.station, 'scrub', receipt, typed, before)
        changed = copy.deepcopy(receipt)
        raw = base64.b64decode(changed['logs']['diff.log']['base64']).replace(b'summary:added:0', b'summary:added:1')
        changed['logs']['diff.log'] = probe.p_packed(raw)
        with self.assertRaises(ValueError):
            probe.p_validate_receipt(self.station, 'scrub', changed, typed, journal)

    def test_sparse_scrub_checked_less_than_total_and_foreign_receipt_refusal(self):
        typed, journal, directory = self.receipt_files('scrub')
        typed['run']['checked_blocks'] = 68
        journal['last_run'] = typed['run']
        path = self.root / (self.station['spec']['array_id'] + '.json')
        path.write_bytes(json.dumps(journal).encode())
        log = directory / 'run.log'
        log.write_bytes(log.read_bytes().replace(b'info_count:69', b'info_count:68'))
        self.proof.begin('scrub')
        receipt = probe.p_receipt(self.station, 'scrub', self.proof)
        self.assertEqual(probe.p_validate_receipt(self.station, 'scrub', receipt, typed, journal), journal)
        for key, value in (('operation_id', self.station['maintenance_ids']['sync']), ('checked_blocks', 70),
                           ('errors_data', None), ('exit_code', False), ('outcome', 'failed')):
            changed = copy.deepcopy(typed)
            changed['run'][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                probe.p_validate_receipt(self.station, 'scrub', receipt, changed, journal)

    def test_failed_journal_and_raw_logs_export_before_success_validation(self):
        typed, journal, directory = self.receipt_files('sync')
        journal.update(stage='needs_attention', pending={'sync': {'operation_id': typed['run']['operation_id']}})
        journal['last_run'].update(outcome='failed', exit_code=1)
        path = self.root / (self.station['spec']['array_id'] + '.json')
        path.write_bytes(json.dumps(journal).encode())
        (directory / 'run.stderr').write_bytes(b'actual failure\r\n')
        self.proof.begin('sync')
        receipt = probe.p_receipt(self.station, 'sync', self.proof)
        self.assertEqual(base64.b64decode(receipt['logs']['run.stderr']['base64']), b'actual failure\r\n')
        self.assertEqual(probe.decode(base64.b64decode(receipt['journal']['base64'])), journal)
        with self.assertRaises(ValueError):
            probe.p_validate_receipt(self.station, 'sync', receipt, typed, journal)
        self.assertEqual(probe.Proof(self.proof.path, 'a' * 64).state['pending'], 'sync')

    def test_nochange_missing_or_conflicting_markers_refuses(self):
        typed, journal, _ = self.receipt_files('nochange')
        self.proof.begin('nochange')
        original = probe.p_receipt(self.station, 'nochange', self.proof)
        for raw in (b'Nothing to do\nEverything OK\n', b'Nothing to do\nNothing to do\n', b'Everything OK\n'):
            receipt = copy.deepcopy(original)
            receipt['logs']['run.stdout'] = probe.p_packed(raw)
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                probe.p_validate_receipt(self.station, 'nochange', receipt, typed, journal)

    def test_actual_files_projected_devices_and_changed_content(self):
        measured = {}
        for key in probe.p_paths(self.station):
            raw = probe.p_config(self.station) if key == 'config' else b'P' * (17 * 1024**2) if key == 'parity' else b'content'
            path = self.write(key, raw)
            measured[key] = probe.file_metric(path, 32 * 1024**2)
            measured[key]['uid'] = 0
            if key == 'config':
                measured[key]['raw'] = base64.b64encode(raw).decode()
        disks = {'parity': {'serial': self.station['spec']['parity'][0]['serial'], 'device': measured['parity']['device']}}
        probe.p_validate_files(self.station, measured, disks, True)
        for key, field, value in (('parity', 'device', 999), ('content_parity', 'sha256', '1' * 64),
                                  ('content_system', 'bytes', 0), ('config', 'uid', 1000),
                                  ('parity', 'nlink', 2), ('config', 'raw', base64.b64encode(b'foreign').decode())):
            changed = copy.deepcopy(measured)
            changed[key][field] = value
            with self.subTest(key=key, field=field), self.assertRaises(ValueError):
                probe.p_validate_files(self.station, changed, disks, True, measured)

    def test_phase_refusal_prevents_all_reads_and_helper_dispatch(self):
        with patch.object(probe, 'invoke_helper') as invoke, patch.object(probe, 'read_namespace', create=True) as read:
            with self.assertRaises(ValueError):
                probe.p_maintenance(self.station, 'sync', self.proof, {}, {}, lambda *args: self.fail('odczyt danych'))
            invoke.assert_not_called()
            read.assert_not_called()

    def test_real_dispatch_sequence_inspect_and_reopen_refusal(self):
        typed, journal, _ = self.receipt_files('sync')
        measured = {}
        for key in probe.p_paths(self.station):
            raw = probe.p_config(self.station) if key == 'config' else b'P' * 4096 if key == 'parity' else b'content'
            measured[key] = probe.file_metric(self.write('metric-' + key, raw))
            measured[key]['uid'] = 0
            if key == 'config':
                measured[key]['raw'] = base64.b64encode(raw).decode()
        disks = {'parity': {'serial': self.station['spec']['parity'][0]['serial'],
                            'device': measured['parity']['device']}}
        self.proof.begin('sync')
        actual_invoke = probe.invoke_helper
        calls = []
        script = ('import sys,json,fcntl; request=json.load(sys.stdin); '
                  'state=json.load(open(sys.argv[1])); '
                  'assert state["pending"]=="sync"; '
                  'assert "sync:"+request["cmd"] in state["dispatched"]; '
                  'lock=open(sys.argv[2],"rb"); fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB); '
                  'reply=json.loads(sys.argv[3]); '
                  'print(json.dumps(reply["state"] if request["cmd"]=="elastic_inspect" else reply))')
        def process_boundary(argv, **kwargs):
            calls.append(json.loads(kwargs['input'])['cmd'])
            return subprocess.run([sys.executable, '-c', script, str(self.proof.path / 'state.json'),
                                   str(self.root / '.storage.lock'), json.dumps(typed)], **kwargs)
        def invoke(station, request, proof):
            return actual_invoke(station, request, proof, run=process_boundary)
        def namespace_read(*args, **kwargs):
            return {'audit': {'journal': journal}, 'value': copy.deepcopy(measured)}
        payload_path = self.write('payload-control', b'unchanged')
        expected = probe.file_metric(payload_path)
        payload_calls = []
        def payload_read(*args):
            payload_calls.append(probe.file_metric(payload_path))
            self.assertEqual(payload_calls[-1], expected)
        with patch.object(probe, 'invoke_helper', side_effect=invoke), \
                patch.object(probe, 'read_namespace', side_effect=namespace_read):
            result = probe.p_maintenance(self.station, 'sync', self.proof, {}, disks, payload_read)
        self.assertEqual(calls, ['elastic_sync', 'elastic_inspect'])
        self.assertEqual(len(payload_calls), 2)
        self.assertEqual(result['run'], typed['run'])
        reopened = probe.Proof(self.proof.path, 'a' * 64)
        for command in ('elastic_sync', 'elastic_inspect'):
            request = {'cmd': command, 'array_id': self.station['spec']['array_id'], 'owner': self.station['spec']['owner']}
            if command == 'elastic_sync':
                request['operation_id'] = self.station['maintenance_ids']['sync']
            with self.subTest(command=command), self.assertRaises(ValueError):
                actual_invoke(self.station, request, reopened,
                              run=lambda *args, **kwargs: self.fail('Drugie wykonanie'))

    def test_inspect_before_mutation_or_foreign_operation_refuses_without_process(self):
        self.proof.begin('sync')
        requests = [
            {'cmd': 'elastic_inspect', 'array_id': self.station['spec']['array_id'], 'owner': self.station['spec']['owner']},
            {'cmd': 'elastic_sync', 'array_id': self.station['spec']['array_id'], 'owner': self.station['spec']['owner'],
             'operation_id': self.station['maintenance_ids']['scrub']}]
        for request in requests:
            with self.subTest(request=request), self.assertRaises(ValueError):
                probe.invoke_helper(self.station, request, self.proof,
                                    run=lambda *args, **kwargs: self.fail('Obcy proces'))
        self.assertEqual(self.proof.state['dispatched'], [])

    def test_mutation_dispatch_fsync_fault_prevents_process(self):
        self.proof.begin('sync')
        request = {'cmd': 'elastic_sync', 'array_id': self.station['spec']['array_id'], 'owner': self.station['spec']['owner'],
                   'operation_id': self.station['maintenance_ids']['sync']}
        with patch.object(probe.os, 'fsync', side_effect=OSError(errno.EIO, 'fsync')):
            with self.assertRaises(OSError):
                probe.invoke_helper(self.station, request, self.proof,
                                    run=lambda *args, **kwargs: self.fail('Proces przed trwałym dowodem'))
        self.assertEqual(probe.Proof(self.proof.path, 'a' * 64).state['pending'], 'sync')


if __name__ == '__main__':
    unittest.main()
