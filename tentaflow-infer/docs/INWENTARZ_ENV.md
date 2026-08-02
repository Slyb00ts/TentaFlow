# Inwentarz zmiennych środowiskowych FORGE_*

Dokument **generowany** przez `cargo xtask env-inventory --write`. Nie edytuj ręcznie.

Podstawa pod kontrakt konfiguracji (`PLAN_NAPRAWY.md` §4.5). Klasa rozstrzyga, dokąd
zmienna trafia: przełącznik ścieżki do `forge.toml`, oprzyrządowanie do flagi CLI,
hak testowy do atrybutu testu. **Do zera musi zejść wyłącznie pierwsza klasa** — i to
ona jest mierzona bramką `env` w `cargo xtask lint`.

| klasa | sztuk | dokąd trafia |
|---|--:|---|
| przełącznik ścieżki | **42** | `forge.toml` |
| oprzyrządowanie | 25 | flaga CLI |
| test | 16 | atrybut testu |
| **razem** | **83** | |

## przełącznik ścieżki (forge.toml)

| zmienna | miejsca użycia |
|---|---|
| `FORGE_ATTN` | crates/forge-kernels/src/launchers.rs:1570<br>crates/forge-kernels/src/launchers.rs:1575<br>crates/forge-kernels/src/launchers.rs:2089<br>crates/forge-kernels/src/registry.rs:95 |
| `FORGE_ATTN_GQA` | crates/forge-engine/src/model.rs:18539 |
| `FORGE_BATCH_MIN` | crates/forge-engine/src/server.rs:659<br>crates/forge-engine/src/server.rs:666 |
| `FORGE_BM16_AUDIT_BATCH` | crates/forge-server/tests/nvfp4_bm16_audit.rs:503 |
| `FORGE_BM16_AUDIT_DIR` | crates/forge-server/tests/nvfp4_bm16_audit.rs:194 |
| `FORGE_BM16_AUDIT_MODE` | crates/forge-server/tests/nvfp4_bm16_audit.rs:4<br>crates/forge-server/tests/nvfp4_bm16_audit.rs:496 |
| `FORGE_DEEPSEEK_V4_DIR` | crates/forge-engine/tests/deepseek_v4_answer.rs:41<br>crates/forge-engine/tests/deepseek_v4_answer.rs:133<br>crates/forge-engine/tests/deepseek_v4_attention_gpu.rs:27<br>crates/forge-engine/tests/deepseek_v4_block_gpu.rs:28 |
| `FORGE_DEEPSEEK_V4_HOST_GB` | crates/forge-engine/tests/deepseek_v4_answer.rs:154 |
| `FORGE_DEEPSEEK_V4_LAYERS` | crates/forge-engine/tests/deepseek_v4_answer.rs:29 |
| `FORGE_DEEPSEEK_V4_ORACLE` | crates/forge-engine/tests/deepseek_v4_attention_gpu.rs:43<br>crates/forge-engine/tests/deepseek_v4_attention_gpu.rs:153<br>crates/forge-engine/tests/deepseek_v4_block_gpu.rs:45<br>crates/forge-engine/tests/deepseek_v4_block_gpu.rs:175 |
| `FORGE_DEEPSEEK_V4_SPILL` | crates/forge-engine/tests/deepseek_v4_answer.rs:37 |
| `FORGE_DELTANET_SCAN_TILED` | crates/forge-kernels/src/launchers.rs:3629 |
| `FORGE_DENSE_PREFILL_BATCH` | crates/forge-engine/src/server.rs:380<br>crates/forge-engine/src/server.rs:404<br>crates/forge-engine/src/server.rs:704 |
| `FORGE_DEVICE` | crates/forge-hal/src/gpu.rs:6<br>crates/forge-hal/src/gpu.rs:41<br>crates/forge-hal/src/gpu.rs:49<br>crates/forge-hal/src/gpu.rs:55 |
| `FORGE_FP8_BN256` | crates/forge-kernels/src/launchers.rs:76 |
| `FORGE_GEMM` | crates/forge-cli/src/main.rs:411<br>crates/forge-cli/src/main.rs:2102<br>crates/forge-cli/src/main.rs:2135<br>crates/forge-cli/src/main.rs:2143 |
| `FORGE_GPU_TEST` | crates/forge-kernels/tests/kv_page_permutation.rs:4<br>crates/forge-kernels/tests/kv_page_permutation.rs:25<br>crates/forge-kernels/tests/kv_page_permutation.rs:26 |
| `FORGE_HYBRID_BATCH_PREFILL` | crates/forge-engine/src/model.rs:3332<br>crates/forge-engine/src/model.rs:17279<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2147<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2149 |
| `FORGE_HYBRID_DECODE_GRAPH` | crates/forge-engine/src/model.rs:198<br>crates/forge-engine/src/model.rs:205 |
| `FORGE_HYBRID_FA_KEY_TILE` | crates/forge-kernels/src/launchers.rs:9092 |
| `FORGE_HYBRID_LAYER_MAJOR_ATTN` | crates/forge-engine/src/model.rs:216<br>crates/forge-engine/src/model.rs:224<br>crates/forge-engine/src/model.rs:14641<br>crates/forge-engine/src/model.rs:14646 |
| `FORGE_HYBRID_LAYER_MAJOR_DELTA_PREPARE` | crates/forge-engine/src/model.rs:243 |
| `FORGE_HYBRID_LAYER_MAJOR_PREFILL` | crates/forge-engine/src/model.rs:195<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2235<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2237<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2257 |
| `FORGE_HYBRID_LAYER_MAJOR_SCAN` | crates/forge-engine/src/model.rs:230<br>crates/forge-engine/src/model.rs:237 |
| `FORGE_HYBRID_PREFILL_BATCH` | crates/forge-engine/src/server.rs:340<br>crates/forge-engine/src/server.rs:358<br>crates/forge-engine/src/server.rs:362<br>crates/forge-engine/src/server.rs:702 |
| `FORGE_HYBRID_PREFILL_CHUNK` | crates/forge-engine/src/model.rs:183<br>crates/forge-engine/src/model.rs:188<br>crates/forge-engine/src/model.rs:281<br>crates/forge-engine/src/model.rs:286 |
| `FORGE_HYBRID_VERIFY_GRAPH` | crates/forge-engine/src/model.rs:12276<br>crates/forge-engine/src/model.rs:13173 |
| `FORGE_KERNEL_DIR` | crates/forge-kernels/src/registry.rs:2598<br>crates/forge-kernels/src/registry.rs:2603<br>crates/forge-kernels/src/registry.rs:2618 |
| `FORGE_MIXED_STEP` | crates/forge-engine/src/server.rs:842<br>crates/forge-engine/src/server.rs:843 |
| `FORGE_MTP_DRAFT_HEAD` | crates/forge-engine/src/model.rs:2885<br>crates/forge-engine/src/model.rs:2894<br>crates/forge-engine/src/model.rs:2899<br>crates/forge-engine/src/model.rs:2949 |
| `FORGE_MTP_EMBEDDING` | crates/forge-engine/src/weights.rs:4425<br>crates/forge-engine/src/weights.rs:4438 |
| `FORGE_MTP_NGRAM_BATCH` | crates/forge-engine/src/server.rs:312<br>crates/forge-engine/src/server.rs:424<br>crates/forge-engine/src/server.rs:698<br>crates/forge-engine/src/server.rs:727 |
| `FORGE_MTP_NGRAM_MIXED_BATCH` | crates/forge-engine/src/server.rs:322<br>crates/forge-engine/src/server.rs:700<br>crates/forge-engine/src/server.rs:727<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:941 |
| `FORGE_MTP_PROFILE_ONCE` | crates/forge-engine/examples/mtp_smoke.rs:84 |
| `FORGE_MTP_PROFILE_PREFILL` | crates/forge-engine/examples/mtp_smoke.rs:80 |
| `FORGE_NATIVE_MTP_B2` | crates/forge-engine/src/server.rs:294<br>crates/forge-engine/src/server.rs:696 |
| `FORGE_NVFP4_CT_BM16` | crates/forge-engine/src/model.rs:5536<br>crates/forge-server/tests/nvfp4_bm16_audit.rs:296<br>crates/forge-server/tests/nvfp4_bm16_audit.rs:344<br>crates/forge-server/tests/nvfp4_bm16_audit.rs:346 |
| `FORGE_NVFP4_CT_LAYOUT` | crates/forge-cli/src/main.rs:1083<br>crates/forge-cli/src/main.rs:1089<br>crates/forge-server/tests/batched_bielik.rs:55 |
| `FORGE_Q4K_INT8_WMMA` | crates/forge-kernels/src/launchers.rs:9489 |
| `FORGE_SILERO_VAD` | crates/forge-onnx/tests/silero_vad.rs:9<br>crates/forge-onnx/tests/silero_vad.rs:31<br>crates/forge-onnx/tests/silero_vad.rs:97 |
| `FORGE_VERIFY_ATTN_SPLIT8` | crates/forge-kernels/src/launchers.rs:11670 |
| `FORGE_W4A8_ALPHA` | crates/forge-cli/src/main.rs:2283<br>crates/forge-engine/src/model.rs:7251<br>crates/forge-engine/src/model.rs:7253<br>crates/forge-formats/src/w4a8.rs:209 |

