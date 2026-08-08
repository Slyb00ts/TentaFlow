# Inwentarz zmiennych środowiskowych FORGE_*

Dokument **generowany** przez `cargo xtask env-inventory --write`. Nie edytuj ręcznie.

Podstawa pod kontrakt konfiguracji (`PLAN_NAPRAWY.md` §4.5). Klasa rozstrzyga, dokąd
zmienna trafia: przełącznik ścieżki do `forge.toml`, oprzyrządowanie do flagi CLI,
hak testowy do atrybutu testu. **Do zera musi zejść wyłącznie pierwsza klasa** — i to
ona jest mierzona bramką `env` w `cargo xtask lint`.

| klasa | sztuk | dokąd trafia |
|---|--:|---|
| przełącznik ścieżki | **42** | `forge.toml` |
| oprzyrządowanie | 26 | flaga CLI |
| test | 16 | atrybut testu |
| **razem** | **84** | |

## przełącznik ścieżki (forge.toml)

| zmienna | miejsca użycia |
|---|---|
| `FORGE_ATTN` | crates/forge-kernels/src/launchers/mod.rs:1471<br>crates/forge-kernels/src/launchers/mod.rs:1476<br>crates/forge-kernels/src/launchers/mod.rs:1761<br>crates/forge-kernels/src/registry.rs:70 |
| `FORGE_ATTN_GQA` | crates/forge-engine/src/model/arch/dense.rs:2155 |
| `FORGE_BATCH_MIN` | crates/forge-engine/src/server.rs:659<br>crates/forge-engine/src/server.rs:666 |
| `FORGE_BM16_AUDIT_BATCH` | crates/forge-server/tests/nvfp4_bm16_audit.rs:503 |
| `FORGE_BM16_AUDIT_DIR` | crates/forge-server/tests/nvfp4_bm16_audit.rs:194 |
| `FORGE_BM16_AUDIT_MODE` | crates/forge-server/tests/nvfp4_bm16_audit.rs:4<br>crates/forge-server/tests/nvfp4_bm16_audit.rs:496 |
| `FORGE_DEEPSEEK_V4_DIR` | crates/forge-engine/tests/deepseek_v4_answer.rs:41<br>crates/forge-engine/tests/deepseek_v4_answer.rs:133<br>crates/forge-engine/tests/deepseek_v4_attention_gpu.rs:27<br>crates/forge-engine/tests/deepseek_v4_block_gpu.rs:28 |
| `FORGE_DEEPSEEK_V4_HOST_GB` | crates/forge-engine/tests/deepseek_v4_answer.rs:154 |
| `FORGE_DEEPSEEK_V4_LAYERS` | crates/forge-engine/tests/deepseek_v4_answer.rs:29 |
| `FORGE_DEEPSEEK_V4_ORACLE` | crates/forge-engine/tests/deepseek_v4_attention_gpu.rs:43<br>crates/forge-engine/tests/deepseek_v4_attention_gpu.rs:153<br>crates/forge-engine/tests/deepseek_v4_block_gpu.rs:45<br>crates/forge-engine/tests/deepseek_v4_block_gpu.rs:175 |
| `FORGE_DEEPSEEK_V4_SPILL` | crates/forge-engine/tests/deepseek_v4_answer.rs:37 |
| `FORGE_DELTANET_SCAN_TILED` | crates/forge-kernels/src/launchers/deltanet.rs:1007 |
| `FORGE_DENSE_PREFILL_BATCH` | crates/forge-engine/src/server.rs:380<br>crates/forge-engine/src/server.rs:404<br>crates/forge-engine/src/server.rs:704 |
| `FORGE_DEVICE` | crates/forge-hal/src/gpu.rs:6<br>crates/forge-hal/src/gpu.rs:41<br>crates/forge-hal/src/gpu.rs:49<br>crates/forge-hal/src/gpu.rs:55 |
| `FORGE_FP8_BN256` | crates/forge-kernels/src/launchers/mod.rs:51 |
| `FORGE_GEMM` | crates/forge-cli/src/main.rs:419<br>crates/forge-cli/src/main.rs:2162<br>crates/forge-cli/src/main.rs:2195<br>crates/forge-cli/src/main.rs:2203 |
| `FORGE_GPU_TEST` | crates/forge-kernels/tests/kv_page_permutation.rs:4<br>crates/forge-kernels/tests/kv_page_permutation.rs:25<br>crates/forge-kernels/tests/kv_page_permutation.rs:26 |
| `FORGE_HYBRID_BATCH_PREFILL` | crates/forge-engine/src/model/arch/hybrid/prefill.rs:1221<br>crates/forge-engine/src/model/debug.rs:80<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2141<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2143 |
| `FORGE_HYBRID_DECODE_GRAPH` | crates/forge-engine/src/model/graph.rs:4<br>crates/forge-engine/src/model/graph.rs:11 |
| `FORGE_HYBRID_FA_KEY_TILE` | crates/forge-kernels/src/launchers/attention.rs:1226 |
| `FORGE_HYBRID_LAYER_MAJOR_ATTN` | crates/forge-engine/src/model/arch/hybrid/core.rs:9<br>crates/forge-engine/src/model/arch/hybrid/core.rs:17<br>crates/forge-engine/src/model/arch/hybrid/core.rs:101<br>crates/forge-engine/src/model/arch/hybrid/core.rs:106 |
| `FORGE_HYBRID_LAYER_MAJOR_DELTA_PREPARE` | crates/forge-engine/src/model/arch/hybrid/prefill.rs:5 |
| `FORGE_HYBRID_LAYER_MAJOR_PREFILL` | crates/forge-engine/src/model/arch/hybrid/core.rs:5<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2229<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2231<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2251 |
| `FORGE_HYBRID_LAYER_MAJOR_SCAN` | crates/forge-engine/src/model/mod.rs:228<br>crates/forge-engine/src/model/mod.rs:235 |
| `FORGE_HYBRID_PREFILL_BATCH` | crates/forge-engine/src/server.rs:340<br>crates/forge-engine/src/server.rs:358<br>crates/forge-engine/src/server.rs:362<br>crates/forge-engine/src/server.rs:702 |
| `FORGE_HYBRID_PREFILL_CHUNK` | crates/forge-engine/src/model/mod.rs:209<br>crates/forge-engine/src/model/mod.rs:214<br>crates/forge-engine/src/model/mod.rs:274<br>crates/forge-engine/src/model/mod.rs:279 |
| `FORGE_HYBRID_VERIFY_GRAPH` | crates/forge-engine/src/model/arch/hybrid/verify.rs:771<br>crates/forge-engine/src/model/mtp.rs:2712 |
| `FORGE_KERNEL_DIR` | crates/forge-kernels/build.rs:5<br>crates/forge-kernels/src/registry.rs:196<br>crates/forge-kernels/src/registry.rs:201<br>crates/forge-kernels/src/registry.rs:216 |
| `FORGE_MIXED_STEP` | crates/forge-engine/src/server.rs:842<br>crates/forge-engine/src/server.rs:843 |
| `FORGE_MTP_DRAFT_HEAD` | crates/forge-engine/src/model/mod.rs:2616<br>crates/forge-engine/src/model/mod.rs:2625<br>crates/forge-engine/src/model/mod.rs:2630<br>crates/forge-engine/src/model/mod.rs:2680 |
| `FORGE_MTP_EMBEDDING` | crates/forge-engine/src/weights.rs:4017<br>crates/forge-engine/src/weights.rs:4030 |
| `FORGE_MTP_NGRAM_BATCH` | crates/forge-engine/src/server.rs:312<br>crates/forge-engine/src/server.rs:424<br>crates/forge-engine/src/server.rs:698<br>crates/forge-engine/src/server.rs:727 |
| `FORGE_MTP_NGRAM_MIXED_BATCH` | crates/forge-engine/src/server.rs:322<br>crates/forge-engine/src/server.rs:700<br>crates/forge-engine/src/server.rs:727<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:935 |
| `FORGE_MTP_PROFILE_ONCE` | crates/forge-engine/examples/mtp_smoke.rs:84 |
| `FORGE_MTP_PROFILE_PREFILL` | crates/forge-engine/examples/mtp_smoke.rs:80 |
| `FORGE_NATIVE_MTP_B2` | crates/forge-engine/src/server.rs:294<br>crates/forge-engine/src/server.rs:696 |
| `FORGE_NVFP4_CT_BM16` | crates/forge-engine/src/model/loader.rs:205<br>crates/forge-server/tests/nvfp4_bm16_audit.rs:296<br>crates/forge-server/tests/nvfp4_bm16_audit.rs:344<br>crates/forge-server/tests/nvfp4_bm16_audit.rs:346 |
| `FORGE_NVFP4_CT_LAYOUT` | crates/forge-cli/src/main.rs:1094<br>crates/forge-cli/src/main.rs:1100<br>crates/forge-server/tests/batched_bielik.rs:55 |
| `FORGE_Q4K_INT8_WMMA` | crates/forge-kernels/src/launchers/gemm/quantized/k_quants.rs:177 |
| `FORGE_SILERO_VAD` | crates/forge-onnx/tests/silero_vad.rs:9<br>crates/forge-onnx/tests/silero_vad.rs:31<br>crates/forge-onnx/tests/silero_vad.rs:97 |
| `FORGE_VERIFY_ATTN_SPLIT8` | crates/forge-kernels/src/launchers/attention.rs:1533 |
| `FORGE_W4A8_ALPHA` | crates/forge-cli/src/main.rs:2343<br>crates/forge-engine/src/model/loader.rs:267<br>crates/forge-engine/src/model/loader.rs:269<br>crates/forge-formats/src/w4a8.rs:209 |

