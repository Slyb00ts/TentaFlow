# =============================================================================
# Plik: test_guest_e2_snapraid.py
# Opis: Lokalne testy faz E2, trwałych plików, parserów i dziedziczenia locka.
# Przykład: python3 -B -m unittest discover -s tests/infra/tentanas-vm -p test_guest_e2_snapraid.py
# =============================================================================

import contextlib
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock

import guest_e2_snapraid as harness
import guest_storage as storage


def small_file(path, limit=128 * 1024, mode=None):
    return Path(path).read_bytes()


class PayloadTests(unittest.TestCase):
    def test_complete_writes_handle_short_write_and_refuse_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'data'
            fd = harness.exclusive(path)
            real_write = os.write
            try:
                with mock.patch.object(harness.os, 'write', side_effect=lambda fd, data: real_write(fd, data[:2])):
                    harness.write_all(fd, b'1234567')
                self.assertEqual(path.read_bytes(), b'1234567')
                with mock.patch.object(harness.os, 'write', return_value=0), self.assertRaises(ValueError):
                    harness.write_all(fd, b'no')
            finally:
                os.close(fd)

    def test_writer_exclusive_symlink_and_durable_order(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            target = base / 'target'
            target.write_text('unchanged')
            link = base / 'link'
            link.symlink_to(target)
            with self.assertRaises(FileExistsError):
                harness.write_json(link, {'no': 'overwrite'}, storage)
            self.assertEqual(target.read_text(), 'unchanged')
            events = []
            sync = os.fsync
            with mock.patch.object(harness.os, 'fsync', side_effect=lambda fd: (events.append('fsync'), sync(fd))[1]), \
                    mock.patch.object(storage, 'flush_directory', side_effect=lambda path: events.append('directory')):
                harness.write_json(base / 'state', {'pending': 'corpus'}, storage)
            self.assertEqual(events, ['fsync', 'directory'])
            self.assertEqual(json.loads((base / 'state').read_text()), {'pending': 'corpus'})

    def test_pending_state_survives_reopen_and_blocks_protect(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(harness, 'BASE', Path(directory)):
            base = Path(directory)
            (base / 'logs').mkdir(mode=0o700)
            state = {'schema': 1, 'stage': 'protect', 'pending': '02-sync'}
            harness.persist(state, storage)
            audit = types.SimpleNamespace(small_file=small_file, decode_json=json.loads)
            with mock.patch.object(harness, 'guard', return_value={}), mock.patch.object(storage, 'private'), \
                    mock.patch.object(harness, 'run_snap') as tool, self.assertRaisesRegex(ValueError, 'brak retry'):
                harness.execute('protect', audit, storage, mock.Mock())
            tool.assert_not_called()
            self.assertEqual(json.loads((base / 'state.json').read_text()), state)

    def test_persist_failure_before_corpus_mkdir_and_before_tool(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory) / 'state'
            union = Path(directory) / 'union'
            union.mkdir()
            measured = {'devices': {str(harness.DATA): 1}, 'union_device': 2, 'binary_sha256': 'binary', 'files': ['config']}
            audit = types.SimpleNamespace(safe_parents=lambda path: None)
            real_stat = os.stat
            with mock.patch.object(harness, 'BASE', base), mock.patch.object(harness, 'UNION', union), \
                    mock.patch.object(harness, 'guard', return_value=measured), mock.patch.object(harness, 'entries', return_value=[]), \
                    mock.patch.object(harness, 'empty_checkpoint'), \
                    mock.patch.object(harness.os, 'stat', side_effect=lambda path, *args, **kwargs: real_stat(base.parent if str(path) == '/' else path, *args, **kwargs)), \
                    mock.patch.object(storage, 'private'), mock.patch.object(harness, 'persist', side_effect=OSError('fsync refused')), \
                    self.assertRaises(OSError):
                harness.execute('corpus', audit, storage, mock.Mock())
            self.assertFalse((union / harness.FOLDER).exists())
            with mock.patch.object(harness, 'guard', return_value=measured), mock.patch.object(harness, 'corpus_metrics', return_value={}), \
                    mock.patch.object(harness, 'persist', side_effect=OSError('write refused')), \
                    mock.patch.object(harness.subprocess, 'run') as run, self.assertRaises(OSError):
                harness.run_snap(audit, storage, mock.Mock(), {'initial': measured, 'original': {}}, '02-sync', ['sync'], 0)
            run.assert_not_called()

    def test_corpus_protect_verify_real_files_and_tool_log_boundaries(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            base, union = parent / 'state', parent / 'union'
            union.mkdir()
            paths = [parent / name for name in ['config', 'content0', 'content1', 'content2', 'parity1', 'parity2']]
            for path in paths:
                path.write_bytes(b'')
            root_dev = parent.stat().st_dev
            def metric(path, device):
                path = Path(path)
                info = path.stat()
                return {'present': True, 'path': str(path), 'bytes': info.st_size, 'allocated': info.st_blocks * 512,
                        'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'inode': info.st_ino, 'device': info.st_dev}
            def guard(audit):
                return {'devices': {str(union): root_dev}, 'union_device': root_dev, 'binary_sha256': 'pinned',
                        'config': str(paths[0]), 'files': [metric(path, root_dev) for path in paths]}
            audit = types.SimpleNamespace(safe_parents=lambda path: None, file_metric=metric,
                                          small_file=small_file, decode_json=json.loads)
            calls = []
            def run(args, **kwargs):
                label = ['01-diff', '02-sync', '03-diff', '04-check', '05-scrub'][len(calls)]
                calls.append(args)
                log = 'blocksize:262144\n'
                if label in ('01-diff', '02-sync', '03-diff'):
                    adding = label != '03-diff'
                    if adding:
                        log += ''.join(f'scan:add:d1:{harness.FOLDER}/{name}\n' for name in harness.FILES)
                    for key, value in {'equal': 0 if adding else 2, 'added': 2 if adding else 0,
                                       'removed': 0, 'updated': 0, 'moved': 0, 'copied': 0, 'restored': 0,
                                       'exit': 'diff' if adding else 'equal'}.items():
                        log += f'summary:{key}:{value}\n'
                if label in ('02-sync', '05-scrub'):
                    if label == '05-scrub':
                        log += 'block_count:68\n'
                    log += 'summary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n'
                    for path in paths[1:4]:
                        path.write_bytes(b'new-content-' + label.encode())
                    if label == '02-sync':
                        for path in paths[4:]:
                            path.write_bytes(b'parity-data')
                if label == '04-check':
                    log += 'summary:error:0\nsummary:error_unrecoverable:0\nsummary:exit:ok\n'
                os.write(kwargs['pass_fds'][1], log.encode())
                os.write(kwargs['stdout'], b'100% completed, 17 MB accessed\nEverything OK\n')
                return subprocess.CompletedProcess(args, 2 if label == '01-diff' else 0)
            real_stat = os.stat
            with mock.patch.object(harness, 'BASE', base), mock.patch.object(harness, 'UNION', union), mock.patch.object(harness, 'DATA', union), \
                    mock.patch.object(harness, 'FILES', {'restore.bin': 1024**2, 'control.bin': 1024**2}), \
                    mock.patch.object(harness, 'guard', side_effect=guard), mock.patch.object(harness, 'empty_checkpoint'), \
                    mock.patch.object(storage, 'private'), mock.patch.object(harness.subprocess, 'run', side_effect=run), \
                    mock.patch.object(harness.os, 'stat', side_effect=lambda path, *args, **kwargs: real_stat(parent if str(path) == '/' else path, *args, **kwargs)), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(harness.execute('preflight', audit, storage, mock.Mock())['status'], 'ready')
                corpus = harness.execute('corpus', audit, storage, mock.Mock())
                original = (base / 'original.json').read_bytes()
                self.assertEqual(corpus['status'], 'corpus_done')
                self.assertEqual(set(corpus['original']['restore.bin']), {'sha256', 'bytes', 'inode', 'device', 'mtime_ns'})
                self.assertEqual(harness.execute('protect', audit, storage, mock.Mock())['status'], 'protected')
                before = (base / 'state.json').read_bytes()
                self.assertEqual(harness.execute('verify', audit, storage, mock.Mock())['status'], 'verified')
                self.assertEqual((base / 'state.json').read_bytes(), before)
                self.assertEqual((base / 'original.json').read_bytes(), original)
                for phase in ('corpus', 'protect'):
                    with self.assertRaises(ValueError):
                        harness.execute(phase, audit, storage, mock.Mock())
                self.assertEqual(len(calls), 5)
                self.assertEqual(sum(args[-1] == 'sync' for args in calls), 1)

    def test_diff_parser_exact_own_two_files(self):
        additions = ''.join(f'scan:add:d1:{harness.FOLDER}/{name}\n' for name in harness.FILES)
        log = 'blocksize:262144\n' + additions + ''.join(f'summary:{key}:{value}\n' for key, value in {
            'equal': 0, 'added': 2, 'removed': 0, 'updated': 0, 'moved': 0, 'copied': 0, 'restored': 0, 'exit': 'diff'}.items())
        harness.validate_log('01-diff', '', log, storage)
        for bad in [log.replace('restore.bin', 'foreign.bin'), log.replace('summary:removed:0', 'summary:removed:1'),
                    log + 'summary:exit:diff\n', log.replace('262144', '524288')]:
            with self.assertRaises(ValueError):
                harness.validate_log('01-diff', '', bad, storage)

    def test_full_scrub_requires_all_68_blocks_and_clean_summary(self):
        stdout = '100% completed, 17 MB accessed\nEverything OK\n'
        log = 'blocksize:262144\nblock_count:68\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n'
        harness.validate_log('05-scrub', stdout, log, storage)
        for bad in [log.replace('block_count:68', 'block_count:67'), log.replace('error_data:0', 'error_data:1'),
                    log.replace('block_count:68', 'block_count:268'), log + 'block_count:68\n']:
            with self.assertRaises((ValueError, RuntimeError)):
                harness.validate_log('05-scrub', stdout, bad, storage)
        with self.assertRaises(RuntimeError):
            harness.validate_log('05-scrub', 'Everything OK', log, storage)

    def test_scrub_real_carriage_return_progress_is_a_complete_line(self):
        raw = (b'Scrubbing...\n1%, 0 MB          \r100% completed, 18 MB accessed in 0:00    \n'
               b'\nEverything OK\nSaving state to /etc/tentanas/e2-xfs-two-snapraid.content...\n')
        log = ('blocksize:262144\nblock_count:68\nsummary:error_file:0\n'
               'summary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / '05-scrub.stdout'
            path.write_bytes(raw)
            stdout = path.read_bytes().decode()
            self.assertIn('\r100%', stdout)
            harness.validate_log('05-scrub', stdout, log, storage)
            harness.validate_log('05-scrub', stdout.replace('\n', '\r\n'), log.replace('\n', '\r\n'), storage)
            for bad in (stdout.replace('100% completed', '99% completed'), stdout.replace('18 MB accessed', '0 MB accessed')):
                with self.assertRaises(RuntimeError):
                    harness.validate_log('05-scrub', bad, log, storage)
            with self.assertRaises(ValueError):
                harness.validate_log('05-scrub', stdout, log.replace('error_io:0', 'error_io:1'), storage)

    def test_sync_real_scan_diff_and_final_ok_are_both_required(self):
        log = ('blocksize:262144\nscan:add:d1:tentanas-e2-corpus/restore.bin\n'
               'scan:add:d1:tentanas-e2-corpus/control.bin\n'
               'summary:equal:0\nsummary:added:2\nsummary:removed:0\nsummary:updated:0\n'
               'summary:moved:0\nsummary:copied:0\nsummary:restored:0\nsummary:exit:diff\n'
               'msg:progress: Syncing...\nsummary:error_file:0\nsummary:error_io:0\n'
               'summary:error_data:0\nsummary:exit:ok\n')
        harness.validate_log('02-sync', '', log, storage)
        for bad in [log.replace('summary:exit:diff\n', ''), log.replace('error_io:0', 'error_io:1'),
                    log.replace('control.bin', 'foreign.bin'), log + 'scan:remove:d1:foreign\n']:
            with self.assertRaises(ValueError):
                harness.validate_log('02-sync', '', bad, storage)

    def test_unchanged_diff_uses_real_equal_tag_not_ok(self):
        log = ('blocksize:262144\nsummary:equal:2\nsummary:added:0\nsummary:removed:0\n'
               'summary:updated:0\nsummary:moved:0\nsummary:copied:0\nsummary:restored:0\nsummary:exit:equal\n')
        harness.validate_log('03-diff', '', log, storage)
        with self.assertRaises(ValueError):
            harness.validate_log('03-diff', '', log.replace('exit:equal', 'exit:ok'), storage)

    def test_check_needs_full_read_and_not_just_exit_zero(self):
        log = 'blocksize:262144\nsummary:error:0\nsummary:error_unrecoverable:0\nsummary:exit:ok\n'
        harness.validate_log('04-check', '100% completed, 17 MB accessed', log, storage)
        for stdout, bad in [('', log), ('100% completed, 17 MB accessed', log.replace('error:0', 'error:1'))]:
            with self.assertRaises(ValueError):
                harness.validate_log('04-check', stdout, bad, storage)

    def test_log_and_lock_fds_really_reach_child_and_command_is_single(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(harness, 'BASE', Path(directory)):
            base = Path(directory)
            (base / 'logs').mkdir()
            (base / 'lock').touch()
            lock = (base / 'lock').open('rb')
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            measured = {'binary_sha256': 'binary', 'files': ['config'], 'config': '/etc/tentanas/snapraid-e2-xfs-two.conf'}
            state = {'initial': measured, 'original': {}}
            audit = types.SimpleNamespace(small_file=small_file)
            real_run = subprocess.run
            calls = []
            def run(args, **kwargs):
                calls.append((args, kwargs))
                lock_fd, log_fd = kwargs['pass_fds']
                code = ('import os,fcntl,sys\n'
                        'os.fstat(int(sys.argv[1]))\n'
                        'other=os.open(sys.argv[3],os.O_RDONLY)\n'
                        'try:\n fcntl.flock(other,fcntl.LOCK_EX|fcntl.LOCK_NB)\n raise RuntimeError("lock missing")\n'
                        'except BlockingIOError: pass\n'
                        'with open(sys.argv[2],"wb") as log: log.write(b"child-log")\n')
                return real_run([sys.executable, '-c', code, str(lock_fd), args[4], str(base / 'lock')], **kwargs)
            try:
                with mock.patch.object(harness, 'guard', return_value=measured), mock.patch.object(harness, 'corpus_metrics', return_value={}), \
                        mock.patch.object(harness, 'persist') as persist, mock.patch.object(harness, 'validate_log'), \
                        mock.patch.object(harness.subprocess, 'run', side_effect=run), contextlib.redirect_stdout(io.StringIO()):
                    harness.run_snap(audit, storage, lock, state, '02-sync', ['sync'], 0)
                persist.assert_called_once()
                self.assertEqual(len(calls), 1)
                args, kwargs = calls[0]
                self.assertEqual(args[:4], ['/usr/bin/snapraid', '-c', measured['config'], '-l'])
                self.assertEqual(args[-1], 'sync')
                self.assertEqual(args[4], '/proc/self/fd/' + str(kwargs['pass_fds'][1]))
                self.assertEqual(kwargs['cwd'], '/')
                self.assertEqual((base / 'logs/02-sync.log').read_text(), 'child-log')
            finally:
                lock.close()

    def test_child_keeps_lock_when_parent_descriptor_is_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'lock'
            path.touch()
            holder = path.open('rb')
            fcntl.flock(holder, fcntl.LOCK_EX | fcntl.LOCK_NB)
            child = subprocess.Popen([sys.executable, '-c', 'import sys; print("ready",flush=True); sys.stdin.read(1)'],
                                     pass_fds=(holder.fileno(),), stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            try:
                self.assertEqual(child.stdout.readline().strip(), 'ready')
                holder.close()
                with path.open('rb') as contender:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
                child.communicate('x', timeout=5)
                with path.open('rb') as contender:
                    fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                holder.close()
                if child.poll() is None:
                    child.kill()
                    child.communicate()

    def test_timeout_preserves_pending_and_never_retries(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(harness, 'BASE', Path(directory)):
            base = Path(directory)
            (base / 'logs').mkdir()
            measured = {'binary_sha256': 'binary', 'files': ['config'], 'config': '/fixed/config'}
            state = {'initial': measured, 'original': {}}
            with mock.patch.object(harness, 'guard', return_value=measured), mock.patch.object(harness, 'corpus_metrics', return_value={}), \
                    mock.patch.object(harness.subprocess, 'run', side_effect=subprocess.TimeoutExpired('snapraid', 300)) as run, \
                    mock.patch.object(harness.os, 'fsync', wraps=os.fsync) as sync, \
                    self.assertRaises(subprocess.TimeoutExpired):
                harness.run_snap(mock.Mock(), storage, mock.Mock(), state, '02-sync', ['sync'], 0)
            self.assertEqual(json.loads((base / 'state.json').read_text())['pending'], '02-sync')
            run.assert_called_once()
            self.assertGreaterEqual(sync.call_count, 6)

    def test_no_mutation_on_foreign_phase_or_failed_guard(self):
        with mock.patch.object(harness, 'load_fixture') as load, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(harness.main(['corrupt']), 1)
            load.assert_not_called()
        for phase in harness.PHASES:
            with mock.patch.object(harness, 'guard', side_effect=ValueError('identity mismatch')), \
                    mock.patch.object(harness, 'persist') as persist, mock.patch.object(harness.subprocess, 'run') as run, \
                    self.assertRaises(ValueError):
                harness.execute(phase, mock.Mock(), storage, mock.Mock())
            persist.assert_not_called()
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
