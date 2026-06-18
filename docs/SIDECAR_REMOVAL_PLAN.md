# Plan migracji: usunięcie sidecara QUIC → bezpośredni HTTP (HttpDirect)

Status: PROPOZYCJA po recenzji codex (korekty naniesione). Cel: usunąć
per-kontenerowy `tentaflow-sidecar` (most QUIC↔HTTP) i ustandaryzować wszystkie
usługi na `Transport::HttpDirect`, gdzie Core mówi HTTP wprost do lokalnie
wystawionego portu silnika.

> Weryfikacja: file:line potwierdzone czytaniem kodu (Claude + codex gpt-5.5).

## 1. Uzasadnienie (potwierdzone w kodzie)

- **Topologia**: nikt nie łączy się bezpośrednio z usługą cross-node. Zawsze przez
  node właściciela, a node lokalnie stawia swoje dockery i gada z nimi po loopbacku.
  `endpoint_url` deployu = `quic://127.0.0.1:<port>` / `http://127.0.0.1:<port>`
  (`docker.rs:1041-1055`) — adres loopback, nieużywalny zdalnie.
- **Cross-node potwierdzony jako proxy przez właściciela** (NIE dial loopbacka):
  executor buduje `MeshForward`/`ModelRequest` i forwarduje do właściciela
  (`runtime/executor.rs:855,1334,1727,1891`), peer odbiera w
  `mesh/inference_proxy.rs:38,171` i uruchamia lokalny router/executor.
- **Sidecar QUIC zawsze biegnie po loopbacku** (`handles_cache.rs:284`
  `quic://127.0.0.1:{port}`), `skip_tls_verify=true`, brak CA, brak tokenów —
  warstwa nie dodaje bezpieczeństwa ponad loopback, tylko hop.
- **Sidecar to translator protokołu** (`sidecar/src/roles/reverse_proxy.rs`):
  tłumaczy binarny `ModelRequest`/`ModelResponse`/`ModelStreamChunk` (po iroh) na
  HTTP silnika. Obsługuje wyłącznie `Completion`, `Embeddings`, `Audio` (TTS/STT),
  `PrefixCacheInit` (no-op, `reverse_proxy.rs:381`). Brak wariantu dla wizji (`/detect`).
- **Core ma już równoważny HttpDirect** dla ścieżki inference (`services/backend/client.rs`,
  `BackendClient`): chat unary `chat_completion`, stream `chat_completion_stream`,
  embeddings `embeddings_request`, STT `audio_transcription` (multipart + pole `data`,
  `client.rs:812`), TTS `audio_speech` (`client.rs:872`), vision jako chat-completions
  (`client.rs:958`). STT/TTS i tak są już HTTP-first (STT legacy QUIC usunięty —
  `routing/stt.rs:136`; TTS QUIC „pending cutover" — `executor.rs:1521`).
- **Native deploy już działa bez sidecara**: `python_bundle.rs:490`
  `transport: Transport::HttpDirect`, `sidecar_port: None`. Dowód, że model docelowy
  jest sprawdzony w produkcji.

> UWAGA — parity NIE jest pełny (patrz §5): `service_call`/addony/Flow `Predict` i
> custom API (`/detect`,`/ocr`,`/parse`) NIE mają dziś ścieżki HttpDirect. To trzeba
> dobudować, zanim sidecar zniknie.

Wniosek: przy tej topologii sidecar jest redundantny dla ścieżki inference, ale jego
usunięcie wymaga najpierw dorobienia HttpDirect w `service_call`. Zysk: prostsze
obrazy, likwidacja kruchego buildu Rusta w każdym obrazie (źródło ostatnich awarii),
jeden proces zamiast dwóch + supervisor.

## 2. Zakres zmian

### Kontenery (`tentaflow-containers/`)
- **25 Dockerfile'ów** z `sidecar-builder` (codex: nie 24 — dochodzi
  `ibm/docker/granite-embedding`, `entrypoint.sh:51`): LLM 6 (vllm, vllm-spark, sglang,
  qwen3-vl, ollama, llama-cpp); STT 3 (whisper, parakeet, qwen-asr); TTS 5 (kokoro,
  kyutai-tts, sherpa-onnx, voxcpm, xtts); Vision 4 (nemotron-ocr, nemotron-parse,
  paddle-ocr, nemotron-yolox); Embeddings 2 (nemotron-embed, nemotron-embed-vl);
  Reranker 2 (nemotron-rerank, nemotron-rerank-vl); ImageGen 1 (comfyui); IBM 2
  (granite-embedding + ...). [Zweryfikować dokładny grep `tentaflow-sidecar` w build.]
