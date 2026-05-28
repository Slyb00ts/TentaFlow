# TentaFlow Unified Frame Protocol v2 (UFP/2)

> **Status:** Draft v0.9 (2026-05-22) — ready for Krok 4c1 implementation per codex review.
>
> Single source of truth for every byte moving between any two TentaFlow components — frontend, core, addons, mesh peers, services, sidecars, future Zig/Go/Python/C# nodes.
>
> **v0.9 changelog (after eighth codex review):**
> - §7.1 receive pipeline: signature verification rephrased as `authenticate envelope per §9 / §11.3` with explicit `IS_SIGNED=1/0` branches. Removed misleading unconditional "verify envelope.auth.signature" wording.
>
> **v0.8 changelog (after seventh codex review):**
> - §7.2 fragmented AAD `flags` normalised: `aad_flags = flags & !IS_LAST_FRAGMENT`. The last-fragment bit is excluded from AAD so all fragments produce identical AAD.
> - §11.3 Session signing simplified: `Session + IS_SIGNED=1` is FORBIDDEN in v2. Signed Session would require a separate session-signing-key registry that is out of scope. Sessions are unsigned (TLS-authenticated); when cryptographic sender proof is needed, use `NodeIdentity` or `UserIdentity`.
> - §16: minor cleanup of stale "deferred to v0.3" / "Codex review of v0.2" wording.
>
> **v0.7 changelog (after sixth codex review):**
> - **CRIT fix** §7.2 fragmented AAD: AAD for fragmented messages now covers ONLY immutable common header fields `0–8 + 10–12` EXCLUDING field 9 `body` (receiver does not have plaintext body before AEAD decrypt; previous v0.6 wording was unimplementable). Body integrity comes from AEAD tag (logical body) + per-fragment Ed25519 signature (fragment body bytes).
> - **MAJOR fix** §11.3 ApiKey mutations: ApiKey envelopes are STRICTLY LIMITED to read/inference kinds. Mutating operations require real `UserIdentity` signed with the user's Ed25519 private key OR a real `Session` established via interactive login. The earlier "gateway maps ApiKey to UserIdentity" model is cryptographically impossible (gateway lacks the user's private key) and has been removed.
>
> **v0.6 changelog (after fifth codex review):**
> - §6.1 Auth schema: sub-fields `subject_id`, `epoch`, `signature`, `session_id` marked OPTIONAL in the schema block; presence rules controlled by §11.3 per `auth.kind`.
> - §9 / §7.1 / §10.2: signature verification is CONDITIONAL on `IS_SIGNED=1`. For unsigned envelopes (Anonymous/ApiKey/Session-without-sig), validator verifies channel/kind policy permits unsigned AND transport/session binding instead of Ed25519 verify.
> - §7.2 fragmented AAD: removed misleading "body length" phrase. AAD `body` for fragmented messages refers to the logical pre-fragmentation `compressed_body` bytes themselves, not a separate length field.
> - §10.1: explicit rejection rules — `fragment_count == 0` → `BodyValidationFailed`; `fragment_index >= fragment_count` → `BodyValidationFailed`.
> - §11.3: ApiKey envelopes restricted to read/inference kinds; mutating Frontend kinds reachable via ApiKey only when edge gateway transforms request into `auth.kind=UserIdentity` after HMAC validation.
>
> **v0.5 changelog (after fourth codex review):**
> - §3.2 envelope schema: `auth` (field 13) marked MANDATORY (was implicitly optional in the schema header — now matches §11.3 mandate).
> - §6.1: removed the misleading ApiKey HMAC sentence; ApiKey HMAC happens at the edge gateway OUTSIDE UFP/2, exactly as §11.3 says.
> - §7.2: AAD definition refined for fragmented messages. AEAD AAD is computed over the **logical envelope** (common immutable fields), NOT per-fragment fields. Per-fragment integrity comes from each fragment's Ed25519 signature.
> - §10.2: explicit reassembly atomicity — `check dedup → buffer fragment → commit dedup` runs under a per-`(source, message_id)` lock. Fragment slots are write-once; duplicate same-`fragment_index` arrivals are deduped at step 6 and never overwrite buffered bytes.
> - §16: removed obsolete "key rotation cadence" open question (§7.2 already pins `2^48 OR 24h`).
>
> **v0.4 changelog (after third codex review):**
> - Fragment dedup commit timing made explicit: dedup commits when fragment is accepted into reassembly buffer (after sig+revocation+time checks), NOT after full reassembly. Duplicate fragments during in-progress reassembly are now correctly deduped.
> - §11.3 auth invariants cleaned up: `auth` field is ALWAYS present in every envelope (carries `auth.kind` minimally); `auth.signature` present iff `IS_SIGNED=1`; ApiKey uses separate transport-level HMAC mechanism (NOT `auth.signature`, which is Ed25519-only).
> - §11.3 per-channel table: replaced "Recommended" with explicit `YES/NO/PER_KIND`.
> - Sender fragmentation pipeline explicitly documented (§10.0).
> - §16 closed items list updated: §11.3 auth table now marked done.
>
> **v0.3 changelog (after codex review of v0.2):**
> - Fragment signature: `auth.signature` MUST differ per fragment (each fragment is independently signed); only `auth.kind`, `subject_id`, `epoch`, `session_id` are required to be identical across fragments.
> - Replay dedup order fixed: signature MUST be verified BEFORE dedup commit. Invalid/forged fragments never occupy a replay-key slot.
> - Key revocation: explicit revoked-key denylist check happens BEFORE the epoch grace window. A revoked key cannot ride the `epoch == cached - 1` grace.
> - §14 threat model row on compression side-channel now points to §8.1 and removed the old incorrect "compress-before-encrypt hides compressibility" claim.
> - Added §11.3 Per-channel authentication requirements (envelope-level invariants the 4c1 validator MUST enforce).
> - Nonce/key-rotation threshold consistency: single normative value `2^48 OR 24h whichever first`. Changelog, §7.2, and §16 aligned.
> - §3.4 flag bit allocation clarified: bits 0–6 allocated, bits 7–31 reserved. `IS_LAST_FRAGMENT` without `IS_FRAGMENT` MUST be rejected.
>
> **v0.2 changelog (after codex review of v0.1):**
> - Removed `IS_FORWARDED` flag (was mutable but inside signed `flags`); forwarding inferred from `len(forwarded_via) > 0`.
> - Pinned transform pipeline order: compress → encrypt → sign. AAD and signature both use `auth.signature := 0x00 × 64` placeholder for unambiguous canonical bytes.
> - Added `fragment_index`/`fragment_count` envelope fields; replay dedup key now `(source.id, message_id, fragment_index)`.
> - Corrected compression side-channel description: CRIME/BREACH-style leakage acknowledged; no-compress policy for secret-bearing channels.
> - `forwarded_via` explicitly labelled "unauthenticated diagnostic metadata" — not trusted for authentication decisions. Future option: per-hop signed receipts.
> - Nonce construction tightened: 32-bit random session prefix + 64-bit monotonic counter per key.
> - Added §11.1 downgrade attack policy + §11.2 key revocation/compromise procedure.
> - Removed `ExternalAPI` channel (was awkward); OpenAI-compat gateway sits at edge, internal `/v1/*` flows on `Frontend` or `Domain`.
> - Clarified Mesh vs Control boundary: heartbeat = Mesh, handshake/auth = Control.

## 1. Goals

1. **One wire format end-to-end.** Frontend → Node A → Node B → Node C → response — every hop sees the same bytes. Routing nodes **never decode body**, only read envelope header to make routing decisions.
2. **Multi-language compatible.** Spec uses CBOR (RFC 8949) so a future Zig, Go, Python or C# node implements UFP/2 by reading this doc — no Rust dependency.
3. **End-to-end authentication.** Sender signs envelope (excluding mutable hop trail). Receiver verifies sender's pubkey directly, regardless of how many hops the message took.
4. **Production performance.** Strict canonical CBOR with optional lz4 compression. Heartbeats, sync batches, GUI updates, frame blobs — all on the same protocol with negligible overhead vs prior CBOR-only mesh path.
5. **Zero parallel-stack.** UFP/2 replaces every prior wire format (CBOR `Envelope`/`MessageBody`, mesh `0x10..0x4C` discriminators standalone, raw HTTP frame pickup body, Faza 6 sdk-spec CBOR envelope v1). After migration: one envelope, one validator, one debug tool.

## 2. Non-goals

- **NOT a transport.** UFP/2 is the application-level frame format. Transport (WebSocket, WebTransport, QUIC over iroh, HTTP/1.1 for `/v1/*` OpenAI-compat) is orthogonal.
- **NOT a session layer.** Session establishment (handshake, capability negotiation, key exchange) lives inside UFP/2 messages on the `Control` channel, not in the envelope itself.
- **NOT a schema for application payloads.** Each `(channel, kind)` pair declares its own body schema — UFP/2 only standardises the envelope.

## 3. Wire format

### 3.1 Encoding profile

**CBOR canonical, Faza 6 deterministic profile** as defined in `tentaflow_sdk_spec::canonical::validate_canonical`:

