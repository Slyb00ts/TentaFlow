# TentaVision F1c-P0 — Design spike: addon UI iframe + vector backend

**Status:** Decision spike, czeka na user-side decyzje Q1-Q7
**Blokuje:** F1c-P1 (UI iframe foundation), F1c-P3 (vector storage)
**Powiązane:**
- `notes/tentavision-f1c-handoff.md` (scope F1c)
- `notes/tentavision-plan.md` §3.3, §9.2, §16.7
- `vendor/ed25519-dalek/` (gotowe, vendored)

---

## A. Addon UI iframe sandbox + postMessage bridge

### A.1 Threat model

Addon UI to HTML/JS bundle dostarczony jako `[[ui_component]]` w manifest TOML.
Renderowany w panelu admina TentaFlow (parent window). Iframe **nie ma**
bezpośredniego dostępu do DOM parenta, ciasteczek, localStorage core.

**Adversary scenarios:**
1. Malicious addon próbuje wyciągnąć admin session cookie → blokowane przez
   brak `allow-same-origin`.
2. XSS injection do parent przez DOM API → blokowane przez sandbox isolation.
3. Request do core HTTP API bez addon-scope permissions → blokowane przez CSP
   `connect-src 'none'` (brak fetch z iframe).
4. Cross-iframe injection (addon A czyta DOM addona B) → różne origins
   (każdy iframe = unique opaque origin pod `sandbox` bez `allow-same-origin`).
5. Privilege escalation: addon UI próbuje host function z permission scope
   większym niż jego backend addon → parent verify `addon_id` przeciw iframe.

### A.2 Sandbox flags decyzja

| Opcja | Flagi | Bezpieczeństwo | Funkcjonalność |
|-------|-------|----------------|----------------|
| Minimal | `allow-scripts` | maksymalne | JS działa, brak popup/forms |
| Medium | `allow-scripts allow-popups` | wysokie | + linki do nowych zakładek |
| Liberal | `allow-scripts allow-same-origin` | **NIEBEZPIECZNE** | pełen dostęp do parent jeśli same-domain |

**Rekomendacja: Minimal** (`sandbox="allow-scripts"`).
Plus CSP per-iframe (zwracane jako nagłówek `Content-Security-Policy` przy
serving bundle z addon FS):

```
default-src 'none';
script-src 'self';
style-src 'self' 'unsafe-inline';
img-src data: blob:;
connect-src 'none';
frame-ancestors 'self';
```

`connect-src 'none'` = brak `fetch`/`XMLHttpRequest`/`WebSocket` z iframe.
Cały ruch idzie przez `window.parent.postMessage()`.

### A.3 postMessage protocol — envelope

```typescript
type BridgeMessage =
  | { kind: "request",  id: string, action: string, payload: object }
  | { kind: "response", id: string, ok: boolean, result?: object, error?: { code: string, message: string } }
  | { kind: "event",    id: null,   topic: string, payload: object };
```

- `id`: ULID, correlation request↔response
- `action`: dot-namespaced (np. `alias.list_owned`, `camera.snapshot`,
  `vector.search`, `ui.notify`)
- Brak `addon_id` w message — parent zna mapping `iframe_element → addon_id`
  z install-time (niemożliwe do podrobienia z addon JS)

### A.4 Origin pinning + addon_id binding

Iframe `src` = `blob:` URL utworzony przez parent z signed bundle bytes.
Każdy iframe ma unique opaque origin (sandbox bez `allow-same-origin`).
Parent trzyma HashMap `<HTMLIFrameElement, addon_id>` ustanowioną przy mount.

Przy odebraniu `message` event:
1. `event.source === iframe.contentWindow` — verify source identity
2. Lookup `addon_id` z HashMap (NIE z message payload — to byłoby impersonowalne)
3. Dispatch do host function z `addon_id` jako security context

### A.5 UI architecture — A vs B

**Opcja A (rekomendowana): UI tylko, backend WASM**
- Iframe = view layer (HTML+JS+canvas)
- Wszystkie wywołania ABI idą przez `postMessage` → parent → host function
  call w kontekście **backend addon's** `AddonState` (już zainstalowany
  WASM addon w wasmtime runtime core)
- Pros: addon backend już istnieje, UI to tylko view; jeden permission model
- Cons: UI nie działa bez backend instance