## oprzyrządowanie (flaga CLI)

| zmienna | miejsca użycia |
|---|---|
| `FORGE_BENCH_FIXED_K` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1669 |
| `FORGE_BENCH_HYBRID_MTP` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3477<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3478 |
| `FORGE_BENCH_HYBRID_PREFILL_B2` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2583<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2584 |
| `FORGE_BENCH_HYBRID_PREFILL_SERVER` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2729 |
| `FORGE_BENCH_HYBRID_TARGET` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3563<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3564 |
| `FORGE_BENCH_MAX_ACTIVE` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3487<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3573 |
| `FORGE_BENCH_MTP_B2_FIXED` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3545<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3546 |
| `FORGE_BENCH_MTP_B2_MATRIX` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3513<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3514 |
| `FORGE_BENCH_MTP_NGRAM_B2_MATRIX` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3529<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3530 |
| `FORGE_BENCH_NVFP4_TILE` | crates/forge-cli/src/main.rs:761<br>crates/forge-cli/src/main.rs:765<br>crates/forge-cli/src/main.rs:779<br>crates/forge-cli/src/main.rs:782 |
| `FORGE_BENCH_PREFILL_CPU_FALLBACK` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2782 |
| `FORGE_BENCH_PROMPT` | crates/forge-model/tests/generate_vs_mlx.rs:281<br>crates/forge-model/tests/gguf_loads.rs:131 |
| `FORGE_BENCH_PROMPT_IDS` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3520<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3536<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3552<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3554 |
| `FORGE_BENCH_PROMPT_IDS_SECOND` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1488 |
| `FORGE_BENCH_PROMPT_TOKENS` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2736 |
| `FORGE_BENCH_REPS` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1478<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1484<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1664<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2590 |
| `FORGE_BENCH_SYNTHETIC_PROMPT_TOKENS` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1495 |
| `FORGE_HYBRID_DEBUG` | crates/forge-engine/src/model/mod.rs:1221<br>crates/forge-engine/src/model/mod.rs:2950 |
| `FORGE_LAYER_TRACE` | crates/forge-engine/src/model/debug.rs:4<br>crates/forge-engine/src/model/debug.rs:10 |
| `FORGE_NGRAM_TRACE` | crates/forge-engine/examples/hybrid_ngram_bench.rs:371<br>crates/forge-engine/examples/hybrid_ngram_bench.rs:396 |
| `FORGE_PREFILL_TRACE` | crates/forge-engine/src/model/mod.rs:951<br>crates/forge-engine/src/model/mod.rs:963 |
| `FORGE_PROFILE_HYBRID_PREFILL` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2672 |
| `FORGE_PROFILE_MTP_MATRIX` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1530 |
| `FORGE_PROFILE_SERVER_CONCURRENCY` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:967 |
| `FORGE_PROFILE_SERVER_PREFILL` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:2783 |
| `FORGE_TRACE_ROUTE` | crates/forge-kernels/src/launchers/gemm/dense.rs:352<br>crates/forge-kernels/src/launchers/gemm/dense.rs:428<br>crates/forge-kernels/src/launchers/gemm/dense.rs:746 |

