// =============================================================================
// File: modules/tentabus.js — TentaBus M1 admin screen (SUM/tentabus/PLAN.md
// §7.3, mockups SUM/mockups/tentabus-final-20260826/{m01,m02,m03,m04,m05,m08}).
// Screens: topic list + create/edit wizard (M01/M02), topic detail with
// overview/partitions/config/ACL tabs (M03), consumer groups + offset reset
// (M04), DLQ per topic (M05), audited message preview (M08). Render/mount/
// unmount contract mirrors `analytics.js` (~line 542); tables via `tf-table`,
// modals via `tf-modal`, live charts via `tf-line-chart`'s `TfCartesianChart`
// base (see the module doc below for the "24h" caveat).
//
// PROTOCOL GAPS (M1 wire vs. the accepted mockups/PLAN — each one is called
// out again at its exact render site so a reviewer does not have to trust
// this comment alone). Follow-up "tor U" narrowed this list — see the
// resolved items marked below:
//  1. RESOLVED (tor U): `BusStatsSnapshotWire` now carries org-wide
//     `totalMsgsInPerSec`/`totalBytesInPerSec`/`totalBytesOnDisk`/`totalLag`/
//     `totalDlqDepth` plus a per-topic `topics[]` breakdown — the KPI strip
//     and per-topic columns below are wired to real numbers. STILL OPEN:
//     there is no history/time-series endpoint, only this polling snapshot
//     (3s cadence) — the mockup's "ostatnie 24 h" charts are rendered here
//     as a rolling in-memory window of the last `MAX_CHART_POINTS` polls,
//     labelled "ostatnie N minut" (i18n `chart_live_window_note`), reset on
//     unmount/remount. There is also no "out"/ack rate, only "in".
//  2. RESOLVED (tor U + M2): `BusPartitionInfoWire` now also carries
//     `earliestOffset`/`sizeBytes`/`segments` (read-only introspection, no
//     throwaway consumer needed) AND, since M2 (PLAN-M2.md §1f),
//     `leaderNodeId`/`leaderEpoch`/`isrCount`/`replicaCount`/`highWatermark`
//     — the partitions tab below shows all of these, plus a computed
//     `leo - hw` lag and an `unavailableReason` state chip (PLAN-M2 §4.1
//     A4: "partycja niedostępna" is a partition STATE, never a producer
//     error) instead of the old static "—" placeholders.
//  3. `BusTopicOptionsWire` still has no node-selection field (only
//     `replication_factor: Option<u32>`) — M2's `create_topic` (PLAN-M2 §1e)
//     picks replica NODES itself from `min(3, healthy same-env nodes)`, the
//     wire never accepts a client-chosen node list. The wizard's node
//     multiselect (fed by `ReplicaListResponse.nodes`, M06's own source) is
//     therefore INFORMATIONAL only: checking/unchecking nodes drives the
//     RF stepper's number (the one field that IS on the wire), never a
//     node-id payload — see `wireNodePicker`'s doc.
//  4. RESOLVED (tor U): `BusOffsetResetMode` gained a 4th `Timestamp{ts_ms}`
//     variant — the reset modal below offers all 4 mockup modes, including
//     a datetime-local picker converted to epoch ms.
//  5. `idempotency_key` is accepted by the wire but rejected fail-closed by
//     `bus::topics::reject_idempotency_key` (CEL evaluator not wired yet) —
//     the wizard shows the field disabled with an M3a chip rather than
//     silently sending a value the server will always refuse.
//  6. ACL only models `subject_type/subject_id/access_level(allow|deny)` —
//     no `produce`/`consume`/`admin` per-action column exists on
//     `resource_permissions` (see `dispatch/bus.rs`'s module doc). The ACL
//     tab below uses allow/deny per subject, not per-action checkboxes.
//  7. RESOLVED (tor U): `MessagesBrowse`/`DlqList` responses now carry a
//     `partitions[]` breakdown (`earliestOffset`/`highWatermark`/
//     `nextOffset`/`hasMore`, per partition) and the matching requests
//     accept `fromOffsets` (per-partition cursors) — M08/M05 below page
//     each partition independently and show earliest/high-watermark.
//     RESOLVED (KRYTYK-M1-R3.md's R3-2 "M08's partition selector only ever
//     works for partition 0"): `buildMessagesBrowseRequest` already sent an
//     explicit `partition` field (this file's side of POSTEP.md's "Decyzje
//     po R3" #1), but `peek_topic` (dispatch/bus.rs, a concurrent backend
//     fala) ignored it and always walked every partition, so a hot
//     partition 0 could exhaust the whole record budget before a later
//     selected partition's records ever reached the client —
//     `filterRecordsByPartition` then filtered a response that, by
//     construction, never contained the selected partition, and
//     `hasMoreForPartitionSelection` read the (also wrong) `partitions[]`
//     breakdown and hid "load more" too. Once `peek_topic` honors
//     `partition` (peeking ONLY that partition, or every partition for
//     "Wszystkie"/`null`), this file needs no further change: the partition
//     `<select>`'s `change` handler already clears the buffer and requests
//     page 1 with `partition` set (`openMessagePreview`), and "load more"
//     already pages the SELECTED partition only via its own `fromOffsets`
//     cursor (`fromOffsetsForPartitionSelection`) — "Wszystkie" still sends
//     no `partition` and pages every partition as before.
//  8. RESOLVED (tor U): `BusCapabilitiesRequest` (`canRead`/`canWrite`/
//     `canAdmin`/`isSiteAdmin`) is fetched once on mount and gates every
//     control below — topic CRUD/pause-resume/DLQ actions need `canAdmin`,
//     offset reset/ACL writes need `isSiteAdmin` (the coarser site-admin
//     tier `dispatch/bus.rs` enforces for those handlers specifically), and
//     a read-only session (`canRead` only) sees the same screens with every
//     action button hidden instead of the earlier `me.role === 'admin'`
//     client-side guess.
//  9. No quota UI: `BusQuotaGetRequest`/`QuotaSetRequest` are wired in
//     `codec.js`, but `SPEC.md` (§4, the mockup map) has no quota screen or
//     "Limity org" card in any of the 8 accepted mockups — deliberately not
//     built here to avoid inventing UI the mockups never asked for.
//  10. M2 (PLAN-M2.md §1f, mockup m06): new M06 "Replikacja i failover" view
//      (`busReplicaListRequest`/`ReplicaListResponse{nodes,partitions,
//      failovers}`), "Przenieś lidera" (`busLeaderTransferRequest`) and
//      "Zmień repliki" (`busReassignRequest`) — both gated `isSiteAdmin()`
//      (PLAN-M2 §1f: both land in `dispatch/bus.rs`'s `bus_dispatch_admin`,
//      the SAME `#[policy(Admin)]` site-admin tier gap #8 above already
//      documents for offset-reset/ACL, not the lighter `canAdmin()` topic-
//      CRUD tier). NOT built: a real "ISR shrink/expand" HISTORY timeline —
//      PLAN-M2 §1e is explicit that there is no per-shrink/expand audit
//      entry, "tylko metryka + zdarzenie UI" — so M06's lag card below
//      shows the partitions' CURRENT `lagging[]` state only (mirrors A4:
//      a state, not an event log), not the mockup's illustrative multi-
//      entry timeline (m06:119-129), which has no wire source to read back.
//  11. W9 (SUM/tentabus/PLAN-APP-PLATFORM.md §6.1/§9i): TentaBus became a
//      non-singleton native app — every request now names the instance it
//      addresses (`BusEnvelope.instance_id` on the wire). `mount(params)`
//      resolves `state.instanceId` from `#/tentabus?instance=<addonId>`
//      (falling back to a same-screen picker/empty-state gate per
//      `resolveInstanceGate`'s doc when the param is missing/unknown, never
//      guessing one) and every request builder below threads it through
//      `requireInstanceId` so a call reaching the wire without it throws
//      instead of silently addressing whichever bus the server defaults to.
//      Deliberately UNCHANGED (owner-accepted mockups,
//      `SUM/mockups/tentabus-app-20260903/SPEC.md`): the tabs, KPI strip,
//      chart, filters and topics/groups/DLQ/replication tables — this wave
//      only threads the instance id through, it does not redraw any of
//      those existing views.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, formatBytes, fmtCompact } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';
import '/js/components/tf-button.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-select.js';
import '/js/components/tf-radio.js';
import '/js/components/tf-input.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-line-chart.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-modal.js';
import '/js/components/tf-spinner.js';

const T = (key, params) => I18n.t(`tentabus.${key}`, params);

// `[native] package_id` (`src/bus/app-manifest.toml`) — filters the
// unified `appsListRequest` roster down to this package's own instances,
// same convention Flow Builder's `bus_instances` dynamic_enum source uses
// server-side (`flows_config.rs`).
const PACKAGE_ID = 'tentabus';

const STATS_POLL_MS = 3000;
// M2: 'replication' (M06) is a 5th tab, added alongside the 3 M1 tabs.
const TABS = ['topics', 'groups', 'dlq', 'replication'];
// The 5 mutually-exclusive views `#tb-panel` can show (4 tabs + topic
// detail) — each gets its OWN persistent container (`ensureViewContainer`)
// so switching between them shows/hides existing DOM instead of tearing it
// down and rebuilding it, keeping scroll position, in-progress search text,
// table sort and focus intact across a tab switch.
const VIEW_SLOTS = ['topics', 'groups', 'dlq', 'detail', 'replication'];
const DLQ_RETRY_ALL_MAX = 500;
// Rolling in-memory window for the "live" M01/M03 charts — there is no
// history/time-series endpoint (module-doc gap #1), so this is the last
// N polls kept only while the screen stays mounted, not real 24h history.
const MAX_CHART_POINTS = 40;
const CHART_WINDOW_MINUTES = Math.round((MAX_CHART_POINTS * STATS_POLL_MS) / 60_000);

// =============================================================================
// Pure helpers — no DOM, no ApiBinary. Unit-tested from
// `tentabus.request-builders.test.js` by brace-extraction (services.js-style),
// since this module pulls in DOM-only custom-element imports at load time.
// =============================================================================

function clampInt(v, min, max, fallback) {
  const n = Math.trunc(Number(v));
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

function clampReplicationFactor(v) {
  return clampInt(v, 1, 7, 3);
}

function clampDlqRetryAllMax(v) {
  return clampInt(v, 1, DLQ_RETRY_ALL_MAX, 100);
}

// W9 (SUM/tentabus/PLAN-APP-PLATFORM.md §3.1/§9i): every TentaBus request
// names its instance — there is no "current bus" once instances exist, and
// a silent default is exactly the cross-instance leak the platform forbids
// (`BusEnvelope`'s own doc, `tentaflow-protocol/src/bus.rs`). Every request
// builder in this file routes its `instanceId` through this one guard, so a
// call site that reaches the wire without `state.instanceId` set (screen
// not mounted with `?instance=`, or a stale detached call racing `unmount`)
// throws here instead of silently addressing whichever bus the server
// might default to.
function requireInstanceId(instanceId) {
  if (typeof instanceId !== 'string' || instanceId === '') {
    throw new Error('tentabus: request requires an instance id — screen not mounted with ?instance=');
  }
  return instanceId;
}

const TOPIC_NAME_RE = /^[a-z0-9][a-z0-9.-]{1,126}$/;

// Mirrors `bus::topics::validate_user_topic_name` (PLAN §7.1's regex) closely
// enough for live feedback; the server remains the authority (`bus.
// invalid_topic_name` is still mapped and surfaced if this passes but the
// server disagrees, e.g. the `__` reserved prefix or the DLQ-suffix budget).
function isValidTopicName(name) {
  const s = String(name || '');
  if (s.startsWith('__')) return false;
  return TOPIC_NAME_RE.test(s);
}

function defaultAcksForRf(rf) {
  return rf >= 3 ? 'quorum' : 'leader';
}

const RETENTION_PRESETS_MS = {
  '24h': 86_400_000,
  '7d': 604_800_000,
  '30d': 2_592_000_000,
  '90d': 7_776_000_000,
  '365d': 31_536_000_000,
};

function retentionPresetFromMs(ms) {
  const n = Number(ms);
  for (const [key, val] of Object.entries(RETENTION_PRESETS_MS)) {
    if (val === n) return key;
  }
  return 'custom';
}

// Owner decision B (durability class UI): a topic response carries a
// resolved `durabilityClass` ("standard"|"critical") once the backend wire
// ships it, but this reads a topic/topic-list row that MAY still predate
// that field (rolling deploy, or an older cached snapshot) — in that case
// it derives the class defensively from the already-resolved `durability`
// policy string the server has always sent: `fsync_batch`/`fsync_batch_full`
// fsync the whole batch before ACK (critical), everything else — including
// `os` (Dev's page cache) and `fsync_interval:<ms>` (Prod/Test's at-most-
// every-N-ms policy) — acks after the write without waiting on that fsync
// (standard). This mirrors the server's own fallback so a client that has
// not redeployed yet still classifies every topic correctly.
function deriveDurabilityClass(topic) {
  const t = topic || {};
  if (t.durabilityClass === 'standard' || t.durabilityClass === 'critical') return t.durabilityClass;
  const durability = typeof t.durability === 'string' ? t.durability : '';
  return durability.startsWith('fsync_batch') ? 'critical' : 'standard';
}

// Builds the SNAKE_CASE `BusTopicOptionsWire`-shaped object the wasm encoder
// passes straight to `serde_json::from_str::<BusTopicOptionsWire>` with no
// field remapping (see `tentaflow-protocol-wasm/src/lib.rs`'s
// `encode_bus_topic_create_request` — a plain JSON parse, unlike almost every
// other request in `codec.js` which accepts camelCase and translates by
// hand). A form field left `undefined`/`''`/`null` is omitted entirely so the
// server's own default (create) / "leave unchanged" (update) semantics apply
// — this function never invents a value the operator did not set.
// `idempotency_key` is deliberately NEVER read from `form` — see this file's
// module-level gap #5.
// `durability_class` is the one exception to this function's own doc above:
// the backend's `BusTopicOptionsWire` models it as a camelCase
// `durabilityClass` field (unlike every neighboring snake_case option), so
// this key is emitted as-is rather than translated like the rest — see
// owner decision B (durability class UI). Only the two real values are ever
// sent; anything else (unset, or a stray value) is omitted so the server's
// own create-default / "leave unchanged" semantics apply, same as `acks`/
// `durability` above.
const DURABILITY_CLASSES = new Set(['standard', 'critical']);

// R5-2 fix (KRYTYK-M1-R5.md b.2, P1: "critical → standard is a silent
// no-op"): the CONTRACT this now relies on is "sending `durabilityClass`
// WITHOUT `durability` switches the topic to the class-derived policy" — so
// a class-only change (the radio alone, advanced select left on
// "Automatycznie") must never put a `durability` key on the wire at all,
// explicit or not. `src.durability === 'auto'` is that exact "left alone"
// state and is OMITTED by default, same as before this fix.
// The one exception is `src.durabilityAutoClear` — set by the wizard only
// when the topic being edited already had an EXPLICIT policy
// (`durabilityExplicit: true`) and the operator deliberately re-selected
// "Automatycznie (wg klasy)" to clear it. That is a real, deliberate
// instruction ("stop overriding"), not a "leave unchanged", so it is sent
// as the literal string `"auto"` — the second half of the contract:
// "sending `durability: 'auto'` clears an explicit policy and resolves from
// the class".
function buildTopicOptionsWire(form) {
  const src = form || {};
  const out = {};
  const putInt = (key, v) => { if (v !== '' && v != null && Number.isFinite(Number(v))) out[key] = Math.trunc(Number(v)); };
  const putStr = (key, v) => { if (v !== '' && v != null) out[key] = String(v); };

  putInt('partitions', src.partitions);
  putInt('retention_ms', src.retentionMs);
  putInt('retention_bytes_per_partition', src.retentionBytesPerPartition);
  putStr('cleanup_policy', src.cleanupPolicy);
  putStr('delivery', src.delivery);
  putInt('dedup_window_ms', src.dedupWindowMs);
  putInt('max_delivery_attempts', src.maxDeliveryAttempts);
  putInt('retry_backoff_ms', src.retryBackoffMs);
  putStr('schema_id', src.schemaId);
  putStr('validation', src.validation);
  putStr('content_type', src.contentType);
  putInt('replication_factor', src.replicationFactor);
  if (src.acks && src.acks !== 'auto') putStr('acks', src.acks);
  if (src.durability === 'auto') {
    if (src.durabilityAutoClear) out.durability = 'auto';
  } else {
    putStr('durability', src.durability);
  }
  if (DURABILITY_CLASSES.has(src.durabilityClass)) out.durabilityClass = src.durabilityClass;
  putInt('max_inline_bytes', src.maxInlineBytes);
  putStr('compression', src.compression);
  return out;
}

// P2 (KRYTYK-M1-R5.md b.3): the advanced "Trwałość zapisu" select had no
// `fsync_interval:<ms>` option — exactly the family Prod/Test's owner
// decision B default (`fsync_interval:50`) lives in — so opening the
// advanced section on a standard-class Prod/Test topic showed an empty
// field with no way to express, or re-express, its own policy. Range
// mirrors the server's own bound on `Durability::FsyncInterval` (bus/
// topics.rs): 1-1000 ms, defaulting to the owner-decision-B value of 50.
function clampFsyncIntervalMs(ms) {
  const n = Number(ms);
  if (!Number.isFinite(n)) return 50;
  return Math.min(1000, Math.max(1, Math.trunc(n)));
}

function formatFsyncIntervalDurability(ms) {
  return `fsync_interval:${clampFsyncIntervalMs(ms)}`;
}

// The M01/M03 "(polityka jawna)" secondary label (KRYTYK-M1-R5.md b.7): a
// tiny pure predicate so the paint-time chip helper and its unit tests share
// one definition of "show the explicit-override label" instead of the chip
// re-deriving it inline.
function shouldShowDurabilityExplicitLabel(topic) {
  return topic?.durabilityExplicit === true;
}

function sumGroupLag(groups) {
  if (!Array.isArray(groups)) return 0;
  return groups.reduce((acc, g) => acc + (Number(g.lagTotal ?? g.lag_total ?? 0) || 0), 0);
}

function computeLagRatio(lag, highWatermark) {
  const l = Number(lag) || 0;
  const hw = Number(highWatermark) || 0;
  if (hw <= 0) return 0;
  return Math.min(1, Math.max(0, l / hw));
}

function lagSeverityClass(ratio) {
  if (ratio >= 0.8) return 'tb-lagbar--danger';
  if (ratio >= 0.4) return 'tb-lagbar--warn';
  return '';
}

// Looks up one topic's row in `BusStatsSnapshotWire.topics` by name — `null`
// when the snapshot has not loaded yet or predates this topic (a brand-new
// topic can lag one poll behind `topics[]`, tor U task 3).
function findTopicStats(statsTopics, name) {
  return (Array.isArray(statsTopics) ? statsTopics : []).find((t) => t.topic === name) || null;
}

// Task 3 (KRYTYK-M1-R2.md's N-2 "KPI = 4, lista = 3" / N-7's
// `tf-system-probe` leaking into the KPI strip): `PROBE_GROUP`
// (`dispatch/bus.rs`'s "fixed, reused consumer group behind every read-only
// probe … obviously non-human") and any other internal `tf-*` group are not
// something an operator manages, so they should never appear in the M04
// table or count toward "Grupy konsumentów"/"Wstrzymane grupy". The backend
// fix (POSTEP.md's "Decyzje koordynatora po krytyku R2" #3) hides them
// server-side; this filters again client-side as defense in depth, and —
// the actual N-2 fix — both the KPI numbers (`paintKpiStrip`) and the M04
// table (`paintGroupsTable`) now read this SAME filtered list, so they
// cannot drift apart the way KPI=4/list=3 did.
function isInternalGroupId(groupId) {
  return typeof groupId === 'string' && groupId.startsWith('tf-');
}

function filterVisibleGroups(groups) {
  return (Array.isArray(groups) ? groups : []).filter((g) => !isInternalGroupId(g.group));
}

// Per-partition paging cursor for the NEXT `MessagesBrowse`/`DlqList` page
// (tor U task 1/2's `partitions[]` + `fromOffsets`): only partitions that
// reported `hasMore` carry a cursor forward, at their own `nextOffset` — a
// partition that already reached its high watermark is simply omitted, not
// re-sent with a stale offset.
function buildFromOffsetsForNextPage(partitions) {
  return (Array.isArray(partitions) ? partitions : [])
    .filter((p) => p.hasMore)
    .map((p) => ({ partition: p.partition, offset: p.nextOffset }));
}

// `<input type="datetime-local">`'s value ("2026-08-27T14:30") has no
// timezone — the browser renders/parses it in the user's LOCAL timezone,
// which is exactly what `new Date(str)` does for that exact string shape,
// so this is a thin, testable wrapper rather than manual epoch math that
// would silently disagree with the input's own display.
function datetimeLocalToTsMs(value) {
  if (!value) return null;
  const ms = new Date(value).getTime();
  return Number.isFinite(ms) ? ms : null;
}

// P3-6: the reset modal's `explicit` mode "Offset" field had no validation
// of its own — `Number('' || 0)` silently coerced an empty field to offset
// `0` (a real, consequential reset to the earliest offset) instead of
// surfacing an error the way the `timestamp` mode's own empty-field check
// already does. A negative number is likewise never a valid offset.
function isValidExplicitOffset(value) {
  const trimmed = String(value ?? '').trim();
  if (trimmed === '') return false;
  const n = Number(trimmed);
  return Number.isFinite(n) && n >= 0;
}

function dlqSourceTopicOptions(topics) {
  return (Array.isArray(topics) ? topics : [])
    .filter((t) => !(t.isDlq ?? t.is_dlq))
    .map((t) => ({ value: t.name, label: t.name }));
}

// R3-1 (KRYTYK-M1-R3.md, P1: "DLQ tab is empty on every entry"): the SINGLE,
// pure, unit-tested decision of "what should the selected DLQ source topic
// become". Root cause of R3-1 was that `paintDlqSourceOptions()` (a paint-time
// function) ALSO mutated `state.dlqSource` as a side effect purely to give the
// `<tf-select>` a sensible display default — so by the time `setTab`'s guard
// asked "is a source already selected?" the answer was already "yes" (set one
// render step earlier), the guard never fired, `selectDlqSource`/
// `loadDlqRecords` never ran, and `state.dlqRecords` stayed `null` forever —
// which `paintDlqTable` rendered as `host.innerHTML = ''`, a silently empty
// tab with no spinner, no error, no way out short of manually changing the
// select (which is the only code path that still called `selectDlqSource`).
// This function has NO side effects — it only computes a value — so it is
// safe to call from every place that can make the DLQ tab visible or change
// its candidate topic list (`ensureDlqTabReady`, called from both `setTab`
// and `loadTopics`) without risking the same race again: there is exactly one
// function, `ensureDlqTabReady`, that ever ACTS on this value.
function resolveDlqEntrySource(currentSource, topics) {
  if (currentSource) return currentSource;
  const first = dlqSourceTopicOptions(topics)[0];
  return first ? first.value : '';
}

// Best-effort preview decode for a record's raw bytes: valid UTF-8 renders as
// text (the common case — JSON/text payloads), anything else falls back to a
// bounded hex dump so binary payloads never render as U+FFFD soup.
function bytesToPreviewText(bytes, maxBytes = 512) {
  if (!bytes || bytes.length === 0) return '';
  const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const slice = arr.subarray(0, maxBytes);
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(slice);
    return text + (arr.length > maxBytes ? '…' : '');
  } catch {
    let hex = '';
    for (let i = 0; i < slice.length; i += 1) {
      hex += slice[i].toString(16).padStart(2, '0');
      if (i < slice.length - 1) hex += ' ';
    }
    return hex + (arr.length > maxBytes ? ' …' : '');
  }
}

function findHeader(headers, key) {
  return (Array.isArray(headers) ? headers : []).find((h) => h.key === key) || null;
}

function headerText(headers, key) {
  const h = findHeader(headers, key);
  return h ? bytesToPreviewText(h.value, 4096) : null;
}

// `true`-shaped iff the decoded JSON matches `flow_engine::blob_store::
// BlobRef` (PLAN §2.4/§6.2 D11) — mirrors `dispatch/bus.rs`'s
// `looks_like_blob_ref` heuristic client-side so a preview can render the
// BlobRef card instead of a raw-bytes dump.
function parseBlobRefJson(bytes) {
  try {
    const text = bytesToPreviewText(bytes, 8192);
    const obj = JSON.parse(text);
    if (obj && typeof obj === 'object'
      && typeof obj.id === 'string'
      && typeof obj.size_bytes === 'number'
      && typeof obj.mime === 'string'
      && typeof obj.sha256 === 'string') {
      return obj;
    }
  } catch {
    // not JSON / not a BlobRef — fall through
  }
  return null;
}