## oprzyrządowanie (flaga CLI)

| zmienna | miejsca użycia |
|---|---|
| `FORGE_BENCH_FIXED_K` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1675 |
| `FORGE_BENCH_HYBRID_MTP` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3483<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3484 |
| `FORGE_BENCH_HYBRID_PREFILL_B2` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2589<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2590 |
| `FORGE_BENCH_HYBRID_PREFILL_SERVER` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2735 |
| `FORGE_BENCH_HYBRID_TARGET` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3569<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3570 |
| `FORGE_BENCH_MAX_ACTIVE` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3493<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3579 |
| `FORGE_BENCH_MTP_B2_FIXED` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3551<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3552 |
| `FORGE_BENCH_MTP_B2_MATRIX` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3519<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3520 |
| `FORGE_BENCH_MTP_NGRAM_B2_MATRIX` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3535<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3536 |
| `FORGE_BENCH_NVFP4_TILE` | crates/forge-cli/src/main.rs:750<br>crates/forge-cli/src/main.rs:754<br>crates/forge-cli/src/main.rs:768<br>crates/forge-cli/src/main.rs:771 |
| `FORGE_BENCH_PREFILL_CPU_FALLBACK` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2788 |
| `FORGE_BENCH_PROMPT_IDS` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3526<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3542<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3558<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3560 |
| `FORGE_BENCH_PROMPT_IDS_SECOND` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1494 |
| `FORGE_BENCH_PROMPT_TOKENS` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2742 |
| `FORGE_BENCH_REPS` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1484<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1490<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1670<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2596 |
| `FORGE_BENCH_SYNTHETIC_PROMPT_TOKENS` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1501 |
| `FORGE_HYBRID_DEBUG` | crates/forge-engine/src/model.rs:1236<br>crates/forge-engine/src/model.rs:3241 |
| `FORGE_LAYER_TRACE` | crates/forge-engine/src/model.rs:956<br>crates/forge-engine/src/model.rs:962 |
| `FORGE_NGRAM_TRACE` | crates/forge-engine/examples/hybrid_ngram_bench.rs:371<br>crates/forge-engine/examples/hybrid_ngram_bench.rs:396 |
| `FORGE_PREFILL_TRACE` | crates/forge-engine/src/model.rs:966<br>crates/forge-engine/src/model.rs:978 |
| `FORGE_PROFILE_HYBRID_PREFILL` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2678 |
| `FORGE_PROFILE_MTP_MATRIX` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1536 |
| `FORGE_PROFILE_SERVER_CONCURRENCY` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:973 |
| `FORGE_PROFILE_SERVER_PREFILL` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2789 |
| `FORGE_TRACE_ROUTE` | crates/forge-kernels/src/launchers.rs:7974<br>crates/forge-kernels/src/launchers.rs:8050<br>crates/forge-kernels/src/launchers.rs:10160 |

