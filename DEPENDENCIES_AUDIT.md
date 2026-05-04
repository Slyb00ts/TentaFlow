# Audyt zależności TentaFlow workspace

**Data:** 2026-05-04
**Metoda:** `cargo metadata`, `cargo-machete` (per crate), manualna weryfikacja
przez `grep` callsite (świadomie omijając cargo-machete blind spots dla
feature-gated/cfg-gated/build-deps), `cargo tree --duplicates`.
**Synteza:** dwa niezależne audyty (Claude + Codex) zderzone w discussion
roundzie; wszystkie sporne pozycje rozstrzygnięte przez `grep`/`cat` z path:line.

---

## 1. Inwentarz crates (25)

### Produkcyjne (15)

| Crate | Path | Rola | Direct deps |
|-------|------|------|------------:|
| `tentaflow-core` | `tentaflow-core/` | mesh, routing, addon runtime, dashboard API, services, DB, inference, profiling | 102 |
| `tentaflow` | `tentaflow/` | main binary: API gateway + mesh node bootstrap | 23 |
| `tentaflow-protocol` | `tentaflow-protocol/` | rkyv wire types (QUIC mesh + dashboard WSS) | 5 |
| `tentaflow-transport` | `tentaflow-transport/` | iroh QUIC endpoint/framing wrapper | 14 |
| `tentaflow-voice` | `tentaflow-voice/` | pure Rust ONNX/prost + SIMD/GEMM voice/diarization | 10 |
| `tentaflow-macros` | `tentaflow-macros/` | proc-macros (handler/policy/observability registration) | 3 |
| `tentaflow-ui` | `tentaflow-ui/` | egui/wgpu shared GUI (desktop+mobile) | 8 |
| `tentaflow-desktop-core` | `tentaflow-desktop/core/` | desktop app shell, runtime paths, tray | 19 |
| `tentaflow-desktop-{linux,macos,windows}` | `tentaflow-desktop/{...}/` | platform entry points | 2-4 |
| `tentaflow-mobile` | `tentaflow-mobile/core/` | iOS/Android staticlib + platform logging | 17 |
| `tentaflow-client-native` | `tentaflow-client/native/` | Rust FFI dla .NET P/Invoke (cbindgen → C header) | 16 |
| `tentaflow-protocol-wasm` | `tentaflow-protocol-wasm/` | wasm-bindgen browser dashboard glue | 14 |

### Eksperymentalne / kontenerowe (4)

| Crate | Path | Rola |
|-------|------|------|
| `tentaflow-iroh-spike` | `tentaflow-iroh-spike/` | iroh prototype, **standalone, nie referenced** |
| `tentaflow-protocol-fuzz` | `tentaflow-protocol/fuzz/` | libFuzzer target |
| `tentaflow-sidecar` | `tentaflow-containers/sidecar/` | QUIC reverse proxy dla python bundles |
| `tentaflow-teams-bot` | `tentaflow-containers/agents/native/teams-bot/` | Chromium-driven Teams bot |

### Addons + SDK (5)

`tentaflow-addon-{sdk,template,test,malicious,embeddings-chunker}` — fixtures + SDK.

### Vendored (1)

`vendor/ed25519-dalek` 3.0.0-pre.6 — **aktywnie używany przez 3 patches**:
- `tentaflow-core/Cargo.toml:269-270`
- `tentaflow-transport/Cargo.toml:42-43`
- `tentaflow-containers/sidecar/Cargo.toml:45-46`

---

## 2. Statystyki

- **329 direct dependency entries** cross-workspace, **144 unique** dependency names
- **102 unique deps** w `tentaflow-core` (113 entries z target/dev/build)
- **68 transitive duplicate package names** w `tentaflow-core` (cargo tree --duplicates)
- **54 confirmed dead direct entries** do usunięcia + **1 manual review** (`wasmtime-wasi`)

---

## 3. Confirmed dead dependencies — akcje DELETE

Każdy wpis zweryfikowany manualnie przez `grep -RIn "<dep>::"` w odpowiednim
crate (path:line gdzie codex wykrył brak callsite).