// Extracts the stable `bus.<code>` token `dispatch/bus.rs::map_bus_error`
// prefixes every error message with (PLAN §6.2: "błędy mapowane... ze
// stabilnymi kodami stringowymi"). Returns `null` when the message does not
// follow that convention (e.g. a transport-level error).
//
// NOT anchored at the start: the string this actually receives is the
// `Error` thrown by `binary-ws-client.js`'s pending-request rejection —
// `protocol error ${code}: ${message}` (e.g. "protocol error BadRequest:
// bus.invalid_topic_config: partitions must be 1-256, got 999") — so
// `bus.<code>` sits AFTER a "protocol error <Kind>: " prefix, never at
// index 0. A leading `^bus\.` anchor never matched that shape and silently
// disabled every one of the 35 translated `tentabus.errors.*` codes (5
// locales) plus the DLQ "not found yet" empty-state branch below.
function busErrorCode(message) {
  const m = /\bbus\.([a-z0-9_]+)/.exec(String(message || ''));
  return m ? m[1] : null;
}

// Maps a thrown `ApiBinary` error to a translated, user-facing string. Falls
// back to the raw server message when no i18n entry exists for the code yet
// (`I18n.t` returns the lookup path itself on a miss — compared against here
// so an untranslated code degrades to information instead of a dotted key).
function mapBusErrorMessage(message, translate) {
  const code = busErrorCode(message);
  if (code) {
    const key = `errors.${code}`;
    const translated = translate(key);
    if (translated !== `tentabus.${key}`) {
      // M2: `bus.not_leader` gets an extra "hint with the leader node" —
      // appended only when the server message actually names one
      // (`extractNotLeaderHint`'s doc) and only when this file's own
      // `errors.not_leader_hint` key resolved (the same miss-degrades-to-
      // nothing convention as the rest of this function, so a coordinator
      // who has not pasted the M2 i18n block yet gets the plain generic
      // message instead of a literal dotted key glued onto it).
      if (code === 'not_leader') {
        const hintNode = extractNotLeaderHint(message);
        if (hintNode) {
          const hintText = translate('errors.not_leader_hint', { node: hintNode });
          if (hintText !== 'tentabus.errors.not_leader_hint') return `${translated} ${hintText}`;
        }
      }
      return translated;
    }
  }
  return String(message || translate('errors.generic'));
}

// =============================================================================
// State
// =============================================================================

// Fail-closed default when `BusCapabilitiesRequest` has not resolved yet or
// errors out — every gated control stays hidden rather than guessing "yes"
// (tor U task 5; mirrors the earlier `me.role === 'admin'` guess it replaces,
// but erring the opposite direction on failure).
const NO_CAPABILITIES = { canRead: false, canWrite: false, canAdmin: false, isSiteAdmin: false };

// `ApiBinary.one('busCapabilitiesRequest')` resolves to the raw dispatch
// body, which is an ENVELOPE, not the capabilities object itself:
// `{ variant: 'BusCapabilitiesResponse', capabilities: { canRead, canWrite,
// canAdmin, isSiteAdmin } }` (`tentaflow-protocol-wasm/src/lib.rs`'s
// `decode_bus_payload` for `BP::CapabilitiesResponse`, mirroring
// `tentaflow-protocol/src/bus.rs`'s `BusPayload::CapabilitiesResponse {
// capabilities: BusCapabilitiesWire }`). Reading the envelope flat (the
// earlier bug) always yields `undefined` for every field, so every
// `canAdmin()`/`isSiteAdmin()` check fails closed — hiding "Nowy topik",
// edit/delete, pause/resume, DLQ retry/discard, and offset reset for EVERY
// user including a site admin. This also accepts an already-flat shape
// (`{ canRead, ... }` with no `.capabilities`) so a future wire
// simplification degrades to "read the fields" instead of re-introducing
// the same silent all-hidden failure.
function unwrapCapabilities(resp) {
  if (resp && typeof resp === 'object') {
    if (resp.capabilities && typeof resp.capabilities === 'object') return resp.capabilities;
    if (typeof resp.canAdmin === 'boolean') return resp;
  }
  return NO_CAPABILITIES;
}

// =============================================================================
// Incremental-repaint helpers (owner requirement: "the chart must not draw
// from zero every time … all other data must only swap values, not
// re-render the page"). Pure, DOM-shape-agnostic (an element-like object with
// `textContent`/`getAttribute`/`setAttribute` is enough), so these are
// unit-tested directly from `tentabus.request-builders.test.js` alongside the
// rest of this file's pure helpers.
// =============================================================================

// Writes `value` into `el.textContent` only when it actually changed — the
// generic "swap the value, do not touch layout" primitive every KPI
// tile/chip/stat patch below is built from.
function patchText(el, value) {
  if (!el) return;
  const next = value == null ? '' : String(value);
  if (el.textContent !== next) el.textContent = next;
}

// Same no-op-on-equal contract as `patchText`, for a custom element's own
// reactive attribute (e.g. `<tf-stat-card value="…">`) instead of a plain
// text node.
function patchAttr(el, name, value) {
  if (!el) return;
  const next = value == null ? '' : String(value);
  if (el.getAttribute(name) !== next) el.setAttribute(name, next);
}

// Ring-buffer append for the "live last N samples" charts (M01 KPI chart,
// M03 overview chart): keeps at most `maxLen` points, oldest evicted first,
// so the series scrolls left sample by sample instead of resetting to empty
// and redrawing from zero. Mutates and returns `arr` (the caller's
// long-lived series array) rather than allocating a new one every poll.
function pushWindowSample(arr, point, maxLen) {
  arr.push(point);
  if (arr.length > maxLen) arr.splice(0, arr.length - maxLen);
  return arr;
}

// Key-based diff between two row-array snapshots (M01 topics table, M04
// groups table): which keys were added/updated/removed, and whether ANYTHING
// changed at all. Used to skip a `tf-table.rows = …` write entirely when a
// poll's freshly computed rows are identical to what is already painted —
// `tf-table` itself recycles `<tr>`/`<td>` by position and only writes a
// cell when its value changed (see tf-table.js's `_renderTbody`/`_writeCell`),
// but it unconditionally REBUILDS each row's action-cell element on every
// `rows = …` (bound-closure buttons need a fresh row reference) — skipping
// the assignment on a no-op poll avoids destroying/recreating those action
// buttons (and any focus/hover state on them) for no reason.
function diffRowsByKey(prevRows, nextRows, keyFn) {
  const prevMap = new Map((Array.isArray(prevRows) ? prevRows : []).map((r) => [keyFn(r), r]));
  const nextMap = new Map((Array.isArray(nextRows) ? nextRows : []).map((r) => [keyFn(r), r]));
  const added = [];
  const updated = [];
  const removed = [];
  for (const [key, row] of nextMap) {
    if (!prevMap.has(key)) added.push(key);
    else if (JSON.stringify(prevMap.get(key)) !== JSON.stringify(row)) updated.push(key);
  }
  for (const key of prevMap.keys()) {
    if (!nextMap.has(key)) removed.push(key);
  }
  return { added, updated, removed, changed: added.length > 0 || updated.length > 0 || removed.length > 0 };
}

// `tf-line-chart`'s entrance draw-in animation already checks this itself
// (`TfCartesianChart._motionAllowed()`) before animating a redraw, but the
// chart is also told explicitly once per mount (`ensureLiveChart`) so a
// reduced-motion session never even flags a pending entrance animation for a
// series update that is about to be a same-instance data swap, not a fresh
// paint.
function prefersReducedMotion() {
  if (typeof globalThis.matchMedia !== 'function') return false;
  try { return globalThis.matchMedia('(prefers-reduced-motion: reduce)').matches; } catch { return false; }
}

// =============================================================================
// M2 replication/failover (M06, plus M03's "Partycje i repliki" tab and
// M02's node picker) — pure helpers. Wire shapes read here are
// `ReplicaListResponse{nodes,partitions,failovers}` (PLAN-M2.md §1f):
// `nodes[{nodeId,label,environment,isLocal,reachable,lastHeartbeatMsAgo,
// leaderCount,followerCount,isrCount}]`, `partitions[{partition,
// leaderNodeId,leaderEpoch,replicas,isr,lagging[{nodeId,lagBytes,lagMs,
// reason}],highWatermark,logEndOffset,unavailableReason}]`,
// `failovers[{atMs,topic,partition,fromNode,toNode,fromEpoch,toEpoch,
// durationMs,reason}]`.
// =============================================================================

// `ApiBinary.one('busReplicaListRequest', ...)` payload builder — `topic`
// omitted (`undefined`, not `''`) fetches the org-wide node roster +
// failover history with no per-partition role matrix (M06's "Wszystkie
// topiki" scope); a concrete topic name scopes `partitions[]` to it.
function buildReplicaListRequest(instanceId, topic) {
  return { instanceId: requireInstanceId(instanceId), topic: topic || undefined };
}

// "Zmień repliki" (M06) request builder. `partition` stays a plain number —
// PLAN-M2's `ReassignRequest.partition: Option<u32>` allows a whole-topic
// reassign, but this module's dialog always targets one row of the matrix,
// so `partition` is required here (never sent as "every partition").
function buildReassignRequest(instanceId, topic, partition, replicaNodeIds) {
  return {
    instanceId: requireInstanceId(instanceId),
    topic,
    partition: partition == null ? undefined : Number(partition),
    replicas: Array.isArray(replicaNodeIds) ? [...replicaNodeIds] : [],
  };
}

// "Przenieś lidera" (M06) request builder.
function buildLeaderTransferRequest(instanceId, topic, partition, targetNodeId) {
  return { instanceId: requireInstanceId(instanceId), topic, partition: Number(partition), targetNodeId };
}

// SPEC D4 (mockup m02-kreator-topiku.html): a node from a DIFFERENT
// environment than this session's OWN node is shown but never selectable —
// Z12 fencing surfaced as a UI blocker, not only a backend one. `localEnv`
// is this session's own `environmentGetKindRequest().kind` ('dev'|'test'|
// 'prod'); `null`/unresolved treats every node as foreign (fail-closed: no
// node is selectable until the local environment is actually known).
function isSameEnvironment(node, localEnv) {
  return !!localEnv && node?.environment === localEnv;
}

function filterSameEnvNodes(nodes, localEnv) {
  return (Array.isArray(nodes) ? nodes : []).filter((n) => isSameEnvironment(n, localEnv));
}

// M02's default RF when the node picker has not been touched by hand yet —
// "Decyzje koordynatora przed startem M2" (PLAN-M2.md, bottom): "domyślne
// RF = min(3, zdrowe węzły w środowisku)", never 0 (a topic always needs at
// least itself as sole replica, even alone in its environment).
function autoReplicationFactor(nodes, localEnv) {
  const healthyCount = filterSameEnvNodes(nodes, localEnv).filter((n) => n.reachable !== false).length;
  return clampReplicationFactor(Math.min(3, Math.max(1, healthyCount)));
}

// M03's "Lag" column: how far the leader's own log-end-offset has run ahead
// of the (safely acknowledged, replicated) high watermark. This is a
// LEADER-side figure, distinct from a specific follower's replication lag
// (`ReplicaLagWire.lagBytes`/`lagMs` in `partitions[].lagging[]`, read
// directly where needed rather than through this helper).
function computeReplicationLag(highWatermark, logEndOffset) {
  const hw = Number(highWatermark) || 0;
  const leo = Number(logEndOffset) || 0;
  return Math.max(0, leo - hw);
}

// M01/M03's ISR-health predicate: fewer in-sync replicas than the replica
// set itself means the partition has already lost redundancy. This is a
// coarser, UI-only "worth a warning chip" signal — the stricter
// write-availability gate (`min_isr = floor(RF/2)+1`, PLAN-M2.md §0
// K-M2-2) lives server-side and surfaces here only via
// `bus.not_enough_replicas`/`unavailableReason`, never re-derived
// client-side from a guessed RF.
function isIsrDegraded(isrCount, replicaCount) {
  return Number(isrCount) < Number(replicaCount);
}

// M06's role-matrix cell (mockup m06:104-113). `leader` wins over `isr`
// (the wire's own `isr[]` conventionally includes the leader too, but the
// pill must show the more specific role); `lagging` only for a replica
// `partitions[].lagging[]` names explicitly (never guessed from offsets);
// `none` covers both "not a replica of this partition" and a foreign-env
// node rendered in the same matrix for context.
function roleForNode(partition, nodeId) {
  if (!partition || !nodeId) return 'none';
  if (partition.leaderNodeId === nodeId) return 'leader';
  if (Array.isArray(partition.lagging) && partition.lagging.some((l) => l.nodeId === nodeId)) return 'lagging';
  if (Array.isArray(partition.isr) && partition.isr.includes(nodeId)) return 'isr';
  return 'none';
}

// Builds one row per partition, one cell per `nodeIds` entry — the pure
// "shape" `paintReplMatrix` diffs (`diffRowsByKey`) and renders; no DOM.
function buildRoleMatrix(partitions, nodeIds) {
  const ids = Array.isArray(nodeIds) ? nodeIds : [];
  return (Array.isArray(partitions) ? partitions : []).map((p) => ({
    partition: p.partition,
    leaderEpoch: p.leaderEpoch,
    highWatermark: p.highWatermark,
    logEndOffset: p.logEndOffset,
    unavailableReason: p.unavailableReason ?? null,
    cells: Object.fromEntries(ids.map((id) => [id, roleForNode(p, id)])),
  }));
}

// "Przenieś lidera" dialog (M06): only a replica ALREADY in ISR may be
// promoted (mirrors `bus/replication/election.rs`'s `choose_candidate`
// hard constraint, PLAN-M2.md §1b K-M2-3 — "kandydatem może być wyłącznie
// węzeł należący do ISR z ostatniego przypisania") and never the current
// leader itself, which is trivially already the leader.
function leaderTransferCandidates(partition) {
  const isr = Array.isArray(partition?.isr) ? partition.isr : [];
  return isr.filter((id) => id !== partition?.leaderNodeId);
}

// M06 node card degraded state (mockup m06:94-98): unreachable wins over
// everything else; otherwise a node is degraded when the CURRENTLY loaded
// (topic-scoped) `partitions[]` names it in some `lagging[]` — see this
// file's module-doc gap #10 for why this can only ever reflect the one
// topic M06 has loaded, not a true cross-topic aggregate.
function nodeDegradedReason(node, partitions) {
  if (!node) return null;
  if (node.reachable === false) return { kind: 'unreachable' };
  for (const p of (Array.isArray(partitions) ? partitions : [])) {
    const lag = (Array.isArray(p.lagging) ? p.lagging : []).find((l) => l.nodeId === node.nodeId);
    if (lag) return { kind: 'lagging', partition: p.partition, lag };
  }
  return null;
}

// `UnavailableReason` (PLAN-M2.md §1e: `NoIsr | NoAssignment | EpochFenced`)
// travels over the wire as whatever `serde`'s default (de)serialization
// picks for a unit-variant enum on `BusPartitionReplicaWire` — this
// tolerates BOTH a snake_case string (`no_isr`) and a bare PascalCase Rust
// variant name (`NoIsr`) landing here, converting either shape into this
// module's own `tentabus.replication.unavailable_<snake>` i18n key so a
// small serde-representation choice on the Rust side (fala 2, not yet
// built when this file was written) cannot silently blank the chip.
function unavailableReasonI18nKey(reason) {
  if (!reason) return null;
  const snake = String(reason).replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();
  return `replication.unavailable_${snake}`;
}

