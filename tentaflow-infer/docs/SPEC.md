# Specyfikacja: uniwersalny, multivendorowy silnik inferencji AI
**Kryptonim roboczy: FORGE** — Rust (systemy) + Mojo (kernele GPU)

Wersja 0.1 · lipiec 2026

> Dokument źródłowy dostarczony przez właściciela projektu. Plan realizacji: `docs/PLAN.md`.

---

## 1. Cele i kryteria sukcesu

### 1.1. Cel nadrzędny
Jeden silnik inferencji obsługujący **wszystkie modalności** (LLM, TTS, STT, text-to-image, video) na **wszystkich głównych GPU** (NVIDIA, AMD, Intel, Apple Silicon) i CPU, z wydajnością przewyższającą llama.cpp i vLLM zarówno w **prefill** (throughput tokenów wejściowych), jak i **decode** (latencja i throughput generacji).

### 1.2. Mierzalne cele wydajnościowe (v1.0)
| Metryka | Cel | Baseline |
|---|---|---|
| Prefill throughput (tok/s, batch) | ≥ 1.15× vLLM | vLLM + FlashAttention-3 |
| Decode throughput (tok/s, batch 64+) | ≥ 1.10× vLLM | vLLM continuous batching |
| TTFT p50 (single user, 7B, 4k ctx) | ≤ 0.85× llama.cpp | llama.cpp na tym samym GPU |
| ITL (inter-token latency) p99 | ≤ 0.90× vLLM | pod SLO-aware schedulerem |
| Speculative + n-gram łącznie | ≥ 2.2× acceptance-adjusted speedup | pojedynczy proposer ~1.6-1.9× |
| Skalowanie multi-node (RoCE, TP=8×2 nody) | ≥ 88% efektywności | NCCL baseline |
| Narzut serwera (QUIC + CBOR) | < 300 µs p99 per request | HTTP/JSON ~1-3 ms |

### 1.3. Anty-cele (świadome wykluczenia w v1)
- Trening / fine-tuning (tylko inferencja; LoRA — tak, ale tylko serwowanie adapterów).
- TPU / trainium / własne ASIC-i — dopiero v2, przez warstwę HAL.
- Windows jako serwer produkcyjny (klient/dev — tak; produkcja: Linux, macOS dla Apple Silicon).

---

## 2. Architektura wysokopoziomowa

```
┌────────────────────────────────────────────────────────────┐
│  SERVING LAYER (Rust)                                      │
│  HTTP/1.1+2 (OpenAI API, Anthropic API) │ QUIC "FORGE-RPC" │
│  Router · AuthN/Z · Rate limit · Multi-model registry      │
├────────────────────────────────────────────────────────────┤
│  ORCHESTRATOR (Rust)                                       │
│  Continuous batching scheduler · SLO/priority queues       │
│  KV Cache Manager (paged, prefix, offload)                 │
│  Speculation Coordinator (draft/MTP/EAGLE + n-gram)        │
│  Multi-node control plane (raft-lite, health, placement)   │
├────────────────────────────────────────────────────────────┤
│  EXECUTION LAYER                                           │
│  Graph IR + compiler (Rust) → kernele (Mojo)               │
│  Modality engines: LLM · TTS · STT · T2I · Video           │
│  Loadery: GGUF · safetensors/HF · ONNX                     │
├────────────────────────────────────────────────────────────┤
│  HAL — Hardware Abstraction Layer (Rust traits + Mojo)     │
│  CUDA │ ROCm/HIP │ Level Zero │ Metal │ CPU (AVX-512/NEON) │
│  Komunikatory: NCCL │ RCCL │ oneCCL │ własny "ForgeCCL"    │
│  Transport: NVLink/xGMI · PCIe P2P · RoCE v2 · TCP LAN     │
└────────────────────────────────────────────────────────────┘
```

### 2.1. Podział Rust / Mojo
**Rust (ok. 70% kodu):** control plane: scheduler, KV manager, pamięć, networking, serwery, loadery formatów, tokenizery, sampling na CPU, telemetria; FFI do runtime'ów vendorów — alokacje, strumienie, grafy, kopie.

**Mojo (ok. 30% kodu, krytyczne):** wszystkie kernele GPU: attention (flash-style, paged), GEMM/GEMV epilogi, dequant (K-quants, AWQ, GPTQ, FP8, FP4), MoE dispatch/combine, RoPE, norm, sampling na GPU, kernele conv/attention dla diffusion i audio.