**Opcja B: UI + WASM in iframe**
- Iframe ma własny WASM module (wasmtime-web w przeglądarce)
- Backend independent
- Pros: standalone UI bez backendu
- Cons: 2x wasm runtime (browser + native), trudne sharing state, większy
  attack surface (WASM exec w browser sandbox), duplikacja permission logic

### A.6 Message validation strictness

**Opcja strict (rekomendowana):** typed JSON schema validator per action.
Niepoprawne payload → response `error.code = "EBADREQ"`.

**Opcja loose:** `payload: any` przekazane do host function as-is, host
deserializuje. Mniej bezpieczne, więcej duplikacji.

### A.7 Capability whitelist

**Opcja auto-derive (rekomendowana):** addon manifest deklaruje
`host_permissions = ["alias.read", "camera.read"]`. postMessage `action`
mapowane na permission scope — jeśli addon nie ma `camera.read`, action
`camera.snapshot` → `EPERM`. Spójne z backend permission model.

**Opcja explicit:** osobna lista `ui_actions = [...]`. Duplikacja, łatwo o desync.

### A.8 Ed25519 signature flow

`[manifest] publisher.ed25519_public_key` deklaruje publisher key (32 bajty
base64). `[[ui_component]] signature` = Ed25519 sig nad SHA-256(bundle bytes).
Przy `addon::install`:

1. Wczytaj bundle z FS sandbox addon
2. SHA-256 → 32 bajty digest
3. Verify(publisher_pk, digest, signature) — z `vendor/ed25519-dalek`
4. Reject install z `AbiError::InvalidArgument` jeśli verify fail
5. INSERT do `ui_components` (DB v26) tylko po verify OK

**Trust store:** Tabela `trusted_publishers (key_b64, label, added_at)`.
TentaFlow corp key wpisany z migracji v26. User-added przez CLI
(`tentaflow-cli addon trust-key <key> --label "ACME Inc"`).

---

## B. Vector storage backend

### B.1 Use cases F1c

- **D4 re-id:** face/person embedding (512d) + cosine k-NN top 10-100,
  z gate enforcement per query (legal_grant_id required)
- **D5 search:** attribute embedding (768d SigLIP2), text query → embed →
  similarity search w nagraniach
- **D6 plates:** 256d LPRNet embedding tablic
- **Future RAG (memory addon):** text embeddings dla LLM context

**Scale F1c MVP:** ~10k-100k embeddings/namespace, 4 namespaces/addon,
~3-5 addonów = ~2M wektorów total worst-case node.

**Scale F2 production:** ~1M-10M embeddings/namespace, multi-addon.

### B.2 Backends — porównanie

| Kryterium | usearch | hnsw_rs | lancedb | qdrant (ext) |
|-----------|---------|---------|---------|--------------|
| Embedded (single binary) | tak (C++ bindings) | tak (pure Rust) | tak (Rust) | nie (proces) |
| Licencja | Apache 2.0 | MIT/Apache | Apache 2.0 | Apache 2.0 |
| Disk persistence (mmap) | tak | częściowe (snapshot) | tak (columnar) | tak |
| HNSW algo | tak (top-tier) | tak | tak | tak |
| Maturity (gh stars) | 2.5k★ aktywny | 200★ aktywny | 5k★ aktywny | 22k★ aktywny |
| Build deps | C++ kompilator | brak | brak | brak (klient) |
| Cross-compile (mobile) | wymaga toolchain C++ | działa wszędzie | działa wszędzie | n/a |
| F1c single-node fit | ✓✓ | ✓ | ✓ | ✗ |

### B.3 Embedded vs external

**Embedded (rekomendowane dla F1c):** single binary, brak extra proc, niska latency.

**External (Qdrant):** lepsze dla F2+ jeśli skala > 10M vec/node, dodatkowy proces.

Strategia: **embedded teraz**, abstrakcja `VectorBackend` trait, future `QdrantBackend` jako opcja w F2+.

### B.4 Cross-compile concern (mobile)

`usearch` ma C++ core — wymaga toolchain dla iOS/Android. **Sprawdź w P3**
czy prebuilt binaries są w crate lub czy potrzebny cc-rs build script.
Jeśli problematyczne dla mobile → fallback na `hnsw_rs` (pure Rust).

### B.5 Per-namespace storage layout

```
<tentaflow_home>/addons/<addon_id>/vectors/
├── attributes.usearch          # mmap HNSW file
├── attributes.meta.toml        # dim=768, metric=cosine, count=12453
├── plates.usearch
├── plates.meta.toml
├── faces.usearch
└── faces.meta.toml
```

