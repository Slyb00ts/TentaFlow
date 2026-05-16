# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

No workspace Cargo.toml — each crate builds independently. The main binary is `tentaflow`.

```bash
# Build main binary (from tentaflow/)
cd tentaflow && cargo build

# Build core library (from tentaflow-core/)
cd tentaflow-core && cargo build

# Run
./tentaflow/target/release/tentaflow --config config.toml

# WASM addons require this target
rustup target add wasm32-wasip1

# Browser protocol glue (tentaflow-protocol-wasm) requires these two.
# Without them build.rs skips generating www/js/protocol/wasm_glue.{js,wasm}
# and the dashboard fails to load in the browser.
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.108 --locked

# Or one-shot: ./scripts/setup.sh (Linux + macOS)
```

Feature flags on `tentaflow-core`:

| Flag | Purpose |
|------|---------|
| `docker` | Docker management (bollard) |
| `inference-llamacpp` | llama.cpp backend |
| `inference-mlx` | Apple MLX (macOS only) |
| `dashboard-api` | Axum HTTP dashboard + API |

## Configuration

`config.toml` at project root. Key sections: `[server]`, `[server.mtls]`, `[protocols.quic]`, `[mesh]`, `[load_balancing]`, `[monitoring]`. Default ports: HTTPS/QUIC on 8090, Prometheus on 9090.

`[server.mtls]` (optional) — Service-to-Core mTLS pinning for `/core/frame/pickup`:

```toml
[server.mtls]
pickup_required = false              # default off (F1a/F1b compat)
client_cert_fingerprints = []        # SHA-256 hex of allowed client leaf certs
```

Production must flip `pickup_required = true` and list at least one fingerprint.

## Transport architecture (2-tier)

TentaFlow runs two transport tiers and every change must respect this split:

### Tier 1: Binary primary (default)

WebTransport `/wt/api` + WebSocket `/ws/api` fallback, binary `MessageBody` protocol.
- Frontend ↔ Core: all admin UI, all data fetching
- Addons ↔ Core (via wasmtime): host functions ABI via addon-sdk wrappers
- Services in mesh ↔ Core: QUIC tunnel (mesh control plane)
- Sub-second response, low overhead, full request/response binary serialization

### Tier 2: HTTP REST secondary

Reserved for external integrations that cannot use the binary protocol:

1. `POST /core/frame/pickup` — Service-to-Core for backend service integrations
   (yolo, whisper inference). Authentication: HMAC `X-Pickup-Token` (one-shot,
   30 s TTL). Production REQUIRES mTLS client cert pinning (`[server.mtls]`).
2. `GET /recordings/<ref>?token=&exp=&ref=` — Browser-friendly signed URL for
   addon-issued recording downloads (PNG snapshots, MP4 segments). HMAC,
   multi-use, 60–3600 s TTL.
3. `GET /frames/<ref>?token=&exp=&ref=` — Same pattern, frame_url for raw RGB24
   bytes from frame_storage LRU. HMAC, multi-use, 60–600 s TTL.

### Security boundary

Both tiers share:
- HMAC SHA-256 token verification (constant-time via `subtle::ConstantTimeEq`)
- Audit log per outcome (`audit_log` + `frame_pickup_log`)
- Rate limit per IP + global (token bucket, 429 + `Retry-After`)
- Path traversal containment (canonicalize + `base_dir.starts_with` check)
- Security response headers: `Cross-Origin-Resource-Policy: same-site`,
  `Referrer-Policy: no-referrer`, `Cache-Control: private, no-store`,
  `X-Content-Type-Options: nosniff`, `Strict-Transport-Security: max-age=63072000;
  includeSubDomains` (HSTS applied unconditionally to every response).

Production TLS profile (enforced in `api::unified_server`):
- TLS 1.3 only (legacy clients explicitly unsupported in F1b)
- AEAD cipher suites only (no CBC, no RC4 — implied by TLS 1.3 lockout)
- HSTS header on every response (200, 401, 403, 404, 429 — no exception)

### Cluster constraint (F1a/F1b single-node only)

