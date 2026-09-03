#!/usr/bin/env python3
# ===== File: llm/patches/glm53/patch_tilelang_gb10.py — TileLang DSA tile for GB10 =====
# Retunes the TileLang sparse-attention tile so the DSA kernel can LAUNCH on
# GB10 (DGX Spark, sm_121). Stock tile asks for 169_984 B of dynamic shared
# memory; consumer/workstation Blackwell caps `shared_memory_per_block_optin`
# at 101_376 B, so the launch fails outright. Datacenter Blackwell (B200/GB200)
# and Hopper have the headroom and must NOT get this patch — it only costs
# throughput there. That is why it lives in the Spark-only image.
#
# Working tile on GB10: block_I=32, num_stages=1, threads=128. The three move
# together: `threads` has to drop to 128 BEFORE `block_I` can drop to 32, or the
# m_i/alpha fragment layouts become unsatisfiable; block_I=16 hits an MMA assert.
#
# Only `sparse_attention_fwd_kernel_v1` is retuned, and that is the whole story
# for GLM-5.3-Flash: `tilelang_sparse_fwd` picks v1 exactly when `tail_dim == 0`
# (NoPE), which is this model, and it never passes the tile arguments — so the
# defaults patched here are what the kernel is traced with. `topk % block_I == 0`
# still holds (index_topk 2048).
#
# Idempotent; a missing or ambiguous anchor is a hard error, so an sglang bump
# fails the build instead of shipping an image whose kernel cannot launch.
import sys
from pathlib import Path

KERNEL_REL = "kernels/ops/attention/dsa/tilelang_kernel.py"


def _sglang_root() -> Path:
    """Katalog pakietu `sglang`. Rozpoznajemy go po ZAWARTOSCI (obecnosc modulu
    kerneli DSA), a nie po nazwie katalogu."""
    if len(sys.argv) > 1:
        p = Path(sys.argv[1]).resolve()
        for cand in (p, p / "sglang"):
            if (cand / KERNEL_REL).is_file():
                return cand
        raise SystemExit(f"{p} nie wyglada na pakiet sglang (brak {KERNEL_REL})")
    try:
        import sglang  # noqa: PLC0415
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"nie znajduje pakietu sglang ({exc}) — podaj sciezke argumentem")
    return Path(sglang.__file__).resolve().parent


root = _sglang_root()
path = root / KERNEL_REL
text = path.read_text()

STOCK = "    block_I=64,\n    num_stages=2,\n    threads=256,\n"
GB10 = "    block_I=32,\n    num_stages=1,\n    threads=128,\n"

if GB10 in text:
    print(f"tilelang GB10 tile -> {path}\n  = juz nalozone")
    raise SystemExit(0)

occurrences = text.count(STOCK)
if occurrences != 1:
    raise SystemExit(
        f"oczekiwano 1 wystapienia domyslnego kafelka w {path}, znaleziono {occurrences}\n"
        f"upstream przesunal ten kod — przenies latke i zaktualizuj obraz bazowy"
    )

path.write_text(text.replace(STOCK, GB10, 1))
print(f"tilelang GB10 tile -> {path}\n  * block_I 64->32, num_stages 2->1, threads 256->128")