- 25 `entrypoint.sh` (wzorzec 2-procesowy sidecar+silnik) — usunąć proces sidecara.
- 25 `config.default.toml` (rola sidecara) — usunąć.
- `_services/*.toml`: `transport = "sidecar-quic"` → `"direct-http"`. Dotyczy też
  **manifestów treningowych** `ml-training`, `autogluon-training`
  (`training/_services/*.toml:24`), nawet jeśli nie mają dziś sidecar Dockerfile —
  manifest musi być spójny.
- Crate `tentaflow-containers/sidecar/` — usunąć po migracji (faza 3).

### Przypadki brzegowe
- **comfyui**: manifest już `direct-http` (`comfyui.toml:12 default_port=5000`), ale
  entrypoint NADAL startuje sidecar i słucha na `COMFY_PORT:-8188` — realna
  niespójność do naprawy (port + usunięcie sidecara).
- **teams-bot**: już `direct-http`, `api=custom`, BEZ sidecara w grepie. Nie jest
  migracją sidecara — użyć jako wzorca/testu ścieżki custom-HTTP.
- **MLX / vllm-metal / qwen3-vl-mlx**: native/embedded, bez Dockera — wyłączone z fazy
  kontenerowej; zostają tylko w testach regresji resolvera.

### Core (`tentaflow-core/src/`)
- `services/deploy/docker.rs`:
  - `backend::run` (`:588`): host publish `host_ip: "0.0.0.0"` → `"127.0.0.1"`
    (KRYTYCZNE — bez sidecara to host-publish pilnuje, by port silnika nie wyszedł do
    LAN). Core łączy się przez `http://127.0.0.1:{host_http}` (`:1043`).
  - `pick_transport()` fallback `None => SidecarQuic` (`:104`) → `HttpDirect`.
- **`services/service_call.rs` (NOWA, WYMAGANA praca)**: dziś dispatch wyłącznie przez
  QUIC clients i zawsze buduje `ModelPayload::Completion` z JSON jako promptem
  (`:478,:492`); host fn deleguje tu (`addon/host_functions/service.rs:78`). Dodać
  ścieżkę HttpDirect: dla `ApiKind` OpenAI-compatible przez `BackendClient`, dla
  `ApiKind::Custom` surowy HTTP POST na endpoint usługi (`/detect`,`/ocr`,`/parse`,
  custom). Bez tego addony/Flow `Predict` przestaną działać po wycięciu sidecara.
- `PrefixCacheInit`: sidecar miał no-op (`reverse_proxy.rs:381`); `BackendClient` nie
  ma odpowiednika. Znaleźć nadawców (`flow_engine`/routing) i albo usunąć call, albo
  dać no-op po stronie Core dla HttpDirect.

## 3. Reguła portów i bind (KRYTYCZNA — poprawiona po recenzji)

Dla HttpDirect: `port_map = [(host_http, internal_port, "tcp")]` (`docker.rs:885`),
`PORT=internal_port` w env (`docker.rs:794`), `endpoint_url = http://127.0.0.1:{host_http}`
(+ suffix `api`, `docker.rs:1041`). Docker forwarduje publish-host → port kontenera
`internal_port`.

Dwie reguły, które MUSZĄ być spełnione razem:
1. **Host publish na `127.0.0.1`** (`backend::run`, zmiana z `0.0.0.0`).
2. **Silnik w kontenerze słucha na `0.0.0.0:$PORT`** — NIE `127.0.0.1`. Ruch z
   docker-publish trafia na interfejs sieciowy kontenera, nie na jego loopback; jeśli
   silnik zostanie na `127.0.0.1` (jak dziś, bo sidecar był w tym samym kontenerze),
   request z hosta go NIE dosięgnie. Dziś prawie wszystkie entrypointy bindują silnik
   do `127.0.0.1` (vllm `entrypoint.sh:60`, whisper `:33`) → trzeba zmienić na `0.0.0.0`.

