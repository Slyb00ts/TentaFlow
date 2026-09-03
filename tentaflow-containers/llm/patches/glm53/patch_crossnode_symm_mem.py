#!/usr/bin/env python3
# ===== File: llm/patches/glm53/patch_crossnode_symm_mem.py — skip the cross-node symm-mem topology probe =====
# `MultimemAllGatherer.__init__` (srt/distributed/device_communicators/
# triton_symm_mem_ag.py) decides whether the symmetric-memory logits all-gather
# is usable by probing same-node-ness with a COLLECTIVE:
#
#     if (tp_group.world_size > 1
#         and get_parallel().config.nnodes > 1
#         and not all(in_the_same_node_as(tp_group.cpu_group, source_rank=0))):
#         disable
#
# The comment right above the call already admits the probe is fragile ("can
# segfault under some EP/mooncake setups"). On the 2x Spark GLM-5.3 cluster it
# deadlocks instead: the probe runs only on ranks whose `get_parallel().config`
# is published, and the NEXTN draft worker's context is exactly the "unpublished
# offline path" case — rank 0's draft enters `broadcast_object_list` and waits
# forever, because rank 1's draft never joins (verified via faulthandler dump:
# scheduler maybe_init_draft_worker -> Glm5NextNextN -> LogitsProcessor ->
# MultimemAllGatherer.__init__ -> in_the_same_node_as, 40+ min of zero progress).
#
# The probe's ONLY cross-node outcome is "disable multimem" — the symmetric
# memory fabric does not span nodes anyway. So the probe is replaced by an
# unconditional disable whenever nnodes > 1: functionally identical for every
# real cross-node deployment, no collective issued, and single-node keeps
# multimem untouched (the same code path that runs today there).
# Idempotent; a missing or ambiguous anchor is a hard error.
import sys
from pathlib import Path

TARGET_REL = "srt/distributed/device_communicators/triton_symm_mem_ag.py"

STOCK = """            if (
                tp_group.world_size > 1
                and get_parallel().config.nnodes > 1
                and not all(in_the_same_node_as(tp_group.cpu_group, source_rank=0))
            ):
"""

REPLACEMENT = """            if tp_group.world_size > 1 and get_parallel().config.nnodes > 1:
                # Cross-node multimem is disabled unconditionally. The stock
                # probe (in_the_same_node_as) is a collective and deadlocks
                # when one rank's parallel config is unpublished — the NEXTN
                # draft worker's context — leaving the other rank in
                # broadcast_object_list forever. The probe's only cross-node
                # outcome is "disable", so skipping it is functionally
                # identical; single-node deployments keep multimem.
"""


def main() -> int:
    root = Path("/sgl-workspace/sglang/python/sglang")
    target = root / TARGET_REL
    if not target.is_file():
        print(f"FATAL: {target} does not exist", file=sys.stderr)
        return 1
    text = target.read_text()
    if REPLACEMENT.strip().splitlines()[0] in text:
        print("patch_crossnode_symm_mem: already applied, skipping")
        return 0
    hits = text.count(STOCK)
    if hits != 1:
        print(
            f"FATAL: anchor found {hits} times (expected 1) in {target}; "
            "sglang moved — re-review the patch",
            file=sys.stderr,
        )
        return 1
    target.write_text(text.replace(STOCK, REPLACEMENT))
    print("patch_crossnode_symm_mem: applied")
    return 0


if __name__ == "__main__":
    sys.exit(main())
