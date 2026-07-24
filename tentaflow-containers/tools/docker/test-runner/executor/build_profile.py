# =============================================================================
# File: executor/build_profile.py — executes unit items that carry a build
# profile {install_cmd, test_cmd, workdir}: runs the commands inside the
# mounted source snapshot. Allowed ONLY on the isolated docker runner — the
# commands are arbitrary binaries the Python socket guard cannot contain, so
# the container sandbox (network none, cap drop) is the actual boundary.
# =============================================================================

from __future__ import annotations

import time
from pathlib import Path
from typing import TYPE_CHECKING

from .junit import ItemOutcome, StepState

if TYPE_CHECKING:
    from .job import ItemContext

MAX_CMD_CHARS = 4096


def run(ctx: "ItemContext") -> ItemOutcome:
    if not ctx.isolated:
        return ItemOutcome(
            status="blocked",
            message="build_profile runs require the isolated docker runner",
        )

    profile = ctx.content.get("build_profile") or {}
    test_cmd = profile.get("test_cmd")
    install_cmd = profile.get("install_cmd") or ""
    workdir_raw = profile.get("workdir") or ""
    if not isinstance(test_cmd, str) or not test_cmd.strip():
        return ItemOutcome(status="error", message="build_profile has no test_cmd")
    for cmd in (install_cmd, test_cmd):
        if len(cmd) > MAX_CMD_CHARS:
            return ItemOutcome(status="error", message="build_profile command too long")

    workdir = Path(workdir_raw)
    if not workdir.is_absolute() or not workdir.is_dir():
        return ItemOutcome(
            status="error",
            message=f"build_profile workdir is not a mounted directory: {workdir_raw!r}",
        )

    deadline = time.monotonic() + ctx.timeout_secs
    steps = []
    plan = []
    if install_cmd.strip():
        plan.append(("install", install_cmd))
    plan.append(("test", test_cmd))

    for index, (name, cmd) in enumerate(plan):
        remaining = int(deadline - time.monotonic())
        if remaining <= 0:
            steps.append(StepState(index=index, name=name, status="error", message="timeout"))
            return ItemOutcome(
                status="error",
                message=f"item timed out after {ctx.timeout_secs}s",
                steps=steps,
            )
        sub = ctx.run(
            ["/bin/sh", "-c", cmd],
            log_name=f"build_{name}.log",
            timeout=remaining,
            cwd=workdir,
        )
        if sub.cancelled:
            steps.append(StepState(index=index, name=name, status="error", message="cancelled"))
            return ItemOutcome(status="error", message="cancelled", steps=steps)
        if sub.timed_out:
            steps.append(StepState(index=index, name=name, status="error", message="timeout"))
            return ItemOutcome(
                status="error",
                message=f"item timed out after {ctx.timeout_secs}s",
                steps=steps,
            )
        if sub.returncode != 0:
            message = f"{name} command exited with code {sub.returncode}"
            steps.append(StepState(index=index, name=name, status="failed", message=message))
            return ItemOutcome(status="failed", message=message, steps=steps)
        steps.append(StepState(index=index, name=name, status="passed"))

    return ItemOutcome(status="passed", steps=steps)
