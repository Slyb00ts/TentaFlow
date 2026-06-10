# Features to Preserve — from current GUI to new UI

**Source of truth:** mockupy v3.1 w `index.html` ustalają kolorystykę, układy, nawigację, komponenty. **Ten dokument to checklist details** których w mockupach nie zdążyłem wyrysować, a które MUSZĄ znaleźć się w nowym UI po refaktorze.

**Zasada:** estetyka/layout/flow = z mockupów. Szczegóły z tej listy dopisać w implementacji. Nic nie znika.

**Generated:** 2026-04-17 · plan-design-review session
**Applies to:** design doc `critix-main-design-20260417-113355.md`

---

## 1. Node Details view (Mesh → {node})

**Reference image:** `node-detail-reference.png` (screenshot z obecnego GUI).

Mockup #6 (Mesh Admin) pokazuje listę nodów. Kliknięcie na node otwiera **Node Details drill-down view** który musi pokazywać:

### 1.1 Header bar
- `← Back to Mesh` link
- Node icon + name (np. `mainpc (local)`)
- Status chip (`connected` / `disconnected`)
- `Add service` button (primary action)
- System info row (under name): OS/distro · container runtime version · GPU model
  - Przykład: `Linux (CachyOS Linux rolling) · Docker 29.4.0 · NVIDIA GeForce RTX 4090`

### 1.2 VRAM top-level bar
- Full-width progress bar
- Label + values: `5.9 GB / 24.0 GB (25%)` format
- Color-coded: green <70%, amber 70-90%, red >90%

### 1.3 Two-column: CPU + Memory
- **CPU box:** percentage bar + temperature (`62°C OK` / `74°C WARN` / `82°C CRIT`)
- **Memory box:** RAM bar (`34.7 GB / 62.4 GB`) + SWAP bar (`18.7 GB / 62.4 GB`)

### 1.4 GPU section (per GPU jeśli multi-GPU)
- `GPU 0: NVIDIA GeForce RTX 4090` header
- GPU usage percentage bar
- VRAM bar (per-GPU, separate od top-level aggregate)
- Temperature indicator (`46°C OK`)
- Power draw (`64W / 450W`)
- If multi-GPU: GPU 1, GPU 2 sections below

### 1.5 Network interfaces section
**Lista wszystkich interfejsów** (eth/wifi/bridge/vpn/thunderbolt):
- Status dot (zielony=up, szary=down)
- Interface name (`br0`, `eno1`, `enp1s0np0`, `thunderbolt0`, `wlan0`)
- Link speed badge: `20G` / `10G` / `100G` / `no link` (color-coded indigo/blue)
- IP address (lub "no link")
- Bandwidth live: `↓ 57.6 KB/s ↑ 28.7 KB/s`
- Special badges po prawej: `PCIE`, `TBS` (thunderbolt), `WIFI` itp.
- Gear icon na końcu wiersza (konfig interfejsu)

### 1.6 Containers section (Docker/Podman integration)
**Tabela kontenerów runnujących na tym node:**
- Kolumny: NAME | IMAGE | STATUS | CPU% | RAM | ACTIONS
- Status color-coded:
  - `Up 10 hours` → zielony chip
  - `Exited (255) 10h ago` → amber/yellow chip
  - `Created` → grey chip
  - `Restarting` → amber
- Actions per row (zależne od stanu):
  - Up: `Stop` / `Restart` / `Logs`
  - Exited/Created: `Start` / `Logs` / `Remove`
- Obsługuje: ghcr.io images, nvcr.io (NIM), portainer_agent, buildx_buildkit

**Current code:** `tentaflow-core/src/api/dashboard/api_portainer.rs` + `wwwroot/js/modules/mesh/MeshNodeDetail.js`

### 1.7 Services dla node (za "Add service" button)
- Modal z listy serwisów sparowanych z tym node
- CRUD services tied do this node
- Current code: `api_services.rs`, `ServiceForm.js`

---

## 2. Addons (WASM) detail panel — 4 tabs

Mockup #21 (Addons) pokazuje **grid kart**. Kliknięcie na kartę otwiera **detail panel** z 4 zakładkami:

### 2.1 Tab: Settings
- **Config schema-driven form** — addon manifest definiuje JSON Schema dla settings
- Form fields dynamic (text / number / select / toggle / textarea / secret)
- Preview prompt templates (if addon uses prompts)
- Save button + reset to defaults
- Per-setting help text (z manifest `description`)

