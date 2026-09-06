#!/usr/bin/env python3
"""Exercise bridge IPC with an empty account, without starting provider login."""

import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bridge", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    args = parser.parse_args()
    codex = args.codex.resolve(strict=True)
    bridge = args.bridge.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="tf-agent-", dir="/tmp") as directory:
        root = Path(directory).resolve()
        for name in ["home", "tmp", "codex", "claude"]:
            (root / name).mkdir(mode=0o700)
        token = "a" * 64
        (root / "bridge-token").write_text(token)
        (root / "bridge-token").chmod(0o600)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        env = {
            "PATH": f"{codex.parent}:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(root / "home"),
            "TMPDIR": str(root / "tmp"),
            "CODEX_HOME": str(root / "codex"),
            "CLAUDE_CONFIG_DIR": str(root / "claude"),
            "TENTAFLOW_ENGINE_ID": "codex",
            "TENTAFLOW_CODING_AGENT_DATA_DIR": str(root),
            "TENTAFLOW_AGENT_EXECUTION": "process",
            "TENTAFLOW_AGENT_RUNTIME_ROOT": str(codex.parent),
            "TENTAFLOW_AGENT_PROXY_PORT": "9",
            "PORT": str(port),
        }
        process = subprocess.Popen([str(bridge)], env=env, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def request(path, credential=None):
            headers = {"Authorization": f"Bearer {credential}"} if credential else {}
            try:
                with opener.open(urllib.request.Request(f"http://127.0.0.1:{port}{path}",
                                                       headers=headers), timeout=10) as response:
                    return response.status, json.load(response)
            except urllib.error.HTTPError as error:
                return error.code, None

        try:
            deadline = time.monotonic() + 10
            while True:
                if process.poll() is not None:
                    raise RuntimeError(process.stderr.read().decode())
                try:
                    assert request("/health")[0] == 200
                    break
                except urllib.error.URLError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.05)
            assert request("/sessions")[0] == 401
            assert request("/sessions", "incorrect")[0] == 401
            assert request("/sessions", token) == (200, {"sessions": []})
            status, auth = request("/auth/status", token)
            assert status == 200 and auth["authenticated"] is False, (status, auth)
            second = subprocess.run([str(bridge)], env=env, capture_output=True, timeout=10)
            assert second.returncode != 0 and b"already running" in second.stderr
            assert not (root / "codex" / "auth.json").exists()
            print("PASS health, IPC authentication, empty account, exclusive account lease")
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
            process.communicate(timeout=10)


if __name__ == "__main__":
    main()
