# Audyt zależności TentaFlow workspace

**Data audytu:** 2026-05-04
**Wersja Rust:** 1.95.0
**Crates w workspace:** 25 (z czego 6 to addons/SDK/fuzz)

---

## 1. Inwentarz crate'ów

### Produkcyjne (15)
| Crate | Rola | LOC Cargo.toml |
|-------|------|----------------|
| `tentaflow-core` | Główna biblioteka — routing, mesh, services, dispatch | 270 |
| `tentaflow` | Binary serwer (main bin) — entry point, CLI, init | ~80 |
| `tentaflow-protocol` | rkyv wire types — ModelRequest, ServiceInfo, etc. | ~50 |
| `tentaflow-transport` | iroh QUIC transport wrapper | ~40 |
| `tentaflow-voice` | BLAS/SIMD GEMM dla embeddings + diarization | ~50 |
| `tentaflow-macros` | Proc-macro dla protocol code-gen | ~20 |
| `tentaflow-ui` | egui/wgpu shared GUI komponenty desktop+mobile | ~40 |
| `tentaflow-desktop/core` | Wspólna logika desktop (config, paths) | ~40 |
| `tentaflow-desktop/macos` | macOS-specific app shell | ~20 |
| `tentaflow-desktop/linux` | Linux app shell | ~20 |
| `tentaflow-desktop/windows` | Windows app shell + winresource | ~20 |
| `tentaflow-mobile` | Mobile binary (Android/iOS) | ~30 |
| `tentaflow-mobile/core` | Wspólna mobile logic | ~30 |
| `tentaflow-client/native` | Rust FFI biblioteka dla .NET P/Invoke | ~40 |
| `tentaflow-protocol-wasm` | wasm-bindgen glue dla browser dashboard | ~30 |

### Eksperymentalne / test (4)
| `tentaflow-iroh-spike` | iroh prototype — **kandydat do delete** | ~25 |
| `tentaflow-protocol/fuzz` | Fuzzing protocol parser | ~15 |
| `tentaflow-containers/sidecar` | QUIC sidecar dla python bundles | ~30 |
| `tentaflow-containers/agents/native/teams-bot` | Teams meeting bot | ~30 |

### Addons + SDK (6)
| `tentaflow-core/addon-sdk/sdk` | WASM addon trait/host functions API | ~15 |
| `tentaflow-core/addon-sdk/template` | Template addon | ~10 |
| `tentaflow-core/addons/test-addon` | Test fixture | ~10 |
| `tentaflow-core/addons/embeddings-chunker` | Real embedding chunker addon | ~10 |
| `tentaflow-core/addons/malicious-addon` | Test fixture (security) | ~10 |

### Vendored (1)
| `vendor/ed25519-dalek` | Local fork (3.0.0-pre.6) | n/a |

---

## 2. Statystyki

- **102 unique deps w `tentaflow-core`** (z `[dependencies]`, `[dev-dependencies]`, target-specific i `[build-dependencies]`)
- **169 unique deps cross-workspace** (suma)
- **65 transitive duplicate crates** (różne wersje używane przez różne deps tree branches)
- **Największe podgrupy unique deps:**
  - HTTP/networking: ~15
  - Crypto/security: ~12
  - Async/sync prymitywy: ~10
  - Serialization: ~8
  - AI/ML inference: ~7
  - Audio: ~6
  - Monitoring/metrics: ~5

---

## 3. Nieużywane zależności (cargo-machete weryfikacja)

### `tentaflow-core` — 7 confirmed dead, 4 false positive

