#!/usr/bin/env python3
"""Exercise bridge IPC and credential materialization with an empty account.

No provider login is started and no real credential is used: the material below
is a synthetic document in the shape Codex writes, which is enough to test the
route Core materializes through.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_for_health(port, timeout=15, alive=None):
    """Blocks until the bridge serves `/health`, which needs no credential."""
    deadline = time.monotonic() + timeout
    while True:
        if alive is not None and not alive():
            raise RuntimeError("the bridge exited before it served /health")
        try:
            with OPENER.open(f"http://127.0.0.1:{port}/health", timeout=5) as response:
                if response.status == 200:
                    return
        except (urllib.error.URLError, TimeoutError):
            pass
        if time.monotonic() >= deadline:
            raise RuntimeError(f"the bridge did not serve /health on {port} within {timeout}s")
        time.sleep(0.05)


def account_environment(root, codex, port, extra=None):
    environment = {
        "PATH": f"{codex.parent}:/usr/bin:/bin:/usr/sbin:/sbin",
        "HOME": str(root / "home"),
        "TMPDIR": str(root / "tmp"),
        "XDG_DATA_HOME": str(root / "data"),
        "TENTAFLOW_ENGINE_ID": "codex",
        "TENTAFLOW_CODING_AGENT_DATA_DIR": str(root),
        "TENTAFLOW_AGENT_EXECUTION": "process",
        "TENTAFLOW_AGENT_RUNTIME_ROOT": str(codex.parent),
        "TENTAFLOW_AGENT_PROXY_PORT": "9",
        "PORT": str(port),
    }
    environment.update(extra or {})
    return environment


def account_directory(directory, token):
    root = Path(directory).resolve()
    # The same private directories the deploy gives a bridge process: no engine
    # credential home among them, because the bridge points every invocation at
    # its own login directory or at a session profile.
    for name in ["home", "tmp", "data"]:
        (root / name).mkdir(mode=0o700)
    (root / "bridge-token").write_text(token)
    (root / "bridge-token").chmod(0o600)
    return root


def egress_socket(root):
    """A Linux sandbox has no route to the host, so the bridge requires the unix
    socket Core opens for the in-sandbox forwarder and refuses to run a CLI
    without one. A listening socket is all this probe owes it: nothing here
    expects provider traffic to succeed, and a bound-but-unaccepted socket is
    what an unreachable provider looks like from inside."""
    if sys.platform != "linux":
        return None, None
    path = root / "egress.sock"
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(path))
    listener.listen(16)
    return {"TENTAFLOW_AGENT_PROXY_SOCKET": str(path)}, listener


# What the parent does is all that matters here: it holds the write end of the
# bridge's stdin and nothing else. Killing it is the only thing this stand-in
# has to do faithfully, and SIGKILL is what it must survive being given.
PARENT_SOURCE = """
import json, subprocess, sys, time
binary, environment, log = sys.argv[1], json.loads(sys.argv[2]), sys.argv[3]
with open(log, "wb") as output:
    child = subprocess.Popen([binary], env=environment, stdin=subprocess.PIPE,
                             stdout=output, stderr=output, start_new_session=True)