**Polityka twarda: 100% kerneli w Mojo, jedna baza kodu.** Jeden kernel, trzy vendory (PTX / AMDGPU / Metal-AIR), tuning per arch przez metaprogramowanie.

**Intel = Tier 1.5 z bramką decyzyjną** (upstream Modular / własny MLIR→SPIR-V / pomost oneDNN — decyzja koniec Fazy 2).

**Interfejs Rust↔Mojo:** stabilne C ABI; kernele AOT per (arch, dtype, wariant); zero runtime'u Mojo w hot path; metadane w manifeście JSON. (Zrealizowane: ADR-0001.)

### 2.2. Ryzyko nr 1 i plan B
Dojrzałość Mojo na AMD/Intel. Mitygacja: kernel registry ze slotami wielu implementacji + awaryjny slot vendor-lib (cuBLASLt/hipBLASLt/MPS) wyłącznie jako bezpiecznik GEMM; ciągły benchmark vs biblioteki vendorów w CI; kwartalny vendor gate review.

---

## 3. HAL — warstwa abstrakcji sprzętu

### 3.1. Rust traits (rdzeń)
```rust
trait Device {
    fn alloc(&self, bytes: usize, kind: MemKind) -> DevPtr;
    fn stream(&self) -> Stream;
    fn capture_graph(&self, f: impl FnOnce(&Stream)) -> ExecGraph;
    fn launch(&self, k: &KernelHandle, grid: Grid, args: &Args, s: &Stream);
    fn copy(&self, src: AnyPtr, dst: AnyPtr, s: &Stream);
    fn event(&self) -> Event;
    fn caps(&self) -> DeviceCaps;
}
```
Backendy: `cuda` (cudarc), `rocm` (hip-sys), `level-zero`, `metal` (objc2-metal), `cpu` (rayon + intrinsics).

### 3.2. Zasady krytyczne dla wydajności
- **Graph capture wszędzie**: decode step jako przechwycony graf per (batch-bucket, model); bucketowanie 1,2,4,...,256.
- **Jeden proces per GPU** (worker), shared memory ring buffers + kolektywy.
- **Alokator**: arena/slab na VRAM (bez cudaMalloc w hot path); pule: wagi (statyczne), KV (paged), aktywacje (ring).
- **Multivendor w węźle**: tylko na granicy PP lub replik; transfer pinned-host bounce.

### 3.3. Komunikatory
NCCL/RCCL/oneCCL przez trait `Collectives`; **ForgeCCL** (Rust) dla grup mieszanych (send/recv, PP), TCP LAN, RoCE v2 (ibverbs, GPUDirect); Apple: unified memory, multi-Mac przez TCP/TB.

---

## 4. Warstwa wykonania modeli

### 4.1. Graph IR i kompilator (Rust)
Lekki IR (~80 opów), passy: constant folding, fuzje (norm+linear, dequant+gemm, SiLU-mul, rope+attention-pack), layout planning, plan pamięci, punkty komunikacji TP/PP/EP. Lowering: op → kernel registry (op, dtype, quant, arch, kształt-bucket) → autotuner (offline cache + online micro-benchmark).

### 4.2. Formaty modeli
GGUF (pełne wsparcie, mmap zero-copy, wszystkie K/IQ/legacy quanty); safetensors + HF config (primary dla LLM/diffusion, deklaratywny rejestr architektur); ONNX (STT/TTS/encodery, subset opset 17+). Kwantyzacje: GGUF K/IQ, GPTQ, AWQ, FP8, INT8 W8A8, **NVFP4/MXFP4**, NF4.

**NVFP4 wszędzie (twarde):** (a) natywnie na Blackwell+; (b) programowo — fused dequant (e2m1 + skale FP8-E4M3 per 16 + skala tensora) → BF16/FP16 w GEMM/GEMV. Wybór z `DeviceCaps`, wspólny golden-test.

Dzień 1 architektur: Llama, Qwen, Mistral/Mixtral, DeepSeek (MLA), Gemma, Phi, Whisper, TTS, SDXL/Flux, video.

### 4.3. Silniki modalności
Wspólny rdzeń + cienkie silniki: LLM (pełny stack), STT (chunked encoder + stanowy decoder, VAD), TTS (LM część przez pipeline LLM + vocoder), T2I (scheduler krokowy), Embeddings/reranking (padding-free packing, bez KV), preprocessing multimodalny (deterministyczne hashowanie → prefix-cache), Video (rozumienie v1, generacja v1.5). **Jeden scheduler zasobów na GPU dla wszystkich modalności** (LLM decode preemptuje diffusion między iteracjami).

