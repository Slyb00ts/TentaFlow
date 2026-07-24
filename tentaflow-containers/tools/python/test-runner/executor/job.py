# =============================================================================
# File: executor/job.py — in-memory job manager: item queue with bounded
# parallelism, per-item timeouts, process-group cancel, artifact collection
# and thread-safe status snapshots. The environment secret lives ONLY in this
# process's memory and in the env of spawned test subprocesses — it is never
# written to disk or logged.
# =============================================================================

from __future__ import annotations

import json
import os
import shutil
import subprocess
import threading
import time
import uuid
from collections import OrderedDict
from dataclasses import dataclass, field
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional

from . import build_profile, run_perf, run_pytest, run_ui
from .junit import ItemOutcome, StepState, SubprocessResult

MAX_SCRIPT_CHARS = 262_144
FINISHED_JOB_TTL_SECS = 24 * 3600
MAX_RETAINED_JOBS = 100
CANCEL_MESSAGE = "cancelled"

ARTIFACT_KIND_BY_SUFFIX = {
    ".png": "screenshot",
    ".jpg": "screenshot",
    ".jpeg": "screenshot",
    ".zip": "trace",
    ".log": "log",
    ".txt": "log",
    ".csv": "perf_stats",
    ".xml": "junit",
    ".har": "har",
}

MIME_BY_SUFFIX = {
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".zip": "application/zip",
    ".log": "text/plain",
    ".txt": "text/plain",
    ".csv": "text/csv",
    ".xml": "application/xml",
    ".json": "application/json",
    ".har": "application/json",
}


def artifact_kind(path: Path) -> str:
    if path.name.endswith(".trace.zip"):
        return "trace"
    return ARTIFACT_KIND_BY_SUFFIX.get(path.suffix.lower(), "other")


def artifact_mime(path: Path) -> str:
    return MIME_BY_SUFFIX.get(path.suffix.lower(), "application/octet-stream")


@dataclass
class ArtifactState:
    name: str
    kind: str
    rel_path: str
    size_bytes: int
    mime: str

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "kind": self.kind,
            "rel_path": self.rel_path,
            "size_bytes": self.size_bytes,
            "mime": self.mime,
        }


@dataclass
class ItemState:
    item_id: str
    kind: str
    language: str
    content: Dict[str, Any]
    config: Dict[str, Any]
    status: str = "pending"
    duration_ms: int = 0
    message: str = ""
    steps: List[StepState] = field(default_factory=list)
    artifacts: List[ArtifactState] = field(default_factory=list)

    def to_dict(self) -> dict:
        return {
            "item_id": self.item_id,
            "kind": self.kind,
            "status": self.status,
            "duration_ms": self.duration_ms,
            "message": self.message,
            "steps": [step.to_dict() for step in self.steps],
            "artifacts": [artifact.to_dict() for artifact in self.artifacts],
        }


@dataclass
class ItemContext:
    item_id: str
    kind: str
    content: Dict[str, Any]
    config: Dict[str, Any]
    workdir: Path
    artifacts_dir: Path
    env: Dict[str, str]
    timeout_secs: int
    isolated: bool
    run: Callable[..., SubprocessResult]


class Job:
    def __init__(
        self,
        job_id: str,
        run_id: str,
        items: "OrderedDict[str, ItemState]",
        environment: Dict[str, Any],
        max_parallel: int,
        item_timeout_secs: int,
        workdir: Path,
    ) -> None:
        self.job_id = job_id
        self.run_id = run_id
        self.items = items
        self.environment = environment
        self.max_parallel = max_parallel
        self.item_timeout_secs = item_timeout_secs
        self.workdir = workdir
        self.status = "running"
        self.created_at = time.time()
        self.finished_at: Optional[float] = None
        self.lock = threading.Lock()
        self.cancel_event = threading.Event()
        self.procs: Dict[str, subprocess.Popen] = {}
        self.perf_summary: List[dict] = []
        self.perf_timeline: List[dict] = []

    def snapshot(self) -> dict:
        with self.lock:
            return {
                "job_id": self.job_id,
                "run_id": self.run_id,
                "status": self.status,
                "items": [item.to_dict() for item in self.items.values()],
                "perf": {
                    "summary": list(self.perf_summary),
                    "timeline": list(self.perf_timeline),
                },
            }