### 2.2 Tab: Logs
- Live log stream (WebSocket follow-tail)
- Severity filter (info / warn / error)
- Search/filter box
- Timestamp + message per row
- Download logs button
- Clear logs button

### 2.3 Tab: Permissions (CRITICAL - rozbudowana)
**Granularne uprawnienia per addon.** Obecnie w kodzie:
- **Kategorie uprawnień:**
  - `network.http` — który whitelist domains
  - `network.tcp` — host:port whitelist
  - `file.read` — path patterns
  - `file.write` — path patterns
  - `db.query` — tabele/queries
  - `event.publish` — event types allowed to emit
  - `event.subscribe` — event types to listen
  - `host.*` — calls do host functions
  - `llm.call` — dostęp do LLM routing
  - `mesh.query` — dostęp do mesh topology
- **Per-permission:**
  - Toggle allow/deny
  - Per-user override (user X zezwala, user Y nie)
  - Per-group override
  - Rate limit per permission (calls/minute)
  - Audit: kiedy ostatnio użyta, ile razy w 24h
- **UI:** hierarchical tree (category → sub-permission → scope), expand/collapse, search box, bulk allow/deny per category
- **Audit trail:** ostatnie 50 permission-check events (who, when, granted/denied, reason)
- **Dry-run mode:** włącz permissions ale loguj tylko (nie enforce) — dla debug

**Current code:** `tentaflow-core/src/addon/permissions.rs` + `wwwroot/js/modules/addons/Addons.js` (~1304 linii — dużo)

### 2.4 Tab: Tools
- **Exposed tool list** (function calls które addon udostępnia)
- Per-tool: nazwa, sygnatura (params), opis, schema JSON
- Test-invoke UI: form żeby wywołać tool manualnie z parametrami
- Invoke history: ostatnie calls + response time + status
- Rate limiting config per tool
- Integration points: które Flow Builder nodes, które prompts, które apps używają danego tool'a

### 2.5 Install/Update flow (nie w tabs ale ważne)
- **Install ZIP:** file picker, upload, validation (manifest.toml check), permission review screen przed instalacją
- **Update:** sprawdź nową wersję, show changelog/diff, update z preservation settings
- **Uninstall:** confirm modal z warning ("will delete addon data: X MB, Y files")
- **Export/Backup:** download addon + settings jako ZIP

---

## 3. Flow Builder — node config panel + run history

Mockup #17 pokazuje canvas + palette. Brakuje:

### 3.1 Node config panel (sidebar right, po kliknięciu noda)
- Per-node typ-specific form:
  - **LLM node:** model select (alias), prompt template picker, temperature, max_tokens, system message, variables binding (input → prompt vars)
  - **STT node:** audio input source, language, diarization toggle, real-time vs batch
  - **TTS node:** voice profile, speed, language, audio output destination
  - **Image Gen node:** model, size, style, count, seed
  - **Condition node:** expression editor (JS/Lua-like), true/false branch wiring preview
  - **Filter node:** regex pattern, keep/drop logic
  - **Transform node:** JSON path / jq-like expression, output schema
  - **Webhook trigger:** URL/path, method, auth (none/token/hmac), expected payload schema
  - **Schedule trigger:** cron expression + timezone, next-run preview
- **Input/Output ports:** drag-to-connect, type-checking (text/audio/image/json)
- **Validation indicators:** red outline gdy config incomplete

### 3.2 Run history + debug
- Tab "Runs" per-flow: lista ostatnich executions (time, trigger, duration, status, result size)
- Drill-down per run: step-by-step trace (input → output per node), timing per step
- Re-run with same input button
- Failed runs highlighted, error message per step

### 3.3 Flow list view (przed builder)
- Grid cards: Name, description, last run, status, enabled toggle
- Categories / tags
- Import/Export flow JSON
- Templates library (pre-built flows: email-summary, meeting-notes, data-sync)

**Current code:** `FlowList.js`, `FlowBuilder.js`, `FlowCanvas.js`, `FlowNodePalette.js`, `FlowNodeConfig.js`

---

## 4. Cluster Wizard — 3 kroki

Mockup #14 (Clusters) pokazuje karty + "Nowy cluster" button. **Button otwiera 3-step wizard:**

### 4.1 Step 1: Basic info
- Cluster name
- Description (opcjonalny)
- Icon/color picker (dla wizualnego rozróżnienia)