### 4.4. Kernele — 100% Mojo
Rodzina attention: flash prefill/decode (MHA/GQA/MQA), MLA, sliding window + sinks, cross/bidirectional, sparse/selected (KV poza VRAM), linear/SSM scan (v1.1). Fused dequant-GEMM/GEMV (cel ≥ 92% peak DRAM BW w decode). Fused MoE (≤ 3 kernele). Sampling GPU (temp/top-k/top-p/min-p/typical + penalties + grammar mask). Norm/RoPE/aktywacje w epilogach.

---

## 5. Scheduler i pamięć

### 5.1. Continuous batching + chunked prefill
Iteracyjny scheduler poziomu tokena; SLO-aware dual queue (latency/throughput, `priority`, `slo.itl_ms`, `slo.ttft_ms`); preempcja recompute/swap; opcjonalna dezagregacja prefill/decode.

### 5.2. KV Cache Manager
Paged KV (16-64 tok.), copy-on-write, radix-tree prefix caching, cache-aware routing; kwantyzacja KV (5.5); MLA latent cache.

### 5.3. Pamięć wag
mmap + direct-to-GPU (GDS), 70B < 20 s z NVMe; hot-swap modeli i multi-LoRA (S-LoRA).

### 5.4. Tiered memory: VRAM → RAM → SSD (wagi ORAZ KV)
`TierManager`: WeightTiering (MoE expert streaming Colibri-style: pinned VRAM / RAM LRU / SSD; heat map + repin z histerezą ~25%; prefetch predykcyjny, hit-rate ≥ 95%) i KVTiering (hot/warm/cold; **agregacja stron w chunki 4-16 MB** append-only — nie 1:1 strony na SSD; reguła transfer-vs-recompute per chunk; trwałe sesje KV opt-in, bitowo zgodne; tier 4 v1.5: zewnętrzny store RDMA).

### 5.5. Kwantyzacja KV — drabinka
FP16/BF16 → FP8 (domyślny kandydat) → INT8 per-channel → INT4/NVFP4-KV → rotacyjne ~3-bit (TurboQuant-class; rotacja fused write-time, residual window 128-256 tok. FP16/FP8, aktywacja od ~4K ctx, walidacja per head_dim). Wszystko w Mojo, rotacja jako parametr compile-time. Bramka PPL/long-context w CI.

---

## 6. Spekulacja: komponowalne propozery + wspólna weryfikacja

### 6.1. Wspólny kontrakt

`trait Proposer { fn propose(&mut self, ctx, budget) -> Result<DraftTree>; }`

`DraftTree` składa się z topologicznie uporządkowanych `DraftNode`. Każdy
węzeł zawiera token, indeks rodzica, głębokość, `source`, opcjonalny
`proposal_logprob` (`q`) i opcjonalny `conditional_confidence`. Kontrakt
reprezentuje łańcuch lub wiele gałęzi bez kopiowania wspólnych prefiksów.
`proposal_logprob` jest obowiązkowy dla akceptacji stochastycznej; sama etykieta
tokenu wystarcza wyłącznie dla ścieżki greedy.

**Zasada twarda: jedna wspólna, lossless weryfikacja targetu.** Ten sam verifier
obsługuje wszystkie propozery i ich kompozycje:

- greedy: akceptuje najdłuższy prefiks zgodny z argmax targetu i zachowuje wynik
  identyczny z sekwencyjnym greedy;
- sampling: stosuje standardową akceptację względem `p/q`, a po odrzuceniu losuje
  z rozkładu resztowego, zachowując dokładnie rozkład targetu;
- drzewo: wykonuje jeden forward z maską tree-attention, mapuje logity targetu na
  węzły i zatwierdza tylko wybraną ścieżkę KV.

