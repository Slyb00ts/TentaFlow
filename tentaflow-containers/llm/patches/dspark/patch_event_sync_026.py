#!/usr/bin/env python3
"""Make the sampled-token device->host copy synchronous on GB10.

On DGX Spark (GB10, sm_121) with CUDA graphs enabled, vLLM hangs in
`cudaEventSynchronize` after a non-blocking device->host copy into pinned
memory. Captured with py-spy while the server was stuck:

    _to_list          (gpu_model_runner.py)   <- transfer_event.synchronize()
    _bookkeeping_sync (gpu_model_runner.py)
    sample_tokens     (gpu_model_runner.py)

At that moment the GPU sits at 0% utilization and the other TP rank is idle
with an empty queue, so nothing is computing and no collective is pending --
the event is simply never signalled. The same signature appears one frame over
in the async-scheduling path (`async_copy_ready_event.synchronize()`), so the
event, not the scheduler, is the common factor. Running with `--enforce-eager`
avoids it entirely, which pins the trigger to graph capture/replay, but costs
roughly 37% of decode throughput (16.7 vs 26.5 tok/s measured here).

The upstream code says outright that it is an optimisation over the plain
`.tolist()` (see the comment referencing vllm-project/vllm#22754): the event
exists only to avoid a device-wide stream sync in disaggregated setups. Falling
back to the blocking form restores correctness on this platform and costs one
sync per step, which we already pay via CUDA graphs.

Idempotent; a missing anchor is a hard error naming the file, so a vLLM bump
fails loudly here instead of silently producing a runtime that hangs.
"""

import sys
from pathlib import Path

applied: list[str] = []
skipped: list[str] = []


def _vllm_root() -> Path:
    if len(sys.argv) > 1:
        p = Path(sys.argv[1]).resolve()
        if (p / "v1" / "worker" / "gpu_model_runner.py").is_file():
            return p
        raise SystemExit(f"nie wyglada na drzewo vllm: {p}")
    import vllm  # noqa: PLC0415

    return Path(vllm.__file__).resolve().parent


root = _vllm_root()


def replace(path: str, old: str, new: str, label: str) -> None:
    p = root / path
    src = p.read_text(encoding="utf-8")
    if new in src:
        skipped.append(label)
        return
    if old not in src:
        raise SystemExit(
            f"missing patch anchor in {p} [{label}]\n"
            "vLLM moved the code -- re-derive the patch against the new tree."
        )
    p.write_text(src.replace(old, new, 1), encoding="utf-8")
    applied.append(label)


# The pinned buffer, the event and the record() call all become dead weight, so
# the whole body goes rather than just the synchronize().
replace(
    "v1/worker/gpu_model_runner.py",
    "        pinned = self.sampled_token_ids_pinned_cpu[: sampled_token_ids.shape[0]]\n"
    "        pinned.copy_(sampled_token_ids, non_blocking=True)\n"
    "        self.transfer_event.record()\n"
    "        self.transfer_event.synchronize()\n"
    "        return pinned.tolist()",
    "        # GB10: the event after a non-blocking D2H copy is never signalled\n"
    "        # under CUDA graphs and the worker hangs here with the GPU idle.\n"
    "        # The blocking form syncs the stream, which is what this code did\n"
    "        # before the disagg optimisation it cites.\n"
    "        return sampled_token_ids.tolist()",
    "gpu_model_runner: _to_list bez zdarzenia",
)

# Same failure one frame over, on the async-scheduling path. A device-wide sync
# is the coarse equivalent of waiting for that event and is safe here.
replace(
    "v1/worker/gpu_model_runner.py",
    "        self.async_copy_ready_event.synchronize()",
    "        # GB10: see _to_list -- this event does not fire under CUDA graphs.\n"
    "        torch.cuda.synchronize()",
    "gpu_model_runner: async_copy_ready_event -> device sync",
)

print(f"event-sync -> {root}")
for a in applied:
    print(f"  * {a}")
for s in skipped:
    print(f"  = {s} (juz nalozone)")
if not applied and not skipped:
    raise SystemExit("nic nie zrobiono — zestaw jest pusty?")