// `bus.not_leader`'s server message MAY carry the current leader's node id
// inline (the exact wording is `dispatch/bus.rs::map_bus_error`'s choice,
// fala 2/agent S — not fixed when this file was written), e.g. "...
// current leader is gcm-core-01" or "leader_node_id=gcm-core-01". This is a
// best-effort regex, not a structured field read: no match degrades to
// `null` (the generic translated message, no hint appended) rather than
// throwing or fabricating a node id.
// `(?<![a-zA-Z_])` guards against matching the "leader" INSIDE "not_leader"
// itself (`bus.not_leader`'s own error code is always a prefix of this exact
// message) — without it, the code's own name would always "win" as the
// first regex match and shadow a real hint appearing later in the string.
function extractNotLeaderHint(message) {
  const m = /(?<![a-zA-Z_])leader[_ ]?(?:node[_ ]?id)?\s*(?:is)?\s*[:=]?\s*["']?([a-zA-Z0-9._-]{2,})["']?/i.exec(String(message || ''));
  return m ? m[1] : null;
}

const state = {
  // W9 (SUM/tentabus/PLAN-APP-PLATFORM.md §6.1): the instance this mount is
  // addressing (`tentabus-<8hex>`) and its display label for the header,
  // both resolved once in `mount(params)` from `?instance=` / the instance
  // gate below — `null` only while `resolveInstanceGate` has not settled or
  // resolved to the gate (picker/empty state) itself.
  instanceId: null,
  instanceLabel: '',
  capabilities: NO_CAPABILITIES,
  tab: 'topics',
  view: null, // null | { kind: 'topic-detail', name }
  detailTab: 'overview',

  topics: [],
  topicsLoaded: false,
  search: '',
  envFilter: '',

  stats: null,
  statsTimer: null,
  // Rolling live window for M01's KPI charts — module-doc gap #1 (no
  // history endpoint), reset in unmount() so a remount starts fresh.
  chartSeries: { msgsIn: [], bytesIn: [], lag: [] },

  detail: null, // { topic, partitions, groups } for state.view.name
  detailLoading: false,
  // Bumped every time `state.detail` is replaced by a REAL fetch (new topic
  // opened, or the same topic's detail reloaded after an edit) — never by a
  // stats poll. `renderDetailBody` compares this against a version stamped
  // on the hero/overview DOM to tell "genuine context change, full rebuild
  // needed" apart from "poll tick, patch values in place only".
  detailVersion: 0,
  // Rolling live window for M03's overview tab, keyed to state.view.name —
  // reset whenever a different topic's detail is opened.
  detailChartSeries: null,
  aclEntries: null,
  aclLoading: false,

  groups: [],
  groupsLoaded: false,
  groupDetail: null, // { group, topic, commitMode, paused, partitions }

  dlqSource: '',
  dlqRecords: null,
  dlqPartitions: [], // BusBrowsePartitionInfoWire[] — per-partition earliest/hwm/nextOffset/hasMore
  dlqHasMore: false,
  dlqNextOffset: 0,
  dlqLoading: false,
  // R3-1 (KRYTYK-M1-R3.md): translated message from the last FAILED first-page
  // load, or `null` when the last attempt succeeded (or none has run yet).
  // Distinguishes "never loaded" from "loaded and failed" so `paintDlqTable`
  // can render an error box with a retry button instead of treating both the
  // same way `dlqRecords == null` used to (a silently empty container).
  dlqError: null,

  // M06 (PLAN-M2.md §1f). `topic`: '' = org-wide scope (node cards +
  // failover history, no role matrix — `ReplicaListResponse.partitions` is
  // only meaningful for a concrete topic); otherwise the topic the role
  // matrix/lag card are scoped to. `localEnv` caches this session's own
  // `environmentGetKindRequest().kind` — fetched lazily (SPEC D4's env
  // fencing needs it for BOTH M06's reassign dialog and M02's node picker,
  // so it is cached at module scope rather than fetched twice).
  repl: {
    topic: '',
    loaded: false,
    loading: false,
    error: null,
    data: null, // ReplicaListResponse { nodes, partitions, failovers }
    localEnv: null,
  },

  // Last-painted table rows, cached to diff against the next poll's freshly
  // computed rows (`diffRowsByKey`) so an unchanged poll skips `tf-table`'s
  // `rows = …` write instead of rebuilding every row's action-cell for
  // nothing (see `diffRowsByKey`'s own doc comment). `nodeCards`/`roleMatrix`
  // are M06's own diff caches (same convention); `failoverKeys` is the
  // append-only timeline's "already rendered" set (see `paintReplFailovers`).
  dom: {
    topicsTableRows: null, groupsTableRows: null,
    nodeCards: null, roleMatrix: null, failoverKeys: null,
  },
};

// `canAdmin` gates topic CRUD/pause-resume/DLQ actions — the same
// `bus.admin` tier `dispatch/bus.rs`'s mutating topic/group/DLQ handlers
// enforce. `isSiteAdmin` gates offset reset / ACL writes — the coarser
// site-admin `#[policy(Admin)]` tier those specific handlers require
// (BusCapabilitiesWire's doc). Both fail closed to `false` before the first
// `busCapabilitiesRequest` resolves.
function canAdmin() {
  return state.capabilities?.canAdmin === true;
}

function isSiteAdmin() {
  return state.capabilities?.isSiteAdmin === true;
}

// =============================================================================
// Instance resolution (W9, SUM/tentabus/PLAN-APP-PLATFORM.md §6.1) — reads
// the instance to address from the Router's `?instance=` query param
// (`app.js:479`'s already-established convention for a native app's own
// route). `mount(params)` calls this ONCE before anything else touches the
// wire; nothing below ever re-derives it mid-session, which is also what
// keeps a leaked poll from ever crossing instances after a tab switch or
// drill-down (those never call `Router.navigate` — see `setTab`/
// `renderTopicDetail` — so the URL's `?instance=` is never at risk of being
// silently dropped the way Code Studio's own hash scheme drops it).
// =============================================================================

/** Every enabled TentaBus instance visible to the caller, `{ addonId, title }[]`. */
async function fetchTentaBusInstances() {
  let apps = [];
  try {
    apps = await ApiBinary.list('appsListRequest', { arrayKey: 'apps' });
  } catch {
    return [];
  }
  return (Array.isArray(apps) ? apps : [])
    .filter((a) => (a.packageId ?? a.package_id) === PACKAGE_ID)
    .map((a) => ({
      addonId: String(a.addonId ?? a.addon_id ?? ''),
      title: String((a.titleKey && I18n.t(a.titleKey)) || a.title || a.addonId || a.addon_id || ''),
      enabled: a.enabled !== false,
    }))
    .filter((a) => a.addonId);
}

/**
 * Resolves which instance this mount addresses. Never guesses: a
 * `requestedId` that names a real instance always wins (even a disabled
 * one — the screen opens and its own requests then fail through the normal
 * error-toast path, exactly as they would if the instance were disabled
 * mid-session); otherwise exactly one ENABLED instance auto-enters, several
 * render the same-screen chooser, and zero render the empty state — per
 * `PLAN-APP-PLATFORM.md §6.1`.
 */
async function resolveInstanceGate(requestedId) {
  const instances = await fetchTentaBusInstances();
  if (requestedId) {
    const named = instances.find((a) => a.addonId === requestedId);
    if (named) return { target: named, instances };
  }
  const enabled = instances.filter((a) => a.enabled);
  if (enabled.length === 1) return { target: enabled[0], instances };
  return { target: null, instances };
}

function renderInstanceGate(instances) {
  const root = byId('tb-root');
  if (!root) return;
  const enabled = instances.filter((a) => a.enabled);
  const body = enabled.length === 0
    ? `<div class="tb-state tb-empty">
        <div>${escapeHtml(T('instance_picker_empty'))}</div>
        <tf-button variant="secondary" id="tb-instance-goto-apps">${escapeHtml(I18n.t('nav.apps_home'))}</tf-button>
      </div>`
    : `<div class="tb-instance-list">${enabled.map((a) => `
        <button type="button" class="tb-instance-row" data-instance="${escapeAttr(a.addonId)}">
          <span class="tb-instance-row-title">${escapeHtml(a.title)}</span>
          <tf-chip status="info">${escapeHtml(a.addonId)}</tf-chip>
        </button>`).join('')}
      </div>`;
  root.innerHTML = `
    <div class="tb-head">
      <div>
        <h1 class="tb-title">${escapeHtml(T('title'))}</h1>
        <div class="tb-sub">${escapeHtml(enabled.length ? T('instance_picker_hint') : T('subtitle'))}</div>
      </div>
    </div>
    <div class="tb-panel">${body}</div>
  `;
  root.querySelector('#tb-instance-goto-apps')?.addEventListener('click', () => Router.navigate('apps-home'));
  root.querySelectorAll('.tb-instance-row').forEach((el) => {
    el.addEventListener('click', () => Router.navigate('tentabus', { instance: el.dataset.instance }));
  });
}

// =============================================================================
// Screen shell (render/mount/unmount contract, wzór analytics.js:542)
// =============================================================================

const TentaBusScreen = {
  get title() { return state.instanceLabel || T('title'); },

  render() {
    return '<div id="tb-root" class="tb-root"></div>';
  },

  async mount(params = {}) {
    const { target, instances } = await resolveInstanceGate(params?.instance || null);
    if (!target) {
      state.instanceId = null;
      state.instanceLabel = '';
      renderInstanceGate(instances);
      return;
    }
    state.instanceId = target.addonId;
    state.instanceLabel = target.title;

    try {
      state.capabilities = unwrapCapabilities(await ApiBinary.one('busCapabilitiesRequest', { instanceId: requireInstanceId(state.instanceId) }));
    } catch {
      state.capabilities = NO_CAPABILITIES;
    }
    const root = byId('tb-root');
    if (!root) return;
    root.innerHTML = shellHtml();

    byId('tb-tabs')?.addEventListener('change', (e) => {
      const id = e.detail?.value;
      if (id && TABS.includes(id)) setTab(id);
    });

    paintHeadActions();
    renderPanel();
    // Task 3: `loadGroups()` used to be lazy (only on the first visit to the
    // Groups tab), so the Topics tab's KPI strip had no `state.groups` to
    // read from and fell back to the server's separate `groupCount`
    // aggregate — the two-source split behind N-2's KPI=4/list=3. Loading it
    // in parallel with the topics list here means the KPI numbers are
    // sourced from the same list the M04 table renders regardless of which
    // tab is open first.
    await Promise.all([loadTopics(), loadGroups()]);
    startStatsPolling();
  },

  unmount() {
    // Stopping the poll BEFORE anything else is the one line in this
    // function that actually matters for isolation (PLAN §6.1's own
    // warning): a leaked `setInterval` surviving into the next mount would
    // keep firing `refreshStats()` against `state.instanceId`, which the
    // very next line is about to repoint at a DIFFERENT instance — that is
    // exactly how one instance's numbers would start bleeding into
    // another's screen.
    stopStatsPolling();
    state.instanceId = null;
    state.instanceLabel = '';
    state.capabilities = NO_CAPABILITIES;
    state.tab = 'topics';
    state.view = null;
    state.detailTab = 'overview';
    state.topics = [];
    state.topicsLoaded = false;
    state.search = '';
    state.envFilter = '';
    state.stats = null;
    state.chartSeries = { msgsIn: [], bytesIn: [], lag: [] };
    state.detail = null;
    state.detailVersion = 0;
    state.detailChartSeries = null;
    state.aclEntries = null;
    state.groups = [];
    state.groupsLoaded = false;
    state.groupDetail = null;
    state.dlqSource = '';
    state.dlqRecords = null;
    state.dlqPartitions = [];
    state.dlqLoading = false;
    state.dlqError = null;
    state.repl = { topic: '', loaded: false, loading: false, error: null, data: null, localEnv: null };
    state.dom = {
      topicsTableRows: null, groupsTableRows: null,
      nodeCards: null, roleMatrix: null, failoverKeys: null,
    };
  },
};

function shellHtml() {
  return `
    <div class="tb-head">
      <div>
        <h1 class="tb-title">${escapeHtml(state.instanceLabel || T('title'))} <tf-chip status="info" title="${escapeAttr(T('instance_label'))}">${escapeHtml(state.instanceId || '')}</tf-chip></h1>
        <div class="tb-sub">${escapeHtml(T('subtitle'))}</div>
      </div>
      <div class="tb-head-actions" id="tb-head-actions"></div>
    </div>
    <tf-tabs id="tb-tabs" value="${escapeAttr(state.tab)}" variant="solid">
      <tf-tab id="topics">${escapeHtml(T('tab_topics'))}</tf-tab>
      <tf-tab id="groups">${escapeHtml(T('tab_groups'))}</tf-tab>
      <tf-tab id="dlq">${escapeHtml(T('tab_dlq'))}</tf-tab>
      <tf-tab id="replication">${escapeHtml(T('tab_replication'))}</tf-tab>
    </tf-tabs>
    <div id="tb-panel" class="tb-panel"></div>
  `;
}

function setTab(id) {
  if (state.view) state.view = null;
  state.tab = id;
  paintHeadActions();
  renderPanel();
  if (id === 'groups' && !state.groupsLoaded) loadGroups();
  if (id === 'dlq') ensureDlqTabReady();
  // 'replication' needs no entry here — `renderPanel()` above already ran
  // `renderReplicationTab`, which triggers its own load the same way
  // (`renderReplicationTab`'s own doc: self-sufficient regardless of entry
  // path, including M03's "otwórz w Replikacji" button).
}

function paintHeadActions() {
  const host = byId('tb-head-actions');
  if (!host) return;
  if (state.tab === 'topics' && !state.view && canAdmin()) {
    host.innerHTML = `<tf-button variant="primary" icon="plus" id="tb-new-topic">${escapeHtml(T('new_topic'))}</tf-button>`;
    byId('tb-new-topic')?.addEventListener('click', () => openTopicWizard(null));
  } else {
    host.innerHTML = '';
  }
}

// Persistent per-view container inside `#tb-panel`, keyed by `VIEW_SLOTS`
// entry (`data-tb-view-slot`, distinct from `ensureSkeleton`'s own
// `data-tb-view` marker on the SAME element — one records "which of the 4
// views is this container for" and never changes once created, the other
// records "which skeleton variant is currently built inside it" and changes
// when the view's own content needs a full rebuild, e.g. a different M03
// topic). Built once, on first visit; never removed for the life of the
// mount, so `unmount()`'s `root.innerHTML = shellHtml()` on the NEXT mount is
// what finally discards it.
function ensureViewContainer(panel, key) {
  let el = panel.querySelector(`:scope > [data-tb-view-slot="${key}"]`);
  if (!el) {
    el = document.createElement('div');
    el.dataset.tbViewSlot = key;
    el.hidden = true;
    panel.appendChild(el);
  }
  return el;
}

function renderPanel() {
  const panel = byId('tb-panel');
  if (!panel) return;
  const activeKey = state.view?.kind === 'topic-detail' ? 'detail' : state.tab;
  let activeEl = null;
  for (const key of VIEW_SLOTS) {
    const el = ensureViewContainer(panel, key);
    el.hidden = key !== activeKey;
    if (key === activeKey) activeEl = el;
  }
  if (!activeEl) return;
  if (activeKey === 'detail') { renderTopicDetail(activeEl); return; }
  if (activeKey === 'topics') { renderTopicsTab(activeEl); return; }
  if (activeKey === 'groups') { renderGroupsTab(activeEl); return; }
  if (activeKey === 'dlq') { renderDlqTab(activeEl); return; }
  if (activeKey === 'replication') { renderReplicationTab(activeEl); return; }
}

// Rebuilds `panel`'s skeleton only when switching CONTEXT within a view
// (preserves focus/scroll/typed-but-not-yet-debounced input across data
// refreshes that call back into the same view's paint function) — e.g. M03
// opening a different topic still needs a full rebuild (`viewId` includes
// the topic name), but a stats poll re-entering the same topic's overview
// does not.
function ensureSkeleton(panel, viewId, buildFn) {
  if (panel.dataset.tbView === viewId) return false;
  panel.innerHTML = buildFn();
  panel.dataset.tbView = viewId;
  return true;
}

// =============================================================================
// Stats polling (M01 KPI strip / M03 overview) — BusStatsSnapshotRequest is a
// plain poll, not a push subscription (PLAN §6.2's StatsSubscribe was not
// wired for M1; see `dispatch/bus.rs`'s module doc). Started once in mount(),
// stopped in unmount() — never left running against an unmounted screen.
// =============================================================================

function startStatsPolling() {
  stopStatsPolling();
  refreshStats();
  state.statsTimer = setInterval(refreshStats, STATS_POLL_MS);
}

function stopStatsPolling() {
  if (state.statsTimer) clearInterval(state.statsTimer);
  state.statsTimer = null;
}

async function refreshStats() {
  try {
    state.stats = await ApiBinary.one('busStatsSnapshotRequest', { instanceId: requireInstanceId(state.instanceId) });
  } catch {
    // Silent — the KPI strip just keeps its last known values (or the
    // loading placeholder if it never loaded), matching the "silently skip"
    // convention `refreshNavCounts` in app.js already uses for count badges.
    return;
  }
  // The rolling window is sampled every poll regardless of which view is
  // visible — only the DOM PATCH below is gated by visibility, so switching
  // back to a tab/topic shows an already-up-to-date, already-scrolled chart
  // instead of a gap or a reset-to-empty series.
  pushChartSample(state.chartSeries, state.stats);
  // M01: KPI strip + chart + topics table only exist inside the Topics view
  // — never touch that DOM while a different tab/topic-detail is showing
  // (task requirement: "polling must not touch panels that are not
  // visible").
  if (state.tab === 'topics' && !state.view) {
    paintKpiStrip();
    updateLiveChartSeries('tb-chart-live', state.chartSeries);
    paintTopicsTable();
  }
  // M06: reuses this SAME 3s cadence (task requirement) rather than its own
  // timer, but only touches its DOM while the tab is actually visible and no
  // topic-detail is open over it — `pollReplication` itself re-fetches and
  // patches in place (diffed node cards/matrix, append-only failover rows),
  // never a full `renderReplicationTab` skeleton rebuild.
  if (state.tab === 'replication' && !state.view) {
    pollReplication();
  }
  if (state.view?.kind === 'topic-detail' && state.detail?.topic) {
    // Sample the OPEN topic's own series every poll (not just while the
    // overview tab is visible) so switching back to it does not lose the
    // window already collected — only the repaint is tab-gated.
    const ts = findTopicStats(state.stats?.topics, state.detail.topic.name);
    if (ts && state.detailChartSeries) pushChartSample(state.detailChartSeries, null, ts.msgsInPerSec, ts.bytesInPerSec, ts.totalLag);
    if (state.detailTab === 'overview') renderDetailBody();
  }
}

// Appends one sample to a rolling `{msgsIn, bytesIn, lag}` window, trimmed to
// `MAX_CHART_POINTS` via `pushWindowSample` — the "live last N minutes"
// replacement for the mockup's unavailable 24h history (module-doc gap #1).
// `x` is a plain HH:MM:SS label (category axis), not an epoch, since
// `tf-line-chart`'s category scale expects display-ready ticks.
function pushChartSample(series, snapshot, msgsIn, bytesIn, lag) {
  const x = new Date().toLocaleTimeString(undefined, { hour12: false });
  const push = (arr, y) => pushWindowSample(arr, { x, y: Number(y) || 0 }, MAX_CHART_POINTS);
  push(series.msgsIn, msgsIn ?? snapshot?.totalMsgsInPerSec ?? 0);
  push(series.bytesIn, bytesIn ?? snapshot?.totalBytesInPerSec ?? 0);
  push(series.lag, lag ?? snapshot?.totalLag ?? 0);
}

function paintKpiStrip() {
  const s = state.stats;
  const set = (id, v) => patchAttr(byId(id), 'value', v == null ? '—' : String(v));
  const setFmt = (id, v, fmt) => patchAttr(byId(id), 'value', v == null ? '—' : fmt(Number(v)));
  const setAccent = (id, accent) => patchAttr(byId(id), 'accent', accent || '');
  set('tb-kpi-topics', s?.topicCount);
  setFmt('tb-kpi-msgs-in', s?.totalMsgsInPerSec, (v) => fmtCompact(v));
  setFmt('tb-kpi-lag', s?.totalLag, (v) => fmtCompact(v));
  setAccent('tb-kpi-lag', (Number(s?.totalLag) || 0) > 0 ? 'warning' : '');
  setFmt('tb-kpi-dlq-depth', s?.totalDlqDepth, (v) => fmtCompact(v));
  setAccent('tb-kpi-dlq-depth', (Number(s?.totalDlqDepth) || 0) > 0 ? 'danger' : '');
  setFmt('tb-kpi-disk', s?.totalBytesOnDisk, (v) => formatBytes(v));
  set('tb-kpi-partitions', s?.partitionCountTotal);
  // Sourced from the same `state.groups` the M04 table renders (task 3),
  // not `BusStatsSnapshotWire.groupCount`/`pausedGroupCount` — those were a
  // separate server-side aggregate that could (and did) disagree with the
  // list. `loadGroups()` is now kicked off unconditionally in `mount()`, not
  // only when the Groups tab is opened, so this has real numbers on the
  // Topics tab too; `s?.groupCount` is only a brief pre-load fallback.
  const visibleGroups = state.groupsLoaded ? filterVisibleGroups(state.groups) : null;
  set('tb-kpi-groups', visibleGroups ? visibleGroups.length : s?.groupCount);
  set('tb-kpi-paused', visibleGroups ? visibleGroups.filter((g) => g.paused).length : s?.pausedGroupCount);
}

function kpiStripHtml() {
  return `
    <div class="tb-kpi-grid">
      <tf-stat-card id="tb-kpi-topics" label="${escapeAttr(T('kpi_topics'))}" value="—" icon="database"></tf-stat-card>
      <tf-stat-card id="tb-kpi-msgs-in" label="${escapeAttr(T('kpi_msgs_in'))}" value="—" suffix="msg/s" icon="zap"></tf-stat-card>
      <tf-stat-card id="tb-kpi-lag" label="${escapeAttr(T('kpi_lag'))}" value="—" icon="trend"></tf-stat-card>
      <tf-stat-card id="tb-kpi-dlq-depth" label="${escapeAttr(T('kpi_dlq_depth'))}" value="—" icon="alert"></tf-stat-card>
      <tf-stat-card id="tb-kpi-disk" label="${escapeAttr(T('kpi_disk'))}" value="—" icon="cylinder"></tf-stat-card>
      <tf-stat-card id="tb-kpi-partitions" label="${escapeAttr(T('kpi_partitions'))}" value="—" icon="layers"></tf-stat-card>
      <tf-stat-card id="tb-kpi-groups" label="${escapeAttr(T('kpi_groups'))}" value="—" icon="cluster"></tf-stat-card>
      <tf-stat-card id="tb-kpi-paused" label="${escapeAttr(T('kpi_paused_groups'))}" value="—" icon="pause" accent="info"></tf-stat-card>
    </div>
    <div class="tb-kpi-note">${sprite('info')}${escapeHtml(T('kpi_note'))}</div>
    <div class="tb-card">
      <div class="tb-c-head"><h3>${escapeHtml(T('chart_throughput_title'))}</h3><div class="tb-hint">${escapeHtml(T('chart_live_window_note', { minutes: CHART_WINDOW_MINUTES }))}</div></div>
      <div class="tb-c-body"><tf-line-chart id="tb-chart-live"></tf-line-chart></div>
    </div>
  `;
}

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// Shared "live window" line chart for M01's org-wide throughput/lag and
// M03's per-topic overview (both fed by `pushChartSample` above) — msgs/s in
// on the primary axis, lag total as a second series so a reviewer sees both
// "is the topic busy" and "is a consumer falling behind" at once.
//
// Split in two on purpose (owner requirement: "the chart must not draw from
// zero every time — it must work incrementally and scroll"):
// `ensureLiveChart` sets the STATIC config (axes/legend/reduced-motion) once,
// right after the `<tf-line-chart>` element is created; `updateLiveChartSeries`
// runs on every poll and touches ONLY the `series` property, on that SAME
// element instance. `tf-line-chart` now exposes a public `updateSeries(
// series)` (`tf-line-chart.js`'s `TfCartesianChart`) that patches the
// existing `<polyline>`/point attributes and plays a translateX scroll
// transition instead of tearing down and rebuilding the SVG — the browser
// element is never destroyed/recreated (unlike the pre-fix M03 overview,
// which rebuilt `<tf-line-chart id="tb-detail-chart">` via
// `body.innerHTML = …` on every 3s poll), the x-axis category scale is fed
// the SAME window array reference `pushWindowSample` scrolls in place, and
// `ensureLiveChart` sets `animate` from `prefersReducedMotion()` once so a
// reduced-motion session never gets a transition on a data swap.
function ensureLiveChart(hostId) {
  const chart = byId(hostId);
  if (!chart) return;
  chart.xAxis = { scale: 'category', min: null, max: null, ticks: null, format: null };
  chart.yAxis = { scale: 'linear', min: 0, max: null, ticks: 4, format: null };
  chart.legend = { position: 'bottom', alignment: 'start' };
  chart.animate = !prefersReducedMotion();
}

function updateLiveChartSeries(hostId, series) {
  const chart = byId(hostId);
  if (!chart) return;
  const nextSeries = [
    {
      id: 'msgsIn', name: T('chart_series_msgs_in'), tone: 'primary', style: 'solid',
      showInLegend: true, points: series.msgsIn.map((p) => ({ x: p.x, y: p.y })),
    },
    {
      id: 'lag', name: T('chart_series_lag'), tone: 'warning', style: 'dashed',
      showInLegend: true, points: series.lag.map((p) => ({ x: p.x, y: p.y })),
    },
  ];
  // Defensive: `updateSeries` is the incremental path (smooth scroll,
  // no SVG teardown) on a current `tf-line-chart`; the plain `series =`
  // setter is still a correct fallback (same shape → it now takes the same
  // incremental path internally anyway) if an older component build ever
  // ends up loaded without it.
  if (typeof chart.updateSeries === 'function') chart.updateSeries(nextSeries);
  else chart.series = nextSeries;
}

// =============================================================================
// Topics tab (M01)
// =============================================================================

async function loadTopics() {
  try {
    state.topics = await ApiBinary.list('busTopicListRequest', { arrayKey: 'topics', payload: { instanceId: requireInstanceId(state.instanceId) } });
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
    state.topics = [];
  }
  state.topicsLoaded = true;
  if (state.tab === 'topics' && !state.view) paintTopicsTable();
  // Covers the race where a user switches to the DLQ tab BEFORE this initial
  // `loadTopics()` (kicked off in parallel from `mount()`) resolves: `setTab`'s
  // own `ensureDlqTabReady()` call ran too early to see any topics yet, so the
  // tab would otherwise sit on its loading placeholder forever once topics
  // finally arrive. Calling the SAME single-source-of-truth helper here (not
  // a second, divergent code path) keeps `state.dlqSource` deterministic
  // regardless of which of the two triggers fires first (R3-1).
  if (state.tab === 'dlq') ensureDlqTabReady();
}

function filteredTopics() {
  const q = state.search.trim().toLowerCase();
  return state.topics.filter((t) => {
    if (q && !t.name.toLowerCase().includes(q)) return false;
    if (state.envFilter && t.environment !== state.envFilter) return false;
    return true;
  });
}

function renderTopicsTab(panel) {
  const rebuilt = ensureSkeleton(panel, 'topics', topicsSkeletonHtml);
  if (rebuilt) { wireTopicsSkeleton(panel); ensureLiveChart('tb-chart-live'); }
  paintKpiStrip();
  paintTopicsTable();
  // Re-entering the Topics tab shows the already-accumulated window
  // immediately (refreshStats keeps sampling `state.chartSeries` even while
  // this tab is hidden) instead of waiting for the next 3s poll tick.
  updateLiveChartSeries('tb-chart-live', state.chartSeries);
}

function topicsSkeletonHtml() {
  return `
    ${kpiStripHtml()}
    <div class="tb-toolbar">
      <tf-searchbox id="tb-search" placeholder="${escapeAttr(T('search_placeholder'))}"></tf-searchbox>
      <tf-select id="tb-env-filter" value="">
        <option value="">${escapeHtml(T('filter_env_all'))}</option>
        <option value="dev">${escapeHtml(T('filter_env_dev'))}</option>
        <option value="test">${escapeHtml(T('filter_env_test'))}</option>
        <option value="prod">${escapeHtml(T('filter_env_prod'))}</option>
      </tf-select>
    </div>
    <div class="tb-card">
      <div class="tb-c-body tb-c-body--table">
        <tf-table id="tb-topics-table" variant="flush">
          <tf-column key="name" label="${escapeAttr(T('col_name'))}" renderer="html" sortable fill></tf-column>
          <tf-column key="msgsInPerSec" label="${escapeAttr(T('col_msgs_in'))}" renderer="num" hide-below="1180"></tf-column>
          <tf-column key="lag" label="${escapeAttr(T('col_lag'))}" renderer="html" hide-below="1024"></tf-column>
          <tf-column key="dlqDepth" label="${escapeAttr(T('col_dlq_depth'))}" renderer="num" hide-below="1180"></tf-column>
          <tf-column key="disk" label="${escapeAttr(T('col_disk'))}" hide-below="1280"></tf-column>
          <tf-column key="partitions" label="${escapeAttr(T('col_partitions'))}" renderer="num" hide-below="900"></tf-column>
          <tf-column key="retention" label="${escapeAttr(T('col_retention'))}" hide-below="900"></tf-column>
          <tf-column key="replication" label="${escapeAttr(T('col_replication'))}"></tf-column>
          <tf-column key="durabilityClass" label="${escapeAttr(T('col_durability_class'))}" renderer="html" hide-below="1024"></tf-column>
          <tf-column key="environment" label="${escapeAttr(T('col_environment'))}" renderer="chip"></tf-column>
          <tf-column key="cleanup" label="${escapeAttr(T('col_cleanup'))}" hide-below="1024"></tf-column>
          <tf-column key="updated" label="${escapeAttr(T('col_updated'))}" hide-below="1024"></tf-column>
        </tf-table>
        <div id="tb-topics-empty" hidden></div>
      </div>
    </div>
  `;
}

function wireTopicsSkeleton(panel) {
  const search = panel.querySelector('#tb-search');
  search?.addEventListener('search', (e) => { state.search = e.detail?.value || ''; paintTopicsTable(); });
  const env = panel.querySelector('#tb-env-filter');
  env?.addEventListener('change', (e) => { state.envFilter = e.detail?.value || ''; paintTopicsTable(); });
  const table = panel.querySelector('#tb-topics-table');
  if (table) {
    wireRowKeyboardActivation(table);
    table.rowActions = (row) => {
      const wrap = document.createElement('div');
      wrap.className = 'tb-row-actions';
      const previewBtn = document.createElement('tf-button');
      previewBtn.setAttribute('variant', 'ghost');
      previewBtn.setAttribute('size', 'sm');
      previewBtn.setAttribute('icon', 'eye');
      previewBtn.title = T('action_preview');
      previewBtn.addEventListener('click', (e) => { e.stopPropagation(); openMessagePreview(row._topicName, row.partitions); });
      wrap.appendChild(previewBtn);
      if (canAdmin()) {
        const delBtn = document.createElement('tf-button');
        delBtn.setAttribute('variant', 'ghost');
        delBtn.setAttribute('size', 'sm');
        delBtn.setAttribute('icon', 'trash');
        delBtn.title = T('action_delete');
        delBtn.addEventListener('click', (e) => { e.stopPropagation(); confirmDeleteTopic(row._topicName); });
        wrap.appendChild(delBtn);
      }
      return wrap;
    };
    table.addEventListener('row-click', (e) => openTopicDetail(e.detail.row._topicName));
  }
}

// Small severity classification for the topics table's raw `total_lag`
// column — unlike `computeLagRatio` (group-level, has a real high-watermark
// denominator), the per-topic KPI has no such scale to divide by, so this
// is a coarse magnitude bucket, not the same ratio math M04 uses.
function topicLagCellHtml(lag) {
  const n = Number(lag) || 0;
  if (n <= 0) return fmtCompact(n);
  const status = n >= 10_000 ? 'err' : n >= 1_000 ? 'warn' : 'ok';
  return `<span class="tf-chip ${status}">${escapeHtml(fmtCompact(n))}</span>`;
}

function paintTopicsTable() {
  const table = byId('tb-topics-table');
  if (!table) return;
  const statsTopics = state.stats?.topics;
  const rows = filteredTopics().map((t) => {
    // R3-4 (KRYTYK-M1-R3.md): a DLQ topic used to get `ts = null` outright,
    // blanking MSG/S, LAG, DLQ *and* DYSK alike — but the KPI strip's
    // "Log na dysku" total already sums every topic's real bytes-on-disk,
    // DLQ topics included, so the DYSK column's `—` made the column's own
    // sum silently disagree with the KPI it should reconcile with (89 KB vs
    // 97 KB in the krytyk's repro). Msg/s, lag and DLQ-depth genuinely do
    // not apply to a topic that IS a dead-letter queue (no consumer lag of
    // its own, no nested DLQ) and stay "—"; disk usage is a real, meaningful
    // number for it and is shown like any other topic's.
    const ts = findTopicStats(statsTopics, t.name);
    const dlqTs = t.isDlq ? null : ts;
    return {
      name: `${escapeHtml(t.name)}${t.isDlq ? ` <span class="tf-chip warn">${escapeHtml(T('badge_dlq'))}</span>` : ''}`,
      msgsInPerSec: dlqTs ? fmtCompact(dlqTs.msgsInPerSec) : '—',
      lag: dlqTs ? topicLagCellHtml(dlqTs.totalLag) : '—',
      dlqDepth: dlqTs ? fmtCompact(dlqTs.dlqDepth) : '—',
      disk: ts ? formatBytes(ts.totalBytesOnDisk) : '—',
      partitions: t.partitions,
      retention: formatRetentionLabel(t.retentionMs),
      replication: `${t.replicationFactor}× (${T(`acks_${t.acks}`)})`,
      durabilityClass: durabilityClassChipHtml(t),
      environment: envChip(t.environment),
      cleanup: t.cleanupPolicy,
      updated: msToDate(t.updatedAtMs),
      _class: t.isDlq ? 'tb-row-dlq' : '',
      _topicName: t.name,
    };
  });
  // Skip the `rows = …` write entirely when nothing actually changed since
  // the last paint (common on a 3s stats poll where most topics are flat
  // between ticks) — `tf-table` recycles `<tr>`/`<td>` by position and only
  // writes a cell on a real value change, but it unconditionally rebuilds
  // each row's action-cell (preview/delete buttons) on every `rows = …`
  // (see `diffRowsByKey`'s doc comment), so this avoids destroying/
  // recreating those buttons — and any hover/focus on them — on a no-op poll.
  const diff = diffRowsByKey(state.dom.topicsTableRows, rows, (r) => r._topicName);
  if (state.dom.topicsTableRows == null || diff.changed) {
    table.rows = rows;
    state.dom.topicsTableRows = rows;
  }
  makeRowsFocusable(table);
  paintTopicsEmptyState(rows.length, table);
}

// P2-2: `tf-table` renders a bare header when `rows=[]` — no message, no
// call to action (there is no built-in empty state in the shared
// component). `state.topics.length === 0` (nothing on the server yet) gets
// the same icon+title+hint+CTA treatment `services.js`'s `.empty-big` uses
// for its own "no services yet" state (a shared, already-styled global
// class, not a new one); a non-empty topic list reduced to zero rows by the
// search/env filter gets a plain "no match" message instead, matching
// `analytics.js`'s text-only `an-empty` convention for a filtered-to-empty
// result.
function paintTopicsEmptyState(visibleRowCount, table) {
  const host = byId('tb-topics-empty');
  if (!host) return;
  if (visibleRowCount > 0) {
    table.hidden = false;
    host.hidden = true;
    host.innerHTML = '';
    return;
  }
  table.hidden = true;
  host.hidden = false;
  if (state.topics.length === 0) {
    host.innerHTML = `
      <div class="empty-big">
        ${sprite('database')}
        <h3>${escapeHtml(T('topics_empty_title'))}</h3>
        <p>${escapeHtml(T('topics_empty_hint'))}</p>
        ${canAdmin() ? `<tf-button variant="primary" icon="plus" id="tb-empty-new-topic">${escapeHtml(T('new_topic'))}</tf-button>` : ''}
      </div>
    `;
    host.querySelector('#tb-empty-new-topic')?.addEventListener('click', () => openTopicWizard(null));
  } else {
    host.innerHTML = `<div class="tb-state tb-empty">${escapeHtml(T('topics_no_match'))}</div>`;
  }
}

// =============================================================================
// Keyboard access for `tf-table` rows (P2-3, WCAG 2.1.1) — the shared
// component (tentaflow-core/www/js/components/tf-table.js, out of this
// module's file scope) marks no `<tr>` focusable and only emits `row-click`
// from a mouse click, so a keyboard-only user could reach every OTHER
// control in M01/M04 but never open M03 (partitions/config/ACL/edit) or a
// group's detail panel. This is a progressive-enhancement layer added from
// outside the component instead: re-applied after every `table.rows = ...`
// (tf-table RECYCLES `<tr>` elements in place across paints — see its own
// `_renderTbody` comment — so this only needs to touch newly-created rows
// each time, not rebind every row on every poll) plus one delegated keydown
// listener per table (added once, never duplicated).
//
// P3-13 (KRYTYK-M1-R2.md): a focusable `<tr>` with no `role`/accessible name
// announces only the concatenated cell text with no cue that it activates
// anything ("lab.results01 185150 B87 dni1×").
//
// R3-3 (KRYTYK-M1-R3.md): the P3-13 fix used `role="button"` to give the row
// an accessible name — but overriding a `<tr>`'s native `role="row"` pulls
// every `<td>` out of the table's accessibility tree, so a screen reader
// stops announcing "column Commit mode: explicit" per cell and instead reads
// the whole row as one giant button with a long comma-joined label. The row
// keeps its NATIVE role (no override — table semantics stay intact) plus
// `tabindex="0"`, its `aria-label`, and an `aria-describedby` pointing at one
// shared, visually-hidden hint per table explaining that Enter/Space opens
// the row's details.
// =============================================================================

function ensureRowActivationHint(table) {
  const root = table.shadowRoot;
  if (!root) return null;
  let hint = root.getElementById('tb-row-activation-hint');
  if (!hint) {
    hint = document.createElement('span');
    hint.id = 'tb-row-activation-hint';
    hint.className = 'tf-visually-hidden';
    hint.textContent = T('row_activate_hint');
    root.appendChild(hint);
  }
  return hint;
}

function makeRowsFocusable(table) {
  const hint = ensureRowActivationHint(table);
  table.shadowRoot?.querySelectorAll('tbody tr[data-idx]').forEach((tr) => {
    if (!tr.hasAttribute('tabindex')) tr.setAttribute('tabindex', '0');
    if (hint) tr.setAttribute('aria-describedby', hint.id);
    const label = Array.from(tr.querySelectorAll('td'))
      .map((td) => td.textContent.trim())
      .filter(Boolean)
      .join(', ');
    if (label) tr.setAttribute('aria-label', label);
  });
}

// Activates the focused row with Enter/Space exactly like a mouse click.
// `keydown` is a composed, bubbling event, so a single listener on the host
// element (light DOM) sees it even though the actual `<tr>` lives inside
// `table`'s shadow root. Only fires when the ROW ITSELF is the original
// target (`composedPath()[0]`) — a focused action button/icon inside the
// row already gets its own native Enter/Space→click, and re-triggering the
// row's own navigation on top of that would double-activate.
function wireRowKeyboardActivation(table) {
  table.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' && e.key !== ' ' && e.key !== 'Spacebar') return;
    const origin = e.composedPath()[0];
    if (!(origin instanceof HTMLElement) || origin.tagName !== 'TR' || origin.dataset.idx == null) return;
    e.preventDefault();
    origin.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  });
}