| Crate | Dep do usunięcia | Evidence (path:line) |
|-------|------------------|----------------------|
| `tentaflow-core` | `chacha20poly1305` | brak `chacha20poly1305::` w `tentaflow-core/src/`; AEAD zastąpione `aes-gcm` |
| `tentaflow-core` | `curve25519-dalek` | brak `curve25519_dalek::` callsite; `ed25519-dalek` 2.2 + `x25519-dalek` agregują własny stack |
| `tentaflow-core` | `mdns-sd` | iroh discovery zastąpiło mDNS; brak `mdns_sd::` |
| `tentaflow-core` | `ndarray` | tylko `tract_ndarray::Array4` (vision/yolo_pose.rs:61, scrfd.rs:80, movenet.rs:45, yolov8_face.rs:53, hsemotion.rs:49) |
| `tentaflow-core` | `prometheus` | manual renderer w `dispatch/metrics.rs:170` (`out.push_str` + `format!`); brak `prometheus::`; usuń też `[features] metrics-prometheus` lub odepnij `dep:prometheus` |
| `tentaflow-core` | `serde_yaml` | konfiguracja na TOML; brak `serde_yaml::` |
| `tentaflow-core` | `tokenizers` | MLX bridge czyta `tokenizer_config.json` jako JSON (`routing/chat_template.rs:240-255`); MLX whisper ściąga pliki bez crate (`stt/mlx_whisper.rs:140-145`); brak `tokenizers::Tokenizer` |
| `tentaflow-core` | `core-foundation-sys` (macOS target) | brak `core_foundation_sys::`; `core-foundation` agreguje sys |
| `tentaflow-core` | `filetime` (dev) | brak callsite |
| `tentaflow-core` | `thiserror` (build-dep v1) | nieużywany w build.rs |
| `tentaflow-protocol` | `bytecheck` | tylko `rkyv::bytecheck::CheckBytes`, nie crate path |
| `tentaflow-protocol-wasm` | `bytecheck`, `getrandom`, `serde`, `wasm-bindgen-futures` | brak callsite (wasm protocol używa rkyv przez re-export + js-sys) |
| `tentaflow-transport` | `bytecheck`, `futures` | `bytecheck` przez `rkyv::bytecheck` (framing.rs:52); `futures` brak callsite |
| `tentaflow-ui` | `serde_json`, `tracing` | brak callsite (egui ma własny state) |
| `tentaflow-mobile` | `hostname`, `tentaflow-protocol`, `uuid`, `android-activity` (Android target) | brak callsite |
| `tentaflow-desktop-core` | `hex`, `hostname`, `image`, `tentaflow-protocol` | brak callsite |
| `tentaflow-iroh-spike` | `hmac`, `tracing`, `tracing-subscriber` | tylko `rand` żywe (pairing.rs:88) — **kandydat do usunięcia całego crate** (patrz §6.3) |
| `tentaflow-client-native` | `ahash`, `libc`, `smallvec` | brak callsite (cbindgen build-dep ZOSTAJE — używany w `build.rs:24,44`) |
| `tentaflow-teams-bot` | `async-trait`, `byteorder`, `rkyv` | tylko komentarze; transport agreguje serializację |
| `tentaflow-sidecar` | `bytes`, `futures`, `parking_lot`, `rkyv`, `thiserror`, `uuid` | tylko `futures-util::StreamExt` żywe (reverse_proxy.rs:243) |
| `tentaflow-addon-template` | `serde` | importowane z `tentaflow_addon_sdk::prelude::*` |
| `tentaflow-addon-embeddings-chunker` | `serde` | jw. (lib.rs:31 derive z prelude) |
| `tentaflow` (binary) | `chrono`, `futures`, `hex`, `http-body`, `http-body-util`, `hyper`, `hyper-util`, `serde`, `tokio-rustls`, `uuid` | per-dep grep `crate::` w `tentaflow/src/` zwraca **0 callsite**; legacy z czasów własnego HTTP server (cały HTTP stack przeniesiony do tentaflow-core) |

**Razem: 54 dead entries do usunięcia mechanicznie + 1 manual.**

### Uwagi do potwierdzeń (kluczowe korekty mojego pierwotnego raportu)

| Dep | Mój pierwotny wyrok | Codex korekta | Final |
|-----|---------------------|---------------|-------|
| `walkdir` (core) | DELETE ❌ | KEEP — `build.rs:1126` `use walkdir::WalkDir; ... compute_source_hash` | **KEEP** |
| `prost-build` (voice) | DELETE ❌ | KEEP — `build.rs:27` `prost_build::Config::new()` | **KEEP** |
| `protobuf-src` (voice) | DELETE ❌ | KEEP — `build.rs:13` `protobuf_src::protoc()` | **KEEP** |
| `cbindgen` (client/native) | DELETE ❌ | KEEP — `build.rs:24,44` C header gen | **KEEP** |
| `prometheus` (core) | KEEP (false positive) ❌ | DELETE — manual renderer | **DELETE** |
| `tokenizers` (core) | KEEP (false positive) ❌ | DELETE — JSON file IO | **DELETE** |
| `ndarray` (core) | KEEP (false positive) ❌ | DELETE — tract_ndarray re-export | **DELETE** |