Port `$PORT`: Core wstrzykuje `PORT=internal_port`. Entrypointy muszą słuchać na
`$PORT`, nie własnym `XXX_PORT`:
- `vllm` OK — Core ustawia też `VLLM_PORT=internal_port` (`docker.rs:795`), entrypoint
  używa `VLLM_PORT` (`entrypoint.sh:17`).
- ROZJAZDY do naprawy: `whisper` (`WHISPER_PORT:-8081` vs `default_port=5030`),
  `nemotron-yolox` (`NEMOTRON_YOLOX_PORT:-8086` vs 5086/87/88), oraz
  `parakeet/qwen-asr/sherpa-onnx/kokoro/nemotron-ocr/nemotron-parse/paddle-ocr/comfyui`
  — wszystkie używają własnego `XXX_PORT`, nie `$PORT`.
- Reguła: entrypoint słucha na `${PORT}` (Core = `internal_port`), bind `0.0.0.0`;
  usunąć pośrednie `XXX_PORT` lub dać `PORT="${XXX_PORT:-$PORT}"` z domyślną `$PORT`.
  `default_port` w manifeście = port kontenera (dowolny, byle entrypoint słuchał na
  tym samym `$PORT`); przy okazji ujednolicić trzy nemotron-yolox (5086/87/88 → jeden).

Native: bez zmian (`python_bundle.rs` już używa `${PORT}` + HttpDirect).

## 4. Dialekty API (do rozstrzygnięcia per silnik)

`build_endpoint_url` dla `openai-compatible` dokleja `/v1` (`deploy/mod.rs:1420`).
- **vLLM/sglang/qwen3-vl**: OpenAI `/v1` — OK, `BackendClient` pasuje.
- **llama.cpp**: ryzyko `/v1/v1/...` albo braku `/v1` — sidecar dla `LlamaCpp` doklejał
  `/v1/chat/completions` do upstreamu bez `/v1` (`reverse_proxy.rs:113`). Potwierdzić
  `ApiKind` w manifeście llama-cpp i bazę endpointu po migracji.
- **sherpa-onnx (TTS)**: manifest `api="sherpa-tts"` (`sherpa-onnx.toml:13`), nie
  OpenAI; `BackendClient` zawsze buduje `/audio/speech` (`client.rs:184`). Potwierdzić,
  czy wrapper sherpa wystawia OpenAI `/audio/speech`, czy trzeba adaptera.
- **Vision/custom (`api=custom`)**: nie OpenAI — obsługa przez nowy custom-HTTP dispatch
  w `service_call` (§2).

## 5. Kroki (fazowane, każda faza samodzielnie testowalna)

### Faza 0 — Core: fundamenty HttpDirect (BLOKUJĄCA dla reszty)
1. `docker.rs backend::run`: host publish `host_ip` → `127.0.0.1`.
2. `docker.rs pick_transport`: fallback `None => HttpDirect`.
3. **`service_call.rs`: dodać dispatch HttpDirect** (OpenAI przez `BackendClient`;
   `ApiKind::Custom` → surowy HTTP POST). To warunek konieczny — bez tego Flow
   `Predict`/addony padną po migracji.
4. `PrefixCacheInit`: rozstrzygnąć (usunąć nadawcę albo no-op po stronie Core).
5. Test: deploy usługi z `transport="direct-http"` (np. ollama), Core dociera po HTTP,
   port niewidoczny w LAN; addon/Predict do usługi działa.

### Faza 1 — pilotaż na jednym silniku (whisper LUB vllm)
6. Manifest: `transport = "direct-http"`.
7. `entrypoint.sh`: usuń proces sidecara; silnik na `${PORT}`, bind `0.0.0.0`;
   healthcheck `pgrep tentaflow-sidecar` → HTTP `/health` (zmapować, czy silnik ma
   `/health` — vLLM ma; whisper-server zweryfikować).
