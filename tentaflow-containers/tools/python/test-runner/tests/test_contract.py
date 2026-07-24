# =============================================================================
# File: tests/test_contract.py — contract tests of the test-runner service:
# health, an api item against a local mock server, allowlist rejection,
# cancel, artifact path-traversal rejection and secret-never-on-disk.
# Run: pytest tests/test_contract.py (from the bundle directory).
# =============================================================================

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

BUNDLE_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BUNDLE_DIR))

# The manager wipes its work root on startup — keep tests away from any real
# runner state and skip the browser download.
_WORK_DIR = tempfile.mkdtemp(prefix="tf-test-runner-")
os.environ["TEST_RUNNER_WORK_DIR"] = os.path.join(_WORK_DIR, "runs")
os.environ["TEST_RUNNER_SKIP_BROWSER_INSTALL"] = "1"
os.environ.pop("TEST_RUNNER_ISOLATED", None)

from fastapi.testclient import TestClient  # noqa: E402

import server  # noqa: E402

SECRET = "s3cr3t-token-do-not-persist"


class _MockHandler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802 — http.server API
        if self.path == "/public":
            body = b"<html><body>ok</body></html>"
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path != "/ping":
            self.send_response(404)
            self.end_headers()
            return
        if self.headers.get("Authorization") != f"Bearer {SECRET}":
            self.send_response(401)
            self.end_headers()
            return
        body = json.dumps({"ok": True}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):  # keep pytest output clean
        pass


@pytest.fixture(scope="module")
def mock_server():
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), _MockHandler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{httpd.server_port}"
    httpd.shutdown()


@pytest.fixture(scope="module")
def client():
    with TestClient(server.app) as test_client:
        yield test_client
    shutil.rmtree(_WORK_DIR, ignore_errors=True)