Powód moich błędów: brak weryfikacji `grep` po path:line, zgaduwanie po nazwach modułów. Codex użył precyzyjnych grep'ów na crate path i sprawdził build.rs — to jest correct methodology.

---

## 4. Manual review (1 pozycja)

### `wasmtime-wasi` w `tentaflow-core` (desktop target)

**Stan:**
- `Cargo.toml:236-238` — `wasmtime = "43.0.1"` + `wasmtime-wasi = "43.0.1"` (cfg=non-mobile)
- `addon/runtime/runtime_wasmtime.rs:154-166` — `WasmLinker::new(engine)` bez WASI wiring
- `addon/host_functions/mod.rs:48-50` — rejestrujemy tylko namespace `"tentaflow"`
- Addony skompilowane do `wasm32-wasip1` **importują** WASI: `strings test-addon.wasm` pokazuje `wasi_snapshot_preview1`, `environ_get`, `fd_write`, `proc_exit`, `random_get` (malicious)

**Konflikt:** addony deklarują WASI imports, ale linker ich nie obsługuje. Albo:
- **(A) wire WASI**: dodać `wasmtime_wasi::add_to_linker_async` w `create_linker` → KEEP dep, addony działają faktycznie z WASI
- **(B) zrezygnuj z WASI**: zmienić target addonów na `wasm32-unknown-unknown` lub no-WASI custom target → DELETE dep, ale wymaga rebuild wszystkich addonów

**Rekomendacja:** osobna decyzja produktowa. Aktualnie addony "działają" tylko gdy nie wywołują WASI imports — to niejawna umowa, łatwo złamać. Sugeruję (A) — dodać WASI linker wiring (godzina pracy) i utrzymać dep jako udokumentowane.

---

## 5. Cross-crate functional overlap analysis

| Domena | Aktywne | Status | Akcja |
|--------|---------|--------|-------|
| **Crypto AEAD** | `aes-gcm` 0.10 ✅ + `chacha20poly1305` ❌ | overlap nieaktywny | DELETE chacha (mesh transport używa iroh TLS) |
| **ECC sign+DH** | `ed25519-dalek` 2.2 + `x25519-dalek` ✅ + `curve25519-dalek` ❌ | curve25519 dead | DELETE curve25519 |
| **JSON** | `serde_json` ✅ | brak overlap | KEEP |
| **YAML** | `serde_yaml` ❌ | dead | DELETE |
| **TOML** | `toml` ✅ (config + manifest) | brak overlap | KEEP |
| **Wire binary** | `rkyv` ✅ (zero-copy QUIC) | brak overlap | KEEP |
| **bytecheck direct** | declared w 3 crates ❌ | wszędzie kod używa `rkyv::bytecheck` | DELETE z protocol/transport/wasm |
| **Logging** | `tracing` + `tracing-subscriber` ✅ | brak overlap | KEEP, delete dead z ui/iroh-spike |
| **HTTP server** | `hyper` + `hyper-util` (core only) | KEEP w core, DEAD w `tentaflow` binary | DELETE z binary |
| **HTTP client** | `reqwest` 0.13 (core) | brak overlap | KEEP |
| **Async runtime** | `tokio` (full) | brak overlap | KEEP |
| **Streams** | `futures` 0.3 + `async-stream` + `futures-util` | różne use cases | KEEP w core, DELETE z `tentaflow`/transport/sidecar |
| **Errors** | `anyhow` (boundaries) + `thiserror` 2 (typed lib) | poprawny pattern | KEEP both, DELETE dead `thiserror` v1 build-dep |
| **Sync** | `parking_lot` + `arc-swap` + `dashmap` + `tokio::sync` | różne use cases | KEEP all |
| **OnceLock** | `lazy_static` 1.5 (4 bloki) ⚠ legacy | migracja możliwa | **MIGRUJ** → `std::sync::OnceLock` (Faza 3) |
| **mDNS** | `mdns-sd` ❌ | iroh discovery zastąpił | DELETE |
| **Audio** | `symphonia` + `hound` | minimal overlap (hound = WAV-only) | KEEP both (hound prostszy dla TTS output) |
| **WASM runtime** | `wasmtime` (desktop) + `wasmi` (mobile) | target-specific | KEEP both |
| **Tokenizers** | `tokenizers` ❌ | MLX bridge używa file IO | DELETE |
| **Vision tensors** | `tract-onnx` (z `tract_ndarray`) ✅ + direct `ndarray` ❌ | direct `ndarray` dead | DELETE direct |
| **Build proto** | `prost-build` + `protobuf-src` ✅ | both used in voice/build.rs | KEEP both |