print(child.pid, flush=True)
time.sleep(600)
"""


def check_bridge_does_not_outlive_its_parent(bridge, codex):
    """A bridge holds the account's exclusive lock, so an orphan makes the
    account unstartable on this node until somebody finds the process. Core
    stops its bridges on the way out; this checks the case it cannot cover — a
    Core killed outright, running no shutdown path at all."""
    with tempfile.TemporaryDirectory(prefix="tf-agent-orphan-", dir="/tmp") as directory:
        root = account_directory(directory, "b" * 64)
        port = free_port()
        egress, listener = egress_socket(root)
        environment = account_environment(root, codex, port,
                                          {"TENTAFLOW_BRIDGE_PARENT_PIPE": "1", **(egress or {})})
        log = root / "bridge.log"
        parent = subprocess.Popen(
            [sys.executable, "-c", PARENT_SOURCE, str(bridge), json.dumps(environment), str(log)],
            stdout=subprocess.PIPE, start_new_session=True)
        child = None
        try:
            child = int(parent.stdout.readline())
            wait_for_health(port, alive=lambda: parent.poll() is None)
            # A living parent keeps the bridge serving; the pipe is a liveness
            # token, not a channel, and nothing is ever written on it.
            time.sleep(1)
            os.kill(child, 0)

            os.kill(parent.pid, signal.SIGKILL)
            deadline = time.monotonic() + 15
            while True:
                try:
                    os.kill(child, 0)
                except ProcessLookupError:
                    break
                if time.monotonic() >= deadline:
                    raise AssertionError(
                        f"bridge {child} outlived its killed parent and still holds account.lock; "
                        f"bridge log: {log.read_text()[-2000:]}")
                time.sleep(0.1)
        finally:
            if parent.poll() is None:
                parent.kill()
            parent.communicate(timeout=10)
            if child is not None:
                try:
                    os.kill(child, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if listener is not None:
                listener.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bridge", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    args = parser.parse_args()
    codex = args.codex.resolve(strict=True)
    bridge = args.bridge.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="tf-agent-", dir="/tmp") as directory:
        token = "a" * 64
        root = account_directory(directory, token)
        port = free_port()
        egress, listener = egress_socket(root)
        env = account_environment(root, codex, port, egress)
        process = subprocess.Popen([str(bridge)], env=env, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True)
        opener = OPENER

        def request(path, credential=None, method=None, body=None):
            headers = {"Authorization": f"Bearer {credential}"} if credential else {}
            payload = None
            if body is not None:
                payload = json.dumps(body).encode()
                headers["Content-Type"] = "application/json"
            call = urllib.request.Request(f"http://127.0.0.1:{port}{path}", headers=headers,
                                          data=payload, method=method)
            try:
                with opener.open(call, timeout=10) as response:
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
            canonical = root / "credentials" / "codex" / "auth.json"
            assert not canonical.exists()
            assert request("/account/credential", token) == (
                200, {"present": False, "engine": "codex"})

            # Materialization: Core hands the account's credential to the bridge,
            # which is the only writer of the canonical file. What comes back has
            # to be the same bytes, the same digest, and the provider identity the
            # material names — that triple is the whole Core-side contract.
            material = json.dumps({"tokens": {"account_id": "acct-42",
                                              "access_token": "at", "refresh_token": "rt"}})
            status, written = request("/account/credential", token, "PUT",
                                      {"material": material})
            assert status == 200 and written["applied"] is True, (status, written)
            assert canonical.exists() and canonical.stat().st_mode & 0o077 == 0
            status, held = request("/account/credential", token)
            assert status == 200, status
            assert held["present"] is True and held["material"] == material, held
            assert held["sha256"] == written["sha256"] == hashlib.sha256(
                material.encode()).hexdigest(), held
            assert held["identity"] == "account:acct-42", held

            # The same material again is not a rotation, and a document that
            # authenticates nobody never replaces a working credential.
            assert request("/account/credential", token, "PUT", {"material": material}) == (
                200, {"applied": False, "sha256": held["sha256"]})
            assert request("/account/credential", token, "PUT", {"material": "{}"})[0] == 400
            assert request("/account/credential", token, "PUT", {"material": "not json"})[0] == 400
            assert request("/account/credential", None, "PUT", {"material": material})[0] == 401
            assert json.loads(canonical.read_text()) == json.loads(material)

            # The login home is ONE directory for the whole account. While a
            # sign-in owns it, no probe may materialize into it: the CLI at the
            # provider is writing the very file a probe would overwrite. A read
            # that can be answered from cache still is; one that would have to
            # ask the CLI is refused by name.
            status, started = request("/auth/start", token, "POST")
            if status == 200:
                flow = started["flow_id"]
                assert request("/models?refresh=1", token)[0] == 400
                assert request("/usage?refresh=1", token)[0] == 400
                assert request("/account/credential", token, "PUT",
                               {"material": material})[0] == 400
                status, during = request("/auth/status", token)
                assert status == 200 and during["status"] == "authenticating", during
                assert request(f"/sessions/{flow}", token, "DELETE")[0] == 200
                assert json.loads(canonical.read_text()) == json.loads(material), (
                    "an unfinished sign-in replaced the account's credential")
                login_home = "a sign-in owns the login home"
            else:
                login_home = (f"login-home exclusion NOT EXERCISED: /auth/start answered "
                              f"{status} on this host")

            check_bridge_does_not_outlive_its_parent(bridge, codex)
            print("PASS health, IPC authentication, empty account, one bridge per account, "
                  "credential materialization and read-back, a bridge does not outlive its "
                  f"parent; {login_home}")
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
            process.communicate(timeout=10)
            if listener is not None:
                listener.close()


if __name__ == "__main__":
    main()
