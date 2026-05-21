# Addon Binary Protocol v1 — Design Document

> Status: **v1.0 — zaakceptowany przez codex review (7 rund)** (2026-05-21)
> Owner: addon platform team
> Wire format: **CBOR — Core Deterministic Encoding** (RFC 8949 §4.2.1 + §4.2.2)
> Replaces: `ui_render_binary` ABI (deprecated, no backward compat)
> Companion: `ADDON_UI_COMPONENT_CATALOG_v1.md` (lista i schemy komponentów; pisany równolegle)

## ⚠ Implementation Directives — MUST be followed

**Te wytyczne obowiązują WSZYSTKICH (ludzi i agentów AI) implementujących ten protokół. Brak wyjątków.**

1. **Production-ready, not MVP.** To jest **docelowe produkcyjne** rozwiązanie. Każda linia kodu musi być gotowa do uruchomienia w produkcji od pierwszego commita. Brak skrótów, brak "naprawimy później", brak placeholder logic.

2. **Zero stubów.** Forbidden: `todo!()`, `unimplemented!()`, `// TODO: implement`, empty function bodies returning defaults, mock responses, `panic!("not yet implemented")`, fake values, scaffolded `Ok(())` bez logiki, "we'll wire this up later" struct stubs. Jeśli brakuje zależności — powiedz o tym i przerwij, nie udawaj implementacji.

3. **Zero backward compatibility.** Stare SDK (`tentaflow-ui-schema`, `tentaflow-addon-sdk` w obecnej formie, `ui_render_binary` ABI, obecny `addon-app.js` rendering) zostają **usunięte** podczas implementacji. Brak aliasów, brak deprecated wrappers, brak `if old { old_path } else { new_path }`, brak feature flag dla starego zachowania. Czysta nowa implementacja, koniec.

4. **Usuwaj stary kod jako idziesz.** Każdy commit przepisujący moduł na nową architekturę MUSI usunąć stary kod tego modułu w tym samym commicie. Brak martwych funkcji, zakomentowanego kodu, "in case we need it" wariantów. Stary TentaVision/Eureka/Contacts/Company Lookup do przepisania w miejscu (nie obok).