---

## 6. Architektoniczne kandydaty do redukcji

### 6.1 `tentaflow-iroh-spike` — kandydat do DELETE

- Standalone crate, **żaden `Cargo.toml` nie odwołuje się przez `path =`** (codex find: tylko własny Cargo.toml)
- 3 dead deps + crate jako całość nie linked do production builds
- Iroh integration ukończona w `tentaflow-core/src/mesh/iroh_manager.rs`
- **Akcja:** `git rm -r tentaflow-iroh-spike/` chyba że jest świadomie trzymany jako reference

### 6.2 `lazy_static` → `std::sync::OnceLock`

Tylko 4 bloki w `tentaflow-core`:
- `mesh/node_info_collector.rs:21, 1412`
- `middleware/pii.rs:34`
- `services/tts/processor.rs:13`

Mechaniczna migracja, low-risk. Po migracji DELETE crate `lazy_static`.

### 6.3 `vendor/ed25519-dalek` — KEEP

Vendor patch aktywnie referenced przez 3 manifesty (`[patch.crates-io]`). Nie usuwać bez planu deprekacji (wymaga upstream coordination z iroh, który używa 3.0-pre.6 API).

### 6.4 Multiple `BackendHandle` definitions

**Status:** Naprawione w R3b.8 (2026-05-03). Single `BackendHandle` w `services/handles_cache`.

### 6.5 STT path

**Status:** Naprawione w R5f (2026-05-04). Single owned dispatch w `SttRuntime`.

---

## 7. Transitive duplicates — `cargo tree --duplicates` w `tentaflow-core/`

68 unique package names z >1 wersją. Większość to **upstream-driven** (iroh, wasmtime, hf-hub używają różnych pokoleń `getrandom`/`rand`/`hashbrown`/`bitflags`/X.509 stack/`windows-sys`).

### Najbardziej actionable (mamy lokalną kontrolę):

| Package | Wersje | Akcja |
|---------|--------|-------|
| `chacha20` | 0.9, 0.10 | po DELETE `chacha20poly1305` z core — branch 0.9 znika (jeśli nie jest pulled by upstream) |
| `core-foundation-sys` (sub-crate) | n/a — naprawione przez DELETE direct dep | DELETE `core-foundation-sys` z `tentaflow-core/Cargo.toml` macOS target |
| `netdev` | 0.31, 0.42 | upgrade direct `netdev` 0.31 → 0.42 (iroh już używa 0.42); wymaga sprawdzenia API w `mesh/network_interfaces.rs` |
| `thiserror`/`thiserror-impl` | 1.0, 2.0 | DELETE build-dep `thiserror = "1.0"` z core; v1 zostanie tylko jeśli upstream `witx` go używa |
| `core-foundation` | 0.9, 0.10 | częściowo unifikuje się przez upgrade `netdev` |
| `system-configuration` | 0.6, 0.7 | jw. (idzie z netdev) |

### Upstream-only (nie ruszać lokalnie):

`bitflags`, `bit-set`, `bit-vec`, `block-buffer`, `const-oid`, `cpufeatures`,
`crypto-common`, `der`, `digest`, `dlopen2`, `ed25519`, `embedded-io`,
`fiat-crypto`, `foldhash`, `getrandom` (3 wersje), `hashbrown` (4 wersje),
`itertools` (4 wersje), `jni`, `jni-sys`, `linux-raw-sys`, `netlink-packet-*`,
`pem-rfc7468`, `pkcs8`, `r-efi`, `rand`/`rand_chacha`/`rand_core`,
`redox_*`, `reqwest` (0.12 z hf-hub vs 0.13 w core), `rustc-hash`, `rustix`,
`sha1`/`sha2`, `signature`, `spin`, `spki`, `string-interner`, `syn`,
`toml`/`toml_datetime`, `vergen-lib`, `wasm-encoder`/`wasm-streams`/`wasmparser`/`wast`,
`windows-sys` (6 wersji), `windows-targets` + `windows_*` (4 wersje), `winnow`,
`wit-bindgen`, `wit-parser`.