### 4.2 Step 2: Node selection
- Lista paired mesh nodes + offline nodes
- Multi-select z checkboxami
- Filter by: GPU only / low-latency (LAN) / same tailnet
- Validation: minimum 1 node
- Per-selected node: rola w clusterze (primary/replica/backup)

### 4.3 Step 3: Strategy + routing
- Strategy select: RoundRobin / LeastLoaded / FirstAvailable / LocalityAware
- Failover within cluster: toggle + "spill-to" target (inny cluster)
- Resource budgeting: max VRAM/RAM per request
- Review summary + Create button

### 4.4 Cluster detail view (po kliknięciu na kartę)
- Topology diagram (jak mesh admin diagram ale ograniczony do clusterowanych nodów)
- Per-node metryki agregowane (avg latency, total VRAM, active requests across cluster)
- Timeline aktywności (request volume over time)
- Edit cluster button

**Current code:** `Clusters.js`, `ClusterWizard.js` (~1174 linii total)

---

## 5. Models — edit form + HuggingFace Hub search

Mockup #15 pokazuje grid kart. Brakuje:

### 5.1 Add/Edit model form
- Pola:
  - ID (unique, snake_case)
  - Display name
  - Source: HuggingFace / Ollama / URL / Local path
  - HF repo ID (dla HF source) z auto-complete
  - Quantization: FP16 / FP32 / Q4_K_M / Q5_K_M / INT8 / INT4
  - Context length (tokens)
  - Model type: LLM / STT / TTS / Image / Embedding / Reranker / Vision
  - Backend engine: llama.cpp / vLLM / MLX / Whisper / XTTS / SD / ComfyUI / auto
  - Target node (które node pulls/runs model)
  - Disk quota (GB)
  - Auto-download on save (toggle)
  - License accepted (toggle, required dla gated HF models)
- Validation: GPU memory estimate vs target node VRAM
- Preview: projected download size + estimated pull time

### 5.2 HF Hub search modal
- Search box z filters: task type (text-generation / ASR / TTS / text-to-image / feature-extraction)
- Filter: quantized only, size <10GB, trending
- Results list z metadata: downloads count, likes, license, size estimate
- Click result → prefill add form z repo ID + metadata

**Current code:** `Models.js`, `ModelForm.js` + `api_hub.rs`

---

## 6. Prompts — editor z variables + test

Mockup #16 pokazuje cards. Brakuje **edit screen**:

### 6.1 Prompt editor view
- Text area z syntax highlighting dla `{{variable}}` placeholders
- **Auto-detected variables list** z typami (string/number/array/file)
- **Test playground:** fill variables → click "Test" → wywołanie LLM z wypełnionym promptem → response preview
- **Versioning:** każdy save = nowa wersja, historia z diff view
- **Usage analytics:** ile razy użyty, avg response length, avg tokens, success rate
- **Tags/categories:** system / user / shared / per-flow
- **Visibility scope:** private / team / global
- **Associated model:** default model do testów
- **Max tokens cap** (override globalny limit dla tego prompta)
- **Temperature default**

**Current code:** `Prompts.js`, `PromptEditor.js`

---

## 7. Rules — 3 tabs deep

Mockup #18 pokazuje tabelę PII. **Pozostałe 2 tabs mają swoje specyfiki:**

### 7.1 Tab: Fast-Path Patterns
- Wzorce gdzie BYPASS LLM (szybka odpowiedź z template)
- Per-pattern:
  - Regex match (input)
  - Response template (z variables z match groups)
  - Fallback to LLM if confidence < X
  - Priority (order)
  - Test: input → preview który pattern match + generated response
- Przykłady built-in: greetings, time/date queries, unit conversions

### 7.2 Tab: TTS Cleaning Rules
- Pre-synthesis text cleanup (przed TTS)
- Per-rule:
  - Regex find
  - Replace template
  - Apply only for language X (optional)
  - Priority
- Przykłady:
  - Remove markdown (`**bold**` → `bold`)
  - Expand abbreviations (`PLN` → `złotych`)
  - Number formatting for speech (`25` → `dwadzieścia pięć`)
  - URL/email omission (skip or replace with "link")

**Current code:** `PiiRules.js`, `FastPathPatterns.js`, `TtsCleaningRules.js`

---

## 8. Service Catalog — Engine Deploy Wizard

Mockup #20 pokazuje katalog. **"Deploy" button otwiera 3-step wizard:**