def _wait_for(client, job_id, predicate, timeout=120.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        snapshot = client.get(f"/runs/{job_id}/status").json()
        if predicate(snapshot):
            return snapshot
        time.sleep(0.3)
    raise AssertionError(f"condition not reached within {timeout}s: {snapshot}")


def _start_run(client, items, environment, run_id="run-test"):
    response = client.post(
        "/runs",
        json={
            "run_id": run_id,
            "items": items,
            "environment": environment,
            "options": {"max_parallel": 2, "item_timeout_secs": 90},
        },
    )
    assert response.status_code == 200, response.text
    return response.json()["job_id"]


def test_health(client):
    data = client.get("/health").json()
    assert data["ok"] is True
    assert data["isolated"] is False
    toolchain = data["toolchains"][0]
    assert toolchain["language"] == "python"
    assert set(toolchain["frameworks"]) == {"pytest", "playwright", "locust", "httpx"}
    assert toolchain["version"]


def test_api_item_against_mock_server(client, mock_server):
    script = (
        "def test_ping(api_client, base_url):\n"
        "    assert base_url\n"
        "    response = api_client.get('/ping')\n"
        "    assert response.status_code == 200\n"
        "    assert response.json()['ok'] is True\n"
    )
    job_id = _start_run(
        client,
        items=[{"item_id": "api-1", "kind": "api", "content": {"script": script}}],
        environment={
            "base_url": mock_server,
            "auth_type": "bearer",
            "secret": SECRET,
            "host_allowlist": ["127.0.0.1"],
        },
    )
    snapshot = _wait_for(client, job_id, lambda s: s["status"] != "running")
    assert snapshot["status"] == "completed"
    item = snapshot["items"][0]
    assert item["status"] == "passed", item
    assert item["steps"] and item["steps"][0]["name"] == "test_ping"
    assert item["steps"][0]["status"] == "passed"
    assert item["duration_ms"] > 0
    log = next(a for a in item["artifacts"] if a["name"] == "console.log")
    downloaded = client.get(f"/runs/{job_id}/artifacts/{log['rel_path']}")
    assert downloaded.status_code == 200

    # The secret must never touch the job directory — scan every file the
    # runner wrote for this job.
    job_dir = Path(os.environ["TEST_RUNNER_WORK_DIR"]) / job_id
    for path in job_dir.rglob("*"):
        if path.is_file():
            assert SECRET.encode() not in path.read_bytes(), path


def test_host_outside_allowlist_is_blocked(client, mock_server):
    script = (
        "import httpx\n"
        "def test_egress():\n"
        "    httpx.get('http://example.com/', timeout=5)\n"
    )
    job_id = _start_run(
        client,
        items=[{"item_id": "api-block", "kind": "api", "content": {"script": script}}],
        environment={"base_url": mock_server, "host_allowlist": ["127.0.0.1"]},
    )
    snapshot = _wait_for(client, job_id, lambda s: s["status"] != "running")
    item = snapshot["items"][0]
    assert item["status"] == "failed", item
    assert "HostNotAllowed" in item["steps"][0]["message"] or "allowlist" in item["steps"][0]["message"]


def test_empty_allowlist_blocks_everything(client, mock_server):
    script = (
        "import httpx\n"
        "def test_egress(base_url):\n"
        "    httpx.get(base_url + '/ping', timeout=5)\n"
    )
    job_id = _start_run(
        client,
        items=[{"item_id": "api-none", "kind": "api", "content": {"script": script}}],
        environment={"base_url": mock_server, "host_allowlist": []},
    )
    snapshot = _wait_for(client, job_id, lambda s: s["status"] != "running")
    assert snapshot["items"][0]["status"] == "failed", snapshot["items"][0]


def test_unit_kind_blocks_network_even_with_allowlist(client, mock_server):
    script = (
        "import httpx\n"
        "def test_no_net():\n"
        "    httpx.get('http://127.0.0.1/', timeout=5)\n"
    )
    job_id = _start_run(
        client,
        items=[{"item_id": "unit-1", "kind": "unit", "content": {"script": script}}],
        environment={"base_url": mock_server, "host_allowlist": ["127.0.0.1"]},
    )
    snapshot = _wait_for(client, job_id, lambda s: s["status"] != "running")
    assert snapshot["items"][0]["status"] == "failed", snapshot["items"][0]


def test_cancel_kills_running_item(client, mock_server):
    script = (
        "import time\n"
        "def test_slow():\n"
        "    time.sleep(60)\n"
    )
    job_id = _start_run(
        client,
        items=[{"item_id": "slow-1", "kind": "api", "content": {"script": script}}],
        environment={"base_url": mock_server, "host_allowlist": ["127.0.0.1"]},
    )
    _wait_for(
        client,
        job_id,
        lambda s: s["items"][0]["status"] == "running",
        timeout=30.0,
    )
    started = time.monotonic()
    response = client.post(f"/runs/{job_id}/cancel")
    assert response.status_code == 200
    snapshot = _wait_for(client, job_id, lambda s: s["status"] == "cancelled", timeout=30.0)
    assert time.monotonic() - started < 20.0
    item = snapshot["items"][0]
    assert item["status"] == "error"
    assert "cancel" in item["message"]


def test_artifact_path_traversal_rejected(client, mock_server):
    script = "def test_ok():\n    assert True\n"
    job_id = _start_run(
        client,
        items=[{"item_id": "trav-1", "kind": "api", "content": {"script": script}}],
        environment={"base_url": mock_server, "host_allowlist": ["127.0.0.1"]},
    )
    _wait_for(client, job_id, lambda s: s["status"] != "running")
    for evil in (
        "%2e%2e/%2e%2e/etc/passwd",
        "..%2f..%2fetc%2fpasswd",
        "trav-1/artifacts/%2e%2e/%2e%2e/%2e%2e/secret",
    ):
        response = client.get(f"/runs/{job_id}/artifacts/{evil}")
        assert response.status_code in (403, 404), (evil, response.status_code)
        if response.status_code == 404:
            # 404 must come from our handler, not from a file that leaked.
            assert response.json()["detail"] in ("artifact not found", "Not Found")
    # Absolute path form.
    response = client.get(f"/runs/{job_id}/artifacts//etc/passwd")
    assert response.status_code in (403, 404)


def test_perf_item_produces_stats(client, mock_server):
    script = (
        "from locust import HttpUser, task, constant\n"
        "class PingUser(HttpUser):\n"
        "    wait_time = constant(0.2)\n"
        "    @task\n"
        "    def ping(self):\n"
        "        self.client.get('/ping')\n"
    )
    job_id = _start_run(
        client,
        items=[
            {
                "item_id": "perf-1",
                "kind": "perf",
                "content": {
                    "script": script,
                    "profile": {"users": 2, "spawn_rate": 2, "duration_secs": 6},
                },
            }
        ],
        environment={
            "base_url": mock_server,
            "auth_type": "none",
            "host_allowlist": ["127.0.0.1"],
        },
    )
    snapshot = _wait_for(client, job_id, lambda s: s["status"] != "running", timeout=180.0)
    item = snapshot["items"][0]
    # Requests hit the mock without Authorization → 401 → locust counts
    # failures but the run itself completes with real stats.
    assert item["status"] in ("passed", "failed"), item
    summary = snapshot["perf"]["summary"]
    assert summary, snapshot
    aggregated = next(row for row in summary if row["endpoint"] == "Aggregated")
    assert aggregated["requests"] > 0
    assert any(a["kind"] == "perf_stats" for a in item["artifacts"])
    assert snapshot["perf"]["timeline"], snapshot


def _chromium_available() -> bool:
    try:
        from playwright.sync_api import sync_playwright

        with sync_playwright() as pw:
            return Path(pw.chromium.executable_path).exists()
    except Exception:
        return False


@pytest.mark.skipif(
    not _chromium_available(),
    reason="Playwright Chromium is not installed in this environment",
)
def test_ui_item_with_page_fixture(client, mock_server):
    script = (
        "def test_open(page, base_url):\n"
        "    page.goto(base_url + '/public')\n"
        "    assert 'ok' in page.content()\n"
    )
    job_id = _start_run(
        client,
        items=[
            {
                "item_id": "ui-1",
                "kind": "ui",
                "content": {"script": script, "config": {"timeout_ms": 20000}},
            }
        ],
        environment={"base_url": mock_server, "host_allowlist": ["127.0.0.1"]},
    )
    snapshot = _wait_for(client, job_id, lambda s: s["status"] != "running", timeout=180.0)
    item = snapshot["items"][0]
    assert item["status"] == "passed", item


def test_build_profile_blocked_on_native(client):
    job_id = _start_run(
        client,
        items=[
            {
                "item_id": "bp-1",
                "kind": "unit",
                "content": {
                    "script": "",
                    "build_profile": {
                        "install_cmd": "true",
                        "test_cmd": "true",
                        "workdir": "/tmp",
                    },
                },
            }
        ],
        environment={"host_allowlist": []},
    )
    snapshot = _wait_for(client, job_id, lambda s: s["status"] != "running")
    item = snapshot["items"][0]
    assert item["status"] == "blocked"
    assert "isolated" in item["message"]


def test_unknown_language_is_skipped(client):
    job_id = _start_run(
        client,
        items=[
            {
                "item_id": "js-1",
                "kind": "api",
                "language": "javascript",
                "content": {"script": "test('x', () => {})"},
            }
        ],
        environment={"host_allowlist": []},
    )
    snapshot = _wait_for(client, job_id, lambda s: s["status"] != "running")
    item = snapshot["items"][0]
    assert item["status"] == "skipped"
    assert "javascript" in item["message"]


def test_unknown_job_is_404(client):
    assert client.get("/runs/deadbeef/status").status_code == 404
    assert client.post("/runs/deadbeef/cancel").status_code == 404
    assert client.get("/runs/deadbeef/artifacts/x.log").status_code == 404