function envChip(env) {
  const status = env === 'prod' ? 'err' : env === 'test' ? 'warn' : 'ok';
  return { status, label: T(`env_${env}`) || env };
}

// Owner decision B: the M01 list column and the M03 config tab both render
// the SAME chip for a topic's durability class — critical highlighted (err
// tone, the same one the lag column already uses for "hot"), standard muted
// — with the resolved `durability` policy string (e.g. `fsync_interval:50`,
// `fsync_batch_full`, `os`) as its tooltip so an operator can see both "how
// safe" (the class) and "how, exactly" (the policy) without a second tab.
//
// R5-1/R5-7 fix (KRYTYK-M1-R5.md b.1/b.7): `deriveDurabilityClass` above
// already trusted a wire-supplied `durabilityClass` first and only fell back
// to deriving one from `durability` when it was absent — the R5 "dead
// column" bug was that the LIST wire (`TopicList` rows) never carried either
// field yet, so every row hit the fallback with an empty `durability` string
// and defaulted to "standard" regardless of the real policy. The backend
// contract now sends `durability`/`durabilityClass`/`durabilityExplicit` on
// both the list and the detail wire, so this needs no change on the
// derivation side — only the NEW "(polityka jawna)" secondary label below,
// which the R5 report flagged as impossible without a stored
// class-vs-override distinction (`durabilityExplicit` is exactly that).
function durabilityClassChipHtml(topic) {
  const cls = deriveDurabilityClass(topic);
  const status = cls === 'critical' ? 'err' : 'neutral';
  const label = T(`durability_class_chip_${cls}`);
  const durability = topic?.durability;
  const title = durability ? escapeAttr(T('durability_class_policy_title', { durability })) : '';
  const chip = `<span class="tf-chip ${status}"${title ? ` title="${title}"` : ''}>${escapeHtml(label)}</span>`;
  if (!shouldShowDurabilityExplicitLabel(topic)) return chip;
  return `${chip} <span class="tb-field-hint tb-durability-explicit">${escapeHtml(T('durability_class_explicit_suffix'))}</span>`;
}

function formatRetentionLabel(ms) {
  const preset = retentionPresetFromMs(ms);
  if (preset !== 'custom') return T(`retention_${preset}`);
  const days = Math.round(Number(ms) / 86_400_000);
  return T('retention_custom_days', { days });
}