**Confirmed dead (do delete):**
| Dep | Wersja | Rationale | Akcja |
|-----|--------|-----------|-------|
| `chacha20poly1305` | 0.10.1 | Cipher zastąpiony przez `aes-gcm` | DELETE |
| `curve25519-dalek` | 4.1.3 | Używamy ed25519-dalek (wraps własny X25519) | DELETE |
| `mdns-sd` | 0.19.0 | mDNS discovery zastąpione przez iroh peer discovery | DELETE |
| `serde_yaml` | 0.9.33 | Konfig migrowała na TOML | DELETE |
| `walkdir` | 2 | Tylko `std::fs` używane | DELETE |
| `filetime` | 0.2 | Brak callerów | DELETE |
| `core-foundation-sys` | 0.8 | `core-foundation` jest re-export | DELETE (sprawdź target-specific) |

**False positives (feature-gated, zostają):**
| Dep | Feature flag | Powód detection failure |
|-----|--------------|------------------------|
| `prometheus` | `metrics-prometheus` | cargo-machete nie skanuje feature-gated |
| `tokenizers` | `inference-mlx` | jw. |
| `wasmtime-wasi` | `wasmtime` re-export | używane tranzytywnie |
| `ndarray` | brak | 5 use-sites w `embeddings/` (false positive cargo-machete) |

### Pozostałe crates — confirmed dead

#### `tentaflow-transport` (2)
- `bytecheck` — protocol used to derive Archive (już teraz nie potrzebne)
- `futures` — tylko sub-deps używają

#### `tentaflow-protocol-wasm` (4)
- `bytecheck`, `getrandom`, `serde`, `wasm-bindgen-futures` — wszystkie nieużywane

#### `tentaflow-iroh-spike` (3)
- `hmac`, `tracing`, `tracing-subscriber` — **CAŁY CRATE prawdopodobnie do delete**

#### `tentaflow-ui` (2)
- `serde_json`, `tracing` — nieużywane

#### `tentaflow-mobile/core` (3)
- `hostname`, `tentaflow-protocol`, `uuid` (+ android-activity dla android)

#### `tentaflow` (10) ⚠ NAJWIĘKSZA LISTA
- `chrono`, `futures`, `hex`, `http-body`, `http-body-util`, `hyper`, `hyper-util`, `serde`, `tokio-rustls`, `uuid`
- Większość pewnie używana przez transitive — tentaflow-core eksportuje. Wymaga manual review.

#### `tentaflow-desktop/core` (4)
- `hex`, `hostname`, `image`, `tentaflow-protocol`

#### `tentaflow-protocol` (1)
- `bytecheck`

#### `tentaflow-containers/sidecar` (6)
- `bytes`, `futures`, `parking_lot`, `rkyv`, `thiserror`, `uuid`

#### `tentaflow-client/native` (4)
- `ahash`, `cbindgen`, `libc`, `smallvec`

#### `tentaflow-containers/agents/native/teams-bot` (3)
- `async-trait`, `byteorder`, `rkyv`

#### `tentaflow-voice` (2)
- `prost-build`, `protobuf-src` — pre-existing nieużywane

**Sumarycznie do delete: ~52 unused deps cross-workspace** (po weryfikacji manualnej).

---

## 4. Duplicated transitive deps (cargo tree --duplicates)

### 65 unique crate names z >1 wersją

**Najczęstsze konflikty:**
| Crate | Wersje | Źródło |
|-------|--------|--------|
| `bitflags` | 1.3.2 + 2.11.1 | symphonia (v1) vs wasmtime/iroh (v2) |
| `hashbrown` | 0.14.5 + 0.16.1 + 0.17.0 | ahash, sled, hashbrown |
| `getrandom` | 0.2 + 0.3 + 0.4 | crypto crates różne pokolenia |
| `rand` | 0.8 + 0.10 | ed25519 vs nasz workspace |
| `rand_core` | 0.6 + 0.10 | jw. |
| `itertools` | 0.10 + 0.12 + 0.13 + 0.14 | różne crates |
| `bit-set`/`bit-vec` | 0.5 + 0.9 | tract-onnx (v0.5) vs wgpu (v0.9) |
| `crypto-common` | 0.1 + 0.2 | digest v0.10 vs v0.11 |
| `digest` | 0.10 + 0.11 | jw. |
| `der`/`spki`/`pkcs8` | 0.7 + 0.8 | X.509 stack przejście |
| `core-foundation` | 0.9 + 0.10 | macOS bindings różne pokolenia |
| `curve25519-dalek` | 4.1.3 + 5.0.0-pre.6 | nasz vendor + iroh |
| `ed25519-dalek` | 2.2.0 + 3.0.0-pre.6 | jw. |
| `reqwest` | 2 versions | nasze + iroh |
| `rustls-pki-types` | 2 versions | jw. |
| `time` | 2 versions | jw. |
| `toml` | 2 versions | jw. |
| `wasmtime-environ`, `wasmtime-internal-core` | wasmtime + nasze | naprzemienne |