DB table (migration v28):
```sql
CREATE TABLE addon_vector_namespaces (
  addon_id TEXT NOT NULL,
  namespace TEXT NOT NULL,
  dim INTEGER NOT NULL,
  metric TEXT NOT NULL,        -- 'cosine' | 'euclidean' | 'dot'
  count INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  PRIMARY KEY (addon_id, namespace)
);
```

Type-safety guard: `vector_upsert(ns, vec)` sprawdza `len(vec) == dim`
z tej tabeli przed call do backend.

### B.6 Distance metric default

| Metric | Use case |
|--------|----------|
| cosine | embeddings normalizowane (face, person, CLIP) — **standard** |
| euclidean | low-dim feature vectors, geo |
| dot | recommender, fast cosine variant na pre-normalized |

Wszystkie 4 namespace w plan §9.2 to cosine. **Rekomendacja: cosine default**,
manifest może override per-namespace.

---

## C. F1c phases — implementation roadmap po P0

| Faza | Zakres | Wymaga decyzji |
|------|--------|----------------|
| **P0** (TEN doc) | Design spike: iframe sandbox + vector backend | Q1-Q7 |
| **P1** | UI iframe foundation: parent harness, sandbox load, postMessage dispatch, blob: URL serving z addon FS, BridgeMessage envelope, registry `iframe → addon_id` | Q1, Q2, Q3, Q4 |
| **P2** | Ed25519 signature verify w `addon::install` + manifest `[[ui_component]]` parse + `trusted_publishers` table v26 + CLI `addon trust-key` | brak |
| **P3** | Vector storage backend wybór + integracja + `addon_vector_namespaces` v28 + `vector_upsert_v1`/`vector_search_v1`/`vector_delete_v1` host functions + per-addon quota | Q5, Q6, Q7 |
| **P4** | Policy/claims engine: tables v27, `gate_check_v1` host fn, multi-sig DPO+supervisor, CLI `policy issue` | brak |
| **P5** | Flow invoke + DAG: operators (Source/Predict/Threshold/Branch/Aggregate/Sink), bounded channels, backpressure audit | brak |
| **P6** | ONVIF GetStreamUri + M15 wizard "Discovered cameras" step | brak |
| **P7** | RBAC partial: users/sessions v29 + `actor_user_id` w audit | brak |

Carry-over z F1b (P2-lab + P6-soak) wpina się przed P1 jako bug-fix gate.

---

## D. Decyzje Q1-Q7 (rekomendacje)

| Q | Pytanie | Opcje | Rekomendacja |
|---|---------|-------|--------------|
| Q1 | Sandbox flags | Minimal / Medium / Liberal | **Minimal** (Liberal = krytyczny risk) |
| Q2 | UI architecture | A (UI tylko, backend WASM) / B (UI + WASM iframe) | **A** (prostsze, jeden permission model) |
| Q3 | Msg validation | Strict (JSON schema) / Loose | **Strict** (lepsze błędy, mniej duplikacji) |
| Q4 | Capabilities source | Auto-derive (z host_permissions) / Explicit | **Auto-derive** (spójność z backend perms) |
| Q5 | Vector backend | usearch / hnsw_rs / lancedb / custom | **usearch** (top perf + mmap). Fallback `hnsw_rs` jeśli mobile C++ blokuje |
| Q6 | Vec deployment | Embedded / External (Qdrant) | **Embedded** (single binary). Trait zostawia ścieżkę do Qdrant w F2+ |
| Q7 | Vec metric | cosine / euclidean / dot | **cosine** (standard dla embeddings) |

---

## E. Identified blockers po P0

| Blocker | Severity | Mitigation |
|---------|----------|------------|
| `usearch` C++ cross-compile dla iOS/Android | Średnie | Verify w P3 — jeśli fail, fallback `hnsw_rs` |
| `[[ui_component]] host_permissions` schema nie sfinalizowane | Niskie | Doprecyzować w P1 — string list permission ID (jak backend) |
| Trust store seed (TentaFlow corp Ed25519 key) — kto generuje? | Średnie | User decision — może być generated w packaging tools przy v0.1 release |
| Iframe `blob:` URL vs `data:` URL — który preferowany? | Niskie | `blob:` preferowane (większe payloady, lepsze GC); P1 prototype obu |

**Brak blockerów dependency:** `ed25519-dalek 2.2.0` już vendored
(`vendor/ed25519-dalek/`, path override w `tentaflow-core/Cargo.toml:114,304`).