function msToDate(ms) {
  if (ms == null) return '—';
  const d = new Date(Number(ms));
  if (Number.isNaN(d.getTime())) return '—';
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

// P3-14 (KRYTYK-M1-R2.md): the DLQ record detail's raw header dump rendered
// `dlq.first_failed_at_ms`/`dlq.last_failed_at_ms` as a bare epoch
// ("1787862468957") — every OTHER millisecond timestamp in this module goes
// through `msToDate`, these two just happened to be decoded like any other
// header's bytes. Only touches `_at_ms`-suffixed keys whose decoded text is
// purely numeric, so a non-numeric or unrelated header is never mangled.
function formatHeaderValue(key, text) {
  if (typeof key === 'string' && key.endsWith('_at_ms') && /^\d+$/.test(text)) {
    return msToDate(Number(text));
  }
  return text;
}

async function confirmDeleteTopic(name) {
  const ok = await tbConfirm({
    title: T('confirm_delete_topic_title'),
    body: T('confirm_delete_topic_body', { name }),
    confirmLabel: T('common_delete'),
  });
  if (!ok) return;
  try {
    await ApiBinary.action('busTopicDeleteRequest', { instanceId: requireInstanceId(state.instanceId), name });
    toast(T('deleted'), 'success');
    if (state.view?.name === name) { state.view = null; renderPanel(); }
    await loadTopics();
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
  }
}

// =============================================================================
// Topic creator/editor wizard (M02)
//
// R5-4 fix (KRYTYK-M1-R5.md b.4, P2): the class radio section below uses
// NATIVE <input type="radio"> instead of the shared <tf-radio-group>/
// <tf-radio> (tentaflow-core/www/js/components/tf-radio.js). That component
// implements roving tabindex (Tab enters/leaves the group once) but never
// wired arrow-key navigation, the other half of the APG radio pattern, so
// the second option ("Krytyczna") was reachable by mouse only; its own
// role="radio" also sits on an empty <span> with the visible label as an
// unassociated sibling, so it had no accessible name either. tf-radio.js is
// a shared component used elsewhere in the app and out of this fala's file
// scope, so per the task brief this falls back to native radios instead of
// patching it: <fieldset>/<legend> plus one <label for> per <input> give
// Tab-enters-once, arrow-key move+select, Space-select, and a real
// accessible name for the group and each option, all from the platform.
// =============================================================================

function openTopicWizard(existing) {
  const isEdit = !!existing;
  // R5-2 fix (KRYTYK-M1-R5.md b.2, P1): the OLD prefill —
  // `existing ? existing.durability : 'auto'` — put the topic's resolved
  // policy into the advanced select for EVERY edit, including one whose
  // policy is purely class-derived (`durabilityExplicit: false`). Once that
  // resolved string happened to match a select option, it was submitted back
  // as an EXPLICIT override (`buildTopicOptionsWire`'s "jawne wygrywa" rule),
  // silently pinning the topic to whatever policy the class used to derive
  // to — so switching the class radio afterwards (critical → standard, the
  // repro in the krytyk report) changed nothing in the database. Only an
  // ALREADY-explicit policy is worth prefilling; everything else starts on
  // "Automatycznie (wg klasy)" so a class-only edit stays class-only.
  const hadExplicitDurability = isEdit && existing.durabilityExplicit === true;
  const existingDurability = hadExplicitDurability ? String(existing.durability || '') : 'auto';
  const isFsyncIntervalDurability = existingDurability.startsWith('fsync_interval:');
  const form = {
    name: existing?.name ?? '',
    partitions: existing?.partitions ?? 8,
    retentionPreset: existing ? retentionPresetFromMs(existing.retentionMs) : '7d',
    retentionMs: existing?.retentionMs ?? RETENTION_PRESETS_MS['7d'],
    replicationFactor: existing?.replicationFactor ?? 3,
    schemaId: existing?.schemaId ?? '',
    validation: existing?.validation ?? 'off',
    delivery: existing?.delivery ?? 'at_least_once',
    dedupWindowMs: existing?.dedupWindowMs ?? RETENTION_PRESETS_MS['24h'],
    maxDeliveryAttempts: existing?.maxDeliveryAttempts ?? 5,
    retryBackoffMs: existing?.retryBackoffMs ?? 1000,
    contentType: existing?.contentType ?? 'application/octet-stream',
    durability: isFsyncIntervalDurability ? 'fsync_interval' : existingDurability,
    fsyncIntervalMs: isFsyncIntervalDurability
      ? clampFsyncIntervalMs(existingDurability.split(':')[1])
      : 50,
    durabilityWasExplicit: hadExplicitDurability,
    durabilityClass: existing ? deriveDurabilityClass(existing) : 'standard',
    acks: existing ? existing.acks : 'auto',
    maxInlineBytes: existing?.maxInlineBytes ?? 1_048_576,
    compression: existing?.compression ?? 'lz4',
    cleanupPolicy: existing?.cleanupPolicy ?? 'delete',
  };

  const body = document.createElement('div');
  body.className = 'tb-wizard-form';
  body.innerHTML = `
    <tf-input id="tb-w-name" label="${escapeAttr(T('wizard_field_name'))}" value="${escapeAttr(form.name)}"
      hint="${escapeAttr(T('wizard_field_name_hint'))}" ${isEdit ? 'disabled' : ''}></tf-input>
    <div class="tb-wizard-grid">
      <tf-input id="tb-w-partitions" type="text" inputmode="numeric" label="${escapeAttr(T('wizard_field_partitions'))}"
        value="${escapeAttr(form.partitions)}" hint="${escapeAttr(T('wizard_field_partitions_hint'))}"></tf-input>
      <tf-select id="tb-w-retention" label="${escapeAttr(T('wizard_field_retention'))}" value="${escapeAttr(form.retentionPreset)}">
        <option value="24h">${escapeHtml(T('retention_24h'))}</option>
        <option value="7d">${escapeHtml(T('retention_7d'))}</option>
        <option value="30d">${escapeHtml(T('retention_30d'))}</option>
        <option value="90d">${escapeHtml(T('retention_90d'))}</option>
        <option value="365d">${escapeHtml(T('retention_365d'))}</option>
      </tf-select>
    </div>

    <div class="tb-card" style="margin-bottom:0">
      <div class="tb-c-head"><h3>${escapeHtml(T('wizard_section_redundancy'))}</h3></div>
      <div class="tb-c-body">
        <div class="tb-rf-row">
          <span>${escapeHtml(T('wizard_field_rf'))}</span>
          <span class="tb-rf-stepper">
            <button type="button" id="tb-w-rf-dec" aria-label="${escapeAttr(T('wizard_rf_dec'))}">−</button>
            <output id="tb-w-rf-value">${form.replicationFactor}</output>
            <button type="button" id="tb-w-rf-inc" aria-label="${escapeAttr(T('wizard_rf_inc'))}">+</button>
          </span>
          <tf-chip id="tb-w-rf-warning" status="warn" class="tb-rf-warning ${form.replicationFactor === 1 ? 'show' : ''}">${escapeHtml(T('wizard_rf_warning'))}</tf-chip>
        </div>
        <p class="tb-field-hint">${escapeHtml(T('wizard_nodes_gap_note'))}</p>
        <div id="tb-w-node-picker-wrap" class="tb-node-picker-wrap">
          <div class="tb-state" id="tb-w-node-picker-loading"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('replication.nodes_loading'))}</div>
          <fieldset class="tb-node-picker" id="tb-w-node-picker" hidden>
            <legend class="tf-visually-hidden">${escapeHtml(T('wizard_field_nodes'))}</legend>
          </fieldset>
        </div>
      </div>
    </div>

    <div class="tb-card" style="margin-bottom:0">
      <div class="tb-c-head"><h3 id="tb-w-durability-class-heading">${escapeHtml(T('wizard_section_durability_class'))}</h3></div>
      <div class="tb-c-body">
        <fieldset class="tb-radio-native-group" id="tb-w-durability-class-group"
          aria-labelledby="tb-w-durability-class-heading">
          <legend class="tf-visually-hidden">${escapeHtml(T('wizard_section_durability_class'))}</legend>
          <label class="tb-radio-native" for="tb-w-durability-standard">
            <input type="radio" id="tb-w-durability-standard" name="tb-w-durability-class" value="standard"
              ${form.durabilityClass === 'standard' ? 'checked' : ''}
              aria-describedby="tb-w-durability-standard-hint" />
            <span class="tb-radio-native-text">
              <span class="tb-radio-native-label">${escapeHtml(T('wizard_durability_class_standard'))}</span>
              <span class="tb-radio-native-hint" id="tb-w-durability-standard-hint">${escapeHtml(T('wizard_durability_class_standard_hint'))}</span>
            </span>
          </label>
          <label class="tb-radio-native" for="tb-w-durability-critical">
            <input type="radio" id="tb-w-durability-critical" name="tb-w-durability-class" value="critical"
              ${form.durabilityClass === 'critical' ? 'checked' : ''}
              aria-describedby="tb-w-durability-critical-hint" />
            <span class="tb-radio-native-text">
              <span class="tb-radio-native-label">${escapeHtml(T('wizard_durability_class_critical'))}</span>
              <span class="tb-radio-native-hint" id="tb-w-durability-critical-hint">${escapeHtml(T('wizard_durability_class_critical_hint'))}</span>
            </span>
          </label>
        </fieldset>
        <tf-chip id="tb-w-durability-explicit-warn" status="warn"
          class="tb-durability-explicit-warn${form.durability !== 'auto' ? ' show' : ''}">${escapeHtml(T('wizard_durability_class_inactive_warning'))}</tf-chip>
        <p class="tb-field-hint">${escapeHtml(T('wizard_durability_class_latency_note'))}</p>
        ${isEdit && form.name.startsWith('__dlq.') ? `<p class="tb-field-hint tb-dlq-durability-note">${escapeHtml(T('wizard_durability_class_dlq_note'))}</p>` : ''}
      </div>
    </div>

    <button type="button" class="tb-adv-toggle" id="tb-w-adv-toggle" aria-expanded="false" aria-controls="tb-w-adv-body">
      <span>${escapeHtml(T('wizard_section_advanced'))}<span class="tb-adv-sub">${escapeHtml(T('wizard_section_advanced_sub'))}</span></span>
      ${sprite('chevron-down')}
    </button>
    <div class="tb-adv-body" id="tb-w-adv-body" hidden>
      <tf-input id="tb-w-schema" label="${escapeAttr(T('wizard_field_schema'))}" value="${escapeAttr(form.schemaId)}"
        placeholder="${escapeAttr(T('wizard_field_schema_hint'))}"></tf-input>
      <div class="tb-wizard-grid">
        <tf-select id="tb-w-validation" label="${escapeAttr(T('wizard_field_validation'))}" value="${escapeAttr(form.validation)}">
          <option value="off">${escapeHtml(T('wizard_validation_off'))}</option>
          <option value="warn">${escapeHtml(T('wizard_validation_warn'))}</option>
          <option value="dlq">${escapeHtml(T('wizard_validation_dlq'))}</option>
        </tf-select>
        <tf-select id="tb-w-delivery" label="${escapeAttr(T('wizard_field_delivery'))}" value="${escapeAttr(form.delivery)}">
          <option value="at_least_once">${escapeHtml(T('wizard_delivery_at_least_once'))}</option>
          <option value="fire_and_forget">${escapeHtml(T('wizard_delivery_fire_and_forget'))}</option>
        </tf-select>
      </div>

      <div class="tb-disabled-row">
        <span>${escapeHtml(T('wizard_field_idempotency_key'))} — ${escapeHtml(T('wizard_idempotency_disabled_hint'))}</span>
        <tf-chip status="info">${escapeHtml(T('chip_soon_m3a'))}</tf-chip>
      </div>

      <div class="tb-wizard-grid--3">
        <tf-input id="tb-w-dedup" type="text" inputmode="numeric" label="${escapeAttr(T('wizard_field_dedup_window_h'))}"
          value="${escapeAttr(Math.round(form.dedupWindowMs / 3_600_000))}"></tf-input>
        <tf-input id="tb-w-attempts" type="text" inputmode="numeric" label="${escapeAttr(T('wizard_field_max_delivery_attempts'))}"
          value="${escapeAttr(form.maxDeliveryAttempts)}"></tf-input>
        <tf-input id="tb-w-backoff" type="text" inputmode="numeric" label="${escapeAttr(T('wizard_field_retry_backoff_ms'))}"
          value="${escapeAttr(form.retryBackoffMs)}"></tf-input>
      </div>

      <tf-input id="tb-w-content-type" label="${escapeAttr(T('wizard_field_content_type'))}" value="${escapeAttr(form.contentType)}"></tf-input>

      <div class="tb-wizard-grid">
        <tf-select id="tb-w-durability" label="${escapeAttr(T('wizard_field_durability'))}" value="${escapeAttr(form.durability)}">
          <option value="auto">${escapeHtml(T('wizard_durability_auto'))}</option>
          <option value="os">${escapeHtml(T('wizard_durability_os'))}</option>
          <option value="fsync_batch">${escapeHtml(T('wizard_durability_fsync_batch'))}</option>
          <option value="fsync_batch_full">${escapeHtml(T('wizard_durability_fsync_batch_full'))}</option>
          <option value="fsync_interval">${escapeHtml(T('wizard_durability_fsync_interval'))}</option>
        </tf-select>
        <tf-select id="tb-w-acks" label="${escapeAttr(T('wizard_field_acks'))}" value="${escapeAttr(form.acks)}">
          <option value="auto">${escapeHtml(T('wizard_auto'))}</option>
          <option value="leader">${escapeHtml(T('wizard_acks_leader'))}</option>
          <option value="quorum">${escapeHtml(T('wizard_acks_quorum'))}</option>
          <option value="all">${escapeHtml(T('wizard_acks_all'))}</option>
        </tf-select>
      </div>
      <tf-input id="tb-w-durability-fsync-ms" type="text" inputmode="numeric" min="1" max="1000"
        label="${escapeAttr(T('wizard_field_fsync_interval_ms'))}" value="${escapeAttr(form.fsyncIntervalMs)}"
        hint="${escapeAttr(T('wizard_field_fsync_interval_ms_hint'))}"
        ${form.durability === 'fsync_interval' ? '' : 'hidden'}></tf-input>
      <p class="tb-field-hint">${escapeHtml(T('wizard_durability_override_hint'))}</p>

      <div class="tb-wizard-grid">
        <tf-input id="tb-w-max-inline" type="text" inputmode="numeric" label="${escapeAttr(T('wizard_field_max_inline_kib'))}"
          value="${escapeAttr(Math.round(form.maxInlineBytes / 1024))}"></tf-input>
        <tf-select id="tb-w-compression" label="${escapeAttr(T('wizard_field_compression'))}" value="${escapeAttr(form.compression)}">
          <option value="lz4">${escapeHtml(T('wizard_compression_lz4'))}</option>
          <option value="none">${escapeHtml(T('wizard_compression_none'))}</option>
        </tf-select>
      </div>

      <div class="tb-disabled-row">
        <span>${escapeHtml(T('wizard_field_cleanup_policy'))} — ${escapeHtml(T('wizard_cleanup_compact_hint'))}</span>
        <tf-chip status="info">${escapeHtml(T('chip_soon_m5'))}</tf-chip>
      </div>
      <div class="tb-disabled-row">
        <span>${escapeHtml(T('wizard_field_encryption'))} — ${escapeHtml(T('wizard_encryption_hint'))}</span>
        <tf-chip status="info">${escapeHtml(T('chip_soon_m5'))}</tf-chip>
      </div>
    </div>
  `;

  const modal = document.createElement('tf-modal');
  modal.setAttribute('title', T(isEdit ? 'wizard_title_edit' : 'wizard_title_create'));
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('size', 'lg');
  const bodySlot = document.createElement('div');
  bodySlot.setAttribute('slot', 'body');
  bodySlot.appendChild(body);
  modal.appendChild(bodySlot);

  const footer = document.createElement('div');
  footer.setAttribute('slot', 'footer');
  footer.className = 'tb-modal-footer';
  const spacer = document.createElement('span');
  spacer.className = 'tb-spacer';
  footer.appendChild(spacer);
  const cancel = document.createElement('tf-button');
  cancel.setAttribute('variant', 'secondary');
  cancel.textContent = T('common_cancel');
  cancel.addEventListener('click', () => closeModal(modal));
  const submit = document.createElement('tf-button');
  submit.setAttribute('variant', 'primary');
  submit.setAttribute('icon', 'save');
  submit.textContent = T(isEdit ? 'wizard_submit_edit' : 'wizard_submit');
  submit.addEventListener('click', () => submitTopicWizard(modal, body, form, isEdit));
  footer.append(cancel, submit);
  modal.appendChild(footer);

  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  trapModalFocus(modal);

  const { setRf } = wireWizardBehavior(body, form);
  loadWizardNodePicker(body, form, setRf);
  modal.addEventListener('close', () => closeModal(modal), { once: true });
}

function wireWizardBehavior(body, form) {
  const nameInput = body.querySelector('#tb-w-name');
  nameInput?.addEventListener('input', () => {
    const v = nameInput.value || '';
    const valid = v === '' || isValidTopicName(v);
    nameInput.setAttribute('error', valid ? '' : T('wizard_field_name_error'));
  });

  const advToggle = body.querySelector('#tb-w-adv-toggle');
  const advBody = body.querySelector('#tb-w-adv-body');
  advToggle?.addEventListener('click', () => {
    const expanded = advToggle.getAttribute('aria-expanded') === 'true';
    advToggle.setAttribute('aria-expanded', String(!expanded));
    if (advBody) advBody.hidden = expanded;
  });

  const rfValue = body.querySelector('#tb-w-rf-value');
  const rfWarning = body.querySelector('#tb-w-rf-warning');
  const setRf = (rf) => {
    form.replicationFactor = clampReplicationFactor(rf);
    if (rfValue) rfValue.textContent = String(form.replicationFactor);
    rfWarning?.classList.toggle('show', form.replicationFactor === 1);
    const decBtn = body.querySelector('#tb-w-rf-dec');
    const incBtn = body.querySelector('#tb-w-rf-inc');
    if (decBtn) decBtn.disabled = form.replicationFactor <= 1;
    if (incBtn) incBtn.disabled = form.replicationFactor >= 7;
  };
  body.querySelector('#tb-w-rf-dec')?.addEventListener('click', () => setRf(form.replicationFactor - 1));
  body.querySelector('#tb-w-rf-inc')?.addEventListener('click', () => setRf(form.replicationFactor + 1));
  setRf(form.replicationFactor);

  // R5-2/P2 (KRYTYK-M1-R5.md b.2/b.4's fix set): the advanced "Trwałość
  // zapisu" select is the ONE control that can make the class radio above it
  // inert server-side ("jawne wygrywa" — an explicit policy always beats the
  // class). The R5 report's b.2 complaint was that this was invisible and
  // static; this keeps the warning chip's visibility live, in both
  // directions, as the operator opens/edits the advanced section — not just
  // set once at render time.
  const durabilitySelect = body.querySelector('#tb-w-durability');
  const fsyncMsInput = body.querySelector('#tb-w-durability-fsync-ms');
  const explicitWarn = body.querySelector('#tb-w-durability-explicit-warn');
  durabilitySelect?.addEventListener('change', () => {
    const value = durabilitySelect.value;
    if (fsyncMsInput) fsyncMsInput.hidden = value !== 'fsync_interval';
    explicitWarn?.classList.toggle('show', value !== 'auto');
  });

  return { setRf };
}

// M2's node picker (module-doc gap #3): fetches `ReplicaListResponse.nodes`
// (topic omitted — the org-wide roster) + this session's own environment
// AFTER the modal is already open and interactive (never blocks the
// wizard), then renders one checkbox per node, same-env only selectable
// (SPEC D4 — a foreign-env node stays visible, disabled, with a tooltip).
// The wire has NO node-selection field on `BusTopicOptionsWire` (still true
// after M2 — see gap #3's updated doc), so this list never feeds the
// submit payload directly: checking/unchecking nodes only calls `setRf`
// with the same-env checked count, keeping the (real, wired) RF stepper
// and this (informational) picker in lockstep in BOTH directions.
async function loadWizardNodePicker(body, form, setRf) {
  const loadingEl = body.querySelector('#tb-w-node-picker-loading');
  const fieldset = body.querySelector('#tb-w-node-picker');
  if (!fieldset) return;
  let nodes = [];
  let localEnv = null;
  try {
    const [resp, env] = await Promise.all([
      ApiBinary.one('busReplicaListRequest', buildReplicaListRequest(state.instanceId, undefined)),
      getLocalEnvironment(),
    ]);
    nodes = Array.isArray(resp?.nodes) ? resp.nodes : [];
    localEnv = env;
  } catch {
    nodes = [];
  }
  // `body` may already be detached (modal closed while the request was in
  // flight) — writing into a detached fieldset is harmless but wiring
  // listeners on it would leak nothing (no more events fire on a detached
  // node), so this is a plain best-effort guard, not a correctness one.
  if (loadingEl) loadingEl.hidden = true;
  if (!nodes.length) {
    fieldset.hidden = true;
    return;
  }
  fieldset.hidden = false;
  fieldset.innerHTML = nodes.map((n) => {
    const foreign = !isSameEnvironment(n, localEnv);
    return `
      <label class="tb-node-picker-item${foreign ? ' is-foreign' : ''}"${foreign ? ` title="${escapeAttr(T('wizard_nodes_foreign_tooltip'))}"` : ''}>
        <input type="checkbox" value="${escapeAttr(n.nodeId)}" ${foreign ? 'disabled' : ''} />
        <span class="tb-node-picker-name">${escapeHtml(n.label || n.nodeId)}</span>
        ${chipHtml(envChip(n.environment))}
      </label>
    `;
  }).join('');
  wireNodePicker(body, fieldset, form, setRf);
}

// Bidirectional sync (checkboxes <-> the stepper's number) so the two
// controls can never disagree about "how many", even though only the
// STEPPER's number ever reaches the wire (`replicationFactor` — see this
// function's caller's doc). Selection order is simply DOM/array order —
// there is no ranking signal from the server to prefer one healthy node
// over another.
function wireNodePicker(body, fieldset, form, setRf) {
  const checkboxes = Array.from(fieldset.querySelectorAll('input[type="checkbox"]'));
  const selectable = checkboxes.filter((c) => !c.disabled);
  const checkedCount = () => selectable.filter((c) => c.checked).length;

  checkboxes.forEach((cb) => {
    cb.addEventListener('change', () => setRf(checkedCount()));
  });

  const syncPickerToRf = () => {
    let current = checkedCount();
    const target = form.replicationFactor;
    for (let i = 0; current < target && i < selectable.length; i += 1) {
      if (!selectable[i].checked) { selectable[i].checked = true; current += 1; }
    }
    for (let i = selectable.length - 1; current > target && i >= 0; i -= 1) {
      if (selectable[i].checked) { selectable[i].checked = false; current -= 1; }
    }
  };
  // A SECOND click listener on the SAME +/- buttons `wireWizardBehavior`'s
  // own `setRf` is already bound to (added first, when the modal opened —
  // this function only runs once the async node fetch resolves). Listener
  // order is add order, so `setRf` has always already mutated
  // `form.replicationFactor` by the time this one reads it — never a stale
  // target.
  body.querySelector('#tb-w-rf-dec')?.addEventListener('click', syncPickerToRf);
  body.querySelector('#tb-w-rf-inc')?.addEventListener('click', syncPickerToRf);
  syncPickerToRf(); // Initial paint: pre-check nodes to match the current RF.
}

async function submitTopicWizard(modal, body, form, isEdit) {
  const name = body.querySelector('#tb-w-name')?.value?.trim() || form.name;
  if (!isEdit && !isValidTopicName(name)) {
    toast(T('wizard_field_name_error'), 'error');
    return;
  }
  const retentionPreset = body.querySelector('#tb-w-retention')?.value || '7d';
  // R5-4: the class radio is now native `<input type="radio">`, not
  // `<tf-radio-group>` — its selected value lives on the checked input, not
  // on a wrapper's own `.value`.
  const durabilityClassValue = body.querySelector('input[name="tb-w-durability-class"]:checked')?.value
    || form.durabilityClass;
  const durabilitySelectValue = body.querySelector('#tb-w-durability')?.value;
  const durabilityWireValue = durabilitySelectValue === 'fsync_interval'
    ? formatFsyncIntervalDurability(body.querySelector('#tb-w-durability-fsync-ms')?.value)
    : durabilitySelectValue;
  const wireForm = {
    partitions: body.querySelector('#tb-w-partitions')?.value,
    retentionMs: RETENTION_PRESETS_MS[retentionPreset] ?? RETENTION_PRESETS_MS['7d'],
    replicationFactor: form.replicationFactor,
    schemaId: body.querySelector('#tb-w-schema')?.value,
    validation: body.querySelector('#tb-w-validation')?.value,
    delivery: body.querySelector('#tb-w-delivery')?.value,
    dedupWindowMs: Number(body.querySelector('#tb-w-dedup')?.value || 0) * 3_600_000,
    maxDeliveryAttempts: body.querySelector('#tb-w-attempts')?.value,
    retryBackoffMs: body.querySelector('#tb-w-backoff')?.value,
    contentType: body.querySelector('#tb-w-content-type')?.value,
    durability: durabilityWireValue,
    // R5-2: only a DELIBERATE "Automatycznie (wg klasy)" pick on a topic
    // that already had an explicit policy sends the literal clearing signal
    // `durability: "auto"` — every other "left on Automatycznie" case omits
    // `durability` entirely so a class-only edit never touches it.
    durabilityAutoClear: form.durabilityWasExplicit === true && durabilitySelectValue === 'auto',
    durabilityClass: durabilityClassValue,
    acks: body.querySelector('#tb-w-acks')?.value,
    maxInlineBytes: Number(body.querySelector('#tb-w-max-inline')?.value || 0) * 1024,
    compression: body.querySelector('#tb-w-compression')?.value,
  };
  const options = buildTopicOptionsWire(wireForm);
  try {
    if (isEdit) {
      await ApiBinary.action('busTopicUpdateRequest', { instanceId: requireInstanceId(state.instanceId), name, options });
      toast(T('saved'), 'success');
      if (state.view?.name === name) await loadTopicDetail(name);
    } else {
      await ApiBinary.action('busTopicCreateRequest', { instanceId: requireInstanceId(state.instanceId), name, options });
      toast(T('created'), 'success');
    }
    closeModal(modal);
    await loadTopics();
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
  }
}

function closeModal(modal) {
  modal.removeAttribute('open');
  setTimeout(() => modal.remove(), 300);
}

// =============================================================================
// Modal focus trap (`<tf-modal>`, tentaflow-core/www/js/components/tf-modal.js,
// has none — Tab cycles out into the page behind the dialog and focus never
// moves into the dialog on open, WCAG 2.1.1/2.4.3). `tf-modal.js` is a
// shared component outside this file's change scope, so every dialog THIS
// module builds traps focus itself instead: `openTopicWizard`,
// `openOffsetResetModal`, `openMessagePreview` call `trapModalFocus`
// directly, and `tbConfirm` (below) replaces the confirm boxes that used to
// go through `TfModal.open()` — the static helper builds its own `<tf-modal>`
// with no way for a caller to reach in and wire a trap on it.
// =============================================================================

// `tf-button`/`tf-input`/`tf-select` (the controls every dialog in this
// module is built from) are light-DOM wrappers around a REAL
// `<button>`/`<input>`/`<select>` — that inner native element is what the
// browser actually places in the Tab order, and it is what the plain tag
// selectors below already match. Listing the wrapper custom elements too
// would add a second, non-focusable "candidate" right before each real one
// (parent precedes child in document order), which could end up chosen as
// the trap's computed first/last element and silently swallow the initial
// autofocus / a wrap-around `.focus()` call.
const FOCUSABLE_SELECTOR = [
  'a[href]', 'button:not([disabled])', 'textarea:not([disabled])',
  'input:not([disabled])', 'select:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(', ');

function focusableElements(container) {
  return Array.from(container.querySelectorAll(FOCUSABLE_SELECTOR))
    .filter((el) => !el.hasAttribute('disabled') && el.getClientRects().length > 0);
}

// Moves focus into `modal` on open, cycles Tab/Shift+Tab within it while
// open, and restores focus to whatever had it before the dialog opened once
// it closes — the three pieces `tf-modal.js` is missing today.
function trapModalFocus(modal) {
  const previouslyFocused = document.activeElement;
  const card = modal._card || modal;
  // Ensures `card.focus()` below actually moves focus even for a dialog
  // with no focusable field of its own (e.g. a body that is pure text) —
  // `tabindex="-1"` makes an element programmatically focusable without
  // adding it to the normal Tab order.
  if (!card.hasAttribute('tabindex')) card.setAttribute('tabindex', '-1');

  const onKeydown = (e) => {
    if (e.key !== 'Tab') return;
    const focusables = focusableElements(card);
    if (!focusables.length) { e.preventDefault(); return; }
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };
  document.addEventListener('keydown', onKeydown, true);

  // `_build()`/`_update()` run synchronously off `setAttribute('open', '')`,
  // but layout (`getClientRects()` inside `focusableElements`) needs one
  // frame to settle before an element is reliably reported as visible.
  requestAnimationFrame(() => {
    const focusables = focusableElements(card);
    (focusables[0] || card).focus();
  });

  // Watches the `open` ATTRIBUTE rather than the `close` EVENT: `tf-modal`
  // only dispatches `close` from its own Escape/backdrop/X dismissal path
  // (`_dismiss()`); every close button THIS module wires (wizard Cancel,
  // `tbConfirm`'s Cancel/confirm, offset-reset Cancel, preview's own close)
  // calls `closeModal()`/`finish()` directly, which just removes the `open`
  // attribute without dispatching that event — a `close`-event-only cleanup
  // would leak the document keydown listener and skip focus restoration on
  // every one of those button paths.
  const observer = new MutationObserver(() => {
    if (modal.hasAttribute('open')) return;
    observer.disconnect();
    document.removeEventListener('keydown', onKeydown, true);
    if (previouslyFocused && typeof previouslyFocused.focus === 'function') {
      previouslyFocused.focus();
    }
  });
  observer.observe(modal, { attributes: true, attributeFilter: ['open'] });
}

// Local replacement for `TfModal.open()` used only for THIS module's confirm
// boxes (delete topic / discard DLQ record / retry-all) — same look
// (title + body text + Cancel/primary buttons), but built directly so
// `trapModalFocus` can be wired on it, which the shared static helper's
// internally-created `<tf-modal>` gives no way to reach.
function tbConfirm({ title, body, confirmLabel, cancelLabel }) {
  return new Promise((resolve) => {
    const modal = document.createElement('tf-modal');
    modal.setAttribute('title', title);
    modal.setAttribute('variant', 'modal');
    modal.setAttribute('size', 'sm');

    const bodySlot = document.createElement('div');
    bodySlot.setAttribute('slot', 'body');
    bodySlot.textContent = body;
    modal.appendChild(bodySlot);

    const footer = document.createElement('div');
    footer.setAttribute('slot', 'footer');
    footer.className = 'tb-modal-footer';
    const spacer = document.createElement('span');
    spacer.className = 'tb-spacer';
    footer.appendChild(spacer);

    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
      closeModal(modal);
    };

    const cancel = document.createElement('tf-button');
    cancel.setAttribute('variant', 'secondary');
    cancel.textContent = cancelLabel ?? T('common_cancel');
    cancel.addEventListener('click', () => finish(false));
    const confirm = document.createElement('tf-button');
    confirm.setAttribute('variant', 'primary');
    confirm.textContent = confirmLabel;
    confirm.addEventListener('click', () => finish(true));
    footer.append(cancel, confirm);
    modal.appendChild(footer);

    document.body.appendChild(modal);
    modal.setAttribute('open', '');
    trapModalFocus(modal);
    modal.addEventListener('close', () => finish(false), { once: true });
  });
}

// =============================================================================
// Topic detail (M03) — overview / partitions / config / ACL
// =============================================================================

function openTopicDetail(name) {
  state.view = { kind: 'topic-detail', name };
  state.detailTab = 'overview';
  state.detail = null;
  state.aclEntries = null;
  // A fresh rolling window per topic — the previous topic's samples would
  // otherwise leak into this one's overview chart (module-doc gap #1).
  state.detailChartSeries = { msgsIn: [], bytesIn: [], lag: [] };
  renderPanel();
  loadTopicDetail(name);
}

async function loadTopicDetail(name) {
  state.detailLoading = true;
  renderDetailBody();
  try {
    state.detail = await ApiBinary.one('busTopicDetailRequest', { instanceId: requireInstanceId(state.instanceId), name });
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
    state.detail = null;
    state.view = null;
    renderPanel();
    return;
  }
  state.detailLoading = false;
  // A REAL context change (new/refreshed topic snapshot) — never bumped by a
  // stats poll — so `renderDetailBody` can tell "rebuild the hero/overview
  // DOM" apart from "just patch the numbers in place".
  state.detailVersion += 1;
  renderDetailBody();
}

function renderTopicDetail(panel) {
  const viewName = state.view.name;
  const rebuilt = ensureSkeleton(panel, `detail:${viewName}`, () => detailSkeletonHtml(viewName));
  if (rebuilt) wireDetailSkeleton(panel, viewName);
  renderDetailBody();
}

function detailSkeletonHtml(name) {
  return `
    <div class="tb-back">
      <tf-button variant="ghost" icon="chevron-left" id="tb-detail-back">${escapeHtml(T('detail_back'))}</tf-button>
    </div>
    <div class="tb-card">
      <div class="tb-c-body" id="tb-detail-hero"></div>
    </div>
    <tf-tabs id="tb-detail-tabs" value="${escapeAttr(state.detailTab)}" variant="solid">
      <tf-tab id="overview">${escapeHtml(T('detail_tab_overview'))}</tf-tab>
      <tf-tab id="partitions">${escapeHtml(T('detail_tab_partitions'))}</tf-tab>
      <tf-tab id="config">${escapeHtml(T('detail_tab_config'))}</tf-tab>
      <tf-tab id="acl">${escapeHtml(T('detail_tab_acl'))}</tf-tab>
    </tf-tabs>
    <div id="tb-detail-panel"></div>
  `;
}

function wireDetailSkeleton(panel, name) {
  panel.querySelector('#tb-detail-back')?.addEventListener('click', () => {
    state.view = null;
    renderPanel();
    paintTopicsTable();
  });
  panel.querySelector('#tb-detail-tabs')?.addEventListener('change', (e) => {
    const id = e.detail?.value;
    if (!id) return;
    state.detailTab = id;
    if (id === 'acl' && state.aclEntries == null) loadAcl(name);
    renderDetailBody();
  });
}

// Renders the topic-detail hero+body. Called both on a REAL context change
// (topic opened/edited — `wireDetailSkeleton`'s tab switch, `loadTopicDetail`)
// AND on every 3s stats poll while the overview tab is visible
// (`refreshStats`) — the two used to be indistinguishable, so a poll tick
// re-ran the SAME full `hero.innerHTML =` / `body.innerHTML =` rebuild as a
// real topic switch, destroying and recreating `<tf-line-chart
// id="tb-detail-chart">` (and the groups-lag list, and the hero's own
// buttons) every 3 seconds — the owner-reported "chart draws from zero"
// bug. `state.detailVersion` (bumped only by `loadTopicDetail`, never by
// `refreshStats`) now tells the two apart: hero/overview markup is only
// rebuilt when the stamped version on the DOM is stale; a same-version call
// (a poll tick, or re-entering the overview tab) only patches the KPI tile
// text and the chart's `series` — never touches the groups-lag list or the
// hero at all, matching task 5 ("groups lag … change only on user action —
// ensure they are not repainted by the stats poll").
function renderDetailBody() {
  const hero = byId('tb-detail-hero');
  const body = byId('tb-detail-panel');
  if (!hero || !body) return;
  if (state.detailLoading || !state.detail) {
    hero.innerHTML = `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('loading'))}</div>`;
    body.innerHTML = '';
    delete hero.dataset.tbHeroVersion;
    delete body.dataset.tbOverviewVersion;
    return;
  }
  const { topic, partitions } = state.detail;
  const versionKey = String(state.detailVersion);

  if (hero.dataset.tbHeroVersion !== versionKey) {
    // N-7 (KRYTYK-M1-R2.md): M03's "Lag grup konsumentów"/mini-KPI has the
    // exact same `tf-system-probe` leak as the M04 KPI strip (task 3) — this
    // topic-detail response carries its own, separate `groups[]` breakdown,
    // so it needs the same client-side filter applied independently.
    const groups = filterVisibleGroups(state.detail.groups);
    hero.innerHTML = heroHtml(topic, groups);
    hero.dataset.tbHeroVersion = versionKey;
    byId('tb-detail-preview')?.addEventListener('click', () => openMessagePreview(topic.name, topic.partitions));
    byId('tb-detail-edit')?.addEventListener('click', () => openTopicWizard(topic));
    byId('tb-detail-delete')?.addEventListener('click', () => confirmDeleteTopic(topic.name));
  }

  if (state.detailTab === 'overview') {
    if (body.dataset.tbOverviewVersion !== versionKey) {
      const groups = filterVisibleGroups(state.detail.groups);
      body.innerHTML = detailOverviewHtml(groups);
      body.dataset.tbOverviewVersion = versionKey;
      ensureLiveChart('tb-detail-chart');
    }
    patchDetailOverviewKpis(findTopicStats(state.stats?.topics, topic.name));
    updateLiveChartSeries('tb-detail-chart', state.detailChartSeries);
  } else {
    delete body.dataset.tbOverviewVersion;
    if (state.detailTab === 'partitions') {
      body.innerHTML = detailPartitionsHtml(partitions, topic.name);
      body.querySelector('#tb-detail-open-replication')?.addEventListener('click', () => openReplicationForTopic(topic.name));
    } else if (state.detailTab === 'config') body.innerHTML = detailConfigHtml(topic);
    else if (state.detailTab === 'acl') renderAclTab(body, topic.name);
  }
}

function chipHtml(chip) {
  return `<tf-chip status="${escapeAttr(chip.status)}">${escapeHtml(chip.label)}</tf-chip>`;
}

function heroHtml(topic, groups) {
  return `
    <div class="tb-hero">
      <div class="tb-hero-ident">
        <div class="tb-hero-name">${escapeHtml(topic.name)}${topic.name.startsWith('__dlq.') ? ` <tf-chip status="warn">${escapeHtml(T('badge_dlq'))}</tf-chip>` : ''}</div>
        <div class="tb-hero-meta">
          ${chipHtml(envChip(topic.environment))}
          <tf-chip status="info">${escapeHtml(topic.delivery)}</tf-chip>
          <tf-chip status="info">${escapeHtml(topic.acks)}</tf-chip>
        </div>
      </div>
      <div class="tb-mini-kpis">
        <div class="tb-mk"><b>${topic.partitions}</b><span>${escapeHtml(T('detail_mk_partitions'))}</span></div>
        <div class="tb-mk"><b>${sumGroupLag(groups)}</b><span>${escapeHtml(T('detail_mk_groups_lag'))}</span></div>
      </div>
      <div class="tb-head-actions">
        <tf-button variant="ghost" icon="eye" id="tb-detail-preview">${escapeHtml(T('detail_preview_messages'))}</tf-button>
        ${canAdmin() ? `<tf-button variant="secondary" icon="edit" id="tb-detail-edit">${escapeHtml(T('detail_edit'))}</tf-button>
        <tf-button variant="danger" icon="trash" id="tb-detail-delete">${escapeHtml(T('detail_delete'))}</tf-button>` : ''}
      </div>
    </div>
  `;
}

// Built once per `state.detailVersion` (see `renderDetailBody`) — the KPI
// tile values start as "—" placeholders with STABLE ids and are patched live
// every poll by `patchDetailOverviewKpis`, the chart element is created here
// and fed by `ensureLiveChart`/`updateLiveChartSeries` (never recreated), and
// the groups-lag list is a plain snapshot of `groups` at build time — it does
// NOT update on its own between rebuilds, by design (task 5: the groups-lag
// list changes only on user action, e.g. re-opening the topic or an offset
// reset, never on a stats poll).
function detailOverviewHtml(groups) {
  const rows = (groups || []).map((g) => `
    <div class="tb-kv-row">
      <div class="tb-kv-key">${escapeHtml(g.group)}</div>
      <div class="tb-kv-val">${g.lagTotal}</div>
    </div>
  `).join('');
  const tile = (id, label) => `<div class="tb-mk"><b id="${id}">—</b><span>${escapeHtml(label)}</span></div>`;
  const tiles = [
    tile('tb-ov-msgs-in', T('detail_ov_msgs_in')),
    tile('tb-ov-bytes-in', T('detail_ov_bytes_in')),
    tile('tb-ov-disk', T('detail_ov_disk')),
    tile('tb-ov-lag', T('detail_ov_lag')),
    tile('tb-ov-dlq-depth', T('detail_ov_dlq_depth')),
  ].join('');
  return `
    <div class="tb-card"><div class="tb-c-body"><div class="tb-mini-kpis tb-mini-kpis--wrap">${tiles}</div></div></div>
    <div class="tb-card">
      <div class="tb-c-head"><h3>${escapeHtml(T('chart_throughput_title'))}</h3><div class="tb-hint">${escapeHtml(T('chart_live_window_note', { minutes: CHART_WINDOW_MINUTES }))}</div></div>
      <div class="tb-c-body"><tf-line-chart id="tb-detail-chart"></tf-line-chart></div>
    </div>
    <div class="tb-card">
      <div class="tb-c-head"><h3>${escapeHtml(T('detail_groups_lag_title'))}</h3></div>
      <div class="tb-c-body">
        ${groups?.length ? `<div class="tb-kv">${rows}</div>` : `<div class="tb-state tb-empty">${escapeHtml(T('empty_topics'))}</div>`}
      </div>
    </div>
  `;
}

// Poll-driven patch for the overview mini-KPI tiles — text-only, keyed by
// the stable ids `detailOverviewHtml` gives each `<b>`, via `patchText`'s
// no-op-on-equal write.
function patchDetailOverviewKpis(ts) {
  patchText(byId('tb-ov-msgs-in'), ts ? `${fmtCompact(ts.msgsInPerSec)} /s` : '—');
  patchText(byId('tb-ov-bytes-in'), ts ? `${formatBytes(ts.bytesInPerSec)}/s` : '—');
  patchText(byId('tb-ov-disk'), ts ? formatBytes(ts.totalBytesOnDisk) : '—');
  patchText(byId('tb-ov-lag'), ts ? fmtCompact(ts.totalLag) : '—');
  patchText(byId('tb-ov-dlq-depth'), ts ? fmtCompact(ts.dlqDepth) : '—');
}

// M2 (PLAN-M2.md §1f, module-doc gap #2 RESOLVED): `partitions[]` now
// carries `leaderNodeId`/`leaderEpoch`/`isrCount`/`replicaCount`/
// `highWatermark` alongside M1's `earliestOffset`/`logEndOffset`/
// `sizeBytes`/`segments` — the old static "—" leader/ISR columns are real
// now. `unavailableReason` renders as a STATE chip (PLAN-M2 §4.1 A4: not a
// producer error) rather than an error box.
function detailPartitionsHtml(partitions, topicName) {
  const rows = (partitions || []).map((p) => {
    const isrCount = p.isrCount ?? (Array.isArray(p.isr) ? p.isr.length : null);
    const replicaCount = p.replicaCount ?? (Array.isArray(p.replicas) ? p.replicas.length : null);
    const degraded = isrCount != null && replicaCount != null && isIsrDegraded(isrCount, replicaCount);
    const lag = computeReplicationLag(p.highWatermark, p.logEndOffset);
    const reasonKey = unavailableReasonI18nKey(p.unavailableReason);
    return `
      <tr class="${p.unavailableReason ? 'tb-row-unavailable' : ''}">
        <td>${p.partition}</td>
        <td class="mono">${p.leaderNodeId ? escapeHtml(p.leaderNodeId) : '—'}</td>
        <td class="mono">${p.leaderEpoch != null ? `e${p.leaderEpoch}` : '—'}</td>
        <td>${isrCount != null && replicaCount != null ? `${isrCount}/${replicaCount}` : '—'}${degraded ? ` <span class="tf-chip warn">${escapeHtml(T('detail_isr_degraded'))}</span>` : ''}</td>
        <td class="mono">${p.earliestOffset}</td>
        <td class="mono">${p.logEndOffset}</td>
        <td class="mono">${p.highWatermark != null ? p.highWatermark : '—'}</td>
        <td class="mono">${p.highWatermark != null ? lag : '—'}</td>
        <td>${formatBytes(p.sizeBytes)}</td>
        <td>${p.segments}</td>
        <td>${reasonKey ? `<span class="tf-chip err" title="${escapeAttr(T(reasonKey))}">${escapeHtml(T('detail_partition_unavailable'))}</span>` : ''}</td>
      </tr>
    `;
  }).join('');
  return `
    <div class="tb-card">
      <div class="tb-c-head">
        <h3>${escapeHtml(T('detail_tab_partitions'))}</h3>
        <tf-button variant="ghost" size="sm" icon="external-link" id="tb-detail-open-replication">${escapeHtml(T('detail_partitions_open_replication'))}</tf-button>
      </div>
      <div class="tb-c-body tb-c-body--table">
        ${partitions?.length ? `
          <table style="width:100%;border-collapse:collapse;font-size:12.5px">
            <thead><tr>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_partition'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_leader'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_epoch'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_isr'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_earliest_offset'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_log_end_offset'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_hw'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_lag'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_size'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_segments'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('detail_col_state'))}</th>
            </tr></thead>
            <tbody>${rows}</tbody>
          </table>
        ` : `<div class="tb-state tb-empty">${escapeHtml(T('empty_topics'))}</div>`}
      </div>
    </div>
  `;
}