**Wniosek:** większość duplikatów to nieuniknione naturalne pokoleniowe rozjazdy między ekosystemowymi crates (iroh używa jeszcze starych pokoleń kryptografii, X.509 stack przechodzi na nowy `der` 0.8). Nie da się tego unifikować bez forka iroh.

**Co MOŻNA zunifikować:**
- `vendor/ed25519-dalek` (3.0.0-pre.6) — sprawdzić czy fork wciąż potrzebny vs upstream 2.2.0
- `curve25519-dalek` 4.1.3 w `tentaflow-core/Cargo.toml` — confirmed unused, można delete
- `bitflags` v1 zostanie póki używamy `symphonia` (audio decoder) — alternatywa: `hound` (już mamy) jako prosty WAV-only decoder

---

## 5. Funkcjonalne overlap'y (różne crates, podobna funkcja)

### 5.1 Crypto / hashing

| Cel | Używane | Status | Rekomendacja |
|-----|---------|--------|--------------|
| Symmetric AEAD | `aes-gcm` 0.10 | ✅ używane | KEEP |
| | `chacha20poly1305` 0.10 | ❌ nieużywane | **DELETE** |
| ECC sign | `ed25519-dalek` 2.2 | ✅ używane | KEEP |
| ECC DH | `curve25519-dalek` 4.1 | ❌ nieużywane | **DELETE** |
| KDF | `hkdf` 0.12 | ✅ używane | KEEP |
| MAC | `hmac` 0.12 | ✅ używane | KEEP |
| Hash | `sha2` 0.10 | ✅ używane | KEEP |
| Password hash | `argon2` 0.5 | ✅ używane | KEEP |
| TLS | `rustls` + `tokio-rustls` + `rustls-pemfile` | ✅ używane | KEEP |
| Cert gen | `rcgen` 0.14 | ✅ używane (mesh self-signed) | KEEP |
| JWT | `jsonwebtoken` 10 | ✅ używane (dashboard auth) | KEEP |
| Random | `rand` 0.10 + `rand_core` 0.10 + `rand_core_06` (alias) + `getrandom` | ✅ wszystkie używane | KEEP — `rand_core_06` wymagane dla ed25519-dalek 2.2 |
| Constant-time compare | `subtle` 2 | ✅ używane | KEEP |

**Akcje:**
- DELETE `chacha20poly1305` z `tentaflow-core/Cargo.toml`
- DELETE `curve25519-dalek` z `tentaflow-core/Cargo.toml`

### 5.2 Async / concurrency

| Cel | Używane | Status | Rekomendacja |
|-----|---------|--------|--------------|
| Runtime | `tokio` (full) | ✅ KEEP | jedyna opcja |
| Sync prymitywy | `parking_lot` + `tokio::sync` + `std::sync` + `arc-swap` | ✅ KEEP | różne use cases |
| Concurrent map | `dashmap` | ✅ KEEP | hot paths (handles cache, strategy state) |
| Atomic ops | `std::sync::atomic` | ✅ KEEP | |
| OnceLock | `lazy_static` 1.5 | ⚠ legacy | **MIGRACJA** do `std::sync::OnceLock` (Rust 1.70+) — usunąć `lazy_static` |
| Streams | `futures` 0.3 + `async-stream` | ✅ KEEP | różne use cases |
| async-trait | `async-trait` | ✅ KEEP | dla trait objects |