HMAC signing keys (PickupToken + frame_url + recording_url + cameras AES-GCM)
and the pickup mTLS allowlist are process-local OR file-based per node.
Multi-node cluster requires P3 mesh key sync (deferred). In single-node
deployments this is acceptable. Multi-node deployments must wait for P3.

### Logging warning

NEVER enable hyper access logging (`RUST_LOG=hyper=debug`) in production without
a query-string scrubber. URLs `/recordings/<ref>?token=<hmac>` would log the
HMAC token wire in plain text via Hyper's request line.

### Default development command

```bash
cargo build --features camera,dashboard-api
```

This is the canonical surface for F1b feature work. `--features dashboard-api`
alone compiles the HTTP/dashboard stack but skips camera ingest; add `camera`
whenever touching `/frames`, `/recordings`, or the pickup tier.

### Production deploy checklist

- [ ] TLS 1.3 enforced (default since E2; do not weaken)
- [ ] HSTS header observed in all responses (verify with `curl -k -I https://.../`)
- [ ] `[server.mtls] pickup_required = true` with at least one fingerprint
- [ ] HMAC token soak test passed (no 429 storms, no token leakage in logs)
- [ ] `RUST_LOG` scoped to crate-level (no `hyper=debug`)

## Conventions

- Comments in code: English only
- Variable/function names: English
- Commit messages: English, format `[type]: description`
- Rust: `rustfmt` defaults, `snake_case` functions, `PascalCase` types
- JS/HTML/CSS: 2-space indent, `camelCase` JS, `kebab-case` CSS
- C#: 4-space indent, `PascalCase` public, `_camelCase` private fields

## Code quality rules (MANDATORY — apply to every change)

These rules apply to humans AND to every AI agent working on this repo. No exceptions unless the user explicitly overrides a specific rule for a specific task.

### 1. No stubs, placeholders, or TODOs
- Every commit must be production-ready. If you cannot finish a feature in this pass, do not ship a partial implementation that pretends to work.
- Forbidden: `todo!()`, `unimplemented!()`, `// TODO: implement`, empty function bodies that return defaults, mock responses, "we'll wire this up later" scaffolding.
- If a dependency is missing, say so and stop. Do not fake it.

### 2. No backward-compatibility shims, no fallbacks
- When you change a function, change it in place. Do not keep the old version around "just in case".
- No alias exports, no deprecated wrappers, no feature flags for old behavior, no `if let Some(old) = ... else { new_path }` fallback chains.
- Exception: only when the user explicitly asks for compat (rare — assume never).

### 3. No versioned function names
- Forbidden: `process_request_v2`, `do_thing_new`, `calculate_ultrafast`, `handle_event_improved`, `user_check_permission_fixed`.
- If you are improving an existing function, **edit it in place**. The git history is the version record; the code should have one name per concept.
- If the signature change breaks callers, update the callers. That is the work.

### 4. Check for existing functions before writing new ones
- Before adding a new function, search the crate (or the relevant module) for something that already does this. Use Grep/ripgrep on likely names, likely signatures, and likely call sites.
- If a similar function exists and almost fits, extend it (new parameter, new enum variant) rather than forking a parallel one.
- This applies to Rust, JS, CSS, DB helpers — everywhere.

### 5. Delete unused code as you go
- When a refactor removes the last caller of a function, delete the function in the same commit. Do not leave dead code "in case we need it".
- Same for unused imports, unused struct fields, unused CSS classes, unused i18n keys, unused SQL helpers.
- `cargo check` warnings about unused items are bugs, not noise.

### 6. Comments describe WHY, not WHAT
- English only.
- File headers stay: `// ============ File: <name> — <1-sentence purpose> ============`.
- Inline comments only when the code's intent is not obvious from names — e.g. a workaround for a known bug, a non-obvious invariant, a performance trick. Do not narrate what the next line does.
- Forbidden: meta-comments like `// CRITICAL:`, `// OPT-001`, `// Fixed in this PR`, `// Changed from X to Y`, `// OWASP-xxx`. Git blame carries history; comments carry intent.

### 8. Always use project web components — never roll your own UI primitive

Project components live under `tentaflow-core/www/js/components/` — currently: `tf-button`, `tf-chip`, `tf-input`, `tf-menu`, `tf-searchbox`, `tf-select`, `tf-table`, `tf-tabs`, `tf-toggle`, `tf-window`.