function detailConfigHtml(topic) {
  const rowsDef = [
    ['name', topic.name, true],
    ['partitions', topic.partitions],
    ['retention_ms', formatRetentionLabel(topic.retentionMs)],
    ['retention_bytes', formatBytes(topic.retentionBytesPerPartition)],
    ['cleanup_policy', topic.cleanupPolicy],
    ['delivery', topic.delivery],
    ['idempotency_key', topic.idempotencyKey || T('na')],
    ['dedup_window_ms', `${Math.round(topic.dedupWindowMs / 3_600_000)} h`],
    ['max_delivery_attempts', topic.maxDeliveryAttempts],
    ['retry_backoff_ms', topic.retryBackoffMs],
    ['schema_id', topic.schemaId || T('schema_none')],
    ['validation', topic.validation],
    ['content_type', topic.contentType, true],
    ['replication_factor', topic.replicationFactor],
    ['acks', topic.acks],
    ['durability', topic.durability],
    ['max_inline_bytes', formatBytes(topic.maxInlineBytes)],
    ['compression', topic.compression],
    ['environment', topic.environment],
  ];
  const renderRow = ([key, val, mono]) => `
    <div class="tb-kv-row">
      <div class="tb-kv-key">${escapeHtml(T(`config_row_${key}`))}</div>
      <div class="tb-kv-val${mono ? ' mono' : ''}">${escapeHtml(String(val))}</div>
    </div>
  `;
  // Owner decision B: the durability-class chip sits right above the
  // existing raw `durability` row (untouched, still the resolved policy
  // string as-is) rather than replacing it — the chip is the "how safe"
  // summary, this row stays the "how, exactly" detail, and the chip's own
  // tooltip/secondary text repeats the same policy string for a reader who
  // lands on this row without the chip's hover state.
  const durabilityClassRow = `
    <div class="tb-kv-row">
      <div class="tb-kv-key">${escapeHtml(T('config_row_durability_class'))}</div>
      <div class="tb-kv-val">
        ${durabilityClassChipHtml(topic)}
        ${topic.durability ? `<span class="tb-field-hint">${escapeHtml(T('durability_class_policy_title', { durability: topic.durability }))}</span>` : ''}
      </div>
    </div>
  `;
  const acksIdx = rowsDef.findIndex(([key]) => key === 'acks');
  const rows = rowsDef.slice(0, acksIdx + 1).map(renderRow).join('')
    + durabilityClassRow
    + rowsDef.slice(acksIdx + 1).map(renderRow).join('');
  return `
    <div class="tb-card">
      <div class="tb-c-body">
        <div class="tb-kv">${rows}
          <div class="tb-kv-row">
            <div class="tb-kv-key">${escapeHtml(T('config_row_encryption_at_rest'))}</div>
            <div class="tb-kv-val">off <span class="tb-field-hint">(${escapeHtml(T('config_not_in_api'))})</span></div>
          </div>
        </div>
      </div>
    </div>
  `;
}

async function loadAcl(topicName) {
  state.aclLoading = true;
  try {
    const resp = await ApiBinary.one('busAclListRequest', { instanceId: requireInstanceId(state.instanceId), topic: topicName });
    state.aclEntries = resp.entries || [];
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
    state.aclEntries = [];
  }
  state.aclLoading = false;
  if (state.view?.kind === 'topic-detail' && state.detailTab === 'acl') {
    const body = byId('tb-detail-panel');
    if (body) renderAclTab(body, topicName);
  }
}

function renderAclTab(body, topicName) {
  if (state.aclEntries == null) {
    body.innerHTML = `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('loading'))}</div>`;
    return;
  }
  const admin = isSiteAdmin();
  const rows = state.aclEntries.map((e) => `
    <tr>
      <td>${escapeHtml(e.subjectType)}</td>
      <td>${escapeHtml(e.subjectId)}</td>
      <td>${chipHtml({ status: e.accessLevel === 'allow' ? 'ok' : 'err', label: T(`acl_access_${e.accessLevel}`) })}</td>
      <td>${admin ? `<tf-button variant="ghost" size="sm" icon="close" class="tb-acl-clear" data-subject-type="${escapeAttr(e.subjectType)}" data-subject-id="${escapeAttr(e.subjectId)}">${escapeHtml(T('acl_action_clear'))}</tf-button>` : ''}</td>
    </tr>
  `).join('');
  body.innerHTML = `
    <div class="tb-gap-note">${sprite('info')}${escapeHtml(T('acl_gap_note'))}</div>
    ${admin ? '' : `<div class="tb-gap-note">${sprite('info')}${escapeHtml(T('acl_admin_required'))}</div>`}
    <div class="tb-card">
      <div class="tb-c-head"><h3>${escapeHtml(T('acl_title'))}</h3></div>
      <div class="tb-c-body">
        <table class="tb-acl-table" style="width:100%;border-collapse:collapse;font-size:12.5px">
          <thead><tr>
            <th style="text-align:left;padding:6px 4px">${escapeHtml(T('acl_col_subject_type'))}</th>
            <th style="text-align:left;padding:6px 4px">${escapeHtml(T('acl_col_subject_id'))}</th>
            <th style="text-align:left;padding:6px 4px">${escapeHtml(T('acl_col_access'))}</th>
            <th></th>
          </tr></thead>
          <tbody>${rows || `<tr><td colspan="4">${escapeHtml(T('acl_empty'))}</td></tr>`}</tbody>
        </table>
        ${admin ? aclAddFormHtml() : ''}
      </div>
    </div>
  `;
  if (admin) {
    body.querySelector('#tb-acl-add-btn')?.addEventListener('click', () => submitAclSet(body, topicName));
    body.querySelectorAll('.tb-acl-clear').forEach((btn) => {
      btn.addEventListener('click', () => setAcl(topicName, btn.dataset.subjectType, btn.dataset.subjectId, 'clear'));
    });
  }
}

function aclAddFormHtml() {
  return `
    <div class="tb-wizard-grid--3" style="margin-top:12px">
      <tf-select id="tb-acl-subject-type" label="${escapeAttr(T('acl_col_subject_type'))}" value="user">
        <option value="user">user</option>
        <option value="group">group</option>
        <option value="api_key">api_key</option>
      </tf-select>
      <tf-input id="tb-acl-subject-id" label="${escapeAttr(T('acl_col_subject_id'))}"></tf-input>
      <tf-select id="tb-acl-access" label="${escapeAttr(T('acl_col_access'))}" value="allow">
        <option value="allow">${escapeHtml(T('acl_access_allow'))}</option>
        <option value="deny">${escapeHtml(T('acl_access_deny'))}</option>
      </tf-select>
    </div>
    <tf-button id="tb-acl-add-btn" variant="secondary" icon="plus" style="margin-top:10px">${escapeHtml(T('acl_add'))}</tf-button>
  `;
}

async function submitAclSet(body, topicName) {
  const subjectType = body.querySelector('#tb-acl-subject-type')?.value || 'user';
  const subjectId = body.querySelector('#tb-acl-subject-id')?.value?.trim();
  const accessLevel = body.querySelector('#tb-acl-access')?.value || 'allow';
  if (!subjectId) { toast(T('acl_subject_required'), 'error'); return; }
  await setAcl(topicName, subjectType, subjectId, accessLevel);
}

async function setAcl(topicName, subjectType, subjectId, accessLevel) {
  try {
    await ApiBinary.action('busAclSetRequest', { instanceId: requireInstanceId(state.instanceId), topic: topicName, subjectType, subjectId, accessLevel });
    toast(T('saved'), 'success');
    await loadAcl(topicName);
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
  }
}

// =============================================================================
// Message preview (M08) — audited bounded/redacted peek, never a real
// consumer (PLAN §6.2's "protokół UI nie jest ścieżką danych").
// =============================================================================

// `fromOffsets` (per-partition cursors, tor U task 1) wins over the legacy
// scalar `fromOffset` on the server when both are sent (codec.js's doc) —
// callers pass only one of the two: `fromOffset` for the very first page
// (no partition breakdown known yet) or `fromOffsets` for every subsequent
// page once `partitions[]` came back from the previous response. `partition`
// (task 2, KRYTYK-M1-R2.md's N-3 / KRYTYK-M1-R3.md's R3-2) is the field the
// concurrent backend fala's `peek_topic` reads to walk ONLY that partition
// (POSTEP.md's "Decyzje po R3" #1) — this file's own payload shape and the
// selection/paging flow around it (`openMessagePreview`'s `change` handler,
// `fromOffsetsForPartitionSelection`) do not need to change once that lands;
// `filterRecordsByPartition` below stays as a client-side safety net (a
// no-op once the server itself only returns the requested partition).
function buildMessagesBrowseRequest(instanceId, topic, fromOffset, limit, fromOffsets, partition) {
  return {
    instanceId: requireInstanceId(instanceId),
    topic,
    fromOffset: fromOffset ?? undefined,
    limit,
    fromOffsets: fromOffsets && fromOffsets.length ? fromOffsets : undefined,
    partition: partition ?? undefined,
  };
}

// M08 partition filter (task 2). Historically a client-side workaround for
// `peek_topic` (dispatch/bus.rs) walking every partition 0..partitions and
// stopping once the record BUDGET ran out (KRYTYK-M1-R2.md's N-3 /
// KRYTYK-M1-R3.md's R3-2 root cause: a hot partition 0 starved every later
// partition within the same 50/100-record page). Kept as a defense-in-depth
// no-op now that `peek_topic` honors `partition` server-side (POSTEP.md's
// "Decyzje po R3" #1) and only ever returns the requested partition's
// records in the first place. `null` means "all partitions", matching the
// mockup's "Wszystkie" option.
function filterRecordsByPartition(records, partition) {
  const list = Array.isArray(records) ? records : [];
  return partition == null ? list : list.filter((r) => r.partition === partition);
}

// The "load more" cursor for the current selection: every partition that
// still `hasMore` (unchanged `buildFromOffsetsForNextPage` behavior) when
// "all partitions" is selected, or ONLY the selected partition's own cursor
// otherwise — never silently widening back to every partition just because
// paging continued.
function fromOffsetsForPartitionSelection(partitions, partition) {
  if (partition == null) return buildFromOffsetsForNextPage(partitions);
  const p = (Array.isArray(partitions) ? partitions : []).find((x) => x.partition === partition);
  return p?.hasMore ? [{ partition: p.partition, offset: p.nextOffset }] : [];
}

// Whether to offer "load more" for the current selection — the server's own
// aggregate `hasMore` (true iff ANY partition has more) over-offers it while
// one partition is selected and only OTHER partitions still have more.
function hasMoreForPartitionSelection(partitions, partition) {
  const list = Array.isArray(partitions) ? partitions : [];
  if (partition == null) return list.some((p) => p.hasMore);
  return !!list.find((p) => p.partition === partition)?.hasMore;
}

// Partition <select> options (mockup M08: "Partycja | Wszystkie ▾") built
// from the topic's own partition COUNT (`BusTopicWire.partitions`, known up
// front), not from a browse response's `partitions[]` breakdown — that
// breakdown only lists partitions the record budget actually reached
// (N-3), which would hide partitions 1..N-1 from the picker itself on
// exactly the topics where the filter matters most.
function partitionFilterOptions(partitionCount, allLabel) {
  const n = Number(partitionCount) || 0;
  const options = [{ value: '', label: allLabel }];
  for (let i = 0; i < n; i += 1) options.push({ value: String(i), label: `P${i}` });
  return options;
}

function openMessagePreview(topicName, partitionCount) {
  const modal = document.createElement('tf-modal');
  modal.setAttribute('title', T('preview_title', { topic: topicName }));
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('size', 'xl');
  const bodySlot = document.createElement('div');
  bodySlot.setAttribute('slot', 'body');
  bodySlot.innerHTML = `
    <div class="tb-audit-banner">${sprite('shield')}<span>${escapeHtml(T('preview_audit_banner'))}</span></div>
    <div class="tb-toolbar" id="tb-preview-toolbar">
      <tf-select id="tb-preview-partition" label="${escapeAttr(T('preview_col_partition'))}"></tf-select>
    </div>
    <div id="tb-preview-body"><div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('loading'))}</div></div>
  `;
  modal.appendChild(bodySlot);
  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  trapModalFocus(modal);
  modal.addEventListener('close', () => closeModal(modal), { once: true });

  const previewState = { records: [], partitions: [], partition: null };
  const partitionSelect = modal.querySelector('#tb-preview-partition');
  partitionSelect?.setOptions(partitionFilterOptions(partitionCount, T('preview_partition_all')), '');
  // Keyboard-accessible for free: `<tf-select>` wraps a native `<select>`
  // (tab-focusable, arrow-key/typeahead option cycling, Enter/Space commit —
  // see tf-select.js), no bespoke listbox needed for the mockup's dropdown.
  partitionSelect?.addEventListener('change', (e) => {
    const raw = e.detail?.value ?? '';
    previewState.partition = raw === '' ? null : Number(raw);
    previewState.records = [];
    previewState.partitions = [];
    loadPreviewPage(modal, topicName, previewState, true);
  });
  loadPreviewPage(modal, topicName, previewState, true);
}

// Per-partition summary chips (earliest offset / high watermark, tor U
// task 1) shown above the record table in both M08 and M05 — the same
// `BusBrowsePartitionInfoWire[]` shape both responses now carry.
function partitionSummaryHtml(partitions) {
  if (!partitions?.length) return '';
  const chips = partitions.map((p) => `
    <span class="tf-chip info" title="${escapeAttr(T('partitions_summary_chip_title', { partition: p.partition, earliest: p.earliestOffset, hwm: p.highWatermark }))}">
      P${p.partition}: ${p.earliestOffset}–${p.highWatermark}
    </span>
  `).join('');
  return `<div class="tb-partition-summary">${chips}</div>`;
}

async function loadPreviewPage(modal, topicName, previewState, isFirstPage) {
  const host = modal.querySelector('#tb-preview-body');
  const fromOffsets = isFirstPage ? undefined : fromOffsetsForPartitionSelection(previewState.partitions, previewState.partition);
  try {
    const resp = await ApiBinary.one(
      'busMessagesBrowseRequest',
      buildMessagesBrowseRequest(state.instanceId, topicName, null, 50, fromOffsets, previewState.partition),
    );
    previewState.records = isFirstPage ? (resp.records || []) : [...previewState.records, ...(resp.records || [])];
    previewState.partitions = resp.partitions || [];
  } catch (err) {
    if (host) host.innerHTML = `<div class="tb-state">${escapeHtml(mapBusErrorMessage(err?.message, T))}</div>`;
    return;
  }
  if (!host) return;
  // `filterRecordsByPartition` re-applies the same filter already sent to
  // the server as `partition` — a no-op once `peek_topic` honors it
  // (POSTEP.md's "Decyzje po R3" #1, see `buildMessagesBrowseRequest`'s doc),
  // kept as a client-side safety net rather than trusting the wire shape
  // blindly.
  const visibleRecords = filterRecordsByPartition(previewState.records, previewState.partition);
  const hasMore = hasMoreForPartitionSelection(previewState.partitions, previewState.partition);
  const loadMoreBtn = hasMore
    ? `<tf-button variant="secondary" id="tb-preview-more" style="margin-top:10px">${escapeHtml(T('preview_load_more'))}</tf-button>`
    : '';
  if (visibleRecords.length === 0) {
    // A specific partition can still have nothing loaded YET even while
    // `hasMore` is true — its own first page may simply not have reached the
    // requested offset range — so offer "load more" here too instead of a
    // flat dead-end empty state (R3-2's server-side partition filter makes
    // an empty result mean "genuinely nothing (yet) at this offset", not the
    // pre-fix "a hot partition 0 starved the shared record budget").
    host.innerHTML = `${partitionSummaryHtml(previewState.partitions)}<div class="tb-state tb-empty">${escapeHtml(T('preview_empty'))}</div>${loadMoreBtn}`;
    host.querySelector('#tb-preview-more')?.addEventListener('click', () => loadPreviewPage(modal, topicName, previewState, false));
    return;
  }
  host.innerHTML = `
    ${partitionSummaryHtml(previewState.partitions)}
    <table class="tb-preview-table" style="width:100%;border-collapse:collapse;font-size:12px">
      <thead><tr>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('preview_col_partition'))}</th>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('preview_col_offset'))}</th>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('preview_col_timestamp'))}</th>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('preview_col_key'))}</th>
        <th></th>
      </tr></thead>
      <tbody>
        ${visibleRecords.map((r, idx) => `
          <tr>
            <td>${r.partition}</td>
            <td>${r.offset}</td>
            <td>${msToDate(r.timestampMs)}</td>
            <td class="mono">${escapeHtml(r.key?.length ? bytesToPreviewText(r.key, 32) : '—')}</td>
            <td>
              ${r.isBlobRef ? `<tf-chip status="info">BlobRef</tf-chip>` : ''}
              ${r.truncated ? `<tf-chip status="warn">${escapeHtml(T('preview_truncated'))}</tf-chip>` : ''}
              <tf-button variant="ghost" size="sm" icon="eye" class="tb-preview-view" data-idx="${idx}">${escapeHtml(T('preview_action_view'))}</tf-button>
            </td>
          </tr>
        `).join('')}
      </tbody>
    </table>
    <div id="tb-preview-detail"></div>
    ${loadMoreBtn}
  `;
  host.querySelectorAll('.tb-preview-view').forEach((btn) => {
    btn.addEventListener('click', () => renderPreviewRecordDetail(host, visibleRecords[Number(btn.dataset.idx)]));
  });
  host.querySelector('#tb-preview-more')?.addEventListener('click', () => loadPreviewPage(modal, topicName, previewState, false));
}

function renderPreviewRecordDetail(host, record) {
  const detail = host.querySelector('#tb-preview-detail');
  if (!detail) return;
  const tfHeaders = ['tf.actor', 'tf.org', 'tf.correlation_id', 'tf.origin']
    .map((k) => [k, headerText(record.headers, k)])
    .filter(([, v]) => v != null);
  const blobRef = record.isBlobRef ? parseBlobRefJson(record.payloadPreview) : null;
  detail.innerHTML = `
    <div class="tb-dlq-detail">
      <div>
        <strong>${escapeHtml(T('preview_headers_title'))}</strong>
        <dl class="tb-header-list">
          ${tfHeaders.map(([k, v]) => `<dt>${escapeHtml(k)}</dt><dd>${escapeHtml(v)}</dd>`).join('') || `<dd>${escapeHtml(T('na'))}</dd>`}
        </dl>
      </div>
      ${blobRef ? `
        <div>
          <strong>${escapeHtml(T('preview_blobref_title'))}</strong>
          <p class="tb-field-hint">${escapeHtml(T('preview_blobref_hint'))}</p>
          <dl class="tb-header-list">
            <dt>id</dt><dd class="mono">${escapeHtml(blobRef.id)}</dd>
            <dt>size_bytes</dt><dd>${escapeHtml(formatBytes(blobRef.size_bytes))}</dd>
            <dt>mime</dt><dd>${escapeHtml(blobRef.mime)}</dd>
            <dt>sha256</dt><dd class="mono">${escapeHtml(blobRef.sha256)}</dd>
          </dl>
        </div>
      ` : `
        <div>
          <strong>${escapeHtml(T('preview_payload_title'))}</strong>
          <div class="tb-payload-preview">${escapeHtml(bytesToPreviewText(record.payloadPreview))}</div>
        </div>
      `}
    </div>
  `;
}

// =============================================================================
// Consumer groups (M04)
// =============================================================================

async function loadGroups() {
  try {
    state.groups = await ApiBinary.list('busGroupListRequest', { arrayKey: 'groups', payload: { instanceId: requireInstanceId(state.instanceId) } });
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
    state.groups = [];
  }
  state.groupsLoaded = true;
  if (state.tab === 'groups' && !state.view) paintGroupsTable();
}

function renderGroupsTab(panel) {
  const rebuilt = ensureSkeleton(panel, 'groups', groupsSkeletonHtml);
  if (rebuilt) wireGroupsSkeleton(panel);
  paintGroupsTable();
  paintGroupDetail();
}

