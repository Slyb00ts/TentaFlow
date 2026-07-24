# =============================================================================
# File: server.py — FastAPI facade of the TentaFlow test-runner service.
# Executes agent-authored test items (pytest / Playwright / Locust / httpx)
# in killable subprocesses with a per-run network allowlist. The environment
# secret is held in memory only — never persisted, never logged.
# Example: POST /runs {"run_id":"r1","items":[...],"environment":{...}}
# =============================================================================

from __future__ import annotations

import asyncio
import importlib.metadata
import os
import platform
import sys
import time
from pathlib import Path
from typing import Any, Dict, List, Literal, Optional

from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse
from pydantic import BaseModel, Field

from executor.job import JobManager, artifact_mime

DEFAULT_PORT = 8093
MAX_PARALLEL = int(os.environ.get("TEST_RUNNER_MAX_PARALLEL", "4"))
ITEM_TIMEOUT_SECS = int(os.environ.get("TEST_RUNNER_ITEM_TIMEOUT", "300"))
ISOLATED = os.environ.get("TEST_RUNNER_ISOLATED") == "1"
BUNDLE_DIR = Path(__file__).resolve().parent
WORK_ROOT = Path(
    os.environ.get(
        "TEST_RUNNER_WORK_DIR",
        Path.home() / ".cache" / "tentaflow" / "test-runner" / "runs",
    )
)

FRAMEWORKS = ("pytest", "playwright", "locust", "httpx")


class RunItemModel(BaseModel):
    item_id: str = Field(min_length=1, max_length=128, pattern=r"^[a-zA-Z0-9._-]+$")
    kind: Literal["ui", "api", "security", "perf", "unit"]
    language: str = Field(default="python", max_length=32)
    content: Dict[str, Any] = Field(default_factory=dict)
    config: Dict[str, Any] = Field(default_factory=dict)


class EnvironmentModel(BaseModel):
    base_url: str = Field(default="", max_length=2048)
    auth_type: Literal["none", "bearer", "api_key", "basic"] = "none"
    secret: str = Field(default="", max_length=8192)
    extra_headers: Dict[str, str] = Field(default_factory=dict)
    host_allowlist: List[str] = Field(default_factory=list, max_length=64)


class OptionsModel(BaseModel):
    max_parallel: Optional[int] = Field(default=None, ge=1, le=32)
    item_timeout_secs: Optional[int] = Field(default=None, ge=5, le=7200)


class RunRequest(BaseModel):
    run_id: str = Field(min_length=1, max_length=128)
    items: List[RunItemModel] = Field(min_length=1, max_length=200)
    environment: EnvironmentModel = Field(default_factory=EnvironmentModel)
    options: OptionsModel = Field(default_factory=OptionsModel)


app = FastAPI(title="TentaFlow Test Runner")
_manager: Optional[JobManager] = None


async def ensure_chromium_installed() -> None:
    """Best-effort Playwright browser install for native deployments. The
    docker image ships browsers preinstalled (skip flag). A failure here must
    not kill the service — api/perf/unit kinds work without a browser; ui
    items will fail with a clear launch error instead."""
    if os.environ.get("TEST_RUNNER_SKIP_BROWSER_INSTALL") == "1":
        return
    marker = WORK_ROOT.parent / "chromium-installed.marker"
    if marker.exists():
        return
    marker.parent.mkdir(parents=True, exist_ok=True)
    proc = await asyncio.create_subprocess_exec(
        sys.executable,
        "-m",
        "playwright",
        "install",
        "chromium",
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )
    stdout, _ = await proc.communicate()
    if proc.returncode != 0:
        output = stdout.decode("utf-8", errors="replace")[-2000:]
        print(f"[test-runner] playwright install chromium failed: {output}", file=sys.stderr)
        return
    marker.write_text(str(time.time()), encoding="utf-8")


@app.on_event("startup")
async def startup() -> None:
    global _manager
    await ensure_chromium_installed()
    _manager = JobManager(
        work_root=WORK_ROOT,
        bundle_dir=BUNDLE_DIR,
        default_max_parallel=max(1, min(32, MAX_PARALLEL)),
        default_item_timeout_secs=max(5, min(7200, ITEM_TIMEOUT_SECS)),
        isolated=ISOLATED,
    )


@app.on_event("shutdown")
async def shutdown() -> None:
    if _manager is not None:
        _manager.shutdown()


def _framework_version(name: str) -> str:
    try:
        return importlib.metadata.version(name)
    except importlib.metadata.PackageNotFoundError:
        return "unknown"


@app.get("/health")
async def health() -> Dict[str, Any]:
    return {
        "ok": True,
        "engine": "test-runner",
        "isolated": ISOLATED,
        "toolchains": [
            {
                "language": "python",
                "frameworks": list(FRAMEWORKS),
                "version": platform.python_version(),
            }
        ],
        "framework_versions": {name: _framework_version(name) for name in FRAMEWORKS},
    }


@app.post("/runs")
async def create_run(request: RunRequest) -> Dict[str, str]:
    if _manager is None:
        raise HTTPException(status_code=503, detail="runner is starting up")
    seen = set()
    for item in request.items:
        if item.item_id in seen:
            raise HTTPException(
                status_code=422, detail=f"duplicate item_id '{item.item_id}'"
            )
        seen.add(item.item_id)
    job_id = await asyncio.to_thread(_manager.create, request.model_dump())
    return {"job_id": job_id}


def _get_job(job_id: str):
    if _manager is None:
        raise HTTPException(status_code=503, detail="runner is starting up")
    job = _manager.get(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail="unknown job")
    return job


@app.get("/runs/{job_id}/status")
async def run_status(job_id: str) -> Dict[str, Any]:
    return _get_job(job_id).snapshot()


@app.post("/runs/{job_id}/cancel")
async def run_cancel(job_id: str) -> Dict[str, Any]:
    job = _get_job(job_id)
    await asyncio.to_thread(_manager.cancel, job)
    return {"ok": True, "status": job.snapshot()["status"]}


@app.get("/runs/{job_id}/artifacts/{rel_path:path}")
async def run_artifact(job_id: str, rel_path: str) -> FileResponse:
    job = _get_job(job_id)
    # Path-traversal containment: reject absolute paths and any dot-dot
    # segment up front, then re-verify the fully resolved path (this also
    # catches symlink escapes) against the job directory.
    candidate_rel = Path(rel_path)
    if (
        not rel_path
        or "\x00" in rel_path
        or candidate_rel.is_absolute()
        or any(part in ("..", "") for part in candidate_rel.parts)
    ):
        raise HTTPException(status_code=403, detail="invalid artifact path")
    root = job.workdir.resolve()
    resolved = (root / candidate_rel).resolve()
    if resolved == root or not resolved.is_relative_to(root):
        raise HTTPException(status_code=403, detail="invalid artifact path")
    if not resolved.is_file():
        raise HTTPException(status_code=404, detail="artifact not found")
    return FileResponse(
        path=str(resolved),
        media_type=artifact_mime(resolved),
        filename=resolved.name,
    )


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(app, host="127.0.0.1", port=int(os.environ.get("PORT", DEFAULT_PORT)))