Kompozycja działa jako **kaskada** (proposer neuralny generuje rdzeń, a n-gram lub
suffix przedłuża zaakceptowany prefiks) albo **drzewo** (propozery dostarczają
gałęzie, koordynator deduplikuje prefiksy). N-gram pozostaje opcjonalnym, tanim
fallbackiem lub rozszerzeniem każdego proposera, a nie osobną pętlą weryfikacji.
`SpeculativeConfig::chain` przechowuje uporządkowaną listę proposerów. Odrzuca
pusty łańcuch i duplikaty, a `NgramProposer`, jeśli występuje, musi być ostatnim
rozszerzeniem. Bieżące CLI tworzy wyłącznie łańcuch jednoelementowy n-gram;
łańcuchy neuralne są na razie dostępne tylko w typowanym API i kończą się
jawnym `Unsupported` przed uruchomieniem silnika.
Budżet węzłów jest adaptacyjny: statystyki akceptacji i rzeczywisty koszt są
liczone per `source`, a proposer jest usypiany, gdy zmierzony speedup spada poniżej
1.05.

### 6.2. Rodziny proposerów

- `NgramProposer` i opcjonalny `SuffixProposer`: bez treningu, używają historii
  bieżącej sekwencji lub indeksu sufiksów; służą też do przedłużania innych draftów.
- `DraftModelProposer`: mały autoregresyjny model zgodny z tokenizerem targetu.
- `MTPProposer`: wykorzystuje głowy Multi-Token Prediction/NextN dostarczone z
  modelem; liczba głów i układ tensora wynikają z manifestu checkpointu.
- `Eagle3Proposer`: bezpośrednio przewiduje tokeny z fuzji cech wielu warstw
  targetu; wymaga checkpointu trenowanego dla wskazanego modelu targetowego.
- `DFlashProposer`: lekki block-diffusion drafter generujący blok równolegle i
  kondycjonowany cechami/KV targetu.
- `DSparkProposer`: półautoregresyjny drafter łączący równoległy backbone z lekką
  sekwencyjną głową Markova albo RNN. Osobna głowa confidence przewiduje
  warunkowe prawdopodobieństwo przeżycia każdej pozycji; Sequential Temperature
  Scaling (STS) kalibruje skumulowane prawdopodobieństwa prefiksów. Scheduler
  dobiera długość weryfikacji per żądanie na podstawie tych prawdopodobieństw,
  bieżącego obciążenia i sprofilowanej krzywej throughputu verifiera. Decyzja jest
  przyczynowa i kończy rozszerzanie prefiksu natychmiast po spadku oczekiwanego
  throughputu, aby nie wprowadzić selection bias.
- `PardProposer` (opcjonalny): równoległy draft adaptowany z modelu
  autoregresyjnego, współdzielony przez zgodną rodzinę targetów.

### 6.3. Checkpointy i licencje

Każdy neuralny proposer jest osobnym artefaktem opisanym przez
`forge-speculation.json`. Manifest zawiera wersję formatu i rodzaj proposera,
źródło, licencje, opis targetu, fingerprinty targetu i tokenizera, tryb kompozycji,
limity drzewa, artefakty i mapowanie tensorów, współdzielone tensory, warstwy i
wymiary cech targetu, dtype, kwantyzację, parametry bloku/dyfuzji/kalibracji oraz
obsługiwane tryby samplingu.

Parser odrzuca nieznane pola, nieprawidłowe lub zduplikowane mapowania, niezgodne
wymiary i wymagania per rodzaj proposera. `SpeculationManifest::load` dodatkowo
kanonikalizuje każdą względną ścieżkę, nie pozwala wyjść poza katalog manifestu,
porównuje SHA-256 i zwraca otwarte, zweryfikowane uchwyty artefaktów. Fingerprinty
targetu i tokenizera są obecnie sprawdzane jako poprawne SHA-256; porównanie ich z
załadowanym modelem nastąpi przy podłączeniu neuralnego runtime.

Manifest podaje też źródło, SPDX ID licencji kodu i wag, warunki redystrybucji
oraz wymagane attribution. Import nie oznacza prawa do redystrybucji: artefakty o
niezgodnej albo nieznanej licencji mogą działać lokalnie tylko zgodnie z ich
warunkami i nie trafiają do obrazów ani wydań FORGE.

### 6.4. Stan realizacji

Obecnie działają liniowy `NgramProposer` oraz natywne MTP/NextN dla gęstego
hybrydowego GGUF `qwen35`. Natywna ścieżka rozpoznaje
`nextn_predict_layers`, wydziela głowę MTP z trunku i współdzieli embedding oraz
głowę wyjściową targetu, jeśli GGUF nie dostarcza ich osobnych wersji. Proposer,
weryfikacja draftu, argmax oraz checkpointy KV i DeltaNet wykonują się na GPU;
retained checkpointy pozwalają zatwierdzić stan bez ponownego skanu warstw
DeltaNet. W trybie z budżetem 3 scheduler adaptacyjnie wybiera K=2 lub K=3 na
podstawie zmierzonego tempa zaakceptowanych tokenów.