function groupsSkeletonHtml() {
  return `
    <div class="tb-card">
      <div class="tb-c-body tb-c-body--table">
        <tf-table id="tb-groups-table" variant="flush">
          <tf-column key="group" label="${escapeAttr(T('groups_col_group'))}" fill sortable></tf-column>
          <tf-column key="topic" label="${escapeAttr(T('groups_col_topic'))}"></tf-column>
          <tf-column key="commitMode" label="${escapeAttr(T('groups_col_commit_mode'))}" hide-below="900"></tf-column>
          <tf-column key="state" label="${escapeAttr(T('groups_col_state'))}" renderer="chip"></tf-column>
        </tf-table>
        <div id="tb-groups-empty" hidden></div>
      </div>
    </div>
    <div id="tb-group-detail"></div>
  `;
}

function wireGroupsSkeleton(panel) {
  const table = panel.querySelector('#tb-groups-table');
  if (!table) return;
  wireRowKeyboardActivation(table);
  table.rowActions = (row) => {
    if (!canAdmin()) return null;
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', 'ghost');
    btn.setAttribute('size', 'sm');
    btn.setAttribute('icon', row.paused ? 'play' : 'pause');
    btn.title = T(row.paused ? 'groups_action_resume' : 'groups_action_pause');
    btn.addEventListener('click', (e) => { e.stopPropagation(); toggleGroupPause(row); });
    return btn;
  };
  table.addEventListener('row-click', (e) => openGroupDetail(e.detail.row.group, e.detail.row.topic));
}

function paintGroupsTable() {
  const table = byId('tb-groups-table');
  if (!table) return;
  const visibleGroups = filterVisibleGroups(state.groups);
  const rows = visibleGroups.map((g) => ({
    group: g.group,
    topic: g.topic,
    commitMode: g.commitMode,
    state: { status: g.paused ? 'warn' : 'ok', label: T(g.paused ? 'groups_state_paused' : 'groups_state_active') },
    paused: g.paused,
  }));
  // Same no-op-poll gate as `paintTopicsTable` (this table is not on the
  // stats-poll path today — `loadGroups()` only runs on user action — but
  // pause/resume reload the whole list, so this still avoids rebuilding
  // every OTHER row's pause/resume button when only one row changed).
  const diff = diffRowsByKey(state.dom.groupsTableRows, rows, (r) => `${r.group}::${r.topic}`);
  if (state.dom.groupsTableRows == null || diff.changed) {
    table.rows = rows;
    state.dom.groupsTableRows = rows;
  }
  makeRowsFocusable(table);
  // P2-2: same bare-header gap as M01 — no groups exist on a fresh install
  // (a group only appears once a consumer reads from a topic), so this adds
  // the missing message rather than leaving a header with zero rows below it.
  const emptyHost = byId('tb-groups-empty');
  if (emptyHost) {
    const empty = visibleGroups.length === 0;
    table.hidden = empty;
    emptyHost.hidden = !empty;
    emptyHost.innerHTML = empty ? `<div class="tb-state tb-empty">${escapeHtml(T('groups_empty'))}</div>` : '';
  }
}

async function toggleGroupPause(row) {
  try {
    await ApiBinary.action(row.paused ? 'busGroupResumeRequest' : 'busGroupPauseRequest', { instanceId: requireInstanceId(state.instanceId), group: row.group, topic: row.topic });
    toast(T('saved'), 'success');
    await loadGroups();
    if (state.groupDetail?.group === row.group && state.groupDetail?.topic === row.topic) await openGroupDetail(row.group, row.topic);
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
  }
}

async function openGroupDetail(group, topic) {
  try {
    const resp = await ApiBinary.one('busGroupDetailRequest', { instanceId: requireInstanceId(state.instanceId), group, topic });
    state.groupDetail = resp.detail;
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
    state.groupDetail = null;
  }
  paintGroupDetail();
}

function paintGroupDetail() {
  const host = byId('tb-group-detail');
  if (!host) return;
  const gd = state.groupDetail;
  if (!gd) { host.innerHTML = ''; return; }
  const rows = (gd.partitions || []).map((p) => {
    const ratio = computeLagRatio(p.lag, p.committedOffset + p.lag);
    return `
      <tr>
        <td>${p.partition}</td>
        <td>${p.committedOffset}</td>
        <td>${p.lag}</td>
        <td><span class="tb-lagbar ${lagSeverityClass(ratio)}" role="img" aria-label="${escapeAttr(T('group_detail_lag_ratio_label', { percent: Math.round(ratio * 100) }))}"><span style="width:${Math.round(ratio * 100)}%"></span></span></td>
        <td>${isSiteAdmin() ? `<tf-button variant="ghost" size="sm" icon="rotate" class="tb-reset-offset" data-partition="${p.partition}">${escapeHtml(T('group_detail_reset_offset'))}</tf-button>` : ''}</td>
      </tr>
    `;
  }).join('');
  host.innerHTML = `
    <div class="tb-card">
      <div class="tb-c-head tb-group-detail-head">
        <h3>${escapeHtml(gd.group)} → ${escapeHtml(gd.topic)}</h3>
        ${chipHtml({ status: gd.paused ? 'warn' : 'ok', label: T(gd.paused ? 'groups_state_paused' : 'groups_state_active') })}
      </div>
      <div class="tb-c-body">
        ${isSiteAdmin() ? '' : `<div class="tb-gap-note">${sprite('info')}${escapeHtml(T('group_detail_admin_required'))}</div>`}
        <table style="width:100%;border-collapse:collapse;font-size:12.5px">
          <thead><tr>
            <th style="text-align:left;padding:6px 4px">${escapeHtml(T('group_detail_col_partition'))}</th>
            <th style="text-align:left;padding:6px 4px">${escapeHtml(T('group_detail_col_committed'))}</th>
            <th style="text-align:left;padding:6px 4px">${escapeHtml(T('group_detail_col_lag'))}</th>
            <th style="text-align:left;padding:6px 4px"><span class="tf-visually-hidden">${escapeHtml(T('group_detail_col_lag_ratio'))}</span></th>
            <th></th>
          </tr></thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
    </div>
  `;
  if (isSiteAdmin()) {
    host.querySelectorAll('.tb-reset-offset').forEach((btn) => {
      btn.addEventListener('click', () => openOffsetResetModal(gd.group, gd.topic, Number(btn.dataset.partition)));
    });
  }
}

function openOffsetResetModal(group, topic, partition) {
  const body = document.createElement('div');
  body.className = 'tb-wizard-form tb-reset-form';
  body.innerHTML = `
    <p>${escapeHtml(T('reset_modal_target', { group, topic, partition }))}</p>
    <tf-select id="tb-reset-mode" label="${escapeAttr(T('reset_field_mode'))}" value="earliest">
      <option value="earliest">${escapeHtml(T('reset_mode_earliest'))}</option>
      <option value="latest">${escapeHtml(T('reset_mode_latest'))}</option>
      <option value="explicit">${escapeHtml(T('reset_mode_explicit'))}</option>
      <option value="timestamp">${escapeHtml(T('reset_mode_timestamp'))}</option>
    </tf-select>
    <tf-input id="tb-reset-offset" type="text" inputmode="numeric" label="${escapeAttr(T('reset_field_offset'))}" hidden></tf-input>
    <tf-input id="tb-reset-ts" type="datetime-local" label="${escapeAttr(T('reset_field_timestamp'))}" hidden></tf-input>
    <p class="tb-field-hint">${escapeHtml(T('reset_audit_note'))}</p>
  `;
  const modeSelect = body.querySelector('#tb-reset-mode');
  const offsetInput = body.querySelector('#tb-reset-offset');
  const tsInput = body.querySelector('#tb-reset-ts');
  modeSelect?.addEventListener('change', (e) => {
    const mode = e.detail?.value;
    if (offsetInput) offsetInput.hidden = mode !== 'explicit';
    if (tsInput) tsInput.hidden = mode !== 'timestamp';
  });

  const modal = document.createElement('tf-modal');
  modal.setAttribute('title', T('reset_modal_title'));
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('size', 'sm');
  const bodySlot = document.createElement('div');
  bodySlot.setAttribute('slot', 'body');
  bodySlot.appendChild(body);
  modal.appendChild(bodySlot);
  const footer = document.createElement('div');
  footer.setAttribute('slot', 'footer');
  footer.className = 'tb-modal-footer';
  const cancel = document.createElement('tf-button');
  cancel.setAttribute('variant', 'secondary');
  cancel.textContent = T('common_cancel');
  cancel.addEventListener('click', () => closeModal(modal));
  const confirm = document.createElement('tf-button');
  confirm.setAttribute('variant', 'danger');
  confirm.textContent = T('reset_confirm');
  confirm.addEventListener('click', async () => {
    const mode = modeSelect?.value || 'earliest';
    if (mode === 'explicit' && !isValidExplicitOffset(offsetInput?.value)) {
      toast(T('reset_field_offset_required'), 'error');
      return;
    }
    const offset = mode === 'explicit' ? Number(offsetInput?.value) : undefined;
    const tsMs = mode === 'timestamp' ? datetimeLocalToTsMs(tsInput?.value) : undefined;
    if (mode === 'timestamp' && tsMs == null) {
      toast(T('reset_field_timestamp_required'), 'error');
      return;
    }
    try {
      await ApiBinary.action('busOffsetResetRequest', { instanceId: requireInstanceId(state.instanceId), group, topic, partition, mode, offset, tsMs });
      toast(T('reset_done'), 'success');
      closeModal(modal);
      await openGroupDetail(group, topic);
    } catch (err) {
      toast(mapBusErrorMessage(err?.message, T), 'error');
    }
  });
  footer.append(cancel, confirm);
  modal.appendChild(footer);
  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  trapModalFocus(modal);
  modal.addEventListener('close', () => closeModal(modal), { once: true });
}

// =============================================================================
// DLQ (M05) — a topic filtered by the `__dlq.` prefix, per PLAN §3.3.
// =============================================================================

function renderDlqTab(panel) {
  const rebuilt = ensureSkeleton(panel, 'dlq', dlqSkeletonHtml);
  if (rebuilt) wireDlqSkeleton(panel);
  paintDlqSourceOptions();
  paintDlqTable();
}

function dlqSkeletonHtml() {
  return `
    <div class="tb-toolbar">
      <tf-select id="tb-dlq-source" label="${escapeAttr(T('dlq_source_label'))}"></tf-select>
      <span class="tb-spacer"></span>
      ${canAdmin() ? `<tf-button variant="danger" icon="rotate" id="tb-dlq-retry-all">${escapeHtml(T('dlq_retry_all'))}</tf-button>` : ''}
    </div>
    <div class="tb-card">
      <div class="tb-c-body tb-c-body--table" id="tb-dlq-body"></div>
    </div>
  `;
}

function wireDlqSkeleton(panel) {
  const select = panel.querySelector('#tb-dlq-source');
  select?.addEventListener('change', (e) => selectDlqSource(e.detail?.value || ''));
  panel.querySelector('#tb-dlq-retry-all')?.addEventListener('click', () => confirmDlqRetryAll());
}

// Paints the `<tf-select>`'s options/display value only — no longer touches
// `state.dlqSource` (R3-1's root cause: this used to pick the same default
// as a PAINT-time side effect, which raced `setTab`'s "has a source been
// selected yet?" guard and always won, so the guard's own selection —  the
// only call site that actually loaded records — never ran). `select.value`
// alone is allowed to preview `options[0]` before `state.dlqSource` is set
// (purely cosmetic, the dropdown cannot be legitimately blank once options
// exist) — `ensureDlqTabReady` below is what actually commits that choice.
function paintDlqSourceOptions() {
  const select = byId('tb-dlq-source');
  if (!select) return;
  const options = dlqSourceTopicOptions(state.topics);
  select.setOptions(options, state.dlqSource || options[0]?.value || '');
}

// R3-1: the only function that ACTS on `resolveDlqEntrySource`'s answer —
// called from exactly two entry points (`setTab` on tab switch,
// `loadTopics` for the "topics arrived after the tab was already open" race)
// so there is no third path that can silently disagree with it. Idempotent:
// re-entering the tab with an already-loaded source is a no-op.
function ensureDlqTabReady() {
  if (state.tab !== 'dlq' || state.view) return;
  const next = resolveDlqEntrySource(state.dlqSource, state.topics);
  if (next !== state.dlqSource) {
    selectDlqSource(next);
    return;
  }
  if (next && state.dlqRecords == null && !state.dlqLoading) loadDlqRecords(true);
}

function selectDlqSource(topicName) {
  state.dlqSource = topicName;
  state.dlqRecords = null;
  state.dlqPartitions = [];
  loadDlqRecords(true);
}

async function loadDlqRecords(isFirstPage = true) {
  if (!state.dlqSource) { state.dlqRecords = []; state.dlqPartitions = []; state.dlqError = null; paintDlqTable(); return; }
  state.dlqLoading = isFirstPage;
  if (isFirstPage) state.dlqError = null;
  paintDlqTable();
  const fromOffsets = isFirstPage ? undefined : buildFromOffsetsForNextPage(state.dlqPartitions);
  try {
    const resp = await ApiBinary.one('busDlqListRequest', {
      instanceId: requireInstanceId(state.instanceId),
      sourceTopic: state.dlqSource,
      limit: 100,
      fromOffsets: fromOffsets && fromOffsets.length ? fromOffsets : undefined,
    });
    state.dlqRecords = isFirstPage ? (resp.records || []) : [...(state.dlqRecords || []), ...(resp.records || [])];
    state.dlqHasMore = !!resp.hasMore;
    state.dlqNextOffset = resp.nextOffset;
    state.dlqPartitions = resp.partitions || [];
    state.dlqError = null;
  } catch (err) {
    // "DLQ never used yet" is expected for a healthy topic (D7's empty rows)
    // — render an empty state instead of a scary toast for that one code.
    if (busErrorCode(err?.message) === 'topic_not_found') {
      state.dlqRecords = [];
      state.dlqPartitions = [];
      state.dlqError = null;
    } else {
      // R3-1: keep the FAILURE reason around (not just a toast, which the
      // user can miss/dismiss) so `paintDlqTable` can render a real error
      // state with a retry action instead of leaving `dlqRecords == null`
      // indistinguishable from "still loading" / "never asked yet".
      const message = mapBusErrorMessage(err?.message, T);
      toast(message, 'error');
      if (isFirstPage) { state.dlqRecords = null; state.dlqError = message; }
    }
  }
  state.dlqLoading = false;
  paintDlqTable();
}

const DLQ_REASON_TONE = {
  schema_violation: 'warn',
  consumer_error: 'err',
  consumer_timeout: 'warn',
  permission_denied: 'err',
  payload_too_large: 'warn',
  blob_missing: 'info',
};

function paintDlqTable() {
  const host = byId('tb-dlq-body');
  if (!host) return;
  if (state.dlqLoading) {
    host.innerHTML = `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('loading'))}</div>`;
    return;
  }
  if (state.dlqRecords == null) {
    // R3-1: this used to be `host.innerHTML = ''` — a silently blank card
    // that gave no cue whether the tab was still loading, had failed, or was
    // simply broken. Not loading + `dlqRecords == null` now means exactly one
    // thing: the last first-page attempt failed (`loadDlqRecords` only ever
    // leaves this combination behind on a non-"topic_not_found" error) — show
    // the reason and a retry button rather than nothing. A transient instant
    // before the first load even starts (state reset, load not yet kicked
    // off) falls back to the same loading copy as the spinner branch above.
    host.innerHTML = state.dlqError
      ? `<div class="tb-state tb-state--error">
           ${sprite('alert')}<span>${escapeHtml(state.dlqError)}</span>
           <tf-button variant="secondary" size="sm" icon="rotate" id="tb-dlq-reload">${escapeHtml(T('dlq_load_error_retry'))}</tf-button>
         </div>`
      : `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('loading'))}</div>`;
    host.querySelector('#tb-dlq-reload')?.addEventListener('click', () => loadDlqRecords(true));
    return;
  }
  if (state.dlqRecords.length === 0) {
    host.innerHTML = `${partitionSummaryHtml(state.dlqPartitions)}<div class="tb-state tb-empty">${escapeHtml(T('dlq_empty_for_topic'))}</div>`;
    return;
  }
  const admin = canAdmin();
  const rows = state.dlqRecords.map((r, idx) => {
    const reason = headerText(r.headers, 'dlq.reason') || 'unknown';
    const attempts = headerText(r.headers, 'dlq.attempts') || '—';
    const errorMsg = headerText(r.headers, 'dlq.error_message') || '';
    return `
      <tr>
        <td>${r.partition}</td>
        <td>${r.offset}</td>
        <td>${msToDate(r.timestampMs)}</td>
        <td><tf-chip status="${DLQ_REASON_TONE[reason] || 'info'}">${escapeHtml(T(`dlq_reason_${reason}`) || reason)}</tf-chip></td>
        <td>${escapeHtml(attempts)}</td>
        <td class="mono" style="max-width:240px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${escapeAttr(errorMsg)}">${escapeHtml(errorMsg)}</td>
        <td>
          <tf-button variant="ghost" size="sm" icon="eye" class="tb-dlq-view" data-idx="${idx}">${escapeHtml(T('preview_action_view'))}</tf-button>
          ${admin ? `
            <tf-button variant="ghost" size="sm" icon="rotate" class="tb-dlq-retry" data-idx="${idx}">${escapeHtml(T('dlq_action_retry'))}</tf-button>
            <tf-button variant="ghost" size="sm" icon="close" class="tb-dlq-discard" data-idx="${idx}">${escapeHtml(T('dlq_action_discard'))}</tf-button>
          ` : ''}
        </td>
      </tr>
      <tr class="tb-dlq-expand" id="tb-dlq-expand-${idx}" hidden><td colspan="7"></td></tr>
    `;
  }).join('');
  host.innerHTML = `
    ${admin ? '' : `<div class="tb-gap-note">${sprite('info')}${escapeHtml(T('dlq_admin_required'))}</div>`}
    ${partitionSummaryHtml(state.dlqPartitions)}
    <table style="width:100%;border-collapse:collapse;font-size:12px">
      <thead><tr>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('dlq_col_partition'))}</th>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('dlq_col_offset'))}</th>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('dlq_col_timestamp'))}</th>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('dlq_col_reason'))}</th>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('dlq_col_attempts'))}</th>
        <th style="text-align:left;padding:6px 4px">${escapeHtml(T('dlq_col_error'))}</th>
        <th></th>
      </tr></thead>
      <tbody>${rows}</tbody>
    </table>
    ${state.dlqHasMore ? `<tf-button variant="secondary" id="tb-dlq-more" style="margin-top:10px">${escapeHtml(T('preview_load_more'))}</tf-button>` : ''}
  `;
  host.querySelectorAll('.tb-dlq-view').forEach((btn) => {
    btn.addEventListener('click', () => toggleDlqExpand(host, Number(btn.dataset.idx)));
  });
  if (admin) {
    host.querySelectorAll('.tb-dlq-retry').forEach((btn) => {
      btn.addEventListener('click', () => dlqRetry(state.dlqRecords[Number(btn.dataset.idx)]));
    });
    host.querySelectorAll('.tb-dlq-discard').forEach((btn) => {
      btn.addEventListener('click', () => confirmDlqDiscard(state.dlqRecords[Number(btn.dataset.idx)]));
    });
  }
  host.querySelector('#tb-dlq-more')?.addEventListener('click', () => loadDlqRecords(false));
}

function toggleDlqExpand(host, idx) {
  const row = host.querySelector(`#tb-dlq-expand-${idx}`);
  if (!row) return;
  const willShow = row.hidden;
  host.querySelectorAll('.tb-dlq-expand').forEach((r) => { r.hidden = true; });
  if (!willShow) return;
  const record = state.dlqRecords[idx];
  const cell = row.querySelector('td');
  const allHeaders = (record.headers || [])
    .map((h) => [h.key, formatHeaderValue(h.key, bytesToPreviewText(h.value, 512))]);
  cell.innerHTML = `
    <div class="tb-dlq-detail">
      <div>
        <strong>${escapeHtml(T('dlq_headers_title'))}</strong>
        <dl class="tb-header-list">
          ${allHeaders.map(([k, v]) => `<dt>${escapeHtml(k)}</dt><dd>${escapeHtml(v)}</dd>`).join('')}
        </dl>
      </div>
      <div>
        <strong>${escapeHtml(T('preview_payload_title'))}</strong>
        <div class="tb-payload-preview">${record.isBlobRef ? escapeHtml(T('preview_blobref_hint')) : escapeHtml(bytesToPreviewText(record.payloadPreview))}</div>
      </div>
    </div>
  `;
  row.hidden = false;
}

async function dlqRetry(record) {
  try {
    await ApiBinary.action('busDlqRetryRequest', { instanceId: requireInstanceId(state.instanceId), sourceTopic: state.dlqSource, partition: record.partition, offset: record.offset });
    toast(T('dlq_retry_done'), 'success');
    await loadDlqRecords();
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
  }
}

// N-5 (KRYTYK-M1-R2.md): the confirm/toast text used to claim a permanent
// tombstone delete ("trwale odrzucony (tombstone)" / "Usunięto.") that
// `bus::dlq_discard` never performed — it only recorded an audit-level
// acknowledgment while the record stayed fully readable. The backend fix
// (POSTEP.md's "Decyzje koordynatora po krytyku R2" #2) makes discard real
// but non-destructive: a durable (org, dlq-topic, partition, offset) marker
// that `DlqList`/retry-all/`dlq_depth` all skip, while the record's bytes
// stay in the log until retention expiry. `confirmLabel` uses the same
// "Odrzuć"/"Discard" label as the row action instead of the generic
// delete label, since this is not a delete.
async function confirmDlqDiscard(record) {
  const ok = await tbConfirm({
    title: T('dlq_discard_confirm_title'),
    body: T('dlq_discard_confirm_body'),
    confirmLabel: T('dlq_action_discard'),
  });
  if (!ok) return;
  try {
    await ApiBinary.action('busDlqDiscardRequest', { instanceId: requireInstanceId(state.instanceId), sourceTopic: state.dlqSource, partition: record.partition, offset: record.offset });
    toast(T('dlq_discard_done'), 'success');
    await loadDlqRecords();
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
  }
}

async function confirmDlqRetryAll() {
  if (!state.dlqSource) return;
  const ok = await tbConfirm({
    title: T('dlq_retry_all_confirm_title'),
    body: T('dlq_retry_all_confirm_body', { topic: state.dlqSource, max: DLQ_RETRY_ALL_MAX }),
    confirmLabel: T('dlq_retry_all'),
  });
  if (!ok) return;
  try {
    const resp = await ApiBinary.action('busDlqRetryAllRequest', { instanceId: requireInstanceId(state.instanceId), sourceTopic: state.dlqSource, maxRecords: clampDlqRetryAllMax(DLQ_RETRY_ALL_MAX) });
    toast(T('dlq_retry_all_result', { retried: resp.retried, failed: resp.failed }), 'success');
    await loadDlqRecords();
  } catch (err) {
    toast(mapBusErrorMessage(err?.message, T), 'error');
  }
}

// =============================================================================
// Replication & failover (M06, PLAN-M2.md §1f, mockup m06-replikacja-
// failover.html) — node health per environment, a per-partition role matrix
// (leader/ISR/lagging), the partitions' CURRENT lag state (see module-doc
// gap #10 for why this is a state list, not a history timeline) and the
// failover audit history. Same persistent-container / diff-in-place
// discipline as the other 4 views: `renderReplicationTab` only rebuilds the
// skeleton on a genuine context change (`ensureSkeleton`), `paintRepl*`
// functions patch already-painted DOM in place on a poll tick
// (`pollReplication`), and node cards / the role matrix are diffed by key
// exactly like M01's topics table / M04's groups table
// (`diffRowsByKey`) so an unchanged poll never touches the action buttons.
// =============================================================================

async function getLocalEnvironment() {
  if (state.repl.localEnv) return state.repl.localEnv;
  try {
    const resp = await ApiBinary.one('environmentGetKindRequest');
    state.repl.localEnv = resp?.kind || null;
  } catch {
    state.repl.localEnv = null;
  }
  return state.repl.localEnv;
}

function replicationTopicOptions(topics) {
  return [{ value: '', label: T('replication.topic_all') }, ...dlqSourceTopicOptions(topics)];
}

function renderReplicationTab(panel) {
  const rebuilt = ensureSkeleton(panel, 'replication', replicationSkeletonHtml);
  if (rebuilt) wireReplicationSkeleton(panel);
  paintReplTopicSelect();
  paintReplNodeCards();
  paintReplMatrix();
  paintReplLagState();
  paintReplFailovers();
  // Self-sufficient regardless of HOW this view became visible — a real tab
  // click (`setTab`'s own guard) or M03's "otwórz w Replikacji" button
  // (`openReplicationForTopic`, which only sets state + calls `renderPanel`,
  // never `setTab`). Both guards check the SAME `loaded`/`loading` flags, so
  // this never double-fetches when `setTab`'s own call already started one.
  if (!state.repl.loaded && !state.repl.loading) loadReplication(state.repl.topic);
}