**Akcje:**
- ROZWAŻYĆ migracja `lazy_static` → `OnceLock` (~20-30 callsite)

### 5.3 Errors

| Cel | Używane | Status |
|-----|---------|--------|
| Library errors | `thiserror` 2 | ✅ KEEP — for typed errors |
| Application errors | `anyhow` 1 | ✅ KEEP — for boundaries |

Pattern jest poprawny (thiserror wewnątrz `crate::error::CoreError`, anyhow jako `Result<T>` na granicach). Zero overlap.

### 5.4 Serialization

| Cel | Używane | Status |
|-----|---------|--------|
| JSON | `serde_json` | ✅ KEEP |
| YAML | `serde_yaml` | ❌ **DELETE** — nieużywane |
| TOML | `toml` | ✅ KEEP |
| Binary wire | `rkyv` 0.8 | ✅ KEEP (zero-copy QUIC) |
| Base64 | `base64` 0.22 | ✅ KEEP |
| Hex | `hex` 0.4 | ✅ KEEP |

### 5.5 HTTP

| Cel | Używane | Status | Rekomendacja |
|-----|---------|--------|--------------|
| Server | `hyper` 1 + `hyper-util` | ✅ KEEP — niskopoziomowy server |
| Client | `reqwest` 0.12 | ✅ KEEP — backendów HTTP |
| Body utils | `http-body` + `http-body-util` | ✅ KEEP |
| WebSocket | `tokio-tungstenite` | ✅ KEEP |
| Multipart | `multer` | ✅ KEEP — `/v1/audio/transcriptions` |

Brak overlap. Stack jest ekonomiczny (jedyna alternatywa to `axum` która agreguje hyper+tower; nie warto migracji).

### 5.6 AI / ML inference

| Cel | Używane | Feature flag | Status |
|-----|---------|--------------|--------|
| LLM (CPU) | `llama-cpp-2` | `inference-llamacpp` | ✅ KEEP |
| LLM (Apple) | mlx-swift bridge (FFI) | `inference-mlx` | ✅ KEEP |
| Whisper STT | `whisper-rs` | `inference-whisper` (default) | ✅ KEEP |
| MLX Whisper | `libloading` (FFI) | `inference-mlx-whisper` | ✅ KEEP |
| Sherpa STT/TTS | `sherpa-rs` | `inference-sherpa` | ✅ KEEP |
| MLX Kokoro TTS | `libloading` (FFI) | `inference-mlx-kokoro` | ✅ KEEP |
| Apple AVSpeech | bezpośrednio (FFI) | (target-gated) | ✅ KEEP |
| Vision/diarization | `tract-onnx` + `tentaflow-voice` | `inference-diarization` | ✅ KEEP |
| HuggingFace download | `hf-hub` | brak | ✅ KEEP |
| Tokenizers | `tokenizers` | `inference-mlx` | ✅ KEEP (false positive cargo-machete) |

### 5.7 Audio

| Cel | Używane | Status |
|-----|---------|--------|
| Codec multi-format | `symphonia` (mp3+vorbis+pcm+ogg) | ✅ KEEP |
| WAV write | `hound` | ✅ KEEP |

Overlap minimal — `symphonia` ma WAV write ale `hound` jest prostszy. Można rozważyć eliminację `hound` na rzecz `symphonia` ale to refactor 2-3h. Niska priorytet.

### 5.8 Database / storage

| Cel | Używane | Status |
|-----|---------|--------|
| SQLite | `rusqlite` (bundled) | ✅ KEEP |
| Filesystem walk | `walkdir` | ❌ **DELETE** — nieużywane |
| Compression | `flate2` | ✅ KEEP |
| Tar | `tar` | ✅ KEEP |
| Temp files | `tempfile` | ✅ KEEP |