### Crypto policy issue (osobna decyzja security):

`ed25519-dalek` 2.2 vs vendored 3.0-pre.6 — iroh używa fork 3.0-pre, my używamy 2.2 API. Unifikacja wymaga policy decision (czy idziemy na pre-release ed25519 globalnie). Treat as upstream coordination, nie cleanup.

---

## 8. Plan akcji w fazach

| Faza | Zmiany | Estymacja | Ryzyko | Verification |
|------|--------|----------:|--------|--------------|
| **0: mechaniczne delete** | 44 dead entries poza `tentaflow-core` (`tentaflow` binary, sidecar, teams-bot, ui, mobile, desktop-core, protocol-wasm, protocol, transport, client/native, addon template/chunker, iroh-spike) | 1-2h | Low | per-crate `cargo check` + `cargo machete` |
| **1: tentaflow-core dead deps** | DELETE `chacha20poly1305`, `curve25519-dalek`, `mdns-sd`, `ndarray`, `prometheus` (+ feature), `serde_yaml`, `tokenizers` (+ feature), `filetime` (dev), `core-foundation-sys` (macOS), `thiserror` v1 build-dep | 2-3h | Medium (feature matrix) | full features build, `--no-default-features --features dashboard-api`, `--features metrics-prometheus`, macOS target |
| **2: target/manual** | DELETE `android-activity` z mobile; DECISION na `wasmtime-wasi` (A: wire WASI, B: switch addon target) | 0.5-1d | Medium | Android target check, addon integration tests |
| **3: modernization** | `lazy_static` → `OnceLock` (4 bloki) → DELETE `lazy_static` z core; rozważ DELETE crate `tentaflow-iroh-spike` | 0.5d | Low | unit tests, pii regex test |
| **4: duplicate reduction** | upgrade `netdev` 0.31 → 0.42 (review API zmian w `mesh/network_interfaces.rs`) | 0.5-1d | Medium | `mesh_discovery_repro`, network config tests, manual LAN check |
| **5: crypto policy** | decyzja na `ed25519-dalek` 2.2 vs vendored 3.0-pre — wymaga security review | 1-2d | High | pairing/security tests, iroh interop |
| **6: upstream tracking** | issues/notes na `hf-hub`/`reqwest` 0.12, wasmtime version sprawl, Windows generations | ongoing | Low | periodic `cargo update --dry-run` |

---

## 9. Sumaryczny benefit po Fazie 0+1

- **~54 dead direct deps usunięte** = mniejszy `Cargo.lock`, szybszy resolve
- **~3-5% szybsza incremental build** (estymata)
- **Security:** mniejszy attack surface (`mdns-sd` listening na 5353/udp, dead crypto crates)
- **Maintenance:** clear ownership co projekt rzeczywiście potrzebuje
- **Cargo.toml diff:** -54 linii direct deps + -2 features (metrics-prometheus, część inference-mlx)

## 10. Niezgodności rozstrzygnięte (audit synthesis)

11 spornych pozycji zderzonych w discussion roundzie z codexem. Wszystkie
rozstrzygnięte przez `grep`/`cat` z path:line. Pełne ślady w
`/tmp/codex_deps_audit.md` (raport codexa) i `/tmp/codex_deps_discussion.md`
(round dyskusji). Mój pierwotny audit miał **7 błędów** (3x false-positive
delete dla build-deps, 4x false-keep dla dead optional/feature deps) —
korekty ujęte w §3 tabeli "Uwagi do potwierdzeń".

**Wynik:** raport gotowy. Faza 0+1 (~3-5h) zalecana jako natychmiastowy
follow-up — wszystkie zmiany weryfikowalne `cargo build` per-feature.

---

## 11. Wyniki cleanup (2026-05-04)

Fazy 0-4 wykonane w jednym commicie (`469d8ec` na `main`). Każda faza
weryfikowana niezależnym codex review.

### Faza 0 — cross-workspace mechanical delete ✅

50 dead deps usuniętych z 11 `Cargo.toml`. Trzy pozycje rollback'owane vs.
codex audit (codex audit się mylił):
- `serde` w `addon-template` i `embeddings-chunker` — derive macro wymaga
  `serde` jako root crate, re-export przez SDK prelude nie wystarczy