function replicationSkeletonHtml() {
  return `
    <div class="tb-toolbar">
      <tf-select id="tb-repl-topic" label="${escapeAttr(T('replication.topic_label'))}"></tf-select>
    </div>
    <div class="tb-card">
      <div class="tb-c-head">
        <h3>${escapeHtml(T('replication.nodes_title'))}</h3>
        <div class="tb-hint">${escapeHtml(T('replication.nodes_hint'))}</div>
      </div>
      <div class="tb-c-body" id="tb-repl-nodes"></div>
    </div>
    <div class="tb-card">
      <div class="tb-c-head">
        <h3 id="tb-repl-matrix-title">${escapeHtml(T('replication.matrix_title_generic'))}</h3>
        <div class="tb-hint">${escapeHtml(T('replication.matrix_hint'))}</div>
      </div>
      <div class="tb-c-body tb-c-body--table" id="tb-repl-matrix-body"></div>
    </div>
    <div class="tb-repl-grid-2">
      <div class="tb-card">
        <div class="tb-c-head"><h3>${escapeHtml(T('replication.lag_state_title'))}</h3></div>
        <div class="tb-c-body" id="tb-repl-lag-state"></div>
      </div>
      <div class="tb-card">
        <div class="tb-c-head">
          <h3>${escapeHtml(T('replication.failover_title'))}</h3>
          <div class="tb-hint">${escapeHtml(T('replication.failover_hint'))}</div>
        </div>
        <div class="tb-c-body tb-c-body--table">
          <table class="tb-fo-table" style="width:100%;border-collapse:collapse;font-size:12px">
            <thead><tr>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('replication.failover_col_partition'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('replication.failover_col_epoch'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('replication.failover_col_nodes'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('replication.failover_col_duration'))}</th>
              <th style="text-align:left;padding:6px 4px">${escapeHtml(T('replication.failover_col_when'))}</th>
            </tr></thead>
            <tbody id="tb-repl-failover-body"></tbody>
          </table>
          <div class="tb-state tb-empty" id="tb-repl-failover-empty" hidden>${escapeHtml(T('replication.failover_empty'))}</div>
        </div>
      </div>
    </div>
  `;
}

function wireReplicationSkeleton(panel) {
  panel.querySelector('#tb-repl-topic')?.addEventListener('change', (e) => {
    state.repl.topic = e.detail?.value || '';
    state.repl.loaded = false;
    loadReplication(state.repl.topic);
  });
}

function paintReplTopicSelect() {
  const select = byId('tb-repl-topic');
  if (!select) return;
  select.setOptions(replicationTopicOptions(state.topics), state.repl.topic);
}

async function loadReplication(topic) {
  state.repl.loading = true;
  paintReplNodeCards();
  try {
    state.repl.data = await ApiBinary.one('busReplicaListRequest', buildReplicaListRequest(state.instanceId, topic));
    state.repl.error = null;
  } catch (err) {
    state.repl.error = mapBusErrorMessage(err?.message, T);
    toast(state.repl.error, 'error');
    state.repl.data = null;
  }
  state.repl.loading = false;
  state.repl.loaded = true;
  // A topic switch (or the very first load) starts every diff cache and the
  // failover "already rendered" set fresh — a stale key from a DIFFERENT
  // topic's partitions must never suppress a real row for the new one.
  state.dom.roleMatrix = null;
  state.dom.failoverKeys = null;
  const body = byId('tb-repl-matrix-body');
  if (body) body.innerHTML = '';
  const foBody = byId('tb-repl-failover-body');
  if (foBody) foBody.innerHTML = '';
  paintReplNodeCards();
  paintReplMatrix();
  paintReplLagState();
  paintReplFailovers();
}

// Poll tick (3s cadence, reused from `refreshStats` — task requirement).
// Re-fetches the SAME scope and re-runs the SAME paint functions as a real
// load, but WITHOUT resetting the diff caches first — `diffRowsByKey`/the
// failover key set are exactly what make this a patch instead of a rebuild.
async function pollReplication() {
  try {
    state.repl.data = await ApiBinary.one('busReplicaListRequest', buildReplicaListRequest(state.instanceId, state.repl.topic));
    state.repl.error = null;
  } catch {
    // Silent — matches `refreshStats`'s own convention: keep the last known
    // state on the screen rather than blanking it or toasting on every
    // missed poll.
    return;
  }
  paintReplNodeCards();
  paintReplMatrix();
  paintReplLagState();
  paintReplFailovers();
}

function nodeCardRows(data) {
  const nodes = Array.isArray(data?.nodes) ? data.nodes : [];
  const partitions = Array.isArray(data?.partitions) ? data.partitions : [];
  return nodes.map((n) => ({
    _key: n.nodeId,
    nodeId: n.nodeId,
    label: n.label || n.nodeId,
    environment: n.environment,
    isLocal: !!n.isLocal,
    reachable: n.reachable !== false,
    lastHeartbeatMsAgo: n.lastHeartbeatMsAgo,
    leaderCount: n.leaderCount,
    followerCount: n.followerCount,
    isrCount: n.isrCount,
    degraded: nodeDegradedReason(n, partitions),
  }));
}

function nodeCardSubText(r) {
  if (!r.reachable) return T('replication.node_unreachable');
  const heartbeat = T('replication.node_heartbeat', { ms: fmtCompact(Number(r.lastHeartbeatMsAgo) || 0) });
  if (!r.degraded || r.degraded.kind !== 'lagging') return heartbeat;
  const lag = r.degraded.lag || {};
  return `${heartbeat} · ${T('replication.node_lagging_note', {
    partition: r.degraded.partition,
    reason: lag.reason || T('replication.node_lagging_reason_unknown'),
  })}`;
}

function nodeCardHtml(r) {
  const cls = ['tb-node-card'];
  if (r.degraded) cls.push('tb-node-card--degraded');
  const dotCls = !r.reachable ? 'tb-node-dot--down' : (r.degraded ? 'tb-node-dot--warn' : 'tb-node-dot--live');
  const key = escapeAttr(r._key);
  return `
    <div class="${cls.join(' ')}" id="tb-repl-node-${key}">
      <div class="tb-node-card-head">
        <span class="tb-node-dot ${dotCls}"></span>
        <span class="tb-node-name">${escapeHtml(r.label)}</span>
        ${chipHtml(envChip(r.environment))}
        ${r.isLocal ? `<span class="tf-chip info">${escapeHtml(T('replication.node_local_badge'))}</span>` : ''}
      </div>
      <div class="tb-node-stats">
        <div><b id="tb-repl-node-${key}-leader">${r.leaderCount ?? '—'}</b><span>${escapeHtml(T('replication.node_stat_leader'))}</span></div>
        <div><b id="tb-repl-node-${key}-follower">${r.followerCount ?? '—'}</b><span>${escapeHtml(T('replication.node_stat_follower'))}</span></div>
        <div><b id="tb-repl-node-${key}-isr">${r.isrCount ?? '—'}</b><span>${escapeHtml(T('replication.node_stat_isr'))}</span></div>
      </div>
      <div class="tb-node-sub" id="tb-repl-node-${key}-sub">${escapeHtml(nodeCardSubText(r))}</div>
    </div>
  `;
}

function patchNodeCard(host, r) {
  const key = CSS.escape(r._key);
  const card = host.querySelector(`#tb-repl-node-${key}`);
  if (!card) return;
  card.classList.toggle('tb-node-card--degraded', !!r.degraded);
  const dot = card.querySelector('.tb-node-dot');
  if (dot) dot.className = `tb-node-dot ${!r.reachable ? 'tb-node-dot--down' : (r.degraded ? 'tb-node-dot--warn' : 'tb-node-dot--live')}`;
  patchText(card.querySelector(`#tb-repl-node-${key}-leader`), r.leaderCount ?? '—');
  patchText(card.querySelector(`#tb-repl-node-${key}-follower`), r.followerCount ?? '—');
  patchText(card.querySelector(`#tb-repl-node-${key}-isr`), r.isrCount ?? '—');
  patchText(card.querySelector(`#tb-repl-node-${key}-sub`), nodeCardSubText(r));
}

function paintReplNodeCards() {
  const host = byId('tb-repl-nodes');
  if (!host) return;
  if (state.repl.loading && !state.repl.data) {
    host.innerHTML = `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('loading'))}</div>`;
    state.dom.nodeCards = null;
    return;
  }
  const rows = nodeCardRows(state.repl.data);
  if (!rows.length) {
    host.innerHTML = `<div class="tb-state tb-empty">${escapeHtml(T('replication.nodes_empty'))}</div>`;
    state.dom.nodeCards = null;
    return;
  }
  const diff = diffRowsByKey(state.dom.nodeCards, rows, (r) => r._key);
  if (state.dom.nodeCards == null || diff.added.length || diff.removed.length) {
    host.innerHTML = `<div class="tb-node-grid">${rows.map(nodeCardHtml).join('')}</div>`;
    state.dom.nodeCards = rows;
    return;
  }
  if (diff.updated.length) {
    rows.forEach((r) => patchNodeCard(host, r));
    state.dom.nodeCards = rows;
  }
}

function roleCellHtml(role) {
  if (role === 'none' || !role) return '<span class="tb-role-cell-empty">—</span>';
  return `<span class="tb-role-pill tb-role-pill--${escapeAttr(role)}">${escapeHtml(T(`replication.role_${role}`))}</span>`;
}

function roleMatrixRowHtml(row, nodes) {
  const key = row._key;
  const cells = nodes.map((n) => `<td id="tb-repl-cell-${key}-${escapeAttr(n.nodeId)}">${roleCellHtml(row.cells[n.nodeId])}</td>`).join('');
  const reasonKey = unavailableReasonI18nKey(row.unavailableReason);
  const actions = isSiteAdmin() ? `
    <td class="tb-row-actions">
      <tf-button variant="ghost" size="sm" class="tb-repl-transfer-leader" data-partition="${row.partition}">${escapeHtml(T('replication.action_transfer_leader'))}</tf-button>
      <tf-button variant="ghost" size="sm" class="tb-repl-reassign" data-partition="${row.partition}">${escapeHtml(T('replication.action_reassign'))}</tf-button>
    </td>` : '';
  return `
    <tr class="${row.unavailableReason ? 'tb-row-unavailable' : ''}" id="tb-repl-row-${key}">
      <td>
        P${row.partition}
        ${reasonKey ? `<div class="tf-chip warn tb-role-unavailable-chip">${escapeHtml(T(reasonKey))}</div>` : ''}
      </td>
      ${cells}
      <td class="mono" id="tb-repl-epoch-${key}">e${row.leaderEpoch}</td>
      ${actions}
    </tr>
  `;
}

function roleMatrixTableHtml(rows, nodes) {
  const nodeCols = nodes.map((n) => `<th>${escapeHtml(n.label || n.nodeId)}</th>`).join('');
  return `
    <table class="tb-role-matrix" id="tb-repl-matrix-table" style="width:100%;border-collapse:collapse;font-size:12px">
      <thead><tr>
        <th>${escapeHtml(T('replication.matrix_col_partition'))}</th>
        ${nodeCols}
        <th>${escapeHtml(T('replication.matrix_col_epoch'))}</th>
        ${isSiteAdmin() ? `<th>${escapeHtml(T('replication.matrix_col_actions'))}</th>` : ''}
      </tr></thead>
      <tbody>${rows.map((r) => roleMatrixRowHtml(r, nodes)).join('')}</tbody>
    </table>
  `;
}

function patchRoleMatrixRow(body, row, nodeIds) {
  const key = CSS.escape(row._key);
  const tr = body.querySelector(`#tb-repl-row-${key}`);
  if (!tr) return;
  tr.classList.toggle('tb-row-unavailable', !!row.unavailableReason);
  nodeIds.forEach((id) => {
    const cell = tr.querySelector(`#tb-repl-cell-${key}-${CSS.escape(id)}`);
    if (cell) cell.innerHTML = roleCellHtml(row.cells[id]);
  });
  patchText(tr.querySelector(`#tb-repl-epoch-${key}`), `e${row.leaderEpoch}`);
}

function wireRoleMatrixActions(body, topic) {
  body.querySelectorAll('.tb-repl-transfer-leader').forEach((btn) => {
    btn.addEventListener('click', () => openLeaderTransferModal(topic, Number(btn.dataset.partition)));
  });
  body.querySelectorAll('.tb-repl-reassign').forEach((btn) => {
    btn.addEventListener('click', () => openReassignModal(topic, Number(btn.dataset.partition)));
  });
}

function paintReplMatrix() {
  const titleEl = byId('tb-repl-matrix-title');
  const body = byId('tb-repl-matrix-body');
  if (!body) return;
  const topic = state.repl.topic;
  if (!topic) {
    patchText(titleEl, T('replication.matrix_title_generic'));
    body.innerHTML = `<div class="tb-state tb-empty">${escapeHtml(T('replication.matrix_select_topic'))}</div>`;
    state.dom.roleMatrix = null;
    return;
  }
  patchText(titleEl, T('replication.matrix_title', { topic }));
  const partitions = state.repl.data?.partitions || [];
  const nodes = state.repl.data?.nodes || [];
  const nodeIds = nodes.map((n) => n.nodeId);
  const rows = buildRoleMatrix(partitions, nodeIds).map((row) => ({ ...row, _key: String(row.partition) }));
  if (!rows.length) {
    body.innerHTML = `<div class="tb-state tb-empty">${escapeHtml(T('empty_topics'))}</div>`;
    state.dom.roleMatrix = null;
    return;
  }
  const diff = diffRowsByKey(state.dom.roleMatrix, rows, (r) => r._key);
  if (state.dom.roleMatrix == null || diff.added.length || diff.removed.length) {
    body.innerHTML = roleMatrixTableHtml(rows, nodes);
    state.dom.roleMatrix = rows;
    wireRoleMatrixActions(body, topic);
    return;
  }
  if (diff.updated.length) {
    rows.forEach((row) => patchRoleMatrixRow(body, row, nodeIds));
    state.dom.roleMatrix = rows;
  }
}

// Module-doc gap #10: no shrink/expand HISTORY exists on the wire (PLAN-M2
// §1e — only a metric + a UI event, never an audit row), so this renders
// the partitions' CURRENT `lagging[]` entries as a flat state list, not the
// mockup's illustrative timeline.
function paintReplLagState() {
  const host = byId('tb-repl-lag-state');
  if (!host) return;
  const topic = state.repl.topic;
  if (!topic) {
    host.innerHTML = `<div class="tb-state tb-empty">${escapeHtml(T('replication.matrix_select_topic'))}</div>`;
    return;
  }
  const nodes = state.repl.data?.nodes || [];
  const items = [];
  for (const p of (state.repl.data?.partitions || [])) {
    for (const lag of (Array.isArray(p.lagging) ? p.lagging : [])) {
      items.push({ partition: p.partition, ...lag });
    }
  }
  const gapNote = `<div class="tb-gap-note">${sprite('info')}${escapeHtml(T('replication.lag_state_gap_note'))}</div>`;
  if (!items.length) {
    host.innerHTML = `${gapNote}<div class="tb-state tb-empty">${escapeHtml(T('replication.lag_state_empty'))}</div>`;
    return;
  }
  const list = items.map((it) => `
    <div class="tb-lag-item">
      <div class="tb-lag-item-head">P${it.partition} · ${escapeHtml(nodeLabelById(nodes, it.nodeId))}</div>
      <div class="tb-lag-item-body">${escapeHtml(T('replication.lag_item_reason', {
        reason: it.reason || T('replication.node_lagging_reason_unknown'),
        bytes: formatBytes(it.lagBytes),
        ms: fmtCompact(Number(it.lagMs) || 0),
      }))}</div>
    </div>
  `).join('');
  host.innerHTML = `${gapNote}<div class="tb-lag-list">${list}</div>`;
}

function failoverKey(e) {
  return `${e.topic}|${e.partition}|${e.atMs}`;
}

function failoverRowHtml(e) {
  return `
    <tr>
      <td class="mono">${escapeHtml(e.topic)} / P${e.partition}</td>
      <td><span class="tf-chip">e${e.fromEpoch} → e${e.toEpoch}</span></td>
      <td class="mono">${escapeHtml(e.fromNode)} → ${escapeHtml(e.toNode)}</td>
      <td>${fmtCompact((Number(e.durationMs) || 0) / 1000)} s</td>
      <td>${escapeHtml(msToDate(e.atMs))}</td>
    </tr>
  `;
}

// Append-only (task requirement: "timeline appended"). Newest-first per the
// mockup's own ordering (m06:135-143); sorted defensively rather than
// trusting the server already returns that order. A poll that brought back
// NO new event (the common case) never touches `tbody` at all — only genuinely
// new keys get a `<tr>` inserted, at the top.
function paintReplFailovers() {
  const body = byId('tb-repl-failover-body');
  const emptyEl = byId('tb-repl-failover-empty');
  if (!body) return;
  const events = Array.isArray(state.repl.data?.failovers) ? state.repl.data.failovers : [];
  if (!events.length) {
    body.innerHTML = '';
    state.dom.failoverKeys = new Set();
    if (emptyEl) emptyEl.hidden = false;
    return;
  }
  if (emptyEl) emptyEl.hidden = true;
  const sorted = [...events].sort((a, b) => (Number(b.atMs) || 0) - (Number(a.atMs) || 0));
  const known = state.dom.failoverKeys instanceof Set ? state.dom.failoverKeys : new Set();
  const newOnes = sorted.filter((e) => !known.has(failoverKey(e)));
  if (body.children.length === 0 || newOnes.length === sorted.length) {
    body.innerHTML = sorted.map(failoverRowHtml).join('');
  } else if (newOnes.length) {
    // Insert as ONE chunk (not one `insertAdjacentHTML('afterbegin', …)` per
    // row) — `newOnes` is already newest-first; inserting row-by-row at
    // 'afterbegin' would reverse THEIR relative order whenever a single poll
    // brings back more than one new failover at once.
    body.insertAdjacentHTML('afterbegin', newOnes.map(failoverRowHtml).join(''));
  }
  state.dom.failoverKeys = new Set(sorted.map(failoverKey));
}

function nodeLabelById(nodes, nodeId) {
  const n = (Array.isArray(nodes) ? nodes : []).find((x) => x.nodeId === nodeId);
  return n?.label || nodeId || '—';
}

// "Przenieś lidera" (mockup's action on a role-matrix row) — target list is
// ISR-only (`leaderTransferCandidates`), same confirm-dialog/focus-trap
// shape as `openOffsetResetModal` above (this module has no shared
// "dialog with one <tf-select>" builder to call into).
function openLeaderTransferModal(topic, partition) {
  const row = (state.repl.data?.partitions || []).find((p) => p.partition === partition);
  if (!row) return;
  const candidates = leaderTransferCandidates(row);
  if (!candidates.length) {
    toast(T('replication.transfer_no_candidates'), 'error');
    return;
  }
  const nodes = state.repl.data?.nodes || [];
  const body = document.createElement('div');
  body.className = 'tb-wizard-form tb-reset-form';
  body.innerHTML = `
    <p>${escapeHtml(T('replication.transfer_modal_body', { topic, partition }))}</p>
    <tf-select id="tb-transfer-target" label="${escapeAttr(T('replication.transfer_field_target'))}" value="${escapeAttr(candidates[0])}">
      ${candidates.map((id) => `<option value="${escapeAttr(id)}">${escapeHtml(nodeLabelById(nodes, id))}</option>`).join('')}
    </tf-select>
    <p class="tb-field-hint">${escapeHtml(T('replication.transfer_hint'))}</p>
  `;

  const modal = document.createElement('tf-modal');
  modal.setAttribute('title', T('replication.transfer_title'));
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('size', 'sm');
  const bodySlot = document.createElement('div');
  bodySlot.setAttribute('slot', 'body');
  bodySlot.appendChild(body);
  modal.appendChild(bodySlot);
  const footer = document.createElement('div');
  footer.setAttribute('slot', 'footer');
  footer.className = 'tb-modal-footer';
  const cancel = document.createElement('tf-button');
  cancel.setAttribute('variant', 'secondary');
  cancel.textContent = T('common_cancel');
  cancel.addEventListener('click', () => closeModal(modal));
  const confirm = document.createElement('tf-button');
  confirm.setAttribute('variant', 'primary');
  confirm.textContent = T('replication.transfer_confirm');
  confirm.addEventListener('click', async () => {
    const targetNodeId = body.querySelector('#tb-transfer-target')?.value;
    if (!targetNodeId) return;
    try {
      await ApiBinary.action('busLeaderTransferRequest', buildLeaderTransferRequest(state.instanceId, topic, partition, targetNodeId));
      toast(T('replication.transfer_done'), 'success');
      closeModal(modal);
      await loadReplication(state.repl.topic);
    } catch (err) {
      toast(mapBusErrorMessage(err?.message, T), 'error');
    }
  });
  footer.append(cancel, confirm);
  modal.appendChild(footer);
  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  trapModalFocus(modal);
  modal.addEventListener('close', () => closeModal(modal), { once: true });
}

// "Zmień repliki" — multiselect of nodes, filtered to the session's own
// environment (SPEC D4); a foreign-env node renders disabled with a
// tooltip instead of being omitted, exactly like M02's node picker below
// (`wireNodePicker`'s doc), reusing the same `.tb-node-picker`/
// `.tb-node-picker-item` markup and CSS.
async function openReassignModal(topic, partition) {
  const localEnv = await getLocalEnvironment();
  const nodes = state.repl.data?.nodes || [];
  const row = (state.repl.data?.partitions || []).find((p) => p.partition === partition);
  const currentReplicas = new Set(row?.replicas || []);

  const body = document.createElement('div');
  body.className = 'tb-wizard-form';
  const items = nodes.map((n) => {
    const foreign = !isSameEnvironment(n, localEnv);
    return `
      <label class="tb-node-picker-item${foreign ? ' is-foreign' : ''}"${foreign ? ` title="${escapeAttr(T('replication.reassign_foreign_tooltip'))}"` : ''}>
        <input type="checkbox" value="${escapeAttr(n.nodeId)}" ${foreign ? 'disabled' : ''} ${currentReplicas.has(n.nodeId) ? 'checked' : ''} />
        <span class="tb-node-picker-name">${escapeHtml(n.label || n.nodeId)}</span>
        ${chipHtml(envChip(n.environment))}
      </label>
    `;
  }).join('');
  body.innerHTML = `
    <p>${escapeHtml(T('replication.reassign_modal_body', { topic, partition }))}</p>
    <fieldset class="tb-node-picker" id="tb-reassign-nodes" aria-labelledby="tb-reassign-nodes-legend">
      <legend id="tb-reassign-nodes-legend">${escapeHtml(T('replication.reassign_field_nodes'))}</legend>
      ${items || `<p class="tb-field-hint">${escapeHtml(T('replication.nodes_empty'))}</p>`}
    </fieldset>
    <p class="tb-field-hint">${escapeHtml(T('replication.reassign_hint'))}</p>
  `;

  const modal = document.createElement('tf-modal');
  modal.setAttribute('title', T('replication.reassign_title'));
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('size', 'sm');
  const bodySlot = document.createElement('div');
  bodySlot.setAttribute('slot', 'body');
  bodySlot.appendChild(body);
  modal.appendChild(bodySlot);
  const footer = document.createElement('div');
  footer.setAttribute('slot', 'footer');
  footer.className = 'tb-modal-footer';
  const cancel = document.createElement('tf-button');
  cancel.setAttribute('variant', 'secondary');
  cancel.textContent = T('common_cancel');
  cancel.addEventListener('click', () => closeModal(modal));
  const confirm = document.createElement('tf-button');
  confirm.setAttribute('variant', 'primary');
  confirm.textContent = T('replication.reassign_confirm');
  confirm.addEventListener('click', async () => {
    const replicas = Array.from(body.querySelectorAll('input[type="checkbox"]:checked')).map((c) => c.value);
    if (!replicas.length) {
      toast(T('replication.reassign_empty_error'), 'error');
      return;
    }
    try {
      await ApiBinary.action('busReassignRequest', buildReassignRequest(state.instanceId, topic, partition, replicas));
      toast(T('replication.reassign_done'), 'success');
      closeModal(modal);
      await loadReplication(state.repl.topic);
    } catch (err) {
      toast(mapBusErrorMessage(err?.message, T), 'error');
    }
  });
  footer.append(cancel, confirm);
  modal.appendChild(footer);
  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  trapModalFocus(modal);
  modal.addEventListener('close', () => closeModal(modal), { once: true });
}

// M03's "otwórz w M06" button: switches straight to the replication tab
// with this topic pre-selected, same as a real tab click.
function openReplicationForTopic(topicName) {
  state.view = null;
  state.tab = 'replication';
  state.repl.topic = topicName;
  state.repl.loaded = false;
  paintHeadActions();
  renderPanel();
}

export default TentaBusScreen;