8. `Dockerfile`: usuń stage `sidecar-builder` + `COPY --from=sidecar-builder` +
   `COPY tentaflow-containers/sidecar` + (jeśli zbędne) `COPY tentaflow-protocol/
   transport/vendor`. Usuń `config.default.toml` sidecara.
9. Rebuild + deploy + test E2E (chat/transcribe + Predict/addon). Potwierdź
   endpoint_url=http://... i poprawny dialekt API.

### Faza 2 — reszta silników (per kategoria)
10. Powtórz Fazę 1 dla pozostałych obrazów (LLM → STT → TTS → embeddings → reranker →
    vision → comfyui/granite). Po każdej kategorii rebuild+test. Napraw rozjazd
    manifest↔entrypoint (comfyui, ollama).
11. Rozstrzygnij manifesty treningowe (`ml-training`, `autogluon-training`).

### Faza 3 — sprzątanie martwego kodu QUIC (osobny, ostrożny PR z `cargo check`)
12. Usuń crate `tentaflow-containers/sidecar/`.
13. Usuń martwy kod QUIC. **Lista (rozszerzona przez codex — niekompletna w v1):**
    `services/runtime/quic_handle.rs`, QUIC dispatch w `services/runtime/executor.rs`,
    `BackendHandle::Quic` w `services/handles_cache.rs`, reconnect loop w
    `services/supervisor.rs`, `net/iroh_client/*`, `services/runtime/transport_client.rs`,
    `services/service_call.rs` (część QUIC), `routing/middleware.rs`,
    `routing/chat.rs` (memory path), `routing/stt.rs` (legacy QUIC speaker/memory),
    `api/dashboard/handlers_browser.rs`, `flow_engine/dispatchers_impl/memory_impl.rs`,
    `flow_engine/dispatchers_impl/quic_finder.rs`, `services/transport.rs`
    (`SidecarQuic`), `services/manifest/types.rs` (`DockerTransport::SidecarQuic`),
    oraz pola/DB: `sidecar_quic_port` w `services_repo/services.rs`, migracje/schema,
    `snapshot_builder.rs`, `registry.rs`, `mesh_registry.rs`.
14. Build context Dockera: gdy ŻADEN Dockerfile nie kopiuje już
    `tentaflow-protocol/transport/vendor/sidecar`, build context może wrócić z
    bundle-root do katalogu Dockerfile'a; `COPY` w searxng/browser-renderer wraca do
    lokalnych ścieżek; bundle (`build.rs`/`deploy/bundle.rs`) nie musi już pakować
    crate'ów Rust do kontekstu dockerów.

## 6. Ryzyka / status (po recenzji)

- **R1 (POTWIERDZONE, nie „do potwierdzenia")**: `service_call`/Flow `Predict`/addony
  są QUIC-only i wysyłają `Completion`. Custom vision `/detect` nie działa po QUIC i
  nie zadziała po HttpDirect bez nowego custom-HTTP dispatchera. → Faza 0 krok 3.
- **R2 (sherpa/llama dialekty)**: §4 — potwierdzić bazę endpointu i adaptery.
- **R3 (cross-node) — OBALONE jako ryzyko**: zdalny Core NIE dialuje cudzego
  loopbacka; idzie przez `MeshForward`→`inference_proxy`→lokalny executor. OK.
- **R4 (`PrefixCacheInit`)**: brak odpowiednika w HttpDirect — rozstrzygnąć (§2).
- **R5 (healthcheck)**: zamiana `pgrep` → HTTP `/health`; zmapować per silnik.
- **R6 (paritet translacji)**: BackendClient ma odpowiedniki (STT `data=`, „pusty
  stream→Error" przez circuit breaker/own handling) — potwierdzić per modalność.

## 7. Rollback

Każda faza per-obraz odwracalna: przywrócenie `transport="sidecar-quic"` + starego
Dockerfile/entrypoint/config.default.toml z gita. Core zachowuje `SidecarQuic` aż do
Fazy 3, więc rollback nie wymaga zmian w Core. UWAGA: Faza 0 (HttpDirect w
`service_call`) jest addytywna — nie psuje ścieżki QUIC, więc bezpieczna przed
pilotażem.