**Rules:**
- For every UI primitive (button, input, select, toggle, chip, tabs, window/modal, searchbox, menu, table) use the `tf-*` component. Zero `<button>`, `<input>`, `<select>`, hand-rolled `.tabs-bar`, hand-rolled modal overlays in feature modules. The only permitted raw `<input>` is `type="file"` (no tf-file-input exists yet).
- If a `tf-*` component is missing a feature you need (animation, slot, event, variant, prop) — **extend the component**, don't build a one-off. Add the prop to the component's API, update its CSS, bump the demo if one exists.
- If a pattern is repeated in feature code (e.g. an oauth-mode radio card pattern, or a permission matrix cell), consider adding a new `tf-*` component. Add it when the pattern appears in 2+ places OR the feature module exceeds ~30 lines of markup for the same element.
- If a component's existing behavior is broken (no animation, wrong focus ring, missing keyboard handler), fix the component rather than working around it in the feature module.
- Code review rejects any diff that renders a custom tab strip, custom toggle, custom select dropdown, custom modal, etc., when a `tf-*` component exists. "Slight visual difference" is not justification — change the component's CSS variant.

**Why:** one-off UI primitives drift in look, accessibility, animation timing, and keyboard behavior. Users notice inconsistency. Components centralize the fixes.

## gstack

For all web browsing, use the `/browse` skill from gstack. Never use `mcp__claude-in-chrome__*` tools.

Available gstack skills:

| Skill | Purpose |
|-------|---------|
| `/browse` | Headless browser for web browsing, QA testing, screenshots |
| `/connect-chrome` | Launch real Chrome controlled by gstack with Side Panel |
| `/qa` | Systematic QA testing + fix bugs found |
| `/qa-only` | QA testing report only (no fixes) |
| `/design-review` | Visual QA: find and fix spacing, hierarchy, AI slop issues |
| `/design-consultation` | Product design system creation |
| `/design-shotgun` | Generate multiple design variants for comparison |
| `/review` | Pre-landing PR review |
| `/ship` | Ship workflow: tests, review, changelog, PR |
| `/land-and-deploy` | Merge PR, wait for CI, verify production |
| `/canary` | Post-deploy canary monitoring |
| `/benchmark` | Performance regression detection |
| `/investigate` | Systematic debugging with root cause analysis |
| `/office-hours` | YC-style forcing questions for startups/builders |
| `/plan-ceo-review` | CEO/founder-mode plan review |
| `/plan-eng-review` | Eng manager plan review |
| `/plan-design-review` | Designer's eye plan review |
| `/autoplan` | Auto-review pipeline (CEO + design + eng) |
| `/retro` | Weekly engineering retrospective |
| `/document-release` | Post-ship documentation update |
| `/codex` | OpenAI Codex CLI: review, challenge, consult |
| `/cso` | Chief Security Officer audit |
| `/setup-browser-cookies` | Import browser cookies for authenticated testing |
| `/setup-deploy` | Configure deployment settings |
| `/careful` | Safety guardrails for destructive commands |
| `/freeze` | Restrict edits to a specific directory |
| `/unfreeze` | Clear freeze boundary |
| `/guard` | Full safety: careful + freeze combined |
| `/gstack-upgrade` | Upgrade gstack to latest version |

## Skill routing

When the user's request matches an available skill, ALWAYS invoke it using the Skill
tool as your FIRST action. Do NOT answer directly, do NOT use other tools first.
The skill has specialized workflows that produce better results than ad-hoc answers.

Key routing rules:
- Product ideas, "is this worth building", brainstorming → invoke office-hours
- Bugs, errors, "why is this broken", 500 errors → invoke investigate
- Ship, deploy, push, create PR → invoke ship
- QA, test the site, find bugs → invoke qa
- Code review, check my diff → invoke review
- Update docs after shipping → invoke document-release
- Weekly retro → invoke retro
- Design system, brand → invoke design-consultation
- Visual audit, design polish → invoke design-review
- Architecture review → invoke plan-eng-review
- Save progress, checkpoint, resume → invoke checkpoint
- Code quality, health check → invoke health
