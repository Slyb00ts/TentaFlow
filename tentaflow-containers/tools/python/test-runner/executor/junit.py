# =============================================================================
# File: executor/junit.py — normalizes pytest-json-report output into the
# item/step contract shared by all pytest-based kinds (ui/api/security/unit).
# Also hosts the shared result dataclasses so runner modules stay cycle-free.
# =============================================================================

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import List, Optional

MAX_MESSAGE_CHARS = 2000

# pytest test outcome → step status of the runner contract. "xpassed" is a
# test that was expected to fail but passed — surfaced as failed so authors
# notice stale expectations.
_OUTCOME_MAP = {
    "passed": "passed",
    "failed": "failed",
    "error": "error",
    "skipped": "skipped",
    "xfailed": "skipped",
    "xpassed": "failed",
}


@dataclass
class StepState:
    index: int
    name: str
    status: str
    message: str = ""

    def to_dict(self) -> dict:
        return {
            "index": self.index,
            "name": self.name,
            "status": self.status,
            "message": self.message,
        }


@dataclass
class SubprocessResult:
    returncode: Optional[int]
    timed_out: bool
    cancelled: bool


@dataclass
class ItemOutcome:
    status: str
    message: str = ""
    steps: List[StepState] = field(default_factory=list)
    perf_summary: List[dict] = field(default_factory=list)
    perf_timeline: List[dict] = field(default_factory=list)


def truncate_message(message: str) -> str:
    message = message.strip()
    if len(message) > MAX_MESSAGE_CHARS:
        return message[: MAX_MESSAGE_CHARS - 1] + "…"
    return message


def sanitize_name(name: str, max_len: int = 80) -> str:
    clean = re.sub(r"[^a-zA-Z0-9._-]+", "_", name).strip("._-")
    return (clean or "item")[:max_len]


def _stage_message(test: dict) -> str:
    for stage_name in ("call", "setup", "teardown"):
        stage = test.get(stage_name) or {}
        if stage.get("outcome") in ("failed", "error"):
            crash = stage.get("crash") or {}
            message = crash.get("message") or ""
            if not message:
                longrepr = stage.get("longrepr")
                message = longrepr if isinstance(longrepr, str) else ""
            if message:
                return truncate_message(message)
    return ""


def _step_name(nodeid: str) -> str:
    _, _, tail = nodeid.partition("::")
    return tail or nodeid


def outcome_from_report(
    report_path: Path,
    sub: SubprocessResult,
    timeout_secs: int,
) -> ItemOutcome:
    """Maps a pytest-json-report file + subprocess exit metadata onto the item
    contract. Runner-level problems (timeout, cancel, missing report) become
    item status 'error'; test-level failures become 'failed'."""
    steps: List[StepState] = []
    data = None
    if report_path.is_file():
        try:
            data = json.loads(report_path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            data = None

    if data is not None:
        for index, test in enumerate(data.get("tests", [])):
            status = _OUTCOME_MAP.get(test.get("outcome", ""), "error")
            steps.append(
                StepState(
                    index=index,
                    name=_step_name(test.get("nodeid", f"test_{index}")),
                    status=status,
                    message=_stage_message(test) if status != "passed" else "",
                )
            )

    if sub.cancelled:
        return ItemOutcome(status="error", message="cancelled", steps=steps)
    if sub.timed_out:
        return ItemOutcome(
            status="error",
            message=f"item timed out after {timeout_secs}s",
            steps=steps,
        )
    if data is None:
        return ItemOutcome(
            status="error",
            message=f"pytest produced no report (exit code {sub.returncode})",
            steps=steps,
        )

    exitcode = data.get("exitcode", 0)
    collect_errors = [
        collector
        for collector in data.get("collectors", [])
        if collector.get("outcome") == "failed"
    ]
    if collect_errors:
        longrepr = collect_errors[0].get("longrepr") or "collection failed"
        return ItemOutcome(
            status="error",
            message=truncate_message(str(longrepr)),
            steps=steps,
        )
    if exitcode == 5 or not steps:
        return ItemOutcome(status="error", message="no tests collected", steps=steps)
    if exitcode not in (0, 1):
        return ItemOutcome(
            status="error",
            message=f"pytest aborted with exit code {exitcode}",
            steps=steps,
        )

    if any(step.status in ("failed", "error") for step in steps):
        first_bad = next(s for s in steps if s.status in ("failed", "error"))
        return ItemOutcome(status="failed", message=first_bad.message, steps=steps)
    if all(step.status == "skipped" for step in steps):
        return ItemOutcome(status="skipped", message="all tests skipped", steps=steps)
    return ItemOutcome(status="passed", steps=steps)