Natywne MTP zachowuje wynik sekwencyjnego greedy i obecnie wymaga
`temperature=0`, próbkowania GPU i braku repetition penalty. `max_active > 1`
jest dopuszczane po atomowym startup preflightcie i działa przez seryjnie
przeplatany forward per sekwencja. Target DeltaNet oraz draft MTP mają
izolowany stan per sekwencja i wspólną pulę stron MTP. Zostało przetestowane na
CUDA/RTX 4090 z `protoLabsAI/ThinkingCap-Qwen3.6-27B-MTP-GGUF`; wspólne źródła
Mojo są przygotowane do dalszego codegenu, ale nie stanowią dowodu uruchomienia
na AMD ani Metal. Szczegółowy wynik znajduje się w
`docs/BENCH_QWEN35_MTP_NVFP4.md`.

Fundament nadal obejmuje typowane `DraftTree`/`DraftNode`, walidację topologii,
`ProposerKind`, `SpeculativeConfig`, `SpeculationCoordinator`, kaskadową
kompozycję liniową, atrybucję statystyk per proposer i parser manifestu.
Reprezentacja przyjmuje gałęzie, lecz bieżący verifier zwraca `Unsupported` dla
draftu rozgałęzionego. `draft-model`, `eagle`, `dflash` i `dspark` pozostają
typowanymi konfiguracjami bez wykonawczego proposera. Weryfikacja drzewiasta i
stochastyczna oraz PARD/Suffix nie są jeszcze zaimplementowane.

