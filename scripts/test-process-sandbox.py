#!/usr/bin/env python3
# ============ File: test-process-sandbox.py — Probe a pinned sandbox runtime against synthetic tenant files. ============

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import socket
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-dir', required=True, type=Path,
                        help='Installed @anthropic-ai/sandbox-runtime package directory')
    parser.add_argument('--report', required=True, type=Path)
    parser.add_argument('--codex', type=Path,
                        help='Optional real CLI executable; only --version is invoked')
    args = parser.parse_args()
    if platform.system() not in ('Darwin', 'Linux'):
        parser.error('This probe requires macOS or Linux; Windows is not validated')
    runtime = args.runtime_dir.resolve(strict=True)
    manifest = json.loads((runtime / 'package.json').read_text())
    if manifest['name'] != '@anthropic-ai/sandbox-runtime' or manifest['version'] != '0.0.75':
        parser.error('This contract probe requires @anthropic-ai/sandbox-runtime 0.0.75')
    node = shutil.which('node')
    if not node:
        parser.error('node is required')
    node = str(Path(node).resolve(strict=True))
    cli = runtime / 'dist/cli.js'
    report = {
        'platform': platform.platform(),
        'runtime_version': manifest['version'],
        'scope': 'Synthetic filesystem/process/network probes; no provider login or model request',
        'checks': [],
    }

    # macOS's default temporary path can exceed the Unix socket path limit
    # once SRT appends its proxy socket name.
    with tempfile.TemporaryDirectory(prefix='tf-srt-', dir='/tmp') as raw:
        root = Path(raw).resolve()
        project = root / 'project a'
        other = root / 'project b'
        private = root / 'profile'
        for directory in (project, other, *(private / name for name in ('tmp', 'config', 'cache', 'codex', 'claude'))):
            directory.mkdir(parents=True)
        (project / 'input.txt').write_text('allowed project\n')
        (other / 'secret.txt').write_text('synthetic other tenant\n')
        (project / 'outside-link').symlink_to(other, target_is_directory=True)
        os.link(other / 'secret.txt', project / 'outside-hardlink')
        config = root / 'policy.json'

        # Only test fixtures and system toolchains are readable. The caller's
        # real HOME, auth caches and TentaFlow database are never granted.
        system_paths = ['/bin', '/sbin', '/usr', '/lib', '/lib64', '/dev', '/etc']
        if platform.system() == 'Darwin':
            system_paths += ['/System', '/Library/Apple', '/private/etc',
                             '/opt/homebrew/Cellar', '/opt/homebrew/opt',
                             '/opt/homebrew/etc/openssl@3/openssl.cnf',
                             '/private/var/select/sh']
        read_paths = [str(Path(p).resolve()) for p in system_paths if Path(p).exists()]
        read_paths += [str(project), str(private), str(Path(node).parent)]
        if args.codex:
            args.codex = args.codex.resolve(strict=True)
            read_paths.append(str(args.codex.parent))
        policy = {
            'filesystem': {
                'denyRead': ['/'], 'allowRead': read_paths,
                'allowWrite': [str(project), str(private)], 'denyWrite': [],
            },
            'network': {'allowedDomains': [], 'deniedDomains': ['*']},
            'allowAppleEvents': False,
            'enableWeakerNestedSandbox': False,
            'enableWeakerNetworkIsolation': False,
        }
        config.write_text(json.dumps(policy))
        env = {
            'PATH': str(Path(node).parent) + ':/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin',
            'HOME': str(private), 'TMPDIR': str(private / 'tmp'),
            'XDG_CONFIG_HOME': str(private / 'config'),
            'XDG_CACHE_HOME': str(private / 'cache'),
            'CODEX_HOME': str(private / 'codex'),
            'CLAUDE_CONFIG_DIR': str(private / 'claude'),
            'LANG': 'en_US.UTF-8',
        }

        def execute(command, sandbox=True):
            argv = [node, str(cli), '--settings', str(config), '--'] + command if sandbox else command
            started = time.monotonic()
            child = subprocess.Popen(argv, cwd=project, env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     text=True, start_new_session=True)
            try:
                stdout, stderr = child.communicate(timeout=20)
                return child.returncode, stdout, stderr, round((time.monotonic() - started) * 1000, 2)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                stdout, stderr = child.communicate()
                return 124, stdout, stderr, round((time.monotonic() - started) * 1000, 2)

        def check(name, command, success, sandbox=True):
            code, stdout, stderr, elapsed = execute(command, sandbox)
            passed = success(code, stdout)
            report['checks'].append({
                'name': name, 'passed': passed, 'exit_code': code,
                'elapsed_ms': elapsed, 'stdout': stdout[-2000:], 'stderr': stderr[-2000:],
            })
            print(('PASS ' if passed else 'FAIL ') + name, flush=True)
            return passed

        can_run = check('allowed_project_read', ['/bin/cat', 'input.txt'],
                        lambda code, out: code == 0 and out == 'allowed project\n')
        if can_run:
            check('allowed_project_write', ['/bin/sh', '-c', 'printf changed > output.txt; cat output.txt'],
                  lambda code, out: code == 0 and out == 'changed')
            check('host_sees_same_file', ['/bin/cat', str(project / 'output.txt')],
                  lambda code, out: code == 0 and out == 'changed', sandbox=False)
            check('private_profile_write', ['/bin/sh', '-c', 'printf private > "$HOME/state"; cat "$HOME/state"'],
                  lambda code, out: code == 0 and out == 'private')
            check('node_reads_allowed_project', [node, '-e',
                  "process.stdout.write(require('fs').readFileSync('input.txt','utf8'))"],
                  lambda code, out: code == 0 and out == 'allowed project\n')

            # A nonzero exit alone is insufficient: the allowed read above
            # proves the runtime launched, and denial must be an OS error.
            read_probe = """const fs = require('fs');
try { fs.readFileSync(process.argv[1]); process.exit(9); }
catch (e) { if (!['EPERM','EACCES','ENOENT'].includes(e.code)) throw e; console.log('denied'); }
"""
            write_probe = """const fs = require('fs');
try { fs.writeFileSync(process.argv[1], 'unwanted'); process.exit(9); }
catch (e) { if (!['EPERM','EACCES','ENOENT','EROFS'].includes(e.code)) throw e; console.log('denied'); }
"""
            denied = lambda code, out: code == 0 and out.strip() == 'denied'
            for name, path in (
                ('other_project_read_denied', other / 'secret.txt'),
                ('parent_traversal_denied', project / '../project b/secret.txt'),
                ('symlink_read_denied', project / 'outside-link/secret.txt'),
                ('hardlink_read_denied', project / 'outside-hardlink'),
            ):
                check(name, [node, '-e', read_probe, str(path)], denied)
            check('other_project_write_denied', [node, '-e', write_probe, str(other / 'new.txt')], denied)
            check('symlink_write_denied', [node, '-e', write_probe, str(project / 'outside-link/new.txt')], denied)
            check('hardlink_write_denied', [node, '-e', write_probe, str(project / 'outside-hardlink')], denied)
            check('child_process_read_denied', [node, '-e',
                  "const c=require('child_process').spawnSync(process.execPath, ['-e',process.argv[1],process.argv[2]], {encoding:'utf8'}); process.stdout.write(c.stdout); process.exit(c.status ?? 8)",
                  read_probe, str(other / 'secret.txt')], denied)

            # The live listener makes ECONNREFUSED a test failure rather than
            # false evidence that a network policy blocked the request.
            with socket.socket() as server:
                server.bind(('127.0.0.1', 0))
                server.listen()
                port = str(server.getsockname()[1])
                network_probe = """const s=require('net').connect(Number(process.argv[1]),'127.0.0.1');
s.on('connect',()=>process.exit(9));
s.on('error',e=>{ if (!['EPERM','EACCES','ENETUNREACH'].includes(e.code)) process.exit(8); console.log('denied'); });
setTimeout(()=>process.exit(7),3000).unref();
"""
                check('direct_host_network_denied', [node, '-e', network_probe, port], denied)

            victim = subprocess.Popen(['/bin/sleep', '30'], env=env, start_new_session=True)
            try:
                check('other_process_signal_denied', [node, '-e',
                      "try {process.kill(Number(process.argv[1]),0); process.exit(9)} catch(e) {if(e.code!=='EPERM' && e.code!=='ESRCH') throw e; console.log('denied')}",
                      str(victim.pid)], denied)
            finally:
                victim.terminate()
                victim.wait(timeout=5)

            if args.codex:
                check('real_codex_starts_without_login', [str(args.codex), '--version'],
                      lambda code, out: code == 0 and 'codex' in out.lower())
            check('baseline_process_start', ['/usr/bin/true'], lambda code, out: code == 0, sandbox=False)
            check('sandbox_process_start', ['/usr/bin/true'], lambda code, out: code == 0)

        report['passed'] = all(item['passed'] for item in report['checks'])
        report['policy'] = policy
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + '\n')
        print('Report: ' + str(args.report), flush=True)
        return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
