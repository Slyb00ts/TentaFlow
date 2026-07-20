# FORGE vs vLLM 0.25.1 — NVFP4 Bielik on RTX 4090

**Date:** 2026-07-20
**GPU:** NVIDIA GeForce RTX 4090 (Ada, sm_89), 24564 MiB
**Driver:** 610.43.02
**vLLM:** 0.25.1 (docker `vllm/vllm-openai:latest`, `vllm.__version__ == 0.25.1`)
**FORGE:** `tentaflow-infer/target/release/forge` (built 2026-07-20 15:10)

Every number below is copied from a real command run on this box. No number is
estimated except the two clearly-labelled derived rates (vLLM prefill/decode from
TTFT/TPOT), whose arithmetic is shown.

---

## 1. This is a genuine like-for-like NVFP4 comparison

Both engines ran the **exact same checkpoint files**:

```
.runtime/models/models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4/snapshots/831550e879fd7d700e3f6d79dffc14373deda3a7/
```

That checkpoint is a standard **compressed-tensors NVFP4** export (llm-compressor
`version 0.14.0.1`), not a FORGE-proprietary container. From `config.json`:

```json
"quantization_config": { "format": "nvfp4-pack-quantized",
  "quant_method": "compressed-tensors", "quantization_status": "compressed",
  "config_groups": { "group_0": { "weights": { "num_bits": 4, "group_size": 16,
      "scale_dtype": "torch.float8_e4m3fn", ... } } }, "ignore": ["lm_head"] }
```

safetensors payload (1203 tensors): `U8` packed 4-bit weights ×280, `F8_E4M3`
block scales ×280, `F32` global scales ×560, `BF16` norms/embeddings ×83.

vLLM 0.25.1 loaded it directly (`quantization=compressed-tensors`), so this is the
**same 7B NVFP4 weights on both engines** — the rare true apples-to-apples. No
substitute model was needed.

### Neither engine is hardware-accelerated for NVFP4 on Ada

NVFP4 tensor cores are Blackwell-only. On the 4090 (Ada sm_89) **both engines run
NVFP4 as a weight-only dequant path**:

- **vLLM** picks the Marlin FP4 kernel and prints the warning verbatim:
  ```
  INFO  Using MarlinNvFp4LinearKernel for NVFP4 GEMM
  WARNING marlin.py:34  Your GPU does not have native support for FP4 computation
          but FP4 quantization is being used. Weight-only FP4 compression will be
          used leveraging the Marlin kernel. This may degrade performance for
          compute-heavy workloads.
  ```
- **FORGE** uses its software FP4 dequant GEMM (no sm_89 FP4 tensor-core path).

So the comparison isolates *engine software quality on the same dequant workload*,
which is exactly what was wanted (unlike the earlier run where vLLM used FP8).

---

## 2. Results

### Single-stream, warm (prefill = 4096-token prompt, decode = 256 tokens)

| Engine | NVFP4 checkpoint | Prefill (pp4096) tok/s | Decode (tg) tok/s |
|--------|------------------|-----------------------:|------------------:|
| **FORGE** | TentaFlow Bielik-PL-Minitron-7B-NVFP4 | **4 374.6** | **130.7** |
| **vLLM 0.25.1** | *same checkpoint* | **≈ 9 415** (derived) | **≈ 145.8** (derived) |

- FORGE: `forge bench --reps 5 --prefix-cache off` (rep 5, warm). Prefix cache
  disabled so warm prefill measures real recompute, not a radix-cache hit.