- RFC 8949 §4.2.1 Core Deterministic Encoding (definite-length only, minimum-width integer arguments, bytewise key order, no duplicate map keys).
- f64-only floats. `f16`/`f32` rejected on decode.
- No `undefined` (0xF7), no 1-byte simple value (0xF8), no reserved 28–30.
- Map keys are u8 integers (envelope) or tstr (free-form payload sub-maps).
- `MAX_NESTING_DEPTH = 64`.

Every UFP/2 envelope MUST pass `validate_canonical` before any further processing. Senders MUST emit canonical encoding. Receivers MUST reject non-canonical input with a `CanonicalError`.

### 3.2 Envelope schema

```
Envelope = {
   0:  protocol_version  u8         ; MUST be 2
   1:  message_id        bstr(16)   ; ULID — time-ordered, sender-unique
   2:  source            NodeAddress
   3:  destination       NodeAddress
   4:  created_at_ms     i64        ; UTC milliseconds, replay window
   5:  flags             u32        ; bitfield, see §3.4
   6:  priority          u8         ; 0=Bulk, 1=Normal, 2=Interactive, 3=Control
   7:  channel           u8         ; routing discriminator, see §4
   8:  kind              u16        ; message type within channel
   9:  body              bstr       ; payload bytes (see §3.5)
  ; — optional fields below (10–12) —
  10:  correlation_id    bstr(16)   ; OPTIONAL, request/response matching
  11:  trace_id          bstr(16)   ; OPTIONAL, distributed tracing
  12:  ttl_ms            u32        ; OPTIONAL, expiry window from created_at_ms
  ; — mandatory authentication —
  13:  auth              Auth       ; MANDATORY on every envelope (carries auth.kind minimum); see §6 and §11.3
  ; — fragmentation fields, MUST be present iff flags & IS_FRAGMENT —
  14:  fragment_index    u16        ; 0-based fragment number
  15:  fragment_count    u16        ; total fragments for this logical message
  ; — mutable hop trail (UNAUTHENTICATED DIAGNOSTIC METADATA, see §5.5) —
  16:  forwarded_via     array<NodeAddress>  ; NOT covered by signature
}
```

Optional fields (10–12) are encoded as omitted CBOR keys when absent (catalog convention; explicit CBOR `null` is rejected on decode). Field 13 (`auth`) is MANDATORY on every envelope. Fields 14–15 MUST be present iff `flags & IS_FRAGMENT = 1` and MUST be absent otherwise. Field 16 is present iff at least one hop has forwarded the envelope.

**All bits of `flags` are immutable end-to-end.** No flag bit is mutated by routing nodes. Forwarding is inferred from `len(forwarded_via) > 0`, not from a flag.

### 3.3 NodeAddress

Universal identifier for any participant in the protocol:

```
NodeAddress = {
  0: kind  u8     ; participant class (see below)
  1: id    bstr   ; Ed25519 public key (32 bytes) — same scheme for nodes AND users
  2: name  tstr   ; OPTIONAL, human-readable, NOT part of identity
}
```

**kind enum:**
- `0x00` **Anonymous** — id is all-zero 32-byte bstr; used for unauthenticated bootstrap (e.g. handshake before key exchange).
- `0x01` **Node** — physical/virtual machine running tentaflow-core; id is the node's Ed25519 signing key.
- `0x02` **User** — human identity; id is the user's Ed25519 key (same scheme as node, so a user can sign messages directly without intermediate trust).
- `0x03` **Service** — auxiliary process (yolo, whisper, future sidecars); id is the service's Ed25519 key registered with its owning node.
- `0x04` **Addon** — WASM addon running inside a node; id is the addon's Ed25519 key issued at install time.
- `0x05` **Broadcast** — id is all-zero 32-byte bstr; envelope targets all peers visible to the source (mesh flooding rules apply).
- `0x06..0xFF` reserved.

**Why Ed25519 for everyone?** Ed25519 is the modern industry standard for new systems (Solana, SSH, WebAuthn/FIDO2, Signal, Tor v3, sigstore, age, modern PGP). 32-byte public key, 64-byte signature, deterministic signing (no nonce reuse risk), fast verification. Note: **NOT compatible with Ethereum/Bitcoin** (which use secp256k1 ECDSA) — UFP/2 stands on its own crypto.

Node identity (already in `sync_nodes`) and user identity (`user_identity_keys`) MUST both use Ed25519 in UFP/2. Existing user_id formats (UUID-style 16-byte) migrate to 32-byte Ed25519 pubkeys at user provisioning.

### 3.4 Flags bitfield (u32)

Bits 0–6 are currently allocated. Bits 7–31 are reserved and MUST be `0` (receivers reject envelopes carrying unknown bits with `BodyValidationFailed` — surfaces protocol drift before silent compatibility issues accumulate).

Allocated bits:

| bit | name | semantics |
|-----|------|-----------|
| `0x0001` | `IS_ENCRYPTED` | body is AEAD ciphertext (see §7) |
| `0x0002` | `IS_COMPRESSED` | body (or AEAD plaintext) is lz4 frame |
| `0x0004` | `REQUIRES_ACK` | receiver MUST send back an envelope with `correlation_id = sender.message_id` and `kind = ACK` (channel-specific kind) |
| `0x0008` | `IS_FRAGMENT` | body is one fragment of a larger logical message |
| `0x0010` | `IS_LAST_FRAGMENT` | when `IS_FRAGMENT=1`, this is the final fragment in the sequence |
| `0x0020` | `IS_SIGNED` | `auth.signature` is present and MUST verify |
| `0x0040` | `IS_BROADCAST` | destination kind is Broadcast; flooding rules apply per channel |

**No mutable flag bits exist** — the entire `flags` u32 is end-to-end immutable and covered by the signature.

**Flag combination validation** (receivers MUST reject):
- `IS_LAST_FRAGMENT` set without `IS_FRAGMENT` set → `BodyValidationFailed`.
- `IS_FRAGMENT` set without `fragment_index`/`fragment_count` envelope fields present → `BodyValidationFailed`.
- `IS_FRAGMENT` clear but `fragment_index` or `fragment_count` envelope field present → `BodyValidationFailed`.
- `IS_LAST_FRAGMENT` set with `fragment_index != fragment_count - 1` → `BodyValidationFailed`.

**Transform pipeline order** is fixed (see §7.1):
- **Send**: structured payload → encode CBOR → `IS_COMPRESSED ? lz4_compress` → `IS_ENCRYPTED ? aead_encrypt` → store as `body` → fill `auth.signature` per §6.3.
- **Receive**: verify signature → reassemble fragments → `IS_ENCRYPTED ? aead_decrypt` → `IS_COMPRESSED ? lz4_decompress` → dispatch by `(channel, kind)`.

### 3.5 Body

`body` is `bstr` (opaque to the envelope layer). Three interpretations exist; the receiver picks based on `(channel, kind)`:

1. **Structured CBOR payload** (the common case). Body is itself a canonical CBOR data item (typically a map) whose schema is declared by the catalog entry for `(channel, kind)`. Receiver validates with `tentaflow_sdk_gen::message::validate_component` or per-channel equivalent.

2. **Opaque blob** (frame pickup, raw streams). Body is application-specific bytes — e.g. raw RGB24 pixels or JPEG bytes for `(channel=FrameBlob, kind=Frame)`. Routing nodes pass through without inspection. End receiver knows how to parse based on `kind`.

3. **Verbatim inner payload during migration** (legacy compatibility). For `(channel=SyncLedger, kind=PushOperation)` body is the existing CBOR `SyncOperation` with its existing Ed25519 signature — UFP/2 wraps without re-encoding, preserving in-flight signatures.

The envelope is encoding-agnostic about body content. **Routing decisions are made entirely from envelope header (fields 0–8 + 10–12).** No hop decodes body.

## 4. Channel/kind taxonomy

`channel` is u8, `kind` is u16. Together they form a 24-bit dispatch key. Channels group related kinds for routing, ACL, rate-limit policies.

| channel | name | kind range | encoding | purpose |
|--------:|------|-----------|----------|---------|
| `0x01` | UI | `0x0001..0x07FF` | Structured CBOR | Addon UI updates (Faza 6 catalog: PanelShell, StatePatch, Action, Event, Command, Batch). Kind = catalog tag. |
| `0x02` | HostFunction | `0x0001..0x00FF` | Structured CBOR | Addon ↔ Core ABI calls (`sql.query`, `llm.chat`, `http.request`, etc.). |
| `0x03` | Stream | `0x0001..0x00FF` | Structured CBOR + fragment bytes | Long-lived streams (LLM tokens, file upload, log tail). Uses `IS_FRAGMENT` heavily. |
| `0x04` | Mesh | `0x0010..0x004C` | Structured CBOR | Peer-to-peer control (existing `MESH_MSG_HEARTBEAT`..`MESH_MSG_FRAME_PROXY_RESPONSE` discriminators become `kind` values). |
| `0x05` | Control | `0x0001..0x00FF` | Structured CBOR | Handshake, auth, heartbeat, resume, rate-limit notifications, session_end. |
| `0x06` | SyncLedger | `0x0001..0x00FF` | Structured CBOR | Existing Sync Ledger ops/acks/pulls/snapshots. |
| `0x07` | Frontend | `0x0001..0xFFFF` | Structured CBOR | Frontend ↔ Core (chat completions, dashboard CRUD, settings) — replaces CBOR `MessageBody` enum. Kind = MessageBody variant index. |
| `0x08` | Domain | `0x0001..0xFFFF` | Structured CBOR | Application domain messages (recorder, scheduler, camera_admin, sync_conflict). |
| `0x09` | FrameBlob | `0x0001..0x000F` | **Body = raw bytes** | Camera frame transport (pixel/JPEG passthrough). Routing nodes forward verbatim. |
| `0x0A..0xFF` | reserved | — | — | Future channels (Zig node, GPU mesh, etc.). |