### 5.9 Mesh / networking

| Cel | Używane | Status |
|-----|---------|--------|
| QUIC mesh | `iroh` 0.98 + `iroh-relay` | ✅ KEEP |
| QUIC transport | `tentaflow-transport` (wrapper) | ✅ KEEP |
| Network detection | `netdev` | ✅ KEEP |
| mDNS | `mdns-sd` | ❌ **DELETE** — zastąpione przez iroh discovery |
| DNS sysconfig | `system-configuration` (transitive) | ✅ KEEP |

### 5.10 Utility

| Cel | Używane | Status |
|-----|---------|--------|
| Time | `chrono` | ✅ KEEP |
| UUID | `uuid` | ✅ KEEP |
| Regex | `regex` | ✅ KEEP |
| Env paths | `dirs` | ✅ KEEP |
| OS info | `sysinfo` | ✅ KEEP |
| FFI utility | `libloading` + `libc` | ✅ KEEP |
| Logging | `tracing` + `tracing-subscriber` | ✅ KEEP |
| Numeric IDs | `inventory` | ✅ KEEP (manifest registry) |

---

## 6. Architektoniczne overlapy (kandydaci do unifikacji)

### 6.1 Multiple BackendHandle definitions
**Status:** Naprawione w R3b.8 (2026-05-03). Single `BackendHandle` w `services/handles_cache`.

### 6.2 STT path
**Status:** Naprawione w R5f (2026-05-04). Single owned dispatch w `SttRuntime`.

### 6.3 `tentaflow-iroh-spike` ⚠
Crate eksperymentalny z 3 unused deps (cały crate prawdopodobnie nieużywany).
- Sprawdzić czy kod jest reference dla iroh integration
- Jeśli iroh integration ukończona w `tentaflow-core/src/mesh/iroh_manager.rs` — **DELETE crate**

### 6.4 Vendored `ed25519-dalek` 3.0.0-pre.6
- Workspace ma `vendor/ed25519-dalek` (3.0.0-pre.6) ale używamy z `crates.io` `ed25519-dalek` 2.2.0
- 2 warianty w drzewie zależności (cargo tree pokazuje oba)
- Sprawdzić CZY vendor jest używany — jeśli nie, **DELETE vendor**

### 6.5 `lazy_static` legacy
~20-30 callsite z `lazy_static!` — wszystkie mogą być zastąpione przez `std::sync::OnceLock` (stable od 1.70). Refactor mechaniczny, low-risk. **Zalecane** ale poza scope tego cleanup'u.

### 6.6 `serde_json` w `tentaflow-ui`
UI używa `egui` które ma własny state management. Brak callsites `serde_json` — usunąć z `tentaflow-ui/Cargo.toml`.

### 6.7 `tracing` w `tentaflow-iroh-spike`, `tentaflow-ui`, `tentaflow-mobile`
Wszystkie 3 to "dummy includes" bez actual use. Usunąć.

---

## 7. Rekomendowany plan akcji

### Faza 1 — bezpieczne delete (zero ryzyka, ~1h)
Usunąć z odpowiednich `Cargo.toml`:

| Crate | Plik | Dep do delete |
|-------|------|---------------|
| tentaflow-core | tentaflow-core/Cargo.toml | `chacha20poly1305`, `curve25519-dalek`, `mdns-sd`, `serde_yaml`, `walkdir`, `filetime` |
| tentaflow-transport | tentaflow-transport/Cargo.toml | `bytecheck`, `futures` |
| tentaflow-protocol | tentaflow-protocol/Cargo.toml | `bytecheck` |
| tentaflow-protocol-wasm | tentaflow-protocol-wasm/Cargo.toml | `bytecheck`, `getrandom`, `serde`, `wasm-bindgen-futures` |
| tentaflow-ui | tentaflow-ui/Cargo.toml | `serde_json`, `tracing` |
| tentaflow-mobile/core | mobile/core/Cargo.toml | `hostname`, `uuid` (+ android-activity sprawdzić cfg) |
| tentaflow-desktop/core | desktop/core/Cargo.toml | `hex`, `hostname`, `image`, `tentaflow-protocol` |
| tentaflow-containers/sidecar | sidecar/Cargo.toml | `bytes`, `futures`, `parking_lot`, `rkyv`, `thiserror`, `uuid` |
| tentaflow-containers/agents/native/teams-bot | teams-bot/Cargo.toml | `async-trait`, `byteorder`, `rkyv` |
| tentaflow-client/native | client/native/Cargo.toml | `ahash`, `libc`, `smallvec` |
| tentaflow | tentaflow/Cargo.toml | `chrono`, `futures`, `hex`, `serde`, `uuid` (po manualnej weryfikacji 10 z listy) |
| tentaflow-voice | voice/Cargo.toml | `prost-build`, `protobuf-src` |

**Uwaga przed delete:** każda dep wymaga `cargo build --all-features` po edycji bo cargo-machete ma blind spots dla:
- target-specific deps (cfg-gated)
- proc-macro deps (re-eksport)
- transitive enable (feature → feature → dep)

### Faza 2 — manual review (~2h)
1. **`tentaflow-iroh-spike`**: czy crate jest jeszcze referenced z root workspace? Jeśli nie — `git rm -r tentaflow-iroh-spike/`.
2. **`vendor/ed25519-dalek`**: sprawdzić czy `Cargo.toml` w workspace odwołuje się przez `path = "vendor/ed25519-dalek"`. Jeśli nie — `git rm -r vendor/ed25519-dalek/`.
3. **`tentaflow/Cargo.toml`** 10 dep z machete listy — niektóre (np. `hyper`, `hyper-util`, `http-body-util`, `tokio-rustls`) mogą być wymagane dla custom server setup w `main.rs`. Per-dep grep.

### Faza 3 — modernization (opcjonalna, ~3h)
1. `lazy_static` → `std::sync::OnceLock` (mechaniczny refactor, ~30 callsites)
2. Rozważ `hound` removal — `symphonia` może to zastąpić (audio output WAV w STT/TTS pipeline)

### Faza 4 — duplicate version reduction (poza naszą kontrolą głównie)
1. Sprawdzić czy `iroh` 0.99 / 0.100 ma update do nowszych curve25519-dalek/rand_core/etc.
2. `bit-set` / `bit-vec` — wymaga upgrade `tract-onnx` (źle utrzymywany crate)
3. `core-foundation` 0.9 vs 0.10 — naturalna migracja, każda upgrade zależności rozwiązuje

---

## 8. Sumaryczny benefit po Fazie 1

- **~52 unused deps cross-workspace usunięte** = mniejszy `Cargo.lock`, szybszy resolve
- **~5-10 transitive crates zniknie** (deps deps)
- **Czas kompilacji:** ~3-5% szybsza incremental build (estymata na podstawie typowych Rust workspace cleanups)
- **Maintenance:** jasniejszy obraz co projekt rzeczywiście potrzebuje
- **Security:** mniejszy attack surface (np. usunięcie `mdns-sd` które potencjalnie nasłuchiwało na 5353/udp)

## 9. Ryzyko

- **Niskie:** Faza 1 — `cargo build --all-features --workspace` po każdej zmianie wykryje regression
- **Średnie:** Faza 2 — delete vendored fork wymaga sprawdzenia czy upstream wersja ma nasze patche
- **Niskie:** Faza 3 — mechaniczna migracja `lazy_static`
- **Wysokie:** Faza 4 — wymaga upstream coordination, poza scope

---

**Wynik:** raport gotowy. Faza 1 (~1h) zalecana jako natychmiastowy follow-up — wszystkie zmiany bezpieczne (cargo build verification per-dep). Pełny cleanup do Fazy 3 ~6h.