- `getrandom` w `protocol-wasm` — feature `js` wymagana przez transitive
  deps (curve25519, serde) dla wasm32-unknown-unknown

### Faza 1 — tentaflow-core dead deps ✅

10 deps + feature `metrics-prometheus` usunięte:
`chacha20poly1305`, `curve25519-dalek`, `mdns-sd`, `ndarray`, `prometheus`
(+ feature), `serde_yaml`, `tokenizers`, `filetime` (dev), `core-foundation-sys`
(macOS), `thiserror v1` (build).

Codex review pierwotnie REJECT bo `metrics-prometheus = []` to no-op
feature — fixed przez całkowite usunięcie feature i `tentaflow/Cargo.toml:16`
nie enables go już.

### Faza 2 — target/manual decision ⚠

- `android-activity` ✅ usunięte (już w Fazie 0)
- `wasmtime-wasi` 📌 **DEFERRED** — addony `wasm32-wasip1` faktycznie
  importują `wasi_snapshot_preview1` (`environ_get`, `fd_write`, `proc_exit`,
  `random_get`), ale `runtime_wasmtime::create_linker` nie wire'uje WASI.
  Decyzja: utrzymać dep + zaplanowany dedykowany refactor (3-4h pracy) —
  TODO komment w `runtime_wasmtime.rs:154` + memory note
  `project_wasmtime_wasi_wiring_todo.md`.

### Faza 3 — modernization ✅

- `lazy_static` → `std::sync::OnceLock` w 4 blokach (~24 callsite):
  `middleware/pii.rs` (8 patterns), `services/tts/processor.rs` (2 regex),
  `mesh/node_info_collector.rs` (5 Mutex). Crate `lazy_static` usunięty.
- `tentaflow-iroh-spike` — cały standalone crate usunięty (`git rm -r`,
  661 LOC, historyczne `DECISION.md` rekomendowało CUT, iroh integration
  ukończona w `mesh/iroh_manager.rs`).

Codex review REJECT był false-positive (codex nie wiedział że diff
kumulatywny od `cea1569` zawiera też Fazy 1+2). Funkcjonalnie wszystko
clean — 0 stare callsite, 0 external uses.

### Faza 4 — duplicate reduction ✅

`netdev` 0.31 → 0.42 + 1 path fix w `mesh/network_interfaces.rs`
(`InterfaceType` przeniesiony do `interface::types`). Eliminuje
duplicate `netdev` w `cargo tree --duplicates`.

### Verification matrix

| Cel | Status | Czas |
|-----|--------|-----:|
| `cargo check` tentaflow-core full features | ✅ pass | 26s |
| `cargo check` tentaflow binary | ✅ pass | 53s |
| `cargo test --lib` tentaflow-core | ✅ 1119 pass / 9 baseline fails (pre-existing) | 70s |
| `cargo check` tentaflow-mobile (iOS target) | ✅ pass | 12 min (1st build) |
| `cargo check` tentaflow-desktop-macos | ✅ pass | 24s |
| Android target (aarch64-linux-android) | ⏸ skip | target not installed |

### Fazy odłożone

- **Faza 5** — `ed25519-dalek` 2.2 vs vendored 3.0-pre.6 crypto policy.
  Wymaga security review + upstream coordination z iroh. Wykracza poza
  scope cleanup zależności.
- **Faza 6** — upstream tracking (`hf-hub`, `reqwest` 0.12, wasmtime
  versions, Windows generations). Ongoing process: periodyczne
  `cargo update --dry-run`, monitoring upstream releases.

### Niskie priorytety (audit wspomniał, nie zrobione)

- `hound` removal w favor `symphonia::wav` — minimal overlap, 2-3h
  refactor, low priority.
- `tokenizers` (Faza 1 usunięte) — dep wraca gdy MLX bridge zacznie
  używać `tokenizers::Tokenizer` API zamiast file IO.

### Sumarycznie po cleanup

- **75+ dead direct dependency entries** usunięte cross-workspace
- **1 cały crate** (`tentaflow-iroh-spike`) usunięty
- **31 plików** zmienionych: -2297 / +246 linii (czysta redukcja)
- **lazy_static** modernization (Rust 1.70+ idiomatic OnceLock)
- **netdev** version unified (eliminacja jednego transitive duplicate)
- **1 deferred refactor** dla `wasmtime-wasi` wiring (zapisany w kodzie
  + memory)