### 8.1 Step 1: Deploy method selection
- Options (zależnie od manifest):
  - **Docker** (jeśli manifest ma `[deploy.docker]`)
  - **Native** (jeśli manifest ma `[deploy.native]` — embedded/binary/python-bundle)
  - **External** (jeśli manifest ma `[deploy.external]` — detect daemon w PATH)
- Per-option: jedno-zdaniowy opis, platform compatibility badge (Linux/macOS/Win/iOS/Android)

### 8.2 Step 2: Model selection
- **Preset model** (z manifest's `[[model_preset]]` array) jako radio
- **Custom HuggingFace search** (input + token field + suggestions)
- Per-model: size, quantization tag, recommended marker

### 8.3 Step 3: Runtime config
- Port (default z manifest `default_port`)
- Container name (auto-generated, editable)
- Target node (który node pulls + runs)
- Resource limits: max VRAM, max RAM
- Env vars (key-value, opcjonalne)
- Review summary + Deploy button

### 8.4 Deploy progress (after submit)
- WebSocket stream progressing through: pull image / download model / start container / wait for health / verify endpoint
- Per-stage: progress bar + log tail
- Cancel button (revocable)
- Final: success toast → redirect do Services list

**Current code:** `EngineDeployWizard.js` + `ws_deploy.rs`

### 8.5 NIM tab specific
- Gridem kart NVIDIA NIM containers (z nvcr.io)
- Per-card: nvcr.io image path, model inside, license note (NVIDIA AI Enterprise)
- Deploy działa tak samo (z NGC credentials z Registries)

---

## 9. Meeting Bot app — extras (user-facing)

Mockup #25 pokazuje podstawy. Brakuje:

### 9.1 Session setup (przed start recording)
- Select meeting platform: Teams / Zoom / Meet / LAN audio capture
- Meeting URL input
- Language select (pl-PL / en-US / multi)
- Diarization: on/off (default on)
- Voice profile enrollment prompt: "Mówisz z uczestnikami którzy nie są wprowadzeni? Enroll ich teraz albo skip"
- Recording quality: standard (16kHz) / high (48kHz)
- Storage location: local only / backup to mesh

### 9.2 Speaker enrollment flow
- Per-user voice profile creation
- Record 30s sample ("przeczytaj ten tekst...")
- Auto-identify w subsequent meetings
- Mapping speaker → user account (linked z Users management)
- Re-train / delete profile options
- **Current code:** `api_voice_profiles.rs` (feature-gated)

### 9.3 Post-meeting actions
- Export transcript: plain text / Markdown / Notion / Google Doc
- Send action items as:
  - Email per assignee
  - Calendar events
  - Jira/Linear tickets (if integration configured)
- Share transcript z uczestnikami (secure link)
- Archive: keep 30d / 90d / forever

### 9.4 Bot mode vs passive mode
- **Bot mode:** TentaFlow joins meeting jako participant (Teams guest account), may speak back (TTS)
- **Passive mode:** TentaFlow tylko słucha przez system audio capture, nie dołącza jako bot (anti-detection)
- Switchable per session

**Current code:** `MeetingBot.js` + `tentaflow-containers/teams-bot/` + prior design doc cluster-wizard-probe (audio pipeline)

---

## 10. Dashboard — metryki + sparklines

Mockup #1 (Admin Home) pokazuje hero + 3 cards. Obecny Dashboard.js (361 linii) ma więcej:

### 10.1 Metryki real-time
- Tokens per second (live graph, last 60s rolling)
- Active requests count
- Latency p50/p95/p99 (sparkline)
- Error rate %
- Mesh health score (0-100, aggregated)
- Storage used / free per node

### 10.2 Sparkline components
- Inline mini-charts w stat cards
- Czas: last 1h / 6h / 24h / 7d selector
- Hover: tooltip z exact value + timestamp
- **Current code:** `SparklineChart.js` (mesh module) — reuse dla dashboard

### 10.3 Service status grid
- Lista wszystkich services z live status dot
- Latency live per service
- Click → jump do Services detail

### 10.4 Recent events feed
- Ostatnie 10 eventów (audit log subset)
- Severity color-coded
- Click → Audit Log z filter pre-applied

### 10.5 Active flows widget
- Runnujące flows z progress
- Trigger source, duration, next step
- Click → Flow Builder run detail

---

## 11. Settings — Speaker Profile config

Mockup #24 pokazuje 6 tabs. **Tab "Speaker Profile" szczegóły:**

- Lista enrolled voice profiles (per-user)
- Enroll new: wybierz user account → record 30s sample
- Re-train: add more samples (improve accuracy over time)
- Quality indicator: sample count, confidence score
- Integration z Meeting Bot + Notes app
- Privacy note: voice embeddings stored locally, never leave node (opcjonalny mesh sync)

---

## 12. API Keys — extras

Mockup #8 pokazuje tabelę. Brakuje:

### 12.1 Per-key configuration (edit modal)
- Nazwa (editable)
- Scopes (checkbox list z 6 scopes)
- Rate limit per key (override default)
- IP allowlist (optional — restrict do konkretnych CIDR)
- Expiration date (optional — auto-revoke)
- Description (metadata)
- Owner: admin który stworzył klucz
- Last IP + user-agent (dla audit)

### 12.2 Usage analytics per key
- Requests count last 24h / 7d / 30d
- Token consumption (dla LLM scope)
- Most-used endpoints
- Error rate
- Suspicious activity detection (nagły spike)

### 12.3 Revoke flow
- Confirmation modal z warning: "X aktywnych klientów zostanie odłączonych"
- Option: soft revoke (still works 24h grace period) vs hard revoke (immediate)
- Revoke reason (audit log field)

---

## 13. Users — groups management

Mockup #9 (Users) pokazuje tabelę. Brakuje **Groups sub-view:**

### 13.1 Groups management
- Lista predefinied groups: `guests` / `users` / `power-users` / `admins`
- Dodawanie custom groups (np. `hr-team`, `developers`)
- Per-group: name, description, member count, permissions summary
- **Per-group permissions:**
  - Apps dostępne (matrix z Applications admin)
  - Mesh trust level (read-only / full)
  - API rate limit (multiplier vs base)
  - Data retention (30d / 90d / unlimited)

### 13.2 User invite flow
- Email invite link
- Set initial group, temporary password
- First-login: change password, enroll voice profile (opcjonalne), accept terms
- Invite link expiration (24h / 7d)

### 13.3 User detail panel (drill-down)
- Login history (time, IP, location, success/fail)
- Active sessions (devices, tokens, revoke per-device)
- API keys owned (link do API Keys filter)
- Permission audit (kiedy co zostało przyznane/zabrane)
- Deactivate (soft delete, preserves audit trail)

**Current code:** `Users.js` (~590 linii + Groups wewnątrz)

---

## 14. Audit Log — extras

Mockup #23 pokazuje tabelę. Brakuje:

### 14.1 Filter builder
- Date range picker (presets: last hour / 24h / 7d / 30d / custom)
- Severity multi-select
- Actor filter (user search z autocomplete)
- Event type filter (CRUD on: services / keys / nodes / flows / users / addons)
- IP filter
- Full-text search across detail field

### 14.2 Event detail modal
- Click row → modal z pełnym JSON payload (before/after diff dla zmian)
- Related events (same correlation_id, same session)
- Export as JSON / CSV

### 14.3 Real-time stream
- WebSocket follow-tail mode (toggle "Live" button)
- Nowe eventy highlight z fade animation
- Pause / resume

### 14.4 Retention + compliance
- Per-severity retention (info 30d, warn 90d, err forever)
- Auto-export snapshot (daily JSON do archive folder)
- Tamper-evident: każdy record z hash chain previous

---

## 15. Chat — playground extras (admin view)

Mockup #3 (Chat) pokazuje user-facing chat. **Admin ma dodatkowy "Playground" access:**

### 15.1 Playground mode (side-by-side)
- Split view: 2 LLM-y równolegle (porównaj responses)
- Różne prompts / różne models
- Rate response quality (thumbs up/down) dla evals
- Export comparison do prompt library

### 15.2 Admin controls w chat head
- Per-message: show token count, latency, which backend served, which fallback (if any) triggered
- Cost estimate (tokens × price per token)
- Raw JSON request/response toggle (dev debug)
- Force specific backend (override alias resolution) dla testów

### 15.3 Conversation export
- Export chat: Markdown / JSON / share link
- Include/exclude system prompts, metadata, timings
- Per-conversation: clone as flow (create Flow Builder flow from chat history)

---

## 16. Tailscale — status detail

Mockup #10 pokazuje toggle + status. Brakuje:

### 16.1 Tailnet connected peers list
- Pełna lista peerów w tailnet (nie tylko TentaFlow nodes — wszystkie)
- Per-peer: hostname, tailscale IP, OS, version, online status, RTT
- Z którymi TentaFlow mesh paired, z którymi nie
- Action: "Paruj z tym peerem" (dla niezalinkowanych)

### 16.2 Connection troubleshooting
- Check tailscaled daemon status (systemctl/launchctl)
- Login state (logged in / needs login URL)
- Derp relay info (który DERP server, RTT)
- MagicDNS status
- Subnet routes (jeśli TentaFlow ogłasza)

### 16.3 Exit node configuration
- Toggle "Use this node as exit node for TentaFlow mesh"
- ACL preview: które operations routowane przez exit node
- Impact warning (latency, legal/compliance)

---

## 17. Services — global settings (trzecia zakładka "Load Balancing")

Mockup #13 ma 3 taby, trzeci jest pusty w mockupie. Zawartość:

### 17.1 Global load balancing defaults
- Default strategy (FirstAvailable / RoundRobin / LeastLoaded)
- Circuit breaker: errors threshold + cooldown period
- Retry policy: max retries, backoff strategy (linear / exponential)
- Timeout per request (ms)

### 17.2 Health check config
- Check interval per service type (LLM: 60s, STT: 120s, itd)
- Health endpoint path override
- Consecutive failures threshold przed mark as down
- Grace period po restart

### 17.3 Request queuing
- Queue depth per service (max pending requests)
- Rejection policy gdy full: 503 / fallback-to-alternative / wait
- Priority lanes: admin/user/guest

**Current code:** routing config w `tentaflow-core/src/routing/`

---

## 18. Common patterns across screens (MUST preserve)

### 18.1 i18n everywhere
- Every label, button, placeholder, error message używa `I18n.t('key')`
- `wwwroot/i18n/pl.json` + `en.json` (28 top-level sections)
- Language switcher (top-right, zgodnie z screenshot: "EN PL")
- Auto-detect browser language + user preference override
- New user-facing strings ZAWSZE z i18n key dodane pre-implementation

### 18.2 Connected/Disconnected badge (top-right)
- Widoczny na każdym ekranie (admin i user)
- `● CONNECTED` (zielony) = WebSocket alive
- `● DISCONNECTED` (czerwony) = reconnecting
- Click → dropdown z diagnostics (latency, last heartbeat)
- Zgodnie ze screenshotem #4

### 18.3 User/role indicator (top-right)
- `admin` badge dla admin (warning color — pomarańczowy)
- `user` badge dla zwykłego (info color — niebieski)
- Click → profile menu (Settings / Logout)
- Zgodnie ze screenshotem #4

### 18.4 Toast notifications
- Success / warning / error toast (top-right, auto-dismiss 4s)
- Pozycja stacked gdy multiple
- Click toast → expand details
- Undo action for destructive (revoke user, delete key) — 10s window

### 18.5 Loading states
- Skeleton screens dla lists/tables (nie spinner)
- Shimmer animation dla cards
- Progress indicators dla long operations (deploy, model pull)
- Empty states z warmth + primary CTA

### 18.6 Keyboard shortcuts
- Global: `/` — focus search (gdzie dostępny)
- Admin: `G D` — Dashboard, `G M` — Mesh, `G S` — Settings (vim-style gg)
- Forms: `Ctrl/Cmd + Enter` — submit
- Modals: `Esc` — close
- Toasts: `z` — undo last

### 18.7 Dark theme first, light theme planned
- Wszystkie mockupy są dark-first
- CSS variables (`--color-bg-primary` itp.) przygotowane dla light theme switch
- User preference + system preference auto-detect
- Per-user persistence w user profile

### 18.8 Responsive breakpoints
- **Desktop ≥1024px:** pełny sidebar + main area (jak mockupy)
- **Tablet 768-1024px:** sidebar collapsible (hamburger), main full-width
- **Mobile <768px:** większość ekranów admin = "Use desktop" message (devtools, flow builder); user apps (Chat, Obrazy, Meeting) full responsive
- Touch-friendly tap targets ≥44px

### 18.9 A11y mandatory
- ARIA landmarks (`main`, `nav`, `aside`, `complementary`)
- Focus visible outline (2px accent color)
- Keyboard navigation wszystkich interactive elementów
- Screen reader announcements dla toasts, modals, loading states
- Color contrast 4.5:1 minimum dla text
- Reduced motion support (prefers-reduced-motion media query)

### 18.10 Permission-aware UI rendering
- Items do których user nie ma dostępu są HIDDEN (nie tylko disabled)
- Admin-only sections (Rules, Registries, Addons) — user nie widzi w nav
- Applications matrix (#22) decyduje które user apps pokazywać per user group
- Fallback screen dla attempted access: "Brak uprawnień · skontaktuj się z adminem"

---

## 19. WebSocket binary protocol integration

### 19.1 Connection lifecycle per screen
- Screen mount → ensure WSS connected (reuse global connection)
- Screen unmount → cancel pending subscriptions (avoid memory leaks)
- Reconnect auto-retry (exponential backoff) + user notification
- Message correlation_id dla request/response matching
- Stream subscriptions (logs, metrics, audit events) via IS_STREAM_CHUNK flag

### 19.2 Offline handling per screen
- Cached data shown z "last updated X min ago" indicator
- Mutations queued while offline, replayed gdy reconnect
- Optimistic UI updates z rollback on conflict
- Clear "you're offline" banner (nie blocking, tylko informujący)

---

## 20. Migration path — existing features to new UI

Per obecny view w `app.js` (16 registered views) → new placement:

| Obecne view | Nowe miejsce w mockupach |
|---|---|
| `dashboard` | Mockup #1 (Admin Home) + `~/.gstack/dashboard-extras.md` dla sparklines/events |
| `services` | Mockup #13 Tab 1 + detail drill-down (screen 1 node view) |
| `apikeys` | Mockup #8 + edit modal extras z §12 |
| `settings` | Mockup #24 z 6 tabs (+ Speaker Profile tab §11) |
| `mesh` | Mockup #6 + Node Details (§1) |
| `clusters` | Mockup #14 + ClusterWizard §4 |
| `prompts` | Mockup #16 + editor §6 |
| `models` | Mockup #15 + form §5 |
| `rules` | Mockup #18 z 3 tabs (§7) |
| `registries` | Mockup #19 |
| `flows` | Mockup #17 + node config §3 |
| `chat` | Mockup #3 (user) + playground admin §15 |
| `addons` | Mockup #21 + 4 tabs detail panel §2 |
| `meeting` | Mockup #25 (user app!) + enrollment §9 |
| `users` | Mockup #9 + groups §13 |
| `audit` | Mockup #23 + filters §14 |

**Plus new, nie-istniejące w obecnym:**
- Applications admin (#22) — NEW feature, matrix per group
- Tailscale (#10) — NEW feature, mesh transport option
- Error screen (#11) — NEW, WASM load failure
- Devtools Trace (#12) — NEW, killer feature
- User Apps Home (#2) — NEW landing dla user role
- Image Gen app (#4), Notes app (#5) — NEW user-facing apps

---

## 21. Quality gates checklist (merge gate additions)

Merge gate criteria z design doc sekcji "Success Criteria" + design-specific:

**Pre-merge design QA (dodać do CI):**
- [ ] Wszystkie ekrany z listy 25 zaimplementowane
- [ ] Wszystkie detale z tego pliku (§1-§18) zachowane
- [ ] i18n: 100% user-facing strings z `I18n.t()` call (grep CI gate)
- [ ] A11y: axe-core audit przechodzi (zero critical violations)
- [ ] Responsive: screenshot tests per viewport (Playwright)
- [ ] Empty states: każda lista/tabela ma empty state (grep CI gate)
- [ ] Loading states: każdy async flow ma skeleton/spinner
- [ ] Error states: każdy failure mode ma user-visible message
- [ ] Permission-aware: viewer with group X widzi tylko uprawnione sections
- [ ] Dark theme: 100% coverage, zero hardcoded hex colors (wszystko przez CSS variables)

---

## Usage

Implementator czyta:
1. Mockupy `index.html` (kolorystyka, układy, flow)
2. Ten plik (details per screen, features must preserve)
3. Design doc `critix-main-design-20260417-113355.md` (backend architecture, WASM pipeline, protocol)
4. Obecny kod `tentaflow-core/wwwroot/js/modules/*` (pełne implementacje do portu)

Każdy z 16 obecnych widoków + 9 nowych ekranów = łącznie 25 screens do implementacji w Phase 3 Big Bang refactoru.

**Estimated scope:** ~12-16 tygodni solo z CC (revised from 9-14 per scope expansions).