Źródła algorytmiczne: [DSpark](https://arxiv.org/abs/2607.05147),
[DFlash](https://arxiv.org/abs/2602.06036),
[EAGLE-3](https://arxiv.org/abs/2503.01840),
[PARD](https://arxiv.org/abs/2504.18583).

---

## 7. Równoległość i multi-node

TP (jednorodne GPU), PP (microbatch interleaving, mieszane vendory OK), EP (all-to-all, hierarchiczny ForgeCCL), DP + cache-aware router. Control plane: gossip + lease, placement solver. Data plane: RoCE v2 (ibverbs, GPUDirect, ECN/PFC) / TCP fallback (io_uring, striping). Macierz v1: NVIDIA/AMD pełne; Intel gated; Apple PP/TCP; mieszane vendory PP/DP.

---

## 8. Warstwa serwowania

### 8.1. HTTP (axum/hyper)
OpenAI-compatible: `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/audio/*`, `/v1/images/generations`, `/v1/models`; SSE; tool calling; `response_format` JSON-schema; logprobs. Anthropic-compatible: `/v1/messages` + streaming eventy.

### 8.1.1. Chat templating (minijinja, HF pycompat) + tool calling
Źródła szablonu: override → tokenizer_config → GGUF → rejestr wbudowanych (ChatML, Llama-2/3, Mistral, Gemma, Phi, Vicuna, Alpaca, Command-R, DeepSeek, Qwen, harmony). Pełny kontrakt `apply_chat_template`. Parsery tool-calls strumieniowe (Hermes/Qwen, Llama 3.x, Mistral, DeepSeek, Command-R, harmony, JSON). Reasoning: `<think>` → `reasoning_content`. `tool_choice` z constrained decoding (gramatyka na GPU). CI: golden vs `transformers.apply_chat_template` top-50 modeli; fuzzing; sandbox renderu.

### 8.1.2. Kompletność API generacji
Stop sequences z holdback; detokenizacja inkrementalna UTF-8-safe; `logit_bias`, banned strings, `min_tokens`, `n`/best-of (CoW KV), `top_logprobs`, `echo`, seed. Constrained decoding: JSON Schema / regex / EBNF-CFG — wspólny automat, maska GPU, cache. Prompt caching jawny (`cache_control`/`prompt_cache_key` → radix + trwałe sesje), `cache_read_tokens` w usage.

### 8.2. FORGE-RPC: QUIC + CBOR
quinn, 0-RTT, 1 request = 1 strumień bidi, DATAGRAM opcjonalnie. CBOR z kluczami całkowitymi, dane masowe jako byte string (packed), CBOR Sequence (RFC 8742), CDDL spec. Typy: Hello/Caps, Infer, TokenDelta (batch ≤ 4 ms), AudioChunk, ImageTile, Cancel, KVSessionAttach, Stats. Cele: TokenDelta < 2 µs, narzut < 300 µs p99. SDK: Rust, Python, TypeScript.

### 8.3. Operacyjność
Prometheus/OTel (TTFT, ITL, acceptance, KV hit-rate, VRAM), `/healthz`, graceful drain, hot reload.

### 8.4. Realtime API (voice-to-voice)
Duplex WS + FORGE-RPC/QUIC DATAGRAM; VAD → STT → LLM → TTS w JEDNYM schedulerze; barge-in; cel: end-of-speech → pierwsze audio < 300 ms p50 na 4090.

### 8.5. Batch / offline API
Asynchroniczne joby (JSONL), kolejka throughput, limity per tenant.

---

## 9. Produkcja

### 9.1. Admission control i backpressure
Projekcja KV/aktywacji przed przyjęciem (429 + `Retry-After` zamiast OOM); limity per klasa SLO i tenant; load shedding; degradacja: najpierw drzewa spekulacji, potem chunk-budżet, potem batch API — nigdy ITL latency.

### 9.2. Odporność
Worker GPU = proces: crash → respawn, re-attach wag, odtworzenie z trwałych sesji KV lub recompute; health per GPU (ECC, throttling, watchdog); graceful drain z migracją sesji.

### 9.3. Multi-tenancy
API keys + OIDC/JWT; mTLS/token w Hello (FORGE-RPC); kwoty; fair-share (ważony DRR na iteracji tokenowej); izolacja prefix-cache per tenant (hash z solą).

### 9.4. Zarządzanie modelami i UX
`forge pull|run|serve|bench|convert`; HF Hub (gated, resume); **auto-planner** (model + GPU → plan TP/PP/EP, KV quant, budżety tierów, buckety — domyślny UX); `forge convert` (kwantyzator offline).

### 9.5. Dystrybucja i bezpieczeństwo
Obrazy OCI, pakiet Pythona, binarki CLI; SemVer. Loadery = granica zaufania: fuzzing, sandbox Jinja, podpisy artefaktów, SBOM.

---

## 10. Jakość, testy, benchmarki
lm-eval-harness gate w CI (kwantyzacja × spekulacja × KV-quant); golden tests per model per vendor (logity vs FP32 CPU); PPL gate; nightly benchmark farm, regresja > 2% blokuje merge; publiczny dashboard vs vLLM/SGLang/llama.cpp/TRT-LLM; fuzzing loaderów; determinizm opt-in.

---

## 11. Plan realizacji — patrz `docs/PLAN.md` (mapowanie faz spec → chunki implementacyjne).

Fazy spec: F0 fundamenty (HAL, IR, loadery, bench harness) → F1 LLM single-GPU NVIDIA+AMD (exit ≥ 1.0× vLLM) → F2 spekulacja + TP + Intel/Apple (exit ≥ 1.10× vLLM decode) → F3 multi-node + MoE + modalności (exit: skalowanie ≥ 88%, MoE 700B na 24 GB + RAM + NVMe, Whisper RTF ≥ 40×) → F4 T2I/video, hardening, v1.0.

### Top ryzyka
Mojo parity na AMD/Intel (registry + gate'y); brak targetu Intel (bramka 3-ścieżkowa); rozmycie fokusa (exit-criteria per faza); RoCE misconfig (fallback TCP + `forge-netcheck`); zgodność OpenAI API (testy kontraktowe vs vLLM).

## Załącznik A: szkic CDDL dla FORGE-RPC (fragment)
```cddl
message = infer-request / token-delta / audio-chunk / cancel / kv-attach / hello
infer-request = { 0: 1, 1: tstr, 2: uint, 3: content, 4: sampling-params,
  ? 5: uint, ? 6: uint, ? 7: bool }
token-delta = { 0: 2, 2: uint, 8: bstr, ? 9: tstr, ? 10: bstr, ? 11: uint, ? 12: usage }
content = { 20: tstr } / { 21: bstr } / { 22: audio } / { 23: image } / { 24: [+ chat-msg] }
```

## Załącznik B: wymagania sieciowe RoCE (skrót)
NIC ≥ 100 GbE RoCE v2, PFC/DCQCN/ECN, MTU 4096+, GPUDirect RDMA; fallback: pinned-host staging + TCP striping.