5. **Single source of truth.** Wszystkie typy/enumy/schemas pochodzą z `tentaflow-sdk-spec`. Brak ręcznie utrzymywanych odpowiedników w innych miejscach kodu. Brak duplikatów. SDK dla innych języków (C#, Python) są **wyłącznie** generowane przez `tentaflow-sdk-gen`.

6. **Brak parallel-stack scaffolding.** Nie ma "starej ścieżki" i "nowej ścieżki" istniejących obok siebie nawet tymczasowo. Każda zmiana przepisuje w miejscu. Code review odrzuca PRs które wprowadzają duplikację.

7. **Jeśli nie pasuje do tych dokumentów — fix dokument przed implementacją.** Nie improvizuj odstępstw "bo tak wygodniej". Każda zmiana w protokole/katalogu wymaga update tych docs + ewentualnie regeneracji manifestu.

Jeśli którakolwiek z tych dyrektyw jest naruszona w PR — PR jest odrzucany. To są twarde reguły, nie sugestie.

---

## 0. Cel dokumentu

Specyfikacja binary protokołu komunikacji **addon (WASM/WASI) ↔ TentaFlow core ↔ frontend (browser/mobile)**, zaprojektowanego dla:

- **Granular UI updates** (slot fragments + reactive state patches, zero pełnych re-renderów)
- **Slot-level sandboxing** (addon pisze tylko do swoich slotów + state namespace)
- **Multi-language SDK** (Rust/C#/Python/…) — wszystkie produkują **bit-identical** wire encoding (CBOR canonical)
- **Multi-platform hosting** (WASM/WASI dla wszystkich języków, działa na Linux/macOS/Windows/iOS/Android)
- **Production-ready** od dnia 1 (no MVP shortcuts, no backward compat)

Protokół jest binarny, używa **CBOR (RFC 8949)** z **deterministic encoding** (§4.2.1) jako wire format. Frontend mówi po nim do core przez WebTransport/WebSocket, addon przez WASM ABI (host functions + callbacks), mesh peers przez QUIC.

## 1. Zasady projektowe

1. **Single source of truth dla typów:** Rust crate `tentaflow-sdk-spec`. Inne języki dostają codegen.
2. **CBOR deterministic everywhere** — nie JSON, nie msgpack, nie protobuf.
3. **Stable identity:** każdy komponent w UI ma `id` deterministyczny w obrębie panelu, frontend trzyma DOM keyed by `id`.
4. **Slot-as-first-class:** addon NIGDY nie wysyła "całego tree", tylko fragmenty do nazwanych slotów + state patches.
5. **State patches before component patches:** dane są oddzielone od struktury UI. Zmiana wartości komórki = state patch, nie nowy fragment.
6. **Local-first interakcje:** modale, drafty inputów, conditional rendering robi frontend bez round-tripu.
7. **Owner-based sandboxing:** każda struktura wisząca w runtime ma `owner_addon_id`, weryfikowany po stronie hosta autorytatywnie + frontend defense-in-depth.
8. **Channel separation:** różne klasy komunikacji (UI, host functions, streams, mesh, control) mają osobne tag spaces i polityki.
9. **Trust boundaries explicit:** trzy granice — Frontend↔Core, Core↔Addon, Core↔Core (mesh). Każda ma osobne validation/audit.
10. **Strict limits:** wszystkie kolekcje, stringi, drzewa mają maksymalne rozmiary. Brak runtime DoS przez gigantyczne payloady.
11. **Hard versioning:** klient i serwer muszą zgadzać się co do `protocol_version`. Brak forward/backward compatibility w v1.

## 2. CBOR encoding profile

### 2.1 Obowiązkowy profil — RFC 8949 Core Deterministic Encoding

Wszystkie wiadomości MUSZĄ być encoded zgodnie z **RFC 8949 §4.2.1 (Core Deterministic Encoding Requirements)** plus **§4.2.2 (preferred serialization for floating-point)**:

- **Integer encoding (§4.2.1):** najmniejsza możliwa reprezentacja (preferred serialization). Wartości `0..=23` → embed w initial byte (1 bajt), `24..=255` → 1+1 bajt, `256..=65535` → 1+2 bajty, itd.
- **Float encoding (§4.2.2 — shortest-form preferred):** float MUSI być encoded w najmniejszej szerokości która zachowuje wartość bez utraty precyzji. Wartość reprezentowalna bez utraty jako half-precision (16-bit) → `0xf9 XXXX`. W przeciwnym razie próbujemy single (32-bit) → `0xfa XXXXXXXX`. W ostateczności double (64-bit) → `0xfb XXXXXXXXXXXXXXXX`. **`+inf`, `-inf` i `NaN` MUSZĄ być encoded jako half-precision** (`0xf9 7c00`, `0xf9 fc00`, `0xf9 7e00` — canonical NaN bit pattern z RFC 8949 §4.2.2).
- **Map key ordering (§4.2.1 bytewise lexicographic):** klucze map sortowane rosnąco po bajtach **ich własnego CBOR encoding** (nie po raw bytes klucza). Tj. integer klucz `5` encodowany jako `0x05` posortuje się przed text string `"a"` encodowanym jako `0x61 0x61`, bo `0x05 < 0x61`.
- **No indefinite-length items:** wyłącznie definite-length arrays, maps, byte strings, text strings. Encoder NIE może użyć `0x9f`, `0xbf`, `0x5f`, `0x7f`.
- **No duplicate map keys.**
- **Semantic tags:** dozwolone tylko wymienione explicit w §2.3. Wszystkie inne tagi → reject.
- **No Unicode normalization. Byte-exact text:** SDK NIE wykonuje żadnej formy normalizacji (no NFC, no NFD, no case folding) na żadnym `tstr`. Strings są przekazywane byte-exact od addona do hosta. Konsekwencja: jeśli addon napisze `"café"` w jednym SDK jako NFC (4 codepoints) a w innym SDK jako NFD (5 codepoints), to są **dwie różne wartości** — addon-author odpowiada za consistency. Contract tests używają explicit fixed byte sequences.
- **Byte strings vs text strings:** binary data (hashes, signatures, UUID, NodeId) → `bstr`. Tekst readable (ids, paths components, messages, action_id) → `tstr`. SDK MUSI walidować że text strings są valid UTF-8 (reject invalid sequences).

### 2.2 Walidacja determinizmu

Każde SDK MUSI implementować encoder zgodny z §2.1 + standalone validator (no-op pass detection). Contract test suite zawiera ~80 reprezentatywnych payloadów (handshake, panel shells, state patches, batches, commands, edge cases dla floats/ints/strings). Test: każdy SDK encode'uje, wynikowe bajty MUSZĄ być bit-identical między językami (verified SHA-256 hash equality).

Walidacja po stronie hosta (decode):
- Reject indefinite-length item → `Error{ProtocolViolation, IndefiniteLengthForbidden}`
- Reject niesortowane klucze → `NonCanonicalKeyOrder`
- Reject integer nie w preferred serialization → `NonCanonicalIntegerWidth`
- Reject float nie w shortest-form → `NonCanonicalFloatWidth`
- Reject duplicate keys → `DuplicateMapKey`
- Reject invalid UTF-8 w `tstr` → `InvalidTextString`
- Reject unknown semantic tag → `UnknownSemanticTag`

### 2.3 Allowed semantic tags

**Brak semantic tagów dozwolonych w v1.** Wszystkie semantic tagi (0, 1, decimal, bigint, base64, …) są **rejected**. UUID/NodeId/binary IDs są raw `bstr` o ustalonej długości. Timestampy są raw `i64` ms (epoch). Decimals nie istnieją — używamy integer (np. kwoty w groszach jako i64).

Powód: semantic tagi w hot path zwiększają cost decode + komplikują canonical equality (różne reprezentacje tej samej wartości). Eliminacja tagów = jeden encoding per wartość.

### 2.4 Wire size profile (orientacyjne)

| Field type | Bytes (typowo) |
|------------|----------------|
| `u8` flag  | 1-2            |
| `u16` tag  | 2-3            |
| `u32` id   | 1-5 (zmienne)  |
| `u64` timestamp ms | 5-9    |
| Short string (≤23 chars) | 1 + N |
| String 24-255 chars | 2 + N  |
| Empty array | 1 byte         |
| Map with 5 keys | ~ keys × (key_bytes + value_bytes) + 1 |

Typowy Action message: 200-500 B. Typowy PanelShell: 3-10 KB. State patch (10 ops): 500-2000 B.

## 3. Channel architecture

Protokół jest podzielony na **5 kanałów logicznych**. Każdy kanał ma własny tag space, własne reguły walidacji, własne polityki rate limit/audit.

| Channel | Code | Direction(s) | Purpose |
|---------|------|--------------|---------|
| `ui`       | 0x01 | Frontend↔Core, Core→Addon, Addon→Core | UI rendering, slots, state, actions, commands |
| `host_fn`  | 0x02 | Addon→Core | Addon woła host services (SQL, LLM, HTTP, vision, ...) |
| `stream`   | 0x03 | Bidirectional | Long-lived streamy (LLM tokens, file upload, video frames) |
| `mesh`     | 0x04 | Core↔Core | Inter-node mesh komunikacja (trust sync, frame proxy, etc.) |
| `control`  | 0x05 | Frontend↔Core | Handshake, lifecycle, auth, capabilities, heartbeat |

Kanały są **enforced**: tag z innego kanału w niewłaściwym envelope = `ProtocolViolation`. Każdy connection negotiates jakie kanały są otwarte podczas handshake (sekcja 5).

**Scope tego dokumentu:** kanały `ui` (§6), `stream` (§7), `control` (§5). Trust boundary: Frontend↔Core, oraz Core↔Addon dla UI dispatch.

**Out of scope tego dokumentu** (są w osobnych specyfikacjach, NIE blokują implementacji UI):
- Kanał `host_fn` (addon woła `sql.query`, `llm.chat`, `http.request`, `vision.frame_get`, …) — osobny dokument `ADDON_HOST_FUNCTIONS_v1.md`.
- Kanał `mesh` (inter-node komunikacja TentaFlow peers) — już istniejące `MESH_MSG_*` protocols, do redokumentowania pod CBOR profile w `MESH_PROTOCOL_v1.md`.

UI channel jest stand-alone — implementacja może iść do produkcji bez finalizacji host_fn/mesh specs. Reservacje tag space dla nich w envelope.channel zapobiegają kolizjom.

## 4. Envelope

Każda wiadomość ma envelope. Reprezentacja CBOR: **map z integer keys** (zwarte, deterministyczne).

**Konwencja struktur:** wszystkie struktury CBOR w tym dokumencie (Envelope, Capability, ProtocolHello, ProtocolWelcome, SessionEnd, …) są encodowane jako CBOR maps z **integer keys** (nie tstr). Mapowanie nazwa-pola → integer key jest **źródłowo zdefiniowane w `tentaflow-sdk-spec`** (Rust crate) i automatycznie propagowane do generated SDK przez `tentaflow-sdk-gen`. Doc używa nazw pól dla czytelności; concrete integer assignments są w `src/protocol/*.rs`. Wyjątek: free-form `map<tstr, Value>` (np. `Capability.params`, `Event.payload`) — używa tstr keys, sortowanych bytewise canonical (§2.1).

```
Envelope (CBOR map):
  0: protocol_version (u16, MUST = 1)
  1: channel          (u8, see §3 table)
  2: msg_id           (u64, monotonic per connection)
  3: correlation_id   (u64 or null, jeśli response/event do msg_id)
  4: ts_ms            (i64, unix epoch ms — server clock truth)
  5: session_id       (bstr 16 — UUID v4, stabilny przez całe życie sesji włącznie z resume)
  6: trace_id         (bstr 16 or null, dla distributed tracing)
  7: deadline_ms      (u32 or null, soft TTL od ts_ms do unieważnienia)
  8: priority         (u8: 0=bulk, 1=normal, 2=interactive, 3=control. Default 1.)
  9: flags            (u32 bitset, see §4.1)
  10: payload         (array [tag: u16, body: any])
```

Wszystkie pola wymagane chyba że "or null".

### 4.1 Flags bitset

| Bit  | Name                       | Meaning |
|------|----------------------------|---------|
| 0    | `FLAG_RELIABLE`            | Wymaga reliable delivery (sesja nie może zgubić). Default ON dla `ui` + `control`. |
| 1    | `FLAG_IDEMPOTENT`          | Bezpieczne retry. Frontend może powtórzyć przy timeout. |
| 2    | `FLAG_REJECT_ON_OVERLOAD`  | Jeśli queue overflowed → reject ten message zamiast drop najstarszego. |
| 3    | `FLAG_AUDIT_REQUIRED`      | Sensitive operation, audit log obowiązkowy. Set przez host, nie addon. |
| 4-31 | reserved                   | MUST be zero. |

Batch nie używa flag — `Batch` payload (tag 0x0160) ma jeden envelope dla wszystkich members; per-member granice idą przez `members[].tag` array, nie przez flags.

### 4.2 ID monotonicity i namespacing

- `msg_id`: unique per (connection, direction). Frontend ma własną sekwencję wysyłanych msg_id, core ma swoją. Brak global ordering.
- `session_id`: stabilny przez resume. Pierwsza wiadomość po reconnect ma ten sam `session_id`.
- `correlation_id`: zawsze referuje `msg_id` z **przeciwnego** kierunku.
- Klient nie generuje msg_id przypisywanych do core'a (i odwrotnie).

## 5. Control channel (handshake, lifecycle)

Tag space: `0x0500–0x05FF`.

### 5.1 Handshake (`ProtocolHello`, `ProtocolWelcome`)

**`ProtocolHello`** (tag `0x0501`, Frontend→Core, pierwsza wiadomość):

```
ProtocolHello:
  protocol_version: u16          (MUST = 1)
  client_version: tstr           ("tentaflow-web/1.0.0")
  capabilities_requested: array<Capability>
  auth: AuthContext              (token + signed metadata)
  resume: Resume or null
  client_credit_budget: CreditBudget  (initial credits klient ma zarezerwowane dla peer)

CreditBudget:
  ui: u32                        (default 256)
  stream_per_open: u32           (default 32)
  # control channel exempt — unlimited
```

```
Capability (CBOR map):
  0: name: tstr                  (np. "ui_v1", "streams_v1", "webtransport_datagrams", "compression_zstd")
  1: version: u32                (capability-specific version, np. catalog_version dla "ui_v1")
  2: hash: bstr 32 or null       (jeśli capability wymaga hash agreement, np. component catalog SHA-256)
  3: params: map<tstr, Value> or null  (capability-specific extra params)

Capability names zdefiniowane w v1:
  - "ui_v1"                      (UI channel; version = catalog_version, hash = catalog SHA-256)
  - "streams_v1"                 (stream channel; version = stream protocol revision)
  - "webtransport_datagrams"     (opcjonalne, tylko jeśli WT dostępny)
  - "compression_zstd"           (opcjonalne)
  - "audio_capture_passthrough"  (per-platform)

AuthContext:
  bearer_token: tstr or null     (JWT lub opaque session token)
  client_cert_fingerprint: bstr or null  (mTLS, jeśli używane)
  device_id: bstr 16 or null     (mobile, dla session pinning)
  origin: tstr                   ("https://app.tentaflow.io" lub package id)

Resume:
  prior_session_id: bstr 16
  last_received_msg_id: u64      (last msg_id otrzymany przez klienta od core)
```

**`ProtocolWelcome`** (tag `0x0502`, Core→Frontend):

```
ProtocolWelcome:
  protocol_version: u16          (MUST = 1)
  server_version: tstr           ("tentaflow-core/0.5.0")
  session_id: bstr 16            (nowe UUID jeśli no-resume, prior_session_id jeśli resume accepted)
  capabilities_granted: array<Capability>
  capabilities_rejected: array<{ capability: tstr, reason: tstr }>
  resume_status: ResumeStatus
  server_limits: ServerLimits
  server_time_ms: i64            (clock sync hint)

ResumeStatus (enum):
  - { kind: "fresh" }                                                       (new session, no prior state)
  - { kind: "resumed", mode: ResumeMode, next_msg_id: u64 }                 (state retained, see §14)
  - { kind: "rejected", reason: tstr }                                      (klient musi zacząć od zera)

ResumeMode (enum):
  - "replay"     (Tier A: core re-sends buffered messages from last_received_msg_id+1; DOM and stores unchanged)
  - "snapshot"   (Tier B: core sends StateSnapshot per open panel; sloty zachowują pre-disconnect DOM content niezależnie od tego co było w buforze — addon może re-issue SlotContent w dowolnym momencie po snapshot; patrz §14.2)

ServerLimits:
  max_message_bytes: u32             (np. 1 MB)
  max_state_path_segments: u16       (np. 32)
  max_components_per_fragment: u16   (np. 1000)
  max_component_depth: u16           (np. 64)
  max_state_patch_ops: u16           (np. 256)
  max_concurrent_streams: u16        (np. 32)
  max_queue_per_channel: u32         (np. 1024 — buffer dla flow-controlled messages gdy peer credit = 0)
  default_rate_limit_actions_per_sec: u16
  server_credit_budget: CreditBudget (initial credits które core daje klientowi)
```

**`ProtocolReject`** (tag `0x0503`, Core→Frontend, wysyłany przed close jeśli handshake fails):

```
ProtocolReject:
  reason: RejectReason
  message: tstr                  (developer-facing, ≤ 256 chars)
  retry_after_ms: u32 or null    (jeśli ratelimit/maintenance)

RejectReason (enum):
  - { kind: "version_mismatch", supported: array<u16> }
  - { kind: "auth_required", method: tstr }
  - { kind: "auth_invalid" }
  - { kind: "auth_expired" }
  - { kind: "origin_blocked" }
  - { kind: "capability_required", capability: tstr }
  - { kind: "rate_limited" }
  - { kind: "maintenance" }
  - { kind: "server_overloaded" }
```

Po wysłaniu `ProtocolReject` core zamyka transport. Klient nie może wysłać nic dalej na tej sesji.

### 5.2 Lifecycle messages

| Tag | Name | Direction | Description |
|-----|------|-----------|-------------|
| 0x0510 | `Heartbeat` | bidirectional | Co 30s; brak 90s → drop |
| 0x0511 | `SessionEnd` | bidirectional | Graceful close z reason |
| 0x0512 | `CapabilityRevoked` | Core→Frontend | Server zwija capability w trakcie sesji (np. utracone permissions) |
| 0x0513 | `RateLimitUpdate` | Core→Frontend | Nowe limity (np. po wypełnieniu kwoty) |

```
Heartbeat:
  (empty payload — timing source = envelope.ts_ms)

SessionEnd:
  code: SessionEndCode
  reason: tstr                   (developer-facing, ≤ 256 chars)

SessionEndCode (enum):
  - "user_initiated"             (klient zamyka świadomie)
  - "server_shutdown"             (planowy shutdown)
  - "idle_timeout"
  - "protocol_error"             (peer naruszył protokół)
  - "auth_expired"
  - "replaced"                    (otwarto nową sesję dla tego device_id / user)

CapabilityRevoked:
  capability: tstr                (capability name z handshake, np. "ui_v1")
  reason: tstr                    (≤ 256 chars)

RateLimitUpdate:
  scope: RateLimitScope
  actions_per_sec: u16            (nowy limit; 0 = pełna blokada)
  retry_after_ms: u32 or null     (sugerowane opóźnienie zanim peer próbuje dalej)

RateLimitScope (enum):
  - { kind: "global" }
  - { kind: "channel", channel: u8 }
  - { kind: "action", action_id: tstr }
```

### 5.3 Flow control (bidirectional credit-based)

| Tag | Name | Direction | Description |
|-----|------|-----------|-------------|
| 0x0520 | `CreditGrant` | bidirectional | Sender grants peer N credits for channel X |
| 0x0521 | `Backpressure` | bidirectional | Peer signals queue near full; sender should slow |
| 0x0522 | `QueueDepth` | bidirectional | Diagnostic snapshot (per stream/channel) |

```
CreditGrant:
  channel: u8                    (which channel credits apply to; 0xFF = all channels)
  credits: u32                   (amount granted; cumulative — peer adds to local credit balance)
  rationale: GrantRationale      (enum: InitialAdvertise | Refill | Recovery)

GrantRationale (enum):
  - "initial_advertise"          (pierwszy grant w sesji, ustawia baseline)
  - "refill"                     (regularne uzupełnienie po konsumpcji)
  - "recovery"                   (po stallu / overload, peer wraca do normalnego flow)

Backpressure:
  channel: u8                    (0xFF = all channels)
  queue_depth: u32               (bieżąca głębokość kolejki sendera)
  queue_capacity: u32            (configured limit; queue_depth/queue_capacity to fill ratio)
  severity: BackpressureSeverity

BackpressureSeverity (enum):
  - "warn"                       (≥ 75% capacity, slow down)
  - "critical"                   (≥ 95% capacity, halt non-essential traffic)

QueueDepth:
  channel: u8                    (0xFF = aggregated across all channels)
  outbound_pending: u32          (messages waiting to send)
  inbound_pending: u32           (messages waiting to process)
  credits_available: u32         (current credit balance for this channel)
  sampled_at_ms: i64             (clock źródło: nadawca QueueDepth)
```

**Symmetric model:** każda strona (Frontend, Core, Addon-through-Core) ma własny credit pool dla każdego kanału. Sender konsumuje 1 credit per reliable message wysłany; receiver grant'uje credits w miarę konsumowania.

**Initial credits (advertised w ProtocolWelcome.server_limits + analogiczne pole w Hello):**
- `ui` channel: 256 credits initial, refill on consume
- `stream` channel: 32 credits per active stream
- `control` channel: **unlimited** (no flow control — control messages bypass credits, ale są subject to rate limit)

**Refill pattern:** receiver grant'uje gdy ≥ 50% credits spent w bieżącym oknie (typowo co kilkadziesiąt messages).

**Credit exhaustion:** sender z `credits = 0` na danym kanale MUSI buforować lokalnie do limitu (typowo `max_queue_per_channel = 1024` messages, advertised w server_limits). Overflow → `Error{QueueOverflowed}` lub drop oldest (z `FLAG_REJECT_ON_OVERLOAD` → reject the new message).

**Po resume:** credits są **resetowane** do initial values. Counters credit są per-session, nie persisted.

**Control channel exemption:** Heartbeat, CapabilityRevoked, RateLimitUpdate, Backpressure, QueueDepth, CreditGrant SAME są poza flow control accounting (nie konsumują credits) — inaczej deadlock przy zerowych credits.

## 6. UI channel — wiadomości

Tag space: `0x0100–0x01FF`.

### 6.1 Tag table

| Tag | Name | Direction | Description |
|-----|------|-----------|-------------|
| 0x0101 | `PanelOpen` | Frontend→Core→Addon | User otwiera panel addona |
| 0x0102 | `PanelShell` | Addon→Core→Frontend | Addon zwraca shell panelu (initial layout + sloty + initial state) |
| 0x0103 | `PanelReady` | Frontend→Core | Frontend zbudował DOM, gotowy na patche |
| 0x0104 | `PanelError` | Core→Frontend | Core nie mógł otworzyć panelu (addon error, timeout, permission deny, addon crashed) |
| 0x0105 | `PanelClose` | Frontend→Core→Addon | User opuszcza panel |
| 0x0106 | `PanelReset` | Core→Frontend | Pełen refresh panelu — nowy epoch (przydzielony przez core), frontend re-builds shell |
| 0x0110 | `SlotContent` | Addon→Frontend | Wypełnij/zamień slot fragmentem |
| 0x0111 | `SlotClear` | Addon→Frontend | Zresetuj slot do default |
| 0x0112 | `SlotShow` | Addon→Frontend | Pokaż slot (np. modal) — bez podmiany content |
| 0x0113 | `SlotHide` | Addon→Frontend | Ukryj slot (np. modal) |
| 0x0120 | `StateSnapshot` | Addon→Frontend | Pełen snapshot state namespace (po resume / reset) |
| 0x0121 | `StatePatch` | Addon→Frontend | Granular state mutation (lista ops) |
| 0x0122 | `StateReset` | Addon→Frontend | Wyczyść namespace, future patches zaczynają od pustego |
| 0x0123 | `PatchRejected` | Frontend→Core | Frontend nie mógł zaaplikować patcha (z reason) |
| 0x0130 | `Action` | Frontend→Core→Addon | User wykonał akcję |
| 0x0131 | `ActionAck` | Addon→Frontend | Confirmation/rejection akcji |
| 0x0140 | `Command` | Addon→Frontend | Side-effect (modal, toast, navigate, focus, ...) |
| 0x0150 | `Event` | bidirectional | Pub/sub event (live updates, system signals) |
| 0x0160 | `Batch` | bidirectional | Atomic/non-atomic batch wielu wiadomości |

### 6.2 Panel lifecycle messages

**`PanelOpen`** (0x0101, Frontend→Core; Core→Addon dispatch carries assigned_epoch):

```
PanelOpen (frontend→core):
  addon_id: tstr
  panel_id: tstr
  ctx: PanelOpenContext

PanelOpenContext (core→addon enriches with assigned_epoch):
  user_id: tstr
  locale: tstr                   (BCP 47, np. "pl-PL")
  theme: tstr                    ("dark" | "light")
  viewport:
    width_px: u32
    height_px: u32
    density: f32                 (devicePixelRatio)
  deep_link: tstr or null
  prefers_reduced_motion: bool
  prefers_high_contrast: bool
  assigned_epoch: u64            (set by core, NOT by frontend; addon MUST echo in PanelShell)
```

**Panel epoch ownership:** epoch jest przydzielany **wyłącznie przez core**. Frontend NIE wysyła epoch w `PanelOpen` (envelope nie ma tego pola dla tego message type). Addon NIE generuje epoch sam — używa `assigned_epoch` z dispatchowanego `PanelOpenContext`. `PanelReset` jest **wyłącznie inicjowany przez core** (np. po addon crash, po fatal protocol error, po explicit admin trigger). Addon który chce zresetować panel używa innych mechanizmów (StateReset + SlotClear sequences).

Frontend wysyła gdy user nawiguje do panelu addona. Core przydziela epoch, dispatch'uje do addona. Addon ma `default_panel_open_timeout_ms` = 2000 ms na odpowiedź `PanelShell` zanim core wysyła `PanelError{code: AddonTimeout}` do frontendu i unloadduje session state dla tego panelu.

**`PanelShell`** (0x0102):

```
PanelShell:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64               (echo z PanelOpenContext.assigned_epoch; addon nie generuje sam)
  layout: Component              (root, typowo structured molecule)
  slots: array<SlotDecl>
  initial_state: array<StateEntry>    (lista tuples zamiast map<StatePath,V> bo StatePath jest złożony)
  initial_commands: array<Command>

StateEntry:
  path: StatePath
  value: Value

SlotDecl:
  id: tstr                       (unique within panel)
  semantics: SlotSemantics       (enum: MainContent | Modal | Drawer | Toast | SidePanel | TabPane | Popover | Custom)
  default_state: SlotDefault     (enum: Empty | Loading | Static(Fragment))
  cache_policy: CachePolicy      (enum: None | OnNavigateBack | TTLSeconds(u32))
  visibility: SlotVisibility     (enum: Always | Hidden | Conditional(StatePath))
  max_payload_bytes: u32 or null (per-slot limit, default = server_limits.max_message_bytes)
```

**`PanelReady`** (0x0103, Frontend→Core):

```
PanelReady:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64
  first_paint_ms: u32            (telemetry: czas od PanelOpen do first paint)
```

Frontend sygnalizuje że DOM zbudowany, subscribers podpięci, gotowy na patche. Patche przed `PanelReady` są **buforowane** po stronie core'a (do limit 1000 ops) i flush'owane po `PanelReady`.

**`PanelReset`** (0x0106):

```
PanelReset:
  addon_id: tstr
  panel_id: tstr
  new_panel_epoch: u64           (MUST be > current epoch)
  reason: tstr
```

Pełen refresh — frontend tear-down obecny shell i czeka na nowy `PanelShell`. Używane gdy addon zmienia layout fundamentalnie (rzadkie).

**`PanelClose`** (0x0105):

```
PanelClose:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64
  reason: CloseReason            (enum: UserNavigated | ConnectionDropped | AddonUnloaded | ServerInitiated)
```

### 6.3 Slot messages

**`SlotContent`** (0x0110):

```
SlotContent:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64                       (MUST match current epoch — stale rejected)
  slot_id: tstr
  fragment: Component                    (typed tree, root może być dowolny komponent)
  state_overlay: array<StateEntry> or null  (atomic state + fragment update; sortowane po canonical StatePath bytes)
```

**Replace strategy:** zawsze REPLACE. Slot ma jedną zawartość naraz. Listy/append idą **wyłącznie** przez `StatePatch` na bound state path. Brak `Append`/`Prepend` w SlotContent — codex caught this, intentional removal.

Host walidacja przed forward do frontend:
1. `(addon_id, panel_id, panel_epoch)` matches aktywny panel
2. `slot_id` ∈ declared slots tego panelu
3. fragment passes component validation (depth, count, schema)
4. `state_overlay` paths ∈ declared state namespace

**`SlotClear`** (0x0111), **`SlotShow`** (0x0112), **`SlotHide`** (0x0113): proste, z `addon_id + panel_id + panel_epoch + slot_id`.

### 6.4 State messages

**`StatePath`** — jeden format wszędzie (strukturalny, typed segments). Brak alternatywnego "string form" — eliminuje ambiguity escape character.

```
StatePath:
  segments: array<PathSegment>            (max 32 segments — see §9)

PathSegment (enum):
  - { kind: "key", value: tstr }          (map key; może zawierać dowolne znaki — protokół nie escape'uje)
  - { kind: "index", value: u32 }         (array index)
```

W miejscach gdzie potrzebny path-keyed lookup w CBOR (np. `StateSnapshot`), używamy **array of `StateEntry { path, value }`**, nigdy `map<StatePath, V>`. CBOR map keys jako złożone struktury są legalne ale problematyczne dla canonical sortowania — eliminujemy.

**`StateSnapshot`** (0x0120):

```
StateSnapshot:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64
  state_revision: u64                     (każdy snapshot/patch zwiększa)
  entries: array<StateEntry>              (full namespace as tuples; sortowane po canonical StatePath bytes dla deterministycznego encoding)
  truncated: bool                         (true jeśli snapshot przekraczał max_message_bytes i był podzielony — patrz §6.4.1)
```

Używane po resume jeśli core nie mógł odtworzyć patch history. Frontend nadpisuje swój store atomicznie.

#### 6.4.1 Snapshot chunking

Jeśli pełen snapshot przekracza `max_message_bytes`, core wysyła sekwencję `StateSnapshot` messages z `truncated: true` w każdym poza ostatnim. Frontend buforuje, aplikuje atomicznie po otrzymaniu ostatniego (`truncated: false`). Wszystkie chunki mają ten sam `state_revision`. Frontend rzuca chunky jeśli przyjdzie wiadomość z innego revision — czeka na fresh snapshot.

**`StatePatch`** (0x0121):

```
StatePatch:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64
  base_revision: u64             (state_revision które addon zakłada)
  new_revision: u64              (po zaaplikowaniu)
  ops: array<PatchOp>

PatchOp:
  path: StatePath
  op: PatchOpKind

PatchOpKind (enum):
  - { kind: "set", value: Value }
  - { kind: "delete" }
  - { kind: "append_array", value: Value }
  - { kind: "prepend_array", value: Value }
  - { kind: "insert_array", index: u32, value: Value }
  - { kind: "remove_array", index: u32 }
  - { kind: "merge_map", value: map }
  - { kind: "increment", delta: i64 }
```

**Reconciliation rules:**
- Frontend MUST track `state_revision` per panel.
- Patch z `base_revision != current_revision` → **reject** with `PatchRejected{ reason: "revision_mismatch", current_revision }`.
- Core po `PatchRejected` może: re-send `StateSnapshot` (full sync), albo retry patch (jeśli intermediate revisions były tylko local optimistic — rare).
- Patch ops aplikują się **atomically** w obrębie jednej wiadomości (all-or-nothing).

**`StateReset`** (0x0122):

```
StateReset:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64
  new_revision: u64              (= 0 lub explicit value)
```

Czyści state namespace addona dla panelu. Komponenty z bindings widzą undefined values (default rendering).

**`PatchRejected`** (0x0123, Frontend→Core):

```
PatchRejected:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64
  rejected_msg_id: u64
  reason: PatchRejectReason
  current_revision: u64 or null

PatchRejectReason (enum):
  - "revision_mismatch"
  - "path_ownership_violation"
  - "path_out_of_namespace"
  - "type_mismatch"
  - "array_bounds"
  - "depth_exceeded"
  - "structural_limit"
```

### 6.5 Action messages

**`Action`** (0x0130):

```
Action:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64
  action_id: tstr                (declared by addon, validated against panel manifest)
  params: map<tstr, Value>
  form_values: map<tstr, FormFieldValue> or null
  user_gesture: bool             (true jeśli wynik direct user click/keystroke, false jeśli automated)
  client_action_id: bstr 16      (idempotency key — retry same client_action_id == replay-safe)

FormFieldValue:
  value: Value
  validated_locally: bool        (frontend wykonał validators z deklaracji)
```

`user_gesture` jest wymagany TRUE dla actions które wywołują `Command::Copy`, `Command::NavigateExternal`, `Command::Download` (browser security model wymaga user activation).

**`ActionAck`** (0x0131):

```
ActionAck:
  addon_id: tstr
  panel_id: tstr
  panel_epoch: u64
  action_id: tstr                (echo z Action; weryfikowane przez frontend i validator)
  client_action_id: bstr 16      (echo z Action — idempotency key)
  status: ActionStatus

ActionStatus (enum):
  - { kind: "ok" }
  - { kind: "rejected", reason: tstr, error_code: u16 }
  - { kind: "permission_denied", required_permission: tstr }
  - { kind: "rate_limited", retry_after_ms: u32 }
  - { kind: "validation_failed", field_errors: array<FieldError> }
  - { kind: "error", error_code: u16, message: tstr }
  - { kind: "redirected", to_action_id: tstr, params: array<ParamEntry> }   (np. wymaga MFA)

FieldError:
  field_id: tstr
  error_code: u16
  message: tstr

ParamEntry:
  key: tstr
  value: Value
```

**Correlation strategy:**
- Envelope `correlation_id: u64` = `Action.msg_id`. Wszystkie wiadomości generowane w odpowiedzi na konkretną Action (StatePatch, SlotContent, Command, ActionAck) MUSZĄ ustawić `correlation_id` w envelope na `Action.msg_id`. Frontend grupuje efekty per correlation_id.
- `client_action_id` (bstr 16) jest **idempotency key** — używany do dedup retry. Pojawia się **tylko** w Action (request) i ActionAck (response). Inne messages w response chain (StatePatch/SlotContent/Command) **nie** zawierają client_action_id — ich powiązanie z Action idzie wyłącznie przez envelope.correlation_id.

### 6.6 Commands

**`Command`** (0x0140) — discriminated union, side-effecty dla frontendu.

```
Command (variants):
  - ShowModal { slot_id: tstr }                                  (slot MUST be SlotSemantics::Modal owned by this addon)
  - HideModal { slot_id: tstr }
  - ShowDrawer { slot_id: tstr, side: DrawerSide }
  - HideDrawer { slot_id: tstr }
  - Toast {
      tone: Tone,
      title: tstr,
      body: tstr or null,
      duration_ms: u32 or null,
      action_label: tstr or null,
      action_id: tstr or null                                    (jeśli user kliknie action w toast)
    }
  - Navigate { panel_id: tstr, deep_link: tstr or null }         (w obrębie tego addona)
  - NavigateAddon { addon_id: tstr, panel_id: tstr }             (cross-addon — requires permission "navigation.cross_addon")
  - NavigateExternal {
      url: tstr,                                                 (MUST be https:// scheme; other schemes rejected)
      target: NavigateTarget                                     (enum: NewTab | SameTab; mobile: SystemBrowser)
    }                                                            (requires permission "navigation.external" + user_gesture=true)
  - Focus { component_id: tstr }
  - Scroll { component_id: tstr, behavior: ScrollBehavior }
  - Copy { value: tstr }                                         (requires user_gesture=true)
  - Download {
      signed_url_ref: tstr,                                      (host resolves to signed URL)
      filename: tstr                                             (MUST match [a-zA-Z0-9._-]+ regex; max 128 chars)
    }                                                            (requires user_gesture=true)
  - SetTitle { value: tstr }                                     (window/tab title; max 256 chars)
  - Confirm {
      title: tstr,
      message: tstr,
      confirm_label: tstr,
      cancel_label: tstr,
      destructive: bool,                                         (visual emphasis)
      on_confirm_action: tstr or null,                           (sends Action with this id if user confirms)
      on_confirm_params: map<tstr, Value> or null,
    }
  - ResetForm { component_id: tstr }
  - SetFormFieldValue { component_id: tstr, value: Value }
  - DismissToasts { tag: tstr or null }                          (null = all)
```

### 6.7 Events

**`Event`** (0x0150):

```
Event:
  source_addon_id: tstr
  topic: Topic                   (compiled, structured)
  payload: Value
  ts_ms: i64                     (event source time, może != envelope.ts_ms)

Topic:
  segments: array<TopicSegment>

TopicSegment (enum):
  - { kind: "literal", value: tstr }
  - { kind: "id", value: tstr }                                  (np. camera_id)
```

Klient subscribes przez (oddzielne) host_fn API z compiled patterns (no glob strings z addonów). Topic permissions są **compiled przez admin UI** — addon manifest deklaruje `[[event_publish]] pattern = ["tentavision.alert", "{level}"]`, admin approve'uje. Brak runtime glob escalation.

### 6.8 Batch

**`Batch`** (0x0160) — wielokrotne payloady w jednym envelope (jeden msg_id, jeden correlation_id, jeden ts_ms). NIE zagnieżdżone envelopes.

```
Batch:
  atomic: bool                   (true = all-or-nothing, false = best-effort)
  members: array<BatchMember>

BatchMember:
  tag: u16                       (payload tag, np. 0x0121 dla StatePatch)
  body: Value                    (payload body matching tag schema)
```

Wszystkie members dziedziczą metadata z outer envelope: `channel`, `msg_id`, `correlation_id`, `ts_ms`, `session_id`, `trace_id`, `priority`, `flags`. Nie ma per-member metadata.

**Atomic batch:** jeśli któryś member fails validation lub aplikacji, **wszystkie są dropped**, frontend zostaje w stanie pre-batch. Core wysyła `Error{...}` z `correlation_id` = batch envelope `msg_id` i `failed_member_index: u32` w details.

**Non-atomic batch:** każdy member validate'owany niezależnie. Failures logged + `Error{...}` per failed member (z indices). Successful members są aplikowane.

**Limity:**
- max 64 members per batch
- total CBOR bytes po encode ≤ `max_message_bytes`
- batch nie może zawierać `Batch` jako member (no nesting)
- batch nie może zawierać control channel messages (Heartbeat, etc.)

## 7. Stream channel

Tag space: `0x0300–0x03FF`.

Dla long-running operations: LLM token streaming, file uploads, video frame streams, search results pagination.

### 7.1 Stream lifecycle

| Tag | Name | Direction | Description |
|-----|------|-----------|-------------|
| 0x0301 | `StreamOpen` | initiator → peer | Otwiera stream, deklaruje kind |
| 0x0302 | `StreamAccepted` | peer → initiator | Stream id przydzielony |
| 0x0303 | `StreamRejected` | peer → initiator | Z error code |
| 0x0310 | `StreamChunk` | bidirectional | Payload chunk |
| 0x0311 | `StreamProgress` | producer → consumer | Optional progress hint (bytes/items/percentage) |
| 0x0320 | `StreamEnd` | producer → consumer | Normal end |
| 0x0321 | `StreamCancel` | bidirectional | Abort z reason |
| 0x0322 | `StreamError` | producer → consumer | Fatal stream error |

```
StreamOpen:
  stream_id: u32                 (initiator-side unique)
  kind: StreamKind               (enum: LlmTokenStream | FileUpload | FileDownload | VideoFramePreview | SearchResults | Custom)
  metadata: map<tstr, Value>     (kind-specific)
  expected_total_bytes: u64 or null
  reliable: bool                 (default true; false → datagram-mode if supported)

StreamChunk:
  stream_id: u32
  sequence: u32                  (chunk index, monotonic)
  data: bstr                     (binary payload OR text for token streams)
  end_of_stream: bool            (terminal chunk — equivalent to StreamEnd inline)
```

Streams są reliable by default. Datagram mode tylko jeśli capability `webtransport_datagrams` negotiated + sender explicit opt-in.

### 7.2 Flow control per stream

Każdy stream ma własny credit pool (default 32 chunks). Consumer wysyła `CreditGrant` per `stream_id` po przetworzeniu.

## 8. Slot ownership & validation

### 8.1 Owner registry (per session)

Core trzyma per-session state:

```
SessionState:
  session_id: bstr 16
  open_panels: map<(addon_id, panel_id), PanelOwnership>
  open_streams: map<u32, StreamOwnership>           (key = stream_id)
  rate_limit_buckets: map<(addon_id, category), TokenBucket>
  credit_pools: map<(direction, channel), CreditPool>

PanelOwnership:
  panel_epoch: u64                                  (assigned przez core w PanelOpen)
  state_revision: u64
  declared_slots: set<tstr>                         (z PanelShell.slots[].id)
  declared_actions: set<tstr>                       (z addon manifest declared_actions[panel_id])
  declared_event_publish: set<TopicPattern>
  declared_event_subscribe: set<TopicPattern>
  declared_local_capabilities: set<LocalCapability> (z manifest — które LocalAction variants może deklarować)
```

**State namespace addressing:** state path NIE jest prefixowane addon/panel — wszystkie state paths są **relative do panel scope**. Core zapamiętuje że `(addon_id, panel_id)` ↔ logiczny namespace. Pełen kwalifikowany path (jeśli kiedyś potrzebny do logów/storage) jest `<addon_id>::<panel_id>::<path_segments>`, ale **w wire nigdy się nie pojawia**. Addon nie może wpłynąć na inny namespace bo wszystkie wiadomości od niego są tagged jego `addon_id`+`panel_id` w payload, validator weryfikuje że match z open_panels.

### 8.2 Validation rules (per message od addona)

Przy odbiorze każdej wiadomości z UI channel od addona przez core:

1. **CBOR Core Deterministic encoding check:** sortowanie kluczy, integer/float width, no indefinite-length, valid UTF-8, no unknown tags.
2. **Channel match:** payload tag jest w deklarowanym channel space (envelope.channel = 0x01 → tag 0x01XX).
3. **Payload schema match:** body pasuje do schema z catalog dla danego tagu.
4. **Panel exists:** dla messages z `(addon_id, panel_id)` w payload — para musi być w `open_panels`.
5. **Panel epoch match:** `panel_epoch` w payload = `PanelOwnership.panel_epoch`. Stale → drop with `Error{StalePanelEpoch}` + audit.
6. **Slot ownership:** dla SlotContent/SlotClear/SlotShow/SlotHide: `slot_id` ∈ `declared_slots`.
7. **Action declared:** dla ActionAck: `action_id` ∈ `declared_actions`.
8. **State path validity:** dla StatePatch/StateReset/initial_state w PanelShell: każdy StatePath ma ≥ 1 segment, ≤ 32 segments, pierwszy segment ≠ reserved root keys (`__system`, `__user`).
9. **State revision continuity:** dla StatePatch: `base_revision` = `PanelOwnership.state_revision`. Mismatch → `Error{RevisionMismatch}`.
10. **Event topic permission:** dla Event publish: `topic` matches one of `declared_event_publish` patterns (compiled).
11. **Local capabilities:** komponenty z handlerami sprawdzane — każdy `LocalAction` variant w handler tree musi być w `declared_local_capabilities` (patrz §10.3).
12. **Structural limits:** patrz §9.
13. **Rate limit:** odpowiedni bucket dla kategorii operation.
14. **Audit:** każda violation kategorii A/B → `audit_log` z full context.

Frontend wykonuje **defensive subset** (envelope canonical, panel_epoch match, slot ownership, structural limits) jako defense in depth. Authoritative validation jest jednak po stronie core.

### 8.3 Reserved root state keys (per panel namespace)

State path jest **strukturalny** (typed segments). Validator porównuje **segments[0]** strict equality, nie prefix matching:

| segments[0] (type, value) | Status | Notes |
|---------------------------|--------|-------|
| `{ kind: "key", value: "__system" }` | RESERVED — read-only przez core | viewport, locale, theme, prefers_reduced_motion |
| `{ kind: "key", value: "__user" }` | RESERVED — read-only z user.profile.read permission | user_id, display_name, avatar_ref |
| `{ kind: "key", value: "__draft" }` | Writable z LocalAction + StatePatch | UI scratch (form drafts, modal open flags) |
| `{ kind: "key", value: "__optimistic" }` | Writable z LocalAction + StatePatch | optimistic mutations czekające na backend confirm |
| `{ kind: "key", value: "__committed" }` | Writable **wyłącznie** z StatePatch (backend) | server-side truth |
| `{ kind: "key", value: "<any other>" }` | Writable z StatePatch | addon custom domain state |
| `{ kind: "index", value: N }` | Forbidden as root segment | state root jest map, nie array |

**Klucz literal "__system" w kontekście nie-root** (np. segments[1].value="__system") jest **dozwolony** — tylko root segment ma znaczenie reserved. Klucz literal `"__system.foo"` jako single segment (segments[0] = `{ kind: "key", value: "__system.foo" }`) NIE jest reserved (różny string), ale jest legalny addon namespace key. Validator porównuje byte-exact string equality `value == "__system"`, NIE prefix matching.

Próba write/delete na zarezerwowanej top-level key z disallowed source → `Error{ReservedNamespace}` + audit (B class).

### 8.4 Reserved slot ids

W global slot space (cross-panel):
- `__shell:*` — core's own UI chrome (sidebar, breadcrumbs, header). Addon NIGDY nie deklaruje slot z tym prefiksem. PanelShell z takim slot_id → reject w handshake.

## 9. Strukturalne limity

Limity są **enforce'owane po stronie core'a** dla każdej wiadomości od addona. Nadprzekroczenie → reject z `Error{StructuralLimit, limit_name: ...}`.

| Limit | Default | Notes |
|-------|---------|-------|
| `max_message_bytes` | 1 MB | po CBOR encode |
| `max_component_depth` | 64 | drzewo komponentów |
| `max_components_per_fragment` | 1000 | total nodes w jednym SlotContent |
| `max_string_bytes` | 16384 | dowolny pojedynczy string field |
| `max_array_length` | 4096 | dowolna array (poza state patches i list bindings) |
| `max_map_entries` | 1024 | dowolna map |
| `max_state_patch_ops` | 256 | per StatePatch |
| `max_state_path_segments` | 32 | depth w StatePath |
| `max_commands_per_message` | 32 | initial_commands lub ActionAck side-effects |
| `max_handlers_per_component` | 16 | event handlers na komponencie |
| `max_local_handler_depth` | 8 | zagnieżdżenie Confirm.then, Sequence, Debounce.then |
| `max_local_handler_steps` | 16 | total steps w jednym handler tree |
| `max_concurrent_streams_per_addon` | 16 | open streams |
| `max_stream_chunk_bytes` | 64 KB | per chunk |
| `max_open_panels_per_addon` | 8 | per session |

Server limits są **advertised w ProtocolWelcome**. SDK MUST respektować (validate przed wysłaniem) — addon dostaje błąd lokalnie zamiast remote rejection.

## 10. Komponenty (Component type)

`Component` to **discriminated union** wszystkich UI primitives. Reprezentacja CBOR: **map z fields** keyed by **integer** (compact):

```
Component (CBOR map):
  0: tag           (u16, stable component discriminant)
  1: id            (tstr, unique within panel — MUST be set)
  2: fields        (map<u8, Value>, type-specific schema)
  3: handlers      (map<EventKind, Handler> or absent)
  4: bind          (BindSpec or absent)
  5: a11y          (Accessibility or absent)
  6: visibility    (Visibility or absent)
  7: test_id       (tstr or absent — for e2e tests, NOT exposed to user)
```

Pełne schemy każdego komponentu w `ADDON_UI_COMPONENT_CATALOG_v1.md`.

### 10.1 Tag space (high byte = category)

| Range | Category |
|-------|----------|
| 0x0000–0x00FF | Structured molecules (Header, PageHeader, EmptyState, AppShell, ...) |
| 0x0100–0x01FF | Layout primitives (Flex, Grid, Stack, Cluster, Card, Divider, Spacer, ...) |
| 0x0200–0x02FF | Data display (Text, Heading, StatCard, Table, List, Tree, Timeline, Heatmap, Chart, ...) |
| 0x0300–0x03FF | Form (Input, Textarea, Select, Combobox, Toggle, Checkbox, DatePicker, FileInput, ...) |
| 0x0400–0x04FF | Action (Button, IconButton, MenuButton, ButtonGroup, Link, SegmentedControl, ...) |
| 0x0500–0x05FF | Feedback (Alert, Toast, Modal, Drawer, Popover, Tooltip, Banner, Skeleton, Spinner, ...) |
| 0x0600–0x06FF | Specialized (VideoStream, Canvas2D, WebGLSurface, WGPUSurface, MapView, CodeEditor, RichText, ...) |
| 0x0700–0x07FF | Domain-specific (PermissionMatrix, NetworkRuleEditor, RelationGraph, AlarmFeed, ...) |
| 0x0F00–0x0FFF | Reserved for future / experimental (private addons may use w/o stability guarantee) |

### 10.2 Accessibility, visibility, bindings

Canonical Accessibility i Visibility — patrz catalog §1.5. Wszystkie pola opcjonalne, używają `BindRef<...>` dla wartości które mogą być dynamiczne. Schemy:

```
Accessibility:
  role: tstr or null
  label: BindRef<tstr> or null
  label_for: tstr or null
  described_by: tstr or null
  live: LiveRegion or null
  expanded: BindRef<bool> or null
  disabled: BindRef<bool> or null
  required: BindRef<bool> or null
  invalid: BindRef<bool> or null
  pressed: BindRef<bool> or null
  selected: BindRef<bool> or null

Visibility:
  visible: BindRef<bool> or null     (default true)
  display_above_breakpoint: Breakpoint or null
  display_below_breakpoint: Breakpoint or null
  hidden_for_assistive: bool         (aria-hidden)

BindSpec (canonical definicja — wykorzystywana w obu dokumentach, patrz catalog §1.4):
  - { kind: "text", path: StatePath, format: ValueFormat or null }
  - { kind: "attr", name: tstr, path: StatePath }
  - { kind: "class_toggle", class_name: tstr, path: StatePath, negate: bool }
  - { kind: "show", path: StatePath, negate: bool }
  - { kind: "list", path: StatePath, item_template_id: tstr, key_field: tstr or null }
  - { kind: "two_way", path: StatePath }                         (form fields)

Wariant `formatted` z wcześniejszych draftów został scalony z `text { format }`.
```

### 10.3 Handlers

```
Handler (discriminated union):
  - Local(LocalAction)
  - Backend {
      action_id: tstr,
      params: map<tstr, Value>,
      optimistic: StatePatch or null,
      on_failure: FailurePolicy                                  (enum: Toast | RevertOptimistic | Custom(LocalAction))
    }
  - Both {
      action_id: tstr,
      params: map<tstr, Value>,
      optimistic: StatePatch,                                    (required)
      on_failure: FailurePolicy                                  (default RevertOptimistic)
    }

LocalAction (discriminated union, capability-gated):
  - ShowModal(tstr)                                              (slot_id of Modal owned by this addon)
  - HideModal(tstr)
  - ToggleSlot(tstr)                                             (show if hidden / hide if shown)
  - SetState { path: StatePath, value: Value }                   (path MUST satisfy §10.3.1 restrictions)
  - DeleteState { path: StatePath }                              (path MUST satisfy §10.3.1 restrictions)
  - Toggle(StatePath)                                            (path MUST satisfy §10.3.1 restrictions; flips bool)
  - Increment { path: StatePath, delta: i64 }                    (path MUST satisfy §10.3.1 restrictions)
  - Navigate(tstr)                                               (panel_id within addon)
  - Focus(tstr)
  - Scroll { component_id: tstr, behavior: ScrollBehavior }
  - Copy(tstr)                                                   (literal value or "<bind:path>" templated; requires user_gesture)
  - Confirm {
      title: tstr,
      message: tstr,
      destructive: bool,
      then: Handler                                              (executed if user confirms; bounded depth)
    }
  - Validate {
      field_component_id: tstr,
      rules: array<ValidationRule>,
      on_invalid: LocalAction                                    (typically SetState marking error)
    }
  - Debounce { ms: u32, then: Handler }                          (ms ≤ 5000)
  - Sequence(array<Handler>)                                     (max 8 items)
  - Conditional {
      when: StateCondition,                                      (simple bool expr against state)
      then: Handler,
      else: Handler or null
    }
  - Noop
```

**Local handler limits (enforced w validator):**
- Total recursion depth ≤ 8 (Sequence/Confirm/Conditional/Debounce nesting)
- Total steps in tree ≤ 16
- `Copy` wymaga `user_gesture: true` at outermost handler invocation. Local handlers nie zawierają `NavigateExternal` — external navigation jest zawsze backend `Command::NavigateExternal` (security check w core).
- `Sequence` max 8 items; recursive Sequence prohibited (linear chains only)
- No cycles (validator walks tree and detects)

#### 10.3.1 Local handler capability matrix

Addon manifest deklaruje które `LocalAction` variants może deklarować w handlerach (admin approve'uje):

```toml
[panel."cameras".local_capabilities]
declarations = [
  "ShowModal", "HideModal", "ToggleSlot",
  "SetState", "DeleteState", "Toggle", "Increment",
  "Navigate", "Focus", "Scroll",
  "Validate", "Debounce", "Sequence", "Conditional", "Noop",
  # NOT declared by default; require explicit approval:
  # "Copy", "Confirm"
]
```

Variants nie wymienione w manifeście są **forbidden** dla tego panelu. Validator odrzuca PanelShell/SlotContent zawierający fragment z disallowed local action.

**SetState/DeleteState/Toggle/Increment path restrictions (strict segment[0] check, byte-exact):**
- Allowed: `segments[0] == { kind: "key", value: "__draft" }` — UI scratch (form drafts, modal open flags, expand state)
- Allowed: `segments[0] == { kind: "key", value: "__optimistic" }` — optimistic state przed backend confirm
- Forbidden: any other `segments[0]` value (włącznie z addon-domain keys i `__committed`)

Path do `__committed` jest writable **wyłącznie** przez StatePatch od backend (addon). LocalAction modyfikacja → `Error{PathOwnershipViolation}` w validatorze.

State convention per panel (recommended; validator enforce'uje tylko reserved keys z §8.3):
- root key `__draft` — local UI scratch (cleared on PanelClose)
- root key `__optimistic` — optimistic state (cleared after matching ActionAck arrives)
- root key `__committed` — authoritative state (mutated by StatePatch from addon backend)
- root keys `__system`, `__user` — reserved (§8.3)
- inne root keys — addon custom domain (writable przez StatePatch only)

### 10.4 Tone, Variant, Density tokens

```
Tone (enum):                     Neutral | Primary | Success | Warning | Critical | Info | Muted
ButtonVariant (enum):            Primary | Secondary | Tertiary | Ghost | Destructive | Link
BadgeVariant (enum):             Solid | Soft | Outline | Pulse | Dot
ChipVariant (enum):              Solid | Soft | Outline | Removable | Selectable | Toggle
Density (enum):                  Compact | Default | Comfortable
Spacing (enum):                  Zero | Xxs | Xs | Sm | Md | Lg | Xl | Xxl     (mapuje na 0/2/4/8/12/16/24/32 px)
TextStyle (enum):                Display | Title | H1 | H2 | H3 | H4 | BodyLg | Body | BodyStrong | Caption | Overline | Code | Mono | Quote
RadiusToken (enum):              None | Xs | Sm | Md | Lg | Xl | Pill | Circle
ShadowToken (enum):              None | Subtle | Medium | Elevated | Floating
BorderToken (enum):              None | Hairline | Thin | Strong | Accent(Tone)
Breakpoint (enum):                Xs | Sm | Md | Lg | Xl | Xxl                   (640/768/1024/1280/1536/1920 px)
IconName (enum):                  Camera | Cpu | AlertTriangle | Globe | ... (~200 named icons, exhaustive list w catalog)
IconSize (enum):                  Xs | Sm | Md | Lg | Xl                          (12/16/20/24/32 px)
ScrollBehavior (enum):            Auto | Smooth | Instant
DrawerSide (enum):                Left | Right | Top | Bottom
NavigateTarget (enum):            NewTab | SameTab | SystemBrowser                (mobile mapping)
LiveRegion (enum):                Off | Polite | Assertive
ValueFormat (enum, mapuje na localized format):
  - Number { decimals: u8, thousands_sep: bool }
  - Currency { code: tstr }                                                      (ISO 4217)
  - Percent { decimals: u8 }
  - Bytes
  - Duration { kind: enum (Short | Long | Stopwatch) }
  - Date { kind: enum (Short | Medium | Long | Full) }
  - Time { kind: enum (Short | Medium | Long) }
  - DateTime { kind: enum (Short | Medium | Long | Full) }
  - Relative                                                                     ("2 minutes ago")
```

## 11. Action flow — pełny round-trip przykład

User klika "Zapisz" w formularzu dodawania kamery:

1. **Frontend zbiera form values** z DOM (komponenty z `bind: TwoWay`).
2. **Frontend wykonuje local validators** (`Validate` handlery z deklaracji formularza). Jeśli invalid → set error states, abort.
3. **Frontend emituje `Action`**:
   ```
   Envelope { v:1, channel:0x01, msg_id:42, payload: Action {
     addon_id:"tentavision", panel_id:"cameras", panel_epoch:7,
     action_id:"camera.create",
     params:{}, form_values:{name:"front-door", ip:"192.168.1.5", ...},
     user_gesture:true, client_action_id: bstr(uuid)
   }}
   ```
4. **Core** waliduje:
   - Envelope canonical CBOR
   - Action exists in panel manifest
   - Rate limit OK
   - Permission OK
   - Forward do addona przez WASM callback `on_action(...)`
5. **Addon** (WASM):
   - Deserialize params/form_values do typed struct (SDK robi pod spodem)
   - Mutuje SQLite (INSERT)
   - Buduje sekwencję odpowiedzi
   - Wywołuje host functions (SDK helper `ui.batch(...)` builds atomic Batch payload):
     ```
     Envelope {
       v:1, channel:0x01, msg_id:100, correlation_id:42, ts_ms:..., session_id:..., trace_id:..., 
       deadline_ms:null, priority:2, flags:FLAG_RELIABLE,
       payload: Batch {
         atomic: true,
         members: [
           { tag:0x0121, body: StatePatch {
               addon_id:"tentavision", panel_id:"cameras", panel_epoch:7,
               base_revision:41, new_revision:42,
               ops:[{
                 path: { segments:[{kind:"key",value:"__committed"},{kind:"key",value:"cameras"},{kind:"key",value:"list"}] },
                 op: { kind:"append_array", value: { id:"C-25", name:"front-door", ... } }
               }]
           }},
           { tag:0x0140, body: Command::HideModal{ slot_id:"camera-add" } },
           { tag:0x0140, body: Command::Toast{ tone:"success", title:"Kamera dodana", body:null, duration_ms:3000, action_label:null, action_id:null } },
           { tag:0x0131, body: ActionAck {
               addon_id:"tentavision", panel_id:"cameras", panel_epoch:7,
               action_id:"camera.create", client_action_id:<uuid bstr>,
               status:{ kind:"ok" }
           }}
         ]
       }
     }
     ```
6. **Core** forwarduje cały batch jako jedna envelope `Batch{atomic:true, members:[...]}`.
7. **Frontend** otrzymuje, dla każdego member:
   - StatePatch → reactive store appen'duje, table subscribers dodają wiersz (zero re-render, tylko jeden insert)
   - HideModal → frontend zamyka modal (exit animation, dom node removed po animacji)
   - Toast → frontend pokazuje toast w `tf-overlay-layer`
   - ActionAck → frontend usuwa loading state z buttona

**Total time:** ~30-80 ms typowo (localhost, in-process WASM). Brak full re-renderu, brak migotania.

## 12. Wersjonowanie i kompatybilność

- `protocol_version: u16` w envelope. v1 = 1.
- Hard requirement: klient i serwer muszą się zgadzać dokładnie. v2 nie rozmawia z v1. Connection rejected z `ProtocolReject{ supported:[1] }`.
- **Brak forward compatibility w fields:** SDK starszej wersji widzący nieznane pole → reject całej wiadomości (strict). Ewolucja przez bump `protocol_version`.
- Component catalog ma osobny `catalog_version: u32` — bumped niezależnie od protocol_version (ale rzadko). Negocjowane w `ProtocolHello.capabilities_requested`.

## 13. Bezpieczeństwo

### 13.1 Trust boundaries

Trzy explicit granice:
1. **Frontend ↔ Core:** untrusted ↔ trusted. Frontend = browser/mobile, może być compromised. Wszystko z frontend jest validated po stronie core'a. mTLS dla mobile pinned device certs (opcjonalne).
2. **Core ↔ Addon:** trusted ↔ semi-trusted. Addon WASM jest sandboxed (wasmtime), ale może próbować escape przez malformed payloads. Każdy host fn call i każda UI message walidowana.
3. **Core ↔ Core (mesh):** trusted ↔ trusted (post-pairing). mTLS + node ID pinning + replay protection (jak obecnie).

### 13.2 URL safety policies

`NavigateExternal.url` jest validowany przez core przez **multi-step pipeline**:

1. **Scheme check:** MUST be `https://`. `http`, `javascript`, `data`, `file`, `vbscript`, `chrome-extension`, custom schemes → reject + audit (A class).
2. **Parse & IDNA canonicalize:** URL parsed wg RFC 3986. Hostname canonicalized przez **IDNA 2008** (Unicode → ASCII punycode). Mixed-script attack detection (np. `tеntaflow.io` z cyrillic 'е') — reject jeśli hostname zawiera mixed scripts poza dozwolonymi parami.
3. **IP literal check:** jeśli hostname jest IP literal (IPv4 lub IPv6), reject (only DNS names allowed). Implication: `https://192.168.1.5/` → blocked.
4. **Private/loopback block:** jeśli IDNA hostname resolves do reserved ranges (RFC 1918, RFC 6890 — `127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `169.254.0.0/16`, `::1`, `fc00::/7`), reject. Walidacja przez DNS resolve **przed** wysłaniem do klienta — core ma DNS cache z TTL.
5. **Port policy:** dozwolone tylko `443` (default https) lub wymienione explicit w manifeście (`[external_navigation] allowed_ports = [443, 8443]`). Inne porty → reject.
6. **Allowlist matching:** manifest addona deklaruje patterns w `[[external_navigation_allowed]]`:
    - Exact match: `"docs.tentaflow.io"` — tylko ten host
    - Wildcard subdomain: `"*.gov.pl"` — matches `foo.gov.pl`, `bar.foo.gov.pl`, ale **NIE** `evilgov.pl` (label boundary enforced — `*` must match ≥1 full DNS label)
    - **Brak allowlist (empty) = ALL navigation external rejected.** v0.2 mówiła "any https" — to było złe. Default-deny.
7. **Redirect policy:** dla server-side fetched URLs (np. download proxy) — core może odpowiedzieć HTTP 3xx. Core MUSI follow redirects tylko jeśli destination hostname też przechodzi steps 1-6. Max 3 redirects. Cross-host redirects logowane (D class audit).

8. **DNS rebinding mitigation:** core's DNS check (step 4) ma znany TOCTOU window — browser może rozwiązać URL inaczej niż core. Dla high-risk navigation core MOŻE wymusić proxy mode (host-side fetch zamiast browser navigation) — flag w manifeście `[external_navigation] proxy_mode = "always"`. W proxy mode core fetcha content, pre-renderuje preview, klient otrzymuje signed URL do downloadu/inline view. Default proxy_mode = "off" — akceptujemy residual TOCTOU risk dla typowej nawigacji (browser sam robi resolution + ma własne anti-rebinding heuristics dla Private Network Access).

`Command::Download.filename`: MUST match `^[a-zA-Z0-9._-]{1,128}$`. No path separators, no shell metacharacters, no null bytes.

`Command::Download.signed_url_ref`: musi rozwiązać się przez host `SignedUrlIssuer` (już istniejący system). Niezarejestrowany ref → `InvalidFilename` error.

### 13.3 Clipboard / user activation

- `Command::Copy` i `LocalAction::Copy` wymagają flagi `user_gesture: true` w outermost Action (handler invoked via direct click/keypress). Programmatic invocation (np. po setTimeout) → frontend rejects + audit.
- Browser API `navigator.clipboard.writeText` wymaga user activation; SDK respektuje.

### 13.4 Form values trust model

- `form_values` z frontend są **always validated po stronie addona** przeciw deklaracji formularza w PanelShell.
- `validated_locally: bool` flag jest tylko **hint** — addon NIE może na nim polegać dla security checks.
- Walidacja po stronie addona zwraca `ActionStatus::ValidationFailed{ field_errors }` z error per field. Frontend mapuje na error badges przy odpowiednich komponentach.

### 13.5 Event topic permissions

- Topics są **compiled patterns**: `[publish] tentavision.alert.{level}` gdzie `{level}` jest typed segment.
- Brak runtime string matching glob z addonów.
- Manifest deklaruje patterns, admin approve'uje per addon (UI checkbox jak network rules).
- Subscribers wymagają `[event_subscribe]` permission z matching pattern.

### 13.6 Audit log entries

Każdy security-relevant event:

```
AuditEntry:
  ts_ms: i64
  session_id: bstr 16
  user_id: tstr or null
  addon_id: tstr
  panel_id: tstr or null
  action: tstr                   (np. "slot_ownership_violation", "rate_limit_exceeded", "url_blocked")
  outcome: AuditOutcome          (enum: Allowed | Denied | Rejected | RateLimited)
  details: map<tstr, Value>
  risk_class: RiskClass          (enum: A | B | C | D — A = highest)
```

Auditable:
- All `Error{...OwnershipViolation}` events (B class)
- Rate limit denials (collapsed per 60s window) (C class)
- URL blocked (NavigateExternal/Download) (B class)
- Permission denied (B class)
- Malformed CBOR (DoS attempt indicator) (B class)
- Oversized payload (B class)
- Unknown tag (A class — protocol attack indicator)
- Stream cancel from peer (D class)

### 13.7 Resource limits

- WASM addon: fuel limit (200M default) + epoch interrupt (1s preemption) + memory cap (256 MB).
- Per-addon max concurrent panels open: 8.
- Per-session max addons concurrently running: 32.
- Per-connection global limits w `ServerLimits` (advertised w `ProtocolWelcome`).

## 14. Reconnect, resume i recovery — unified model

Jeden spójny model recovery z 3 trybami w zależności od dostępnego state w core.

### 14.1 Session state retention

`session_id` ma TTL **60 sekund** po disconnect (configurable, default 60s). Core trzyma per-session:
- `open_panels` snapshot (z bieżącym epoch, state_revision, declared_slots)
- Replay buffer: ostatnie messages wysłane do klienta, max 1000 messages **lub** 10 MB total bytes (whichever first)

Po TTL → session state usunięty, resume nie możliwy.

### 14.2 Three-tier recovery

Frontend reconnect → `ProtocolHello` z `resume: { prior_session_id, last_received_msg_id }`. Core wybiera **jeden z trzech trybów** based on availability:

**Tier A — full replay (zalecany happy path):**
- Warunek: session w TTL + replay buffer ma messages od `last_received_msg_id + 1`
- `ProtocolWelcome.resume_status: { kind: "resumed", next_msg_id, mode: "replay" }`
- Core re-wysyła buffered messages w kolejności od `last_received_msg_id + 1`
- Frontend stosuje normalnie — state continues unchanged

**Tier B — snapshot fallback:**
- Warunek: session w TTL + open_panels znane + replay buffer overflowed (more messages than buffered) ALE state_revisions znane
- `ProtocolWelcome.resume_status: { kind: "resumed", next_msg_id, mode: "snapshot" }`
- Core wysyła `StateSnapshot` per open panel (z current state_revision) zamiast individual patches
- Frontend nadpisuje stores atomicznie per panel
- Frontend NIE re-buildi DOM — komponenty z bindings re-render z nowych state values
- **Sloty zachowują pre-disconnect DOM content** (nie ma reset do default_state). Addon może wysłać `SlotContent` w dowolnym momencie po snapshot żeby zaktualizować slot.

**Tier C — reject (fresh start):**
- Warunek: session past TTL OR core lost session_state OR critical inconsistency
- `ProtocolWelcome.resume_status: { kind: "rejected", reason }`
- Frontend tear-down wszystkich open paneli, wysyła `PanelOpen` ponownie per panel które chce otworzyć
- Addon dispatched z fresh PanelOpenContext (nowy epoch z core)
- Pełen restart UI

### 14.3 Frontend strategy

Frontend MUSI być przygotowany na każdy z trzech tierów. Implementacja:

```
on ProtocolWelcome:
  match resume_status:
    "fresh" → mount initial UI, send PanelOpen for first panel
    "resumed", "replay" → continue from buffered state (DOM and stores unchanged)
    "resumed", "snapshot" → store_replace_per_panel(new_snapshots), DOM stays
    "rejected" → tear_down_all(), re-open required panels
```

Krytyczne: **NIE ma scenariusza** gdzie zarówno replay jak i snapshot fallback są używane jednocześnie. Tier B wyklucza Tier A.

### 14.4 Credits i flow control po resume

Po resume credits są **resetowane do initial values** advertised w `ProtocolWelcome.server_limits`. Stare credits z poprzedniej sesji są odrzucone. Klient i core wymieniają fresh `CreditGrant` jak na fresh connection.

## 15. Performance budget

| Metric | Target |
|--------|--------|
| Panel open → first paint (cached PanelShell) | <50 ms |
| Panel open → first paint (cold addon, in-process WASM) | <300 ms |
| Action click → DOM update visible (local handler) | <16 ms (one frame) |
| Action click → DOM update visible (backend, localhost) | <100 ms |
| Action click → DOM update visible (mesh peer) | <250 ms |
| StatePatch apply (10 ops, 100 subscribers) | <2 ms |
| WebSocket frame size (typical Action) | <500 B |
| WebSocket frame size (full panel shell) | 3-10 KB |
| Heartbeat overhead | <120 B/s per connection |
| CBOR encode/decode (typical Action, all 3 langs) | <20 µs |
| End-to-end action latency budget (frontend click → addon handle → frontend DOM updated) | p50 <80 ms, p99 <250 ms (in-process) |

## 16. Error codes (canonical enum)

Wszystkie błędy zwracają strukturalny `ErrorCode` (u16) plus optional human-readable `message: tstr` (English, ≤ 256 chars).

```
ErrorCode (u16):

# Protocol layer (0x1000–0x10FF)
0x1001 ProtocolVersionMismatch
0x1002 NonCanonicalEncoding
0x1003 NonCanonicalKeyOrder
0x1004 NonCanonicalIntegerWidth
0x1005 DuplicateMapKey
0x1006 IndefiniteLengthForbidden
0x1007 UnknownSemanticTag
0x1008 InvalidTextString               (e.g. non-UTF8)
0x1009 UnknownPayloadTag
0x100A WrongChannel
0x100B MissingRequiredField
0x100C TypeMismatch

# Structural limits (0x1100–0x11FF)
0x1101 MessageTooLarge
0x1102 ComponentDepthExceeded
0x1103 ComponentCountExceeded
0x1104 StringTooLong
0x1105 ArrayTooLong
0x1106 MapTooLarge
0x1107 StatePatchOpsExceeded
0x1108 PathDepthExceeded
0x1109 CommandsPerMessageExceeded
0x110A HandlerDepthExceeded
0x110B HandlerStepsExceeded
0x110C BatchMembersExceeded

# Lifecycle (0x1200–0x12FF)
0x1201 UnknownPanel
0x1202 StalePanelEpoch
0x1203 PanelAlreadyOpen
0x1204 PanelNotReady
0x1205 RevisionMismatch
0x1206 SnapshotRequired

# Sandbox (0x1300–0x13FF)
0x1301 SlotOwnershipViolation
0x1302 PathOwnershipViolation
0x1303 ReservedNamespace
0x1304 UnknownSlot
0x1305 UnknownAction
0x1306 UnknownEventTopic
0x1307 TopicPatternViolation

# Authorization (0x1400–0x14FF)
0x1401 PermissionDenied
0x1402 CapabilityNotGranted
0x1403 AuthExpired
0x1404 AuthInvalid
0x1405 OriginNotAllowed
0x1406 UserGestureRequired

# Validation (0x1500–0x15FF)
0x1501 FieldValidationFailed
0x1502 InvalidUrl
0x1503 InvalidFilename
0x1504 InvalidIcon
0x1505 InvalidToneVariant
0x1506 InvalidLocale
0x1507 InvalidDuration
0x1508 InvalidColorToken             (jeśli ktoś próbuje wysłać raw color zamiast tokenu)
0x1509 InvalidStatePath
0x150A NonCanonicalFloatWidth
0x150B LocalCapabilityNotDeclared

# Resource (0x1600–0x16FF)
0x1601 RateLimited
0x1602 QueueOverflowed
0x1603 BackpressureBlocked
0x1604 StreamLimitExceeded
0x1605 FuelExhausted
0x1606 MemoryExhausted

# Internal (0x1F00–0x1FFF)
0x1F01 InternalError
0x1F02 AddonCrashed
0x1F03 AddonTimeout
0x1F04 AddonUnloaded
```

Klient renderuje user-friendly message z lokalizacji (i18n key per error code). `message` w wire jest dla developer/log.

## 17. Multi-language SDK obligations

Każde SDK (Rust/C#/Python) musi:

- **Produkować bit-identical CBOR encoding** dla tej samej semantycznie wiadomości (verified contract test suite, SHA-256 equality).
- Mapować wszystkie typy/enumy 1:1 ze `tentaflow-sdk-spec`.
- **Walidować inputs po stronie addona** przed wysłaniem (krótszy round-trip dla błędów developer; soft check, host zawsze re-validates).
- Implementować ergonomic builders dla komponentów (idiomatyczne per język).
- Generować **canonical CBOR** zgodny z RFC 8949 §4.2.1 — biblioteki muszą być skonfigurowane do deterministic mode.
- Mieć contract test runner — uruchamia common test suite z `tentaflow-sdk-spec/contract-tests/` i sprawdza wynik z host validator binary (CLI).

## 18. Implementation notes (decisions finalized in v0.3)

Wszystkie krytyczne pytania zostały zaadresowane w treści. Decyzje finalized poniżej dla referencji implementacji:

1. **Cross-tab synchronization:** Per-tab connection. Core broadcasts identyczne patches do wszystkich tabów oglądających ten sam panel (każdy ma własną sesję). BroadcastChannel jest tylko optymalizacją intra-browser (avoid duplicate WS connections) i NIE jest źródłem prawdy.

2. **State patch coalescing:** Core MOŻE coalesce sekwencji `Set` ops na ten sam StatePath w oknie ≤16 ms (latest-wins). Inne PatchOpKind (append, insert, increment, merge) NIE są coalesce'owane (zmienia semantykę).

3. **Component catalog version negotiation:** `catalog_version: u32` jako część Capability w handshake. Frontend deklaruje supported max version, core negocjuje. Mismatch → reject w `ProtocolWelcome.capabilities_rejected`.

4. **Datagram opt-in:** `webtransport_datagrams` capability + per-message `FLAG_RELIABLE = 0` opt-in dla niskoryzykowych ephemerals (Heartbeat, StreamProgress). UI state messages MUSZĄ być reliable.

5. **Compression:** `compression_zstd` jako optional capability. Stream chunks > 4 KB rozważane do kompresji w v1.1. Nie blokuje v1.0 ship.

6. **Tier B vs Tier A boundary:** core decyduje based on `last_received_msg_id` — jeśli buffer ma wszystkie messages od tego punktu → Tier A. Jeśli był overflow → Tier B. Tier C tylko jeśli TTL minął lub session_state utracony.

## 19. Następne kroki

1. **Ten dokument** — review przez codex (drugi round) + akceptacja User.
2. **`ADDON_UI_COMPONENT_CATALOG_v1.md`** — pełny katalog ~150 komponentów ze schematami (osobny dokument, równolegle review).
3. **`tentaflow-sdk-spec` crate** — typed Rust definitions zgodne z protokołem.
4. **`tentaflow-sdk-gen`** — generator self-test (Rust → Rust).
5. **Frontend reactive store + slot manager** — `tentaflow-core/www/js/runtime/`.
6. **Host validator + dispatch** — `tentaflow-core/src/addon/protocol/`.
7. **End-to-end smoke test** — minimalny addon (1 panel, 1 action, 1 state patch).
8. **Generator backends C# + Python** — po stabilizacji Rust SDK.
9. **WASI hosting C# + Python** — wasmtime config dla .NET 10 i CPython 3.13.
10. **Przepisanie TentaVision, Eureka, Contacts, Company Lookup** — na nowy SDK.