def _kill_proc_tree(proc: subprocess.Popen) -> None:
    """Kills the whole process group so pytest/locust/browser children die too."""
    if os.name == "posix":
        try:
            os.killpg(os.getpgid(proc.pid), 9)
            return
        except (ProcessLookupError, PermissionError, OSError):
            pass
        try:
            proc.kill()
        except OSError:
            pass
    else:
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(proc.pid)],
            capture_output=True,
            check=False,
        )


class JobManager:
    def __init__(
        self,
        work_root: Path,
        bundle_dir: Path,
        default_max_parallel: int,
        default_item_timeout_secs: int,
        isolated: bool,
    ) -> None:
        self.work_root = work_root
        self.bundle_dir = bundle_dir
        self.default_max_parallel = default_max_parallel
        self.default_item_timeout_secs = default_item_timeout_secs
        self.isolated = isolated
        self.jobs: Dict[str, Job] = {}
        self.lock = threading.Lock()
        # Jobs are memory-resident only, so directories left by a previous
        # process are unreachable garbage — start from a clean root.
        shutil.rmtree(self.work_root, ignore_errors=True)
        self.work_root.mkdir(parents=True, exist_ok=True)

    # ---- public API ---------------------------------------------------------

    def create(self, payload: Dict[str, Any]) -> str:
        self._prune_finished()
        job_id = uuid.uuid4().hex
        items: "OrderedDict[str, ItemState]" = OrderedDict()
        for raw in payload["items"]:
            items[raw["item_id"]] = ItemState(
                item_id=raw["item_id"],
                kind=raw["kind"],
                language=raw.get("language") or "python",
                content=raw.get("content") or {},
                config=raw.get("config") or {},
            )
        options = payload.get("options") or {}
        max_parallel = int(options.get("max_parallel") or self.default_max_parallel)
        max_parallel = max(1, min(32, max_parallel))
        item_timeout = int(
            options.get("item_timeout_secs") or self.default_item_timeout_secs
        )
        item_timeout = max(5, min(7200, item_timeout))
        workdir = self.work_root / job_id
        workdir.mkdir(parents=True, exist_ok=True)
        job = Job(
            job_id=job_id,
            run_id=payload["run_id"],
            items=items,
            environment=payload.get("environment") or {},
            max_parallel=max_parallel,
            item_timeout_secs=item_timeout,
            workdir=workdir,
        )
        with self.lock:
            self.jobs[job_id] = job
        threading.Thread(
            target=self._execute, args=(job,), name=f"job-{job_id[:8]}", daemon=True
        ).start()
        return job_id

    def get(self, job_id: str) -> Optional[Job]:
        with self.lock:
            return self.jobs.get(job_id)

    def cancel(self, job: Job) -> None:
        with job.lock:
            if job.status != "running":
                return
            job.cancel_event.set()
            procs = list(job.procs.values())
            for item in job.items.values():
                if item.status == "pending":
                    item.status = "skipped"
                    item.message = "run cancelled before start"
        # Hard kill: pytest would not flush its JSON report on SIGTERM anyway,
        # and partial results (logs, artifacts already on disk) are preserved.
        for proc in procs:
            _kill_proc_tree(proc)

    def shutdown(self) -> None:
        with self.lock:
            jobs = list(self.jobs.values())
        for job in jobs:
            self.cancel(job)

    # ---- internals ----------------------------------------------------------

    def _prune_finished(self) -> None:
        now = time.time()
        with self.lock:
            finished = [
                job
                for job in self.jobs.values()
                if job.finished_at is not None
            ]
            victims = [
                job
                for job in finished
                if now - job.finished_at > FINISHED_JOB_TTL_SECS
            ]
            overflow = len(self.jobs) - len(victims) - MAX_RETAINED_JOBS + 1
            if overflow > 0:
                survivors = sorted(
                    (job for job in finished if job not in victims),
                    key=lambda job: job.finished_at,
                )
                victims.extend(survivors[:overflow])
            for job in victims:
                self.jobs.pop(job.job_id, None)
        for job in victims:
            shutil.rmtree(job.workdir, ignore_errors=True)

    def _execute(self, job: Job) -> None:
        try:
            with ThreadPoolExecutor(
                max_workers=job.max_parallel, thread_name_prefix=f"item-{job.job_id[:8]}"
            ) as pool:
                futures = [
                    pool.submit(self._run_item, job, item_id) for item_id in job.items
                ]
                for future in futures:
                    future.result()
            with job.lock:
                job.status = "cancelled" if job.cancel_event.is_set() else "completed"
                job.finished_at = time.time()
        except Exception as exc:  # a runner bug must surface as job error, not hang
            with job.lock:
                job.status = "error"
                job.finished_at = time.time()
                for item in job.items.values():
                    if item.status in ("pending", "running"):
                        item.status = "error"
                        item.message = f"runner failure: {exc}"

    def _run_item(self, job: Job, item_id: str) -> None:
        item = job.items[item_id]
        with job.lock:
            if job.cancel_event.is_set() or item.status != "pending":
                if item.status == "pending":
                    item.status = "skipped"
                    item.message = "run cancelled before start"
                return
            item.status = "running"

        started = time.monotonic()
        try:
            outcome = self._dispatch(job, item)
        except Exception as exc:  # defensive: never let one item kill the pool
            outcome = ItemOutcome(status="error", message=f"runner failure: {exc}")

        artifacts = self._collect_artifacts(job, item_id)
        duration_ms = int((time.monotonic() - started) * 1000)
        with job.lock:
            item.status = outcome.status
            item.message = outcome.message
            item.steps = outcome.steps
            item.artifacts = artifacts
            item.duration_ms = duration_ms
            if outcome.perf_summary:
                job.perf_summary.extend(outcome.perf_summary)
            # First perf item wins the timeline: points are relative to their
            # own item start, so mixing several items would interleave badly.
            if outcome.perf_timeline and not job.perf_timeline:
                job.perf_timeline = outcome.perf_timeline

    def _dispatch(self, job: Job, item: ItemState) -> ItemOutcome:
        if item.language.strip().lower() != "python":
            return ItemOutcome(
                status="skipped",
                message=f"language '{item.language}' is not supported by this runner",
            )
        script = item.content.get("script")
        if isinstance(script, str) and len(script) > MAX_SCRIPT_CHARS:
            return ItemOutcome(status="error", message="script exceeds the size limit")

        ctx = self._make_context(job, item)
        if item.kind == "ui":
            return run_ui.run(ctx)
        if item.kind == "perf":
            return run_perf.run(ctx)
        if item.kind == "unit" and item.content.get("build_profile"):
            return build_profile.run(ctx)
        return run_pytest.run(ctx)

    def _make_context(self, job: Job, item: ItemState) -> ItemContext:
        item_dir = job.workdir / item.item_id
        artifacts_dir = item_dir / "artifacts"
        artifacts_dir.mkdir(parents=True, exist_ok=True)
        env = self._base_env(job, item, artifacts_dir)

        def run_subprocess(
            cmd: List[str],
            log_name: str = "console.log",
            extra_env: Optional[Dict[str, str]] = None,
            timeout: Optional[int] = None,
            cwd: Optional[Path] = None,
        ) -> SubprocessResult:
            return self._run_subprocess(
                job,
                item.item_id,
                cmd,
                cwd=cwd or item_dir,
                env={**env, **(extra_env or {})},
                timeout=timeout or job.item_timeout_secs,
                log_path=artifacts_dir / log_name,
            )

        return ItemContext(
            item_id=item.item_id,
            kind=item.kind,
            content=item.content,
            config=item.config,
            workdir=item_dir,
            artifacts_dir=artifacts_dir,
            env=env,
            timeout_secs=job.item_timeout_secs,
            isolated=self.isolated,
            run=run_subprocess,
        )

    def _base_env(self, job: Job, item: ItemState, artifacts_dir: Path) -> Dict[str, str]:
        environment = job.environment
        env = os.environ.copy()
        # Inherited pytest options could change collection semantics for the
        # untrusted scripts — drop them for deterministic runs.
        env.pop("PYTEST_ADDOPTS", None)
        existing_pythonpath = env.get("PYTHONPATH", "")
        pythonpath = str(self.bundle_dir)
        if existing_pythonpath:
            pythonpath = pythonpath + os.pathsep + existing_pythonpath
        env.update(
            {
                "PYTHONPATH": pythonpath,
                "PYTHONDONTWRITEBYTECODE": "1",
                "TF_BASE_URL": environment.get("base_url") or "",
                "TF_AUTH_TYPE": environment.get("auth_type") or "none",
                "TF_EXTRA_HEADERS": json.dumps(environment.get("extra_headers") or {}),
                "TF_HOST_ALLOWLIST": ",".join(environment.get("host_allowlist") or []),
                "TF_ARTIFACTS_DIR": str(artifacts_dir),
            }
        )
        # Unit items are fully offline — they get neither network nor secret.
        if item.kind == "unit":
            env["TF_NET_BLOCK_ALL"] = "1"
            env.pop("TF_SECRET", None)
        else:
            env["TF_SECRET"] = environment.get("secret") or ""
        return env

    def _run_subprocess(
        self,
        job: Job,
        item_id: str,
        cmd: List[str],
        cwd: Path,
        env: Dict[str, str],
        timeout: int,
        log_path: Path,
    ) -> SubprocessResult:
        if job.cancel_event.is_set():
            return SubprocessResult(returncode=None, timed_out=False, cancelled=True)
        popen_kwargs: Dict[str, Any] = {}
        if os.name == "posix":
            popen_kwargs["start_new_session"] = True
        else:
            popen_kwargs["creationflags"] = getattr(
                subprocess, "CREATE_NEW_PROCESS_GROUP", 0
            )
        timed_out = False
        with open(log_path, "ab") as log_file:
            proc = subprocess.Popen(
                cmd,
                cwd=str(cwd),
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=log_file,
                stderr=subprocess.STDOUT,
                **popen_kwargs,
            )
            with job.lock:
                # Cancel may have raced with the spawn — kill immediately.
                if job.cancel_event.is_set():
                    _kill_proc_tree(proc)
                job.procs[item_id] = proc
            try:
                proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                _kill_proc_tree(proc)
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    pass
            finally:
                with job.lock:
                    job.procs.pop(item_id, None)
        return SubprocessResult(
            returncode=proc.returncode,
            timed_out=timed_out,
            cancelled=job.cancel_event.is_set(),
        )

    def _collect_artifacts(self, job: Job, item_id: str) -> List[ArtifactState]:
        artifacts_dir = job.workdir / item_id / "artifacts"
        records: List[ArtifactState] = []
        if not artifacts_dir.is_dir():
            return records
        for path in sorted(artifacts_dir.rglob("*")):
            if not path.is_file():
                continue
            try:
                size = path.stat().st_size
            except OSError:
                continue
            records.append(
                ArtifactState(
                    name=path.name,
                    kind=artifact_kind(path),
                    rel_path=path.relative_to(job.workdir).as_posix(),
                    size_bytes=size,
                    mime=artifact_mime(path),
                )
            )
        return records