## test (atrybut testu)

| zmienna | miejsca użycia |
|---|---|
| `FORGE_BIELIK_TEST_TOKENS` | crates/forge-server/tests/batched_bielik.rs:413 |
| `FORGE_EMBED_TEST_MODEL` | crates/forge-server/tests/e2e_embeddings.rs:7<br>crates/forge-server/tests/e2e_embeddings.rs:22 |
| `FORGE_HYBRID_INTERLEAVE_AUDIT` | crates/forge-engine/examples/hybrid_ngram_bench.rs:764 |
| `FORGE_MTP_CATCHUP_AUDIT` | crates/forge-engine/examples/hybrid_ngram_bench.rs:758 |
| `FORGE_MTP_INTERLEAVE_AUDIT` | crates/forge-engine/examples/hybrid_ngram_bench.rs:767 |
| `FORGE_NGRAM_STATE_AUDIT` | crates/forge-engine/examples/hybrid_ngram_bench.rs:755 |
| `FORGE_TEST_ACTIVATIONS_MB` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:796 |
| `FORGE_TEST_FORCE_DELTA_KEY_VALUE` | crates/forge-engine/src/model/mod.rs:2866 |
| `FORGE_TEST_HYBRID_GGUF` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:4<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1779<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1780<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:1783 |
| `FORGE_TEST_HYBRID_LAYER_MAJOR_PRIORITY` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3335 |
| `FORGE_TEST_HYBRID_PREFILL_EXPECT_B2` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3105 |
| `FORGE_TEST_HYBRID_PREFILL_PRIORITY` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3235 |
| `FORGE_TEST_HYBRID_PREFILL_SERVER` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3034<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3035<br>crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3184 |
| `FORGE_TEST_MOE_GGUF` | crates/forge-engine/tests/moe_prefill_gpu.rs:4<br>crates/forge-engine/tests/moe_prefill_gpu.rs:79<br>crates/forge-engine/tests/moe_prefill_gpu.rs:80<br>crates/forge-engine/tests/moe_prefill_gpu.rs:83 |
| `FORGE_TEST_MTP_NGRAM_MIXED_SERVER` | crates/forge-engine/tests/hybrid_state_pool_gpu.rs:3016 |
| `FORGE_TP_TEST_MODEL` | crates/forge-engine/tests/tp_shard_load.rs:11<br>crates/forge-engine/tests/tp_shard_load.rs:25<br>crates/forge-engine/tests/tp_shard_load.rs:56 |