- vLLM: `vllm bench serve --max-concurrency 1`. Derived rates:
  - prefill = 4096 tok / 0.43505 s TTFT = **9 415 tok/s**
  - decode  = 1000 / 6.86 ms TPOT   = **145.8 tok/s**
  (vLLM's own "Output token throughput 117 tok/s" is end-to-end incl. TTFT; the
  TPOT-based 145.8 is the steady-state decode rate, the fair analogue of FORGE's tg.)

**vLLM wins single-stream on both axes:** ~2.15× prefill, ~1.12× decode.

### Concurrency sweep — aggregate output tok/s (identical client, prompt, max_tokens=256)

Same harness (`concbench.py`) fired at both servers: N identical Polish chat
requests, temperature 0, 256 max tokens (both engines generated the full 256 at
every level, so token counts are equal — clean aggregate comparison).

| Concurrency N | FORGE agg tok/s | vLLM 0.25.1 agg tok/s | vLLM advantage |
|--------------:|----------------:|----------------------:|---------------:|
| 1  | 151.7 | 165.1  | 1.09× |
| 4  | 151.6 | 655.7  | 4.33× |
| 8  | 151.6 | 1 279.3 | 8.44× |
| 16 | 213.2 | 2 363.3 | 11.1× |
| 32 | 400.0 | 4 225.0 | 10.6× |

- FORGE: `forge serve --max-active 32` (default `--batch-min 12`).
- vLLM: `--max-num-seqs 32` (matched to the sweep ceiling; continuous batching on).

**Scaling shape (the decisive finding):**
- **vLLM scales near-linearly** with continuous batching: 165 → 4225 tok/s from
  N=1 to N=32 = **25.6× throughput on 32× load** (per-request cost barely rises:
  165 → 132 tok/s).
- **FORGE scales flat, then modestly.** Aggregate is pinned at ~151 tok/s for
  N=1/4/8 (single-stream throughput split across the requests → no batching gain),
  and only rises to 213 (N=16) and 400 (N=32) once FORGE's batched decode path
  engages above `batch-min`. That is **2.6×** across the whole 1→32 range.
- At N=32, vLLM delivers **10.6× the aggregate throughput** of FORGE.

The "FORGE scales flat" behaviour observed earlier is **reconfirmed on NVFP4**: the
default fused single-seq path (below `batch-min=12`) does not batch, so 4 or 8
concurrent streams complete at the same aggregate rate as one.

---

## 3. FORGE `batch-min` tuning (does not change the conclusion)

To be fair to FORGE, its batched path was forced on early with `--batch-min 2`:

| N | FORGE default (bm=12) | FORGE `--batch-min 2` |
|--:|----------------------:|----------------------:|
| 1  | 151.7 | 151.9 |
| 4  | 151.6 | **52.3** |
| 8  | 151.6 | 106.4 |
| 16 | 213.2 | 200.3 |
| 32 | 400.0 | 428.2 |

Lowering `batch-min` makes low concurrency **worse** (52 tok/s at N=4) and barely
moves the N=32 ceiling (428 vs 400). The default is FORGE's best config; its
aggregate ceiling on this GPU is ~400–430 tok/s regardless. So FORGE's honest best
still trails vLLM's 4225 tok/s at N=32 by ~10×.

---

## 4. Fairness caveats (read before quoting a number)

1. **Same checkpoint, same weights, same NVFP4 format** — the strongest possible
   fairness. Both run the identical `nvfp4-pack-quantized` files.
2. **Both dequant, neither HW-accelerated** on Ada. Marlin (vLLM) vs FORGE's
   software FP4. A Blackwell GPU would change absolute numbers for both; the
   *relative* engine comparison here is the point.
3. **Single-stream tools differ by engine** (each engine's native bench): FORGE
   `forge bench` warm rep, vLLM `vllm bench serve` conc-1. The concurrency sweep,
   which carries the headline conclusion, uses the **identical** `concbench.py`
   client + prompt against both OpenAI servers — that half is strictly apples-to-apples.
4. **vLLM ran with cudagraphs ON** (not `--enforce-eager`). Initial launch OOM'd
   during graph capture because the desktop already holds ~1.7 GiB of the 24 GiB;
   fixed by `--gpu-memory-utilization 0.85 --max-num-seqs 32` (capping seqs shrinks
   graph capture to the sweep range). This is vLLM's normal, favourable config —
   no handicap applied.
5. **vLLM `--max-num-seqs 32`** was set equal to the sweep ceiling so vLLM was not
   given a larger batching budget than FORGE's `--max-active 32`. If anything this
   slightly *under*-states vLLM (its default is 256+).
6. **KV/prefix caching**: both engines had their default prefix caching on for the
   concurrency sweep (identical prompts share a prefix on both). FORGE single-stream
   prefill was measured with prefix cache OFF to avoid a cache-hit inflating it
   (with it on, reps 2–5 reported a false ~48 000 tok/s).
7. FORGE single-stream decode (130.7 tok/s) is rock-steady across reps; vLLM
   single-stream via `concbench` N=1 short-prompt was 165.1 tok/s, consistent with
   the TPOT-derived 145.8 tok/s (short prompt adds a little).

---

## 5. Verdict

On the **same NVFP4 Bielik-7B checkpoint**, dequant path on both (Ada, no FP4
tensor cores):

- **Single stream:** vLLM 0.25.1 wins — ~2.15× prefill, ~1.12× decode. FORGE is
  competitive on decode (130.7 vs 145.8 tok/s) but its dense prefill GEMM is roughly
  half Marlin's throughput.
- **Concurrency:** not close. vLLM's continuous batching scales near-linearly to
  4225 tok/s at N=32; FORGE's aggregate is flat to N=8 and tops out near 400 tok/s,
  a ~10× gap under load. This is the real, reconfirmed weakness — FORGE currently
  serves concurrent NVFP4 traffic at close to single-stream aggregate until its
  batched path engages, and even then its ceiling is an order of magnitude below vLLM.

The truly like-for-like NVFP4-vs-NVFP4 comparison was achievable (no substitute
model), and vLLM 0.25.1 is decisively ahead on multi-stream serving throughput.

---

## Appendix — exact commands & raw output

### Environment
```
$ nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv,noheader
NVIDIA GeForce RTX 4090, 610.43.02, 24564 MiB
$ docker run --rm --entrypoint python3 vllm/vllm-openai:latest -c "import vllm; print(vllm.__version__)"
0.25.1
```

### FORGE single-stream (warm, prefix cache off)
```
$ forge bench <ckpt> --prompt-tokens 4096 --tokens 256 --reps 5 --prefix-cache off
model loaded in 38.5s
rep 1/5: prefill 1.114s (3676.4 tok/s) | decode 1.951s (130.7 tok/s)
rep 5/5: prefill 0.938s (4368.8 tok/s) | decode 1.950s (130.7 tok/s)
| phase   | tokens | seconds | tok/s   |
| prefill |   4096 |   0.936 |  4374.6 |
| decode  |    255 |   1.950 |   130.7 |
```

### vLLM launch (same checkpoint)
```
$ docker run -d --name vllm_bench --gpus all --privileged -v /dev:/dev --ipc=host \
    -v .runtime/models:/models:ro -p 8000:8000 vllm/vllm-openai:latest \
    /models/models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4/snapshots/831.../ \
    --served-model-name bielik-nvfp4 --host 0.0.0.0 --port 8000 \
    --gpu-memory-utilization 0.85 --max-model-len 8192 --max-num-seqs 32
...
Using MarlinNvFp4LinearKernel for NVFP4 GEMM
WARNING marlin.py:34  Your GPU does not have native support for FP4 computation ...
Available KV cache memory: 15.32 GiB ; GPU KV cache size: 100,384 tokens
```
(`--privileged -v /dev:/dev` works around the nvidia-container-toolkit /dev/nvidia-uvm
bug on this box; `--max-num-seqs 32` + util 0.85 avoid the cudagraph-capture OOM the
desktop's 1.7 GiB residency otherwise causes.)

### vLLM single-stream
```
$ docker exec vllm_bench vllm bench serve --backend openai \
    --base-url http://127.0.0.1:8000 --model bielik-nvfp4 --tokenizer <ckpt> \
    --dataset-name random --random-input-len 4096 --random-output-len 256 \
    --num-prompts 6 --max-concurrency 1 --ignore-eos
============ Serving Benchmark Result ============
Output token throughput (tok/s):         117.19
Total token throughput (tok/s):          1992.16
Mean TTFT (ms):                          435.05
Mean TPOT (ms):                          6.86
Mean ITL  (ms):                          6.86
```

### Concurrency sweep — identical `concbench.py` on both
```
FORGE  (forge serve --max-active 32, default --batch-min 12):
N=  1  wall=  1.69s  out_tok=  256  AGG=  151.7 tok/s
N=  4  wall=  6.75s  out_tok= 1024  AGG=  151.6 tok/s
N=  8  wall= 13.51s  out_tok= 2048  AGG=  151.6 tok/s
N= 16  wall= 19.21s  out_tok= 4096  AGG=  213.2 tok/s
N= 32  wall= 20.48s  out_tok= 8192  AGG=  400.0 tok/s

vLLM 0.25.1 (--max-num-seqs 32):
N=  1  wall=  1.55s  out_tok=  256  AGG=  165.1 tok/s
N=  4  wall=  1.56s  out_tok= 1024  AGG=  655.7 tok/s
N=  8  wall=  1.60s  out_tok= 2048  AGG= 1279.3 tok/s
N= 16  wall=  1.73s  out_tok= 4096  AGG= 2363.3 tok/s
N= 32  wall=  1.94s  out_tok= 8192  AGG= 4225.0 tok/s
```

`concbench.py`: N threads POST identical `/v1/chat/completions` (fixed Polish
prompt, `temperature 0`, `max_tokens 256`, non-stream); aggregate =
Σ completion_tokens / wall-clock (first send → last response).