## test (atrybut testu)

| zmienna | miejsca użycia |
|---|---|
| `FORGE_BIELIK_TEST_TOKENS` | crates/forge-server/tests/batched_bielik.rs:413 |
| `FORGE_EMBED_TEST_MODEL` | crates/forge-server/tests/e2e_embeddings.rs:7<br>crates/forge-server/tests/e2e_embeddings.rs:22 |
| `FORGE_HYBRID_INTERLEAVE_AUDIT` | crates/forge-engine/examples/hybrid_ngram_bench.rs:764 |
| `FORGE_MTP_CATCHUP_AUDIT` | crates/forge-engine/examples/hybrid_ngram_bench.rs:758 |
| `FORGE_MTP_INTERLEAVE_AUDIT` | crates/forge-engine/examples/hybrid_ngram_bench.rs:767 |
| `FORGE_NGRAM_STATE_AUDIT` | crates/forge-engine/examples/hybrid_ngram_bench.rs:755 |
| `FORGE_TEST_ACTIVATIONS_MB` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:794 |
| `FORGE_TEST_FORCE_DELTA_KEY_VALUE` | crates/forge-engine/src/model.rs:3157 |
| `FORGE_TEST_HYBRID_GGUF` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:4<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1785<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1786<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1789 |
| `FORGE_TEST_HYBRID_LAYER_MAJOR_PRIORITY` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3341 |
| `FORGE_TEST_HYBRID_PREFILL_EXPECT_B2` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3111 |
| `FORGE_TEST_HYBRID_PREFILL_PRIORITY` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3241 |
| `FORGE_TEST_HYBRID_PREFILL_SERVER` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3040<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3041<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3190 |
| `FORGE_TEST_MTP_NGRAM_MIXED_SERVER` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3022 |
| `FORGE_TEST_POOL_RESERVE_MB` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:806 |
| `FORGE_TP_TEST_MODEL` | crates/forge-engine/tests/tp_shard_load.rs:11<br>crates/forge-engine/tests/tp_shard_load.rs:25<br>crates/forge-engine/tests/tp_shard_load.rs:56 |