OpenAI-compatible `/v1/*` API is **NOT a UFP/2 channel**. It is an external HTTP/JSON gateway at the network edge. Internally, the gateway translates HTTP requests into UFP/2 envelopes on `channel=0x07 Frontend` (for chat/completion) or `channel=0x08 Domain` (for model management, embeddings, etc.), and translates UFP/2 responses back into JSON/SSE for the HTTP client. External clients NEVER see UFP/2.

**Mesh vs Control channel boundary** (clarified from v0.1):
- **Mesh (0x04)**: peer-to-peer steady-state operations — heartbeat, topology gossip, sync push/ack/pull, frame proxy, peer discovery, trust pairing handshakes after initial Control/Hello.
- **Control (0x05)**: session-level operations — protocol Hello/Welcome, authentication, key rotation initiation, capability negotiation, session_end, rate-limit notifications, fatal protocol errors. Most Control messages are exchanged once per session; Mesh messages flow continuously.

Kind values are stable forever once allocated. New kinds append. **Renumbering is a breaking protocol version bump.**

Full per-kind schema is published as machine-readable manifest (`tentaflow-sdk-gen/catalog-manifest/v2.cbor`) — multi-language SDKs generate types from it.

## 5. Routing

### 5.1 Direct delivery

If `destination` is reachable directly (peer is in this node's connection table), forward the envelope bytes unchanged on the transport. No mutation.

### 5.2 Multi-hop forwarding

If `destination` is not directly reachable, this node MUST:

1. Look up next-hop in its mesh topology table.
2. Append its own `NodeAddress` to `forwarded_via`. (This mutates the envelope but is OUTSIDE signature scope.)
3. Forward to next hop.

**Loop prevention**: if `len(forwarded_via) >= 32`, drop the envelope and emit an `Control / ForwardingError` envelope back to source. Rationale: 32-hop diameter covers any plausible mesh topology with margin; longer chains indicate misconfiguration or routing loop.

### 5.3 Broadcast (`IS_BROADCAST=1`, destination.kind=Broadcast)

Per-channel flooding rules:
- **Mesh channel**: flood to all trusted peers except those already in `forwarded_via`.
- **SyncLedger channel**: targeted via Sync Policy (NOT true broadcast despite kind=Broadcast convention — kept for ABI uniformity, actual recipients selected by `can_node_receive_sync_resource`).
- **Other channels**: broadcast is a protocol error; receivers reject with `Control / InvalidBroadcast`.

### 5.4 Signature preservation through hops

`forwarded_via` (field 16) is the ONLY mutable envelope field; routing nodes append themselves to it during forwarding. All other fields (0–15) are IMMUTABLE end-to-end. `auth.signature` covers fields 0–15 in their canonical CBOR encoding (see §6.3). Routing nodes update `forwarded_via` without invalidating the source's signature because field 16 is excluded from signature scope.

### 5.5 `forwarded_via` is unauthenticated diagnostic metadata

The `forwarded_via` array is **NOT cryptographically authenticated**. A malicious hop can:
- Forge entries to misattribute routing through nodes that never saw the envelope.
- Reorder or delete entries to hide its own participation.
- Truncate the chain to make a multi-hop message appear direct.

`forwarded_via` MUST be treated as **best-effort diagnostic / debugging metadata only**. Authentication, ACL decisions, and audit logging MUST NOT trust the hop trail.

If end-to-end verifiable routing audit is required (future requirement), the protocol can add an optional `route_attestations` field (array of per-hop signed receipts, each signed by that hop over the previous chain state). Deferred to UFP/3.

The destination authenticates the **source** directly via `auth.signature`; that's the only cryptographic identity guarantee the protocol provides regardless of how many hops carried the envelope.

## 6. Authentication (`auth`)

```
Auth = {
  0: kind         u8           ; MANDATORY; 0=Anonymous, 1=Session, 2=ApiKey, 3=NodeIdentity, 4=UserIdentity
  1: subject_id   bstr(32)     ; OPTIONAL; presence rules per §11.3 (present for Session/NodeIdentity/UserIdentity)
  2: epoch        u32          ; OPTIONAL; presence rules per §11.3 (present for NodeIdentity/UserIdentity)
  3: signature    bstr(64)     ; OPTIONAL; presence iff IS_SIGNED=1; always Ed25519 over canonical envelope (§6.3)
  4: session_id   bstr(16)     ; OPTIONAL; presence rules per §11.3 (present for Session/ApiKey)
}
```

Only field `0: kind` is mandatory inside the `Auth` map. The remaining fields are conditionally present per the rules in §11.3, depending on `auth.kind` and the `IS_SIGNED` flag. The validator (4c1) enforces presence constraints from §11.3, NOT from this schema block.

### 6.1 Auth kinds

- **Anonymous** — no signature, no session. Allowed only for `Control / Hello` envelopes at handshake boundary.
- **Session** — short-lived auth bound to a session_id. In UFP/2 v2, Session envelopes MUST have `IS_SIGNED=0` (server trusts the TLS-authenticated transport channel + session binding). `IS_SIGNED=1` with `Session` is REJECTED (`BodyValidationFailed`) — a separate per-session signing-key registry would be needed and is out of scope for v2. When cryptographic sender proof is required (cross-org, untrusted transport), use `NodeIdentity` or `UserIdentity` instead.
- **ApiKey** — long-lived auth for external API consumers (`/v1/*` OpenAI-compat clients). The **HMAC validation of the API key happens at the edge gateway BEFORE the request is wrapped into a UFP/2 envelope** — UFP/2 itself NEVER carries HMACs. `auth.signature` is exclusively Ed25519 (§11.3). Inside UFP/2 an ApiKey envelope carries `auth.session_id = api_key_id` for audit/routing; the gateway acts as the identity-asserting party for subsequent core processing. `IS_SIGNED` MUST be `0` on ApiKey envelopes.
- **NodeIdentity** — sender is a node, signs with its node Ed25519 key. Used for mesh + sync traffic.
- **UserIdentity** — sender is a user, signs with their user Ed25519 key. Used for high-trust operations (administrative ops, signed addon installs).

### 6.2 Permission epoch

`epoch` MUST match or exceed the receiver's known `policy_epoch` for the source. If the receiver sees a higher epoch (newer policy) than the sender claims, the message MAY still be accepted but receiver re-evaluates ACL using the higher epoch. If sender claims a higher epoch than the receiver knows, the receiver requests a policy refresh before processing.

This integrates with the existing Sync Permission Engine (`sync_user_org_profiles`, `sync_resource_acl`, etc.).

### 6.3 Signature scope

`signature` = Ed25519 sign(private_key, canonical_envelope_bytes_for_signing)

Where `canonical_envelope_bytes_for_signing` is the canonical CBOR encoding of the envelope map containing **fields 0–15** (every immutable field), with two transformations:

1. `auth.signature` is REPLACED by `bstr(0x00 × 64)` (zeroed placeholder of identical length).
2. `forwarded_via` (field 16) is OMITTED entirely.

Rationale:
- The zeroed placeholder keeps map serialization length deterministic, so the receiver can re-construct the exact bytes that were signed without guessing serialization decisions.
- Excluding only field 16 — the single unauthenticated hop-trail field — allows hop mutation without invalidating the source's signature.
- Including fields 14–15 (`fragment_index`, `fragment_count`) when present prevents an attacker from rearranging fragments to confuse the receiver: each fragment's envelope is independently signed with its own index/count.

Verification:
1. Receiver reconstructs `canonical_envelope_bytes_for_signing` by re-encoding the envelope with `auth.signature := 0x00 × 64` and dropping field 16.
2. Verifies `Ed25519::verify(auth.subject_id, signature, reconstructed_bytes)`.
3. Rejects with `InvalidSignature` (§11 error code `0x0005`) if verification fails.

## 7. Encryption (`IS_ENCRYPTED`)

### 7.1 Transform pipeline order

The sender pipeline is fixed and unambiguous:

```
structured_payload
  → cbor_canonical_encode                  (produces `plaintext_body`)
  → IS_COMPRESSED ? lz4_compress           (produces `compressed_body`)
  → IS_ENCRYPTED  ? aead_encrypt(key, nonce, aad, body_so_far)
                                            (produces `nonce || ciphertext || tag`)
  → assign result to envelope.body
  → compute auth.signature per §6.3        (signature covers the final body bytes)
```

The receiver runs the inverse:

```
authenticate envelope per §9 / §11.3:
  → if IS_SIGNED=1: verify auth.signature (§6.3)
  → if IS_SIGNED=0: verify unsigned policy + transport/session binding
  → if IS_FRAGMENT: reassemble per §10
  → IS_ENCRYPTED  ? aead_decrypt(key, nonce, aad, body)
  → IS_COMPRESSED ? lz4_decompress
  → cbor_canonical_validate + decode
  → dispatch by (channel, kind)
```

This ordering guarantees:
- Signature covers the wire-final `body` bytes (encrypted+compressed). Tampering at any layer is detected before the receiver wastes work on decrypt/decompress.
- AAD does NOT need to include `auth.signature` (signature is computed AFTER encryption, so chicken-and-egg is avoided).

### 7.2 AEAD construction

When `IS_ENCRYPTED` is set, `envelope.body` is laid out as:

```
body = nonce(12) || ciphertext(N) || aead_tag(16)
```

- **AEAD algorithm**: IETF ChaCha20-Poly1305 (RFC 8439) with 256-bit key and 96-bit nonce. Rationale: constant-time on all architectures, no AES-NI dependency (critical for Zig/embedded ports and ARM without crypto extensions), 256-bit security level, simple to implement.
- **AAD** (additional authenticated data): canonical CBOR encoding of envelope fields covering the immutable header. AAD shape depends on whether the message is fragmented:
  - **Non-fragmented envelope**: AAD covers fields `0–8 + 10–12` (every immutable field EXCEPT `body` (9), `auth` (13), `forwarded_via` (16)). Fields 14–15 are absent. AAD `flags` is the envelope's `flags` value as-is.
  - **Fragmented envelope**: AEAD encryption runs ONCE over the assembled `compressed_body` BEFORE fragmentation (see §10.0). The AAD is the **logical envelope AAD** — the canonical CBOR encoding of fields `0–8 + 10–12` from the LOGICAL (un-fragmented) envelope, **EXCLUDING field 9 `body`**. The body's confidentiality+integrity is provided by the AEAD tag computed over the logical `compressed_body`; AAD covers only the header. Excluding `body` from AAD is necessary because the receiver does not have the plaintext body before decryption (it has only `nonce || ciphertext || tag` after reassembly). Fragment-specific fields (14 `fragment_index`, 15 `fragment_count`, and per-fragment `body` bytes) are NOT part of AAD; per-fragment integrity comes from each fragment's Ed25519 signature (§6.3), which DOES cover fields 14–15 and the per-fragment `body`.

  **AAD `flags` value for fragmented messages**: defined as `aad_flags = fragment.flags & !IS_LAST_FRAGMENT` (the last-fragment bit masked off). Rationale: `IS_LAST_FRAGMENT` is the ONLY bit that varies across fragments of one logical message, and AEAD AAD MUST be identical for sender (computed once before fragmentation) and every receiver (recomputed from any fragment's header). Masking off `IS_LAST_FRAGMENT` yields a stable AAD `flags` value identical on every fragment of the same logical message. All other flag bits (`IS_FRAGMENT`, `IS_ENCRYPTED`, `IS_COMPRESSED`, `IS_BROADCAST`, `IS_SIGNED`) are identical across fragments per §10.1 and are included in AAD verbatim.

  Implementation note: senders construct the logical envelope skeleton (header fields with `flags = sender_logical_flags | IS_FRAGMENT`, body=placeholder), compute the AAD over fields 0–8+10–12 of that logical envelope (skipping field 9, masking `IS_LAST_FRAGMENT` from flags), then run AEAD(key, nonce, AAD, compressed_body) to produce `nonce || ciphertext || tag`. They then split this ciphertext into fragment bodies. Receivers reassemble fragment bodies into the AEAD ciphertext, reconstruct the logical AAD from any fragment's COMMON fields (masking `IS_LAST_FRAGMENT` from flags), and decrypt. Both sides build AAD from header alone — no body needed for AAD.

  Why split: AEAD encryption happens once over the logical body. If AAD included per-fragment fields, every fragment would need its own AEAD pass with its own key/nonce, defeating the single-encryption design. Per-fragment signature provides the per-fragment integrity needed for replay/dedup; AEAD provides logical-body confidentiality.
- **Key derivation**: per-pair X25519 ECDH session key derived during handshake. One key per (source NodeAddress, destination NodeAddress) ordered pair, rotated on the existing mesh key-rotation event (predecessor: `MESH_MSG_KEY_ROTATION` discriminator `0x0025`; under UFP/2: `channel=0x05 Control, kind=KeyRotation`).
- **Nonce construction** (replaces v0.1's random-only scheme): each session key carries a deterministic monotonic counter. Nonce layout: `random_session_prefix(4 bytes)` || `counter_big_endian(8 bytes)`. The 4-byte random prefix is generated when the key is established and is fixed for the key's lifetime; the 8-byte counter increments by 1 per encrypted message. This formally prevents nonce reuse without coordinating randomness sources across implementations.
- **Key rotation threshold**: rotate before `counter == 2^48` (well below ChaCha20-Poly1305's 2^64 nonce limit) **OR** before key lifetime exceeds **24 hours**, whichever comes first. This is the single normative threshold; implementations MUST trigger rotation at the earlier of the two limits.
- **Counter persistence**: senders MUST persist the counter to durable storage frequently enough that a crash cannot replay a counter value. Recommended: persist every 1024 increments and on graceful shutdown; on startup, advance counter by 1024 to skip any unflushed range.

### 7.3 When to encrypt

Encryption is OPTIONAL per channel:
- **Mesh, SyncLedger, Frontend over intra-cluster TLS 1.3**: encryption typically OFF — TLS provides confidentiality at the transport layer.
- **Multi-hop through untrusted intermediates**: encryption ON for end-to-end confidentiality (intermediate hops see encrypted body only).
- **Cross-organisation federation (future)**: encryption ON; transport TLS does not extend across org boundaries.

Per-channel encryption policy defaults are listed in the channel taxonomy (§4) once finalised in v0.3.

## 8. Compression (`IS_COMPRESSED`)

When `IS_COMPRESSED` is set, the body (before encryption per §7.1) is lz4-compressed:

```
compressed_body = lz4_frame(plaintext_body)
```

- **Algorithm**: lz4 frame format (header + blocks + content checksum). NOT raw lz4 block — frame format is self-describing and standard across all language implementations: `https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md`.
- **Threshold**: compress only if `body_size_estimate > 4096` bytes. Smaller messages skip compression (overhead > savings).
- **Why lz4 not zstd**: lz4 is ~10× faster at compression (~500 MB/s) and ~3× faster at decompression (~5 GB/s). For real-time mesh heartbeats, GUI updates, sync push batches, latency matters more than the ~30% extra wire bytes vs zstd. Future versions MAY add `IS_COMPRESSED_ZSTD` flag for offline snapshot blobs where ratio matters more than throughput.
- **Order**: compression happens BEFORE encryption (see §7.1 pipeline). This ordering is standard practice — encrypting compressed data is normal; compressing encrypted data is useless (ciphertext is incompressible).

### 8.1 Side-channel risk: CRIME / BREACH-style leakage

**Compressing secret-bearing bodies that also contain attacker-influenced plaintext can leak the secret via ciphertext length, even with strong AEAD.** This is the CRIME/BREACH class of attack: an attacker who can inject chosen prefixes into the plaintext and observe ciphertext sizes can binary-search the secret one byte at a time by detecting which prefixes compress well (because they match the secret).

This risk EXISTS in UFP/2 whenever ALL THREE conditions are true:
1. The body contains a secret (auth token, key material, private addon data).
2. The body ALSO contains attacker-influenced plaintext (e.g. user-supplied form fields echoed back).
3. The attacker can observe envelope sizes (intra-cluster TLS hides per-message size from external observers but NOT from intermediate mesh hops, and NOT from the local network in cross-org futures).

**Mitigation policy** (per-channel, documented in §4):
- **UI, HostFunction, Stream, Frontend channels**: MAY use compression. Application code MUST NOT mix attacker-controlled fields with secrets in the same body. Token-bearing fields go in `auth.signature` (not in body), reducing exposure.
- **Control channel handshake messages bearing fresh secrets** (key exchange, session establishment): `IS_COMPRESSED` MUST be 0 regardless of size. Senders that violate this MUST be rejected by receivers.
- **SyncLedger channel**: compression allowed (sync operations are signed, attacker cannot inject plaintext into another node's operation).
- **FrameBlob channel**: compression allowed (raw pixel/JPEG data, no secrets, no attacker plaintext mixing).

Padding to fixed-size buckets is NOT specified in v2 (deferred to UFP/3 if cross-org untrusted intermediates become a requirement).

The earlier claim in v0.1 that "compress-before-encrypt hides compressibility from observers" was WRONG and has been removed.

## 9. Replay protection

Receivers maintain two anti-replay mechanisms:

1. **Time skew window**: reject if `|now_ms - created_at_ms| > 30_000` (30 second window covers clock skew, queueing, slow links). Per-channel override is allowed (e.g. Mesh heartbeat may use 5 s; offline sync replay may use 5 min).
2. **Message ID dedup**: LRU cache of `(source.id, message_id, fragment_index)` triples for the last `N=10000` entries (per-source, configurable). If the triple is seen before in the window → reject as replay.

**Fragment-aware dedup is critical**: all fragments of one logical message share the same `message_id` (§10), so dedup keyed only on `(source, message_id)` would reject every fragment except the first. Including `fragment_index` in the dedup key makes each fragment dedup independently. For non-fragmented envelopes, `fragment_index` is absent (per §3.2) and is treated as a fixed sentinel value `0xFFFF` in the dedup key.

**Verification-before-commit ordering** (anti-poisoning): receivers MUST verify the envelope's signature (§6.3) **before** inserting the dedup triple into the LRU cache. Otherwise an attacker could send a forged envelope with target `message_id`/`fragment_index`, poisoning the dedup key so the legitimate envelope is later rejected as a replay. Order:

1. Decode envelope (canonical CBOR validation).
2. Validate flag combinations (§3.4) AND `auth` field invariants (§11.3 — `auth.kind` mapping to required/forbidden sub-fields, channel/kind policy compliance).
3. Authenticate sender:
   - If `IS_SIGNED=1`: verify `auth.signature` per §6.3. Reject `InvalidSignature` on failure.
   - If `IS_SIGNED=0`: verify the channel/kind policy permits unsigned (§11.3 table), AND verify the transport/session binding (e.g. TLS session matches `auth.session_id`, or the envelope arrived on an established mesh peer matching `source.id`). Reject `InvalidSignature` if unsigned is not permitted, `PermissionDenied` if transport/session binding fails.
4. Check epoch / revocation (§11.2). Skip for `auth.kind == Anonymous`.
5. Check time skew window.
6. Check dedup LRU; if present → reject; if absent → DO NOT INSERT YET.
7. Process the envelope:
   - **Non-fragment** envelope: decrypt, decompress, dispatch by `(channel, kind)`.
   - **Fragment** envelope (`IS_FRAGMENT=1`): insert into reassembly buffer keyed by `(source.id, message_id)`. If all `fragment_count` fragments now present, run §10.2 reassembly → decrypt+decompress+dispatch.
8. Commit dedup triple `(source.id, message_id, fragment_index)` to LRU. The commit point is:
   - **Non-fragment**: after successful dispatch (step 7 fully completed).
   - **Fragment**: after step 7 inserted the fragment into the reassembly buffer (regardless of whether reassembly is complete). This is critical — without committing here, a duplicate fragment arriving mid-reassembly would NOT be deduped and could overwrite or conflict with the buffered original.

Invalid fragments (bad sig, bad epoch, bad time skew, etc.) never reach step 7 and never occupy a dedup slot, so they cannot block subsequent valid arrivals. A correctly-signed duplicate of an already-buffered fragment is deduped at step 6 on the second arrival.

Forwarded messages: intermediate hops MUST NOT perform replay checks on `(source, message_id, fragment_index)` — only the final destination does. Otherwise the same message would be rejected at the second hop because the first hop already "saw" it.

## 10. Fragmentation

For payloads exceeding the transport MTU (WebSocket frame max, QUIC stream chunk, configurable receiver limit), the sender splits the logical message into fragments. Each fragment is a complete UFP/2 envelope.

### 10.0 Sender fragmentation pipeline

Pipeline order (extends §7.1 for fragmented messages):

1. Encode the structured payload as canonical CBOR → `plaintext_body`.
2. If `IS_COMPRESSED`: `compressed = lz4_compress(plaintext_body)`. Else `compressed = plaintext_body`.
3. If `IS_ENCRYPTED`: `encrypted = nonce || aead_encrypt(key, nonce, aad, compressed) || tag`. Else `encrypted = compressed`. (AEAD AAD per §7.2 uses the COMMON immutable fields shared across all fragments, NOT per-fragment fields — see below.)
4. Determine fragment size based on transport MTU and per-channel limits. Split `encrypted` into N fragments of body bytes.
5. For each fragment `i` in `0..N`:
   a. Construct envelope with `flags |= IS_FRAGMENT`, `fragment_index = i`, `fragment_count = N`, `flags |= IS_LAST_FRAGMENT iff i == N-1`, `body = fragment_bytes[i]`.
   b. Copy all common fields (source, destination, channel, kind, priority, created_at_ms, ttl_ms, trace_id, correlation_id, message_id, auth.kind, auth.subject_id, auth.epoch, auth.session_id) from the logical message.
   c. Compute `auth.signature` over THIS fragment's canonical envelope per §6.3. Each fragment gets a unique signature because each fragment's `body` and `fragment_index` differ.
6. Transmit fragments in order (transport MAY reorder; reassembly handles arrival order per §10.2).

**AEAD nonce per fragment**: encryption is applied ONCE to the assembled `encrypted` blob in step 3, before fragmentation. There is a SINGLE nonce + tag for the logical message, embedded in fragment 0's body bytes (the nonce is the leading 12 bytes of `encrypted`). Subsequent fragments carry only ciphertext chunks. Receivers reassemble per §10.2, then strip nonce+tag from the leading 12+16 bytes of the assembled body, then decrypt. This avoids per-fragment nonce coordination overhead.

### 10.1 Fragment envelope

When `flags & IS_FRAGMENT = 1`:
- `message_id`: same value across all fragments of one logical message (this is the "logical message ID"; per-fragment uniqueness comes from `fragment_index`).
- Field 14 `fragment_index`: 0-based fragment number (u16). MUST satisfy `fragment_index < fragment_count`; receivers reject with `BodyValidationFailed` otherwise.
- Field 15 `fragment_count`: total fragment count, declared on every fragment (u16). MUST be `> 0` AND `<= 65535`; receivers reject with `BodyValidationFailed` on `fragment_count == 0`. MUST match across all fragments of the same logical message; mismatch (a fragment arriving with a different `fragment_count` for the same `(source, message_id)`) → `FragmentAssemblyError`.
- `flags & IS_LAST_FRAGMENT = 1` iff `fragment_index == fragment_count - 1`.
- All immutable fields except `body`, `fragment_index`, and `auth.signature` MUST be identical across fragments of one logical message: `source`, `destination`, `channel`, `kind`, `priority`, `created_at_ms`, `flags` (modulo `IS_LAST_FRAGMENT`), `ttl_ms`, `trace_id`, `correlation_id`, `fragment_count`, and within `auth`: `kind`, `subject_id`, `epoch`, `session_id`.
- **Each fragment is independently signed** — `auth.signature` MUST DIFFER per fragment because each signature covers that fragment's unique `body` + `fragment_index` (and possibly differing `IS_LAST_FRAGMENT` bit in `flags`). Receivers verify each fragment's signature against its own canonical envelope per §6.3.

### 10.2 Reassembly

Receivers buffer fragments keyed by `(source.id, message_id)`. The reassembly buffer for one key holds an array of `fragment_count` slots, each `None` initially.

**Atomicity** (required for thread-safe / concurrent receivers): for a given key `(source.id, message_id)`, the sequence `check dedup → buffer fragment → commit dedup` MUST run under a per-key mutex. Two concurrent threads receiving fragments of the same logical message MUST serialize their buffer-insert + dedup-commit operations. This prevents races where two threads both observe the dedup slot as absent and both try to write the same `fragment_index`.

**Write-once fragment slots**: when a fragment arrives, the receiver checks `buffer.slots[fragment_index]`:
- If `None`: store the fragment's body bytes. Commit dedup. Continue.
- If `Some(existing_bytes)`: this is a duplicate fragment. Compare the dedup LRU first — if the dedup key is already present, drop silently (replay). If the dedup key is absent (LRU eviction) AND the new bytes are byte-identical to existing, accept silently (idempotent retry). If the bytes DIFFER, reject as `FragmentAssemblyError` (§11 code `0x000C`) — this indicates conflicting fragments with the same index, which is always an attack or implementation bug.

**Completion**: once all `fragment_count` slots are populated:
1. Verify every fragment's signature individually (§6.3) — this MUST be done lazily, on arrival of each fragment (not deferred to completion), because it gates the buffer-insert step.
2. Concatenate `body` bytes in `fragment_index` order.
3. The concatenated bytes are now the logical message's body; apply the §7.1 receive pipeline (decrypt → decompress → CBOR decode → dispatch).
4. Free the reassembly buffer slot.

### 10.3 Reassembly limits and timeouts

- **Max fragments per logical message**: 65535 (u16 limit). Channels using fragmentation MUST document a lower practical limit per use case.
- **Reassembly buffer per (source, message_id)**: max 64 MB. Receivers MUST reject and emit `FragmentAssemblyError` (§11 code `0x000C`) if exceeded.
- **Reassembly timeout**: 60 seconds from first-seen fragment. If `fragment_count` fragments do not arrive in time, discard buffered state and emit `FragmentAssemblyError`.
- **Out-of-order arrival**: allowed (transport may reorder); reassembly indexes by `fragment_index`.
- **Duplicate fragment**: dedup per §9 already rejects exact replay of `(source, message_id, fragment_index)`; a duplicate arriving after the original is dropped silently as a replay.

### 10.4 Channels and fragmentation

- **Mesh heartbeat, Control handshake**: MUST NOT fragment. Receivers reject `IS_FRAGMENT=1` on these channels as `BodyValidationFailed`.
- **Stream channel (LLM tokens, file upload, log tail)**: ALWAYS fragments above transport MTU. Per-channel max fragments documented in Stream channel spec.
- **FrameBlob (camera frames > MTU)**: fragments allowed. Single frame typically fits in one envelope; high-resolution video frames may fragment.
- **SyncLedger snapshot blobs**: fragments allowed for large snapshots (>4 MB body).
- Other channels: fragment when needed, no special constraints.

## 11. Error handling

Receivers reject malformed envelopes by responding with an envelope on the same connection:

```
channel = 0x05 (Control)
kind = 0x00FF (ProtocolError)
correlation_id = offending_message_id
body = {
  0: error_code     u16    ; see below
  1: error_message  tstr   ; human-readable
  2: field_path     tstr   ; OPTIONAL, where the defect was detected
}
```

Standard `error_code` values:

| code | name |
|------|------|
| `0x0001` | CanonicalEncoding (envelope is not canonical CBOR) |
| `0x0002` | UnknownProtocolVersion |
| `0x0003` | UnknownChannel |
| `0x0004` | UnknownKind |
| `0x0005` | InvalidSignature |
| `0x0006` | ExpiredEpoch (epoch < receiver_known_epoch − 1) |
| `0x0007` | ReplayDetected |
| `0x0008` | ClockSkewExceeded |
| `0x0009` | NestingTooDeep |
| `0x000A` | DecryptionFailed (AEAD tag verify failed) |
| `0x000B` | DecompressionFailed (lz4 frame invalid) |
| `0x000C` | FragmentAssemblyError |
| `0x000D` | ForwardingLoop (forwarded_via.len() >= 32) |
| `0x000E` | BodyValidationFailed (schema mismatch) |
| `0x000F` | PermissionDenied |
| `0x0010` | RateLimited (sender exceeded quota) |
| `0x0011` | UnsupportedCompression |
| `0x0012..0xFFFF` | reserved |

### 11.1 Downgrade attack policy

UFP/2 has a single live protocol version (`protocol_version = 2`). There is NO negotiated downgrade to any prior protocol.

- Every receiver MUST reject envelopes with `protocol_version != 2` with `UnknownProtocolVersion` (§11 code `0x0002`). Rejection is final — receivers do not propose alternatives.
- After the Faza 6 Krok 4 migration (commits 4c1–4c6), the v1 CBOR `Envelope` type is DELETED from the codebase. No dispatch path exists that accepts v1 bytes.
- During the migration window itself (between 4c1 and 4c6), receivers MAY temporarily accept both v1 and v2 on a single dedicated transport endpoint to keep the system green between commits. As soon as 4c6 lands, that endpoint hard-rejects anything that isn't v2.
- A future UFP/3 upgrade SHALL follow the same model: hard cutover, no silent downgrade, no negotiation field. The `protocol_version` byte is the only version signal.
- All rejected-protocol-version events MUST be logged (with `source.id`, `created_at_ms`, observed version) to detect tooling lag or active downgrade attempts post-migration.

### 11.2 Key revocation and compromise procedure

Ed25519 identity keys can be compromised (stolen, leaked, recovered from a decommissioned device). UFP/2 relies on the existing Sync Permission Engine (`policy_epoch`) for fast revocation:

1. **Revocation event**: an admin invokes the revocation API (out-of-band; UFP/2 does not specify the API). The revocation:
   - Marks the compromised key as revoked in `sync_nodes` (for node keys) or `user_identity_keys` (for user keys).
   - Increments `policy_epoch` for every org the key was a member of.
   - Issues a Control / KeyRevocation envelope signed by an org admin's key, flooded across the mesh via `IS_BROADCAST` semantics.

2. **In-flight messages signed by the revoked key**:
   - Already-accepted messages (committed to sync ledger, executed actions) STAY VALID. Revocation is forward-looking, not retroactive — historical actions cannot be safely undone by revoking the key.
   - New messages arriving after the revocation MUST be rejected with `InvalidSignature` (§11 code `0x0005`) regardless of cryptographic validity, because `auth.epoch < receiver_known_epoch`.

3. **Epoch enforcement on receive** (codifies §6.2):
   - **Step A — Revocation denylist check (hard-deny, no grace).** Receiver first checks whether `auth.subject_id` is in the local revoked-key denylist (populated from received `KeyRevocation` Control envelopes). If revoked → reject as `InvalidSignature` (§11 code `0x0005`) regardless of `auth.epoch`. The revoked-key denylist takes priority over every other check; the epoch grace window in step B does NOT apply.
   - **Step B — Epoch grace window.** If the subject is NOT in the denylist, receiver compares `auth.epoch` against its locally cached `policy_epoch` for the source identity:
     - If `auth.epoch < cached_epoch - 1`: reject as `ExpiredEpoch` (§11 code `0x0006`). One-epoch grace covers in-flight messages from non-revoked subjects mid-policy-propagation (e.g. ACL update unrelated to revocation).
     - If `auth.epoch > cached_epoch`: receiver fetches the latest policy from a trusted peer (via Mesh / SyncLedger pull), then re-evaluates. If the refresh reveals the subject is revoked, fall back to step A.
     - If `cached_epoch` is stale (no policy refresh in > 5 minutes), receiver issues a policy refresh proactively.

4. **Replacement key issuance**: after revocation, the identity owner generates a new Ed25519 keypair and registers it via the same admin API. The new pubkey gets a new entry in `sync_nodes` or `user_identity_keys` with the incremented epoch. Existing connections holding session keys derived from the revoked key MUST be torn down.

5. **Quorum protection for admin keys**: revocation of an admin key requires a multi-signature operation from a quorum of remaining admins (out-of-band administrative protocol, not defined in UFP/2).

Worst-case revocation latency: bounded by `policy_epoch` propagation through the mesh. For a typical 10-node cluster with 500ms heartbeat, full propagation is ~3s. Receivers that have not heard from any peer in > 5 minutes are operating on potentially-stale epoch data and SHOULD treat all incoming traffic with elevated caution (e.g. reject `Domain` admin operations, allow `Mesh` heartbeat only).

### 11.3 Per-channel authentication requirements

The envelope-level validator (4c1) MUST enforce the following invariants on every incoming envelope before dispatch. Channel-specific application logic adds further checks on top.

**`auth` field is mandatory on EVERY envelope.** The field is never absent. At minimum it carries `auth.kind`; other sub-fields are present according to the rules below. Field 13 changes from "optional" in earlier drafts to "mandatory" in v0.4.

**Envelope-level invariants (all channels):**

- `auth` field MUST be present in every envelope. `auth.kind` MUST be one of the values defined in §6.1.
- `auth.signature` MUST be present iff `IS_SIGNED=1`. Mismatch → `BodyValidationFailed`.
- `auth.signature`, when present, is ALWAYS an Ed25519 signature (64 bytes) computed per §6.3. It is NEVER an HMAC, MAC, or any other algorithm. The field name `signature` is exclusively Ed25519.
- `auth.subject_id` MUST be present iff `auth.kind ∈ {Session, NodeIdentity, UserIdentity}`. For `Anonymous` and `ApiKey` it MUST be absent (Anonymous has no subject; ApiKey identity is carried in `auth.session_id`).
- `auth.session_id` MUST be present iff `auth.kind ∈ {Session, ApiKey}`. For Anonymous/Node/User it MUST be absent (session-less authentication).
- `auth.epoch` MUST be present for `NodeIdentity`/`UserIdentity` (used in §11.2 revocation flow). MAY be omitted for `Session`/`ApiKey`/`Anonymous` (epoch=0 assumed).

**Per-auth-kind invariants:**

- `auth.kind == Anonymous` ⇒ `IS_SIGNED=0`, `auth.signature` absent, `auth.subject_id` absent, `auth.session_id` absent, `auth.epoch` absent.
- `auth.kind == NodeIdentity` ⇒ `IS_SIGNED=1`, `auth.signature` present, signature MUST verify against `auth.subject_id` (Ed25519 pubkey of the node).
- `auth.kind == UserIdentity` ⇒ same as NodeIdentity but `subject_id` is a user's Ed25519 pubkey.
- `auth.kind == Session` ⇒ `auth.session_id` present, `auth.subject_id` present (resolved identity of the user/node who established the session). `IS_SIGNED` MUST be `0`. `Session + IS_SIGNED=1` is REJECTED with `BodyValidationFailed` — session authentication relies on TLS transport + session binding lookup, not on per-envelope signatures. Mutating operations needing cryptographic non-repudiation MUST use `NodeIdentity` or `UserIdentity` instead.
- `auth.kind == ApiKey` ⇒ `auth.session_id` present (carries the api_key_id). `IS_SIGNED=0` always — ApiKey authentication uses **transport-level HMAC** (HTTP header `X-Tentaflow-Signature` validated by the OpenAI-compat edge gateway before the request is wrapped into a UFP/2 envelope). UFP/2 itself does NOT carry HMAC; the API key check happens before envelope construction. Within UFP/2, **ApiKey envelopes are STRICTLY LIMITED to read/inference kinds** (e.g. `Frontend/ChatCompletion`, `Frontend/Embeddings`, `Domain/ModelList`, `Domain/ServiceList`). Mutating kinds (admin Frontend/Domain operations, addon installation, ACL changes, etc.) are NOT reachable via ApiKey under any circumstances; the receiver MUST reject with `PermissionDenied` (§11 code `0x000F`). To perform mutating operations, an external client MUST either obtain a real interactive `Session` (browser login) OR present a real `UserIdentity` signed with the user's Ed25519 private key (out-of-band signed request, gateway transports verbatim). The gateway has no way to forge a UserIdentity signature without the user's private key, so gateway-signed UserIdentity envelopes are forbidden.

`IS_ENCRYPTED=1` envelopes additionally MUST satisfy:
- `auth.kind ∈ {Session, NodeIdentity, UserIdentity}` (need an authenticated key-exchange partner).
- The receiver MUST have a valid session key for `(source.id, destination.id)` pair, established via prior key exchange.

**Per-channel default policy:**

| channel | auth.kind required | IS_SIGNED required | IS_ENCRYPTED default | Notes |
|--------:|--------------------|--------------------|----------------------|-------|
| `0x01` UI | Session or UserIdentity | PER_KIND | OFF (TLS transport) | Per-kind: state mutations require IS_SIGNED=1; pure render messages may be NO. |
| `0x02` HostFunction | NodeIdentity (addon's host) or UserIdentity | YES | OFF | Addon ABI boundary — every call signed. |
| `0x03` Stream | Session, NodeIdentity, or UserIdentity | PER_KIND | OFF (per-chunk overhead too high) | Stream/Open MUST be signed; subsequent chunks NO (covered by session). |
| `0x04` Mesh | NodeIdentity | YES | OFF (intra-cluster TLS) | Mesh peers always sign with node key. |
| `0x05` Control | Anonymous (Hello only) OR NodeIdentity/UserIdentity | PER_KIND | OFF | `Hello` is NO (Anonymous); everything else YES. |
| `0x06` SyncLedger | NodeIdentity | YES | OFF | Sync ops also have inner Ed25519 sig over CBOR body; envelope adds outer sig over wrapper. |
| `0x07` Frontend | Session (browser) OR ApiKey (external) | PER_KIND | OFF (TLS) | Read-only requests may be NO; mutating requests YES. |
| `0x08` Domain | Session OR NodeIdentity OR UserIdentity | PER_KIND | OFF | Read: NO; admin: YES (UserIdentity required). |
| `0x09` FrameBlob | NodeIdentity (issuing node) | YES | OFF | Frame data signed by source node. |

**Cross-channel rules:**
- `IS_ENCRYPTED=1` with `auth.kind == Anonymous` is rejected on EVERY channel (cannot encrypt without an authenticated key-exchange partner).
- `auth.kind == Anonymous` is ONLY accepted for `(channel=0x05 Control, kind=Hello)`. Any other channel/kind with Anonymous → `PermissionDenied` (§11 code `0x000F`).
- A channel marked `IS_SIGNED required = YES` rejects unsigned envelopes with `InvalidSignature` (§11 code `0x0005`) regardless of transport.
- `IS_ENCRYPTED` policy may be channel-overridden via per-pair handshake (e.g. multi-hop forwarding through untrusted intermediates flips Mesh to encryption-required for that pair); overrides are negotiated via `Control / EncryptionPolicy` messages.

The 4c1 implementation MUST include test coverage for every invariant in this section, including negative tests for each violation.

## 12. Multi-language considerations

UFP/2 is defined by THIS document plus the canonical CBOR validator (`tentaflow-sdk-spec::canonical`) and the schema manifest (`tentaflow-sdk-gen/catalog-manifest/v2.cbor`).

A new language port (Zig, Go, Python, C#, Swift, Kotlin) MUST:
1. Implement canonical CBOR encoder/decoder matching the Faza 6 deterministic profile (no `f16`/`f32`, no `undefined`, no indefinite-length, minimum-width arguments, bytewise key order).
2. Implement Ed25519 sign/verify (RFC 8032).
3. Implement ChaCha20-Poly1305 AEAD (RFC 8439).
4. Implement lz4 frame format (RFC unofficial: `https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md`).
5. Implement the `Envelope` schema from §3.2 exactly.
6. Generate per-`(channel, kind)` payload types from the manifest.

The manifest is itself a UFP/2-emittable artifact (`channel=Control, kind=ManifestExport`). A new port can bootstrap by fetching the manifest from any tentaflow-core node and generating its type system from it.

## 13. Migration from prior protocols (Faza 6 Krok 4c sequencing)

Migration is performed in **6 atomic commits**, each leaving the system green (`cargo build && cargo test` pass) and each removing the corresponding legacy code in the same commit. **No permanent parallel stack.**

### 4c1 — Envelope core (sdk-spec)
- Add `Envelope`, `NodeAddress`, `Auth`, encoder/decoder to `tentaflow-sdk-spec/src/protocol/frame.rs`.
- Add channel/kind taxonomy constants.
- Tests: encode/decode roundtrip, canonical validation, signature verification, lz4 compression, AEAD encryption, fragment assembly.
- **Used by zero channels.** This commit only adds the type; nothing depends on it yet.

### 4c2 — Mesh channel migration
- Replace `tentaflow-protocol::Envelope` (CBOR) for mesh traffic with UFP/2.
- All `MESH_MSG_*` discriminators become `kind` values under `channel=0x04`.
- DELETE: `tentaflow-protocol::mesh::*` CBOR encoders/decoders.
- Tests: mesh heartbeat, peer pairing, frame proxy, all sync_ledger mesh messages.

### 4c3 — Sync ledger channel migration
- Sync operations wrap inside `(channel=0x06, kind=PushOperation/Ack/Pull/...)` envelopes.
- Body remains CBOR `SyncOperation` (preserves existing Ed25519 sigs in flight).
- DELETE: bespoke sync push/pull wire types (only kept as logical schema, no longer their own outer envelope).
- Tests: full sync flow A→B with old operations still verifying.

### 4c4 — Frontend channel migration (BREAKING for browser)
- Frontend WS protocol switches from CBOR `Envelope`/`MessageBody` to UFP/2 with `channel=0x07`.
- Frontend JS (`www/js/protocol/codec.js`, `www/js/protocol/wasm_glue.js`) regenerated for CBOR.
- DELETE: `tentaflow-protocol::envelope::*` CBOR, `tentaflow-protocol::message_body::*` CBOR variants.
- Coordinated deploy: core + dashboard JS must ship together. Browser cache busts on UFP/2 cutover.
- Tests: dashboard E2E (Playwright) on every MessageBody variant.

### 4c5 — Addon UI/Stream/HostFunction channels migration
- Addon ↔ Core (wasmtime) and addon ↔ Frontend (proxied through core) switch to UFP/2 with `channel=0x01/0x02/0x03`.
- Body schemas come from existing Faza 6 sdk-spec catalog.
- DELETE: Faza 6 v1 envelope (`tentaflow-sdk-spec::protocol::envelope::Envelope` — the OLD one, distinct from new UFP/2 `frame::Envelope`).
- Tests: existing 385 sdk-spec + 39 sdk-gen tests must pass.

### 4c6 — FrameBlob + ExternalAPI + Domain channels
- HTTP frame pickup (`/core/frame/pickup`) wraps response in UFP/2 `channel=0x09 FrameBlob` envelope. Raw RGB/JPEG bytes go in `body` with `IS_COMPRESSED` if applicable.
- OpenAI-compat `/v1/*` API converts internal UFP/2 responses to the JSON SSE format clients expect (clients NEVER see UFP/2 directly; conversion is at the edge).
- Domain messages (recorder, scheduler, sync_conflict) move from `tentaflow-protocol::message_body` variants into `channel=0x08`.
- DELETE: any remaining CBOR message types.
- Tests: frame pickup E2E, OpenAI completion E2E, scheduler binary protocol E2E.

After 4c6: ONLY UFP/2 exists. All other wire formats are removed from the codebase. The migration window is roughly 6 commits, each codex-reviewed, each atomic. No version negotiation, no fallback path.

## 14. Security threat model

Brief enumeration; full STRIDE/OWASP coverage lives in `docs/SECURITY_THREAT_MODEL.md` (TODO).

| Threat | Mitigation |
|--------|------------|
| Wire tampering (header) | Ed25519 signature covers all immutable fields (§6.3) + AEAD AAD includes them (§7) |
| Wire tampering (body) | If `IS_ENCRYPTED`, AEAD tag verifies body. Otherwise integrity comes from sender's envelope signature. |
| Replay | message_id LRU dedup + 30s clock skew window (§9) |
| Hop tampering (auth) | Source-to-destination Ed25519 signature covers fields 0–15 (everything except `forwarded_via`). Hop CANNOT impersonate, modify body, or alter routing metadata without invalidating signature. |
| Hop tampering (`forwarded_via`) | `forwarded_via` is unauthenticated diagnostic metadata (§5.5). Hop CAN forge, reorder, or truncate the chain. Receivers MUST NOT use `forwarded_via` for authentication, ACL, or audit decisions — only for human-debug. Cryptographic hop attestation deferred to UFP/3. |
| Downgrade attack | Single live protocol version (`protocol_version = 2`). Receivers reject any other version with `UnknownProtocolVersion`. No negotiation, no fallback. Rejected-version events MUST be logged (§11.1). |
| Key compromise | Revocation flow via `policy_epoch` increment + Control / KeyRevocation broadcast. Worst-case propagation ~3s on typical mesh. Receivers reject messages signed with `auth.epoch < cached_epoch - 1`. Stale-epoch receivers (no policy refresh > 5 min) treat traffic with elevated caution (§11.2). |
| Hop snooping | Mesh transport is TLS 1.3 (intra-cluster). For untrusted hops, `IS_ENCRYPTED` provides E2EE. |
| Impersonation | Ed25519 public-key auth (no shared secret to leak). Trust establishment via mesh pairing + sync identity registry. |
| Replay across cluster | `auth.epoch` tied to permission epoch; expired epoch rejected. |
| DoS via deeply nested CBOR | `MAX_NESTING_DEPTH=64` enforced by canonical validator. |
| DoS via huge payload | Per-channel max body size (TODO: enumerate in this doc); fragmentation has channel-specific max fragment count. |
| Side-channel via compression (CRIME/BREACH class) | Compress-before-encrypt does NOT hide compressibility from observers — ciphertext size still reveals compressed plaintext size. Mitigation policy per channel in §8.1: Control handshake messages with fresh secrets MUST set `IS_COMPRESSED=0`; application code on compressible channels MUST NOT mix secrets with attacker-influenced fields in the same body. Padding to fixed-size buckets deferred to UFP/3. |

## 15. Performance characteristics (target)

Measured against current CBOR baselines on a typical TentaFlow deployment (Linux x86_64, ~10 mesh peers, ~5 active addons):

| Operation | CBOR baseline | UFP/2 target | Notes |
|-----------|---------------|--------------|-------|
| Encode envelope (no body) | 0.5 µs | < 2 µs | CBOR canonical sort overhead |
| Decode envelope (header only) | 0.3 µs | < 1.5 µs | Skip body decoding for routing |
| Validate canonical | n/a (CBOR bytecheck) | < 3 µs | One pass over raw bytes |
| Mesh heartbeat round-trip (local) | 80 µs | < 100 µs | TLS+UFP/2 overhead minimal |
| Sync push batch (500 ops, 2.5MB CBOR) | CBOR: 2.5MB on wire | UFP/2 + lz4: ~600KB on wire | Net win |
| Frontend chat completion token | CBOR: 200B | UFP/2: ~300B | Negligible per-token, 50% larger but lz4 buys back if streamed in batches |
| Routing hop CPU (B forwards A→C) | CBOR: ~5 µs (decode+route+encode) | UFP/2: < 1 µs (header parse + bytes copy) | Major win — no body decode |

The "routing hop CPU" line is the architectural payoff: at 1000 mesh msgs/sec through a relay node, UFP/2 saves ~4 ms/sec of CPU. At 100k msgs/sec (large mesh), that's 40% of a core saved on routing alone.

## 16. Open questions / TODO

Closed in v0.8 (after seventh codex review):
- [x] Fragmented AAD `flags` ambiguity — `aad_flags = flags & !IS_LAST_FRAGMENT` (last-fragment bit masked off, all other bits identical across fragments).
- [x] Signed Session verification key ambiguity — `Session + IS_SIGNED=1` is FORBIDDEN in v2. Sessions are unsigned. NodeIdentity/UserIdentity for crypto-signed sender proof.

Closed in v0.7 (after sixth codex review):
- [x] **CRIT** §7.2 fragmented AAD includes unverifiable body — AAD now excludes field 9 `body` entirely for fragmented envelopes. AEAD tag covers logical body confidentiality+integrity; AAD covers header only.
- [x] **MAJOR** §11.3 ApiKey → UserIdentity mapping impossible — removed. ApiKey strictly limited to read/inference; mutations require real UserIdentity (out-of-band user-signed) or interactive Session.

Closed in v0.6 (after fifth codex review):
- [x] §9 / §10.2 unconditional sig verification — now conditional on `IS_SIGNED=1`; unsigned envelopes verify channel/kind policy + transport binding.
- [x] §6.1 Auth schema sub-fields — marked OPTIONAL with presence delegated to §11.3.
- [x] §7.2 fragmented AAD "body length" misstatement — corrected; AAD encodes logical body bytes directly.
- [x] §10.1 missing fragment range checks — explicit `fragment_count > 0` and `fragment_index < fragment_count` rejection rules.
- [x] §11.3 ApiKey + mutating Frontend kinds — clarified: gateway maps to UserIdentity for mutations.

Closed in v0.5 (after fourth codex review):
- [x] §3.2 schema `auth` field — explicitly MANDATORY (matches §11.3).
- [x] §6.1 ApiKey HMAC contradiction — removed; HMAC happens at edge gateway only.
- [x] §7.2 AAD for fragmented messages — split into non-fragmented AAD (fields 0–8+10–12) and logical-envelope AAD for fragmented (encryption pre-fragmentation).
- [x] §10.2 reassembly atomicity + write-once fragment slots — explicit mutex + slot semantics.
- [x] Status header version bumped to v0.5.
- [x] Key rotation cadence removed from open list (§7.2 definitive).

Closed in v0.4 (after third codex review):
- [x] Fragment dedup commit timing — explicit: dedup commits when fragment buffered, not after reassembly (§9 step 8).
- [x] §11.3 auth invariants — `auth` field always present; `auth.signature` strictly Ed25519; ApiKey uses transport HMAC outside UFP/2.
- [x] §11.3 table "Recommended" replaced with explicit `YES/NO/PER_KIND`.
- [x] Sender fragmentation pipeline — added §10.0 with encrypt-then-fragment ordering.
- [x] Per-channel auth requirements table (was open) — closed by §11.3.

Closed in v0.3 (after second codex review of v0.2):
- [x] Fragment signature contradiction — `auth.signature` differs per fragment; other auth fields identical (§10.1).
- [x] Replay dedup before signature verification — explicit step ordering, dedup only commits on successful processing (§9).
- [x] Revoked-key grace bypass — denylist check happens before epoch grace (§11.2 step A).
- [x] Compression threat row contradiction — §14 row now references §8.1 policy.
- [x] Per-channel auth requirements — added §11.3 with envelope-level invariants and per-channel table.
- [x] Nonce threshold inconsistency — unified to `2^48 OR 24h whichever first`.
- [x] Flag bit allocation ambiguity — explicit bits 0–6 allocated, 7–31 reserved (§3.4).
- [x] `IS_LAST_FRAGMENT` without `IS_FRAGMENT` — explicit rejection rule (§3.4).

Closed in v0.2 (after codex review):
- [x] Mutable flag inside signed `flags` — `IS_FORWARDED` removed; routing inferred from `len(forwarded_via) > 0`.
- [x] AAD chicken-and-egg with `auth.signature` — pipeline order pinned (§7.1); signature computed AFTER encryption.
- [x] Fragment vs replay conflict — `fragment_index`/`fragment_count` added; dedup key extended.
- [x] Compression side-channel misstatement — corrected to acknowledge CRIME/BREACH (§8.1).
- [x] `forwarded_via` trust ambiguity — explicitly labelled unauthenticated diagnostic metadata (§5.5).
- [x] Nonce policy too weak — deterministic counter construction + persistence (§7.2).
- [x] Downgrade attack policy — added §11.1.
- [x] Key revocation procedure — added §11.2.
- [x] `ExternalAPI` awkward channel — removed; OpenAI-compat at edge.
- [x] Mesh/Control boundary — clarified in §4.

Still open (deferred to v0.3 or implementation):
- [ ] Per-channel max body size limits (DoS hardening) — enumerate per channel in §4.
- [ ] Per-channel max fragment count limits — currently global 65535; document per-channel practical caps.
- [ ] Manifest publication: which `(channel, kind)` exposes the catalog v2 manifest? Proposal: `(Control 0x05, kind=0x0010 ManifestRequest / 0x0011 ManifestResponse)`.
- [ ] `IS_COMPRESSED_ZSTD` future flag — defer to UFP/3 unless concrete use case emerges.
- [ ] User identity migration: existing UUID-style user_id (16B) → 32-byte Ed25519 pubkey. Provisioning UX, key custody, recovery model?
- [ ] Anonymous channel boundary: only `Control / Hello` allowed pre-handshake, or also `Stream` for unauthenticated public endpoints?
- [ ] Browser ↔ Core: WebTransport datagrams (lossy, low-latency) vs WebTransport streams vs WebSocket — UFP/2 framing identical across all three?
- [ ] Canonical test vectors for envelope encoding, signature scope, AAD, AEAD nonce construction. (Required deliverable for multi-language implementations to verify against.)
- [ ] Error response rate limiting (`ProtocolError` envelopes shouldn't be amplifiable into DoS).
- [ ] Counter persistence implementation detail: per-key counter file vs in-process atomic vs sync ledger-backed?
- [ ] Re-validate compression policy decisions per channel after threat model deep-dive (§14).
- [ ] Codex review of v0.2 (this revision).

## 17. References

- RFC 8949 — Concise Binary Object Representation (CBOR)
- RFC 8032 — Edwards-Curve Digital Signature Algorithm (EdDSA)
- RFC 8439 — ChaCha20 and Poly1305 for IETF Protocols
- LZ4 Frame Format — `https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md`
- TentaFlow Faza 6 plan — `docs/ADDON_REWRITE_PHASE_6_PLAN.md` (local untracked)
- TentaFlow Addon Binary Protocol v1 (predecessor) — `docs/ADDON_BINARY_PROTOCOL_v1.md`
- TentaFlow Addon UI Component Catalog v1 — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md`
- TentaFlow Mesh Protocol v1 (predecessor) — `docs/MESH_PROTOCOL_v1.md`
- TentaFlow Sync Ledger Plan — `docs/SYNC_LEDGER_PLAN.md`
