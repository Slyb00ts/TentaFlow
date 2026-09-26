// =============================================================================
// File: modules/tentabus.js — the TentaBus screen shell (SUM/tentabus/
// PLAN-UI-20260923.md U0, mockups SUM/mockups/tentabus-20260923 T01/T11/T12):
// breadcrumb, the TentaBus header card with the instance picker, the six
// underlined main tabs (Przegląd / Topiki / Odbiorcy / Nieprzetworzone /
// Wzory wiadomości / Kopie i nody) with their counters, the address
// (`#/tentabus?instance=…&tab=…&topic=…&group=…`, see modules/tentabus/
// routes.js) and the polling every tab reads. Przegląd, Topiki (with the
// topic creator, the delete window and the message preview) and Wzory
// wiadomości live in modules/tentabus/*. The topic detail, Odbiorcy,
// Nieprzetworzone and Kopie i nody bodies below are the M1/M2 views (topic
// detail, consumer groups + offset reset, unprocessed messages per topic,
// replication) until their U2–U4 packages replace them.
//
// PROTOCOL GAPS (M1 wire vs. the accepted mockups/PLAN — each one is called
// out again at its exact render site so a reviewer does not have to trust
// this comment alone). Follow-up "tor U" narrowed this list — see the
// resolved items marked below:
//  1. RESOLVED (tor U): `BusStatsSnapshotWire` now carries org-wide
//     `totalMsgsInPerSec`/`totalBytesInPerSec`/`totalBytesOnDisk`/`totalLag`/
//     `totalDlqDepth` plus a per-topic `topics[]` breakdown — the per-topic
//     numbers below are real. STILL OPEN: the topic detail's chart reads only
//     this polling snapshot (3s cadence), rendered
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
//  4. RESOLVED (tor U): `BusOffsetResetMode` gained a 4th `Timestamp{ts_ms}`
//     variant — the reset modal below offers all 4 mockup modes, including
//     a datetime-local picker converted to epoch ms.
//  6. ACL only models `subject_type/subject_id/access_level(allow|deny)` —
//     no `produce`/`consume`/`admin` per-action column exists on
//     `resource_permissions` (see `dispatch/bus.rs`'s module doc). The ACL
//     tab below uses allow/deny per subject, not per-action checkboxes.
//  7. RESOLVED (tor U): `MessagesBrowse`/`DlqList` responses now carry a
//     `partitions[]` breakdown (`earliestOffset`/`highWatermark`/
//     `nextOffset`/`hasMore`, per partition) and the matching requests
//     accept `fromOffsets` (per-partition cursors) — the unprocessed-message
//     list below pages each partition independently.
//  8. RESOLVED (tor U): `BusCapabilitiesRequest` (`canRead`/`canWrite`/
//     `canAdmin`/`isSiteAdmin`) is fetched once on mount and gates every
//     control below — every mutating action needs `canAdmin`, and a
//     read-only session (`canRead` only) sees the same screens with every
//     action button hidden instead of the earlier `me.role === 'admin'`
//     client-side guess. Offset reset, ACL writes, reassignment and leader
//     transfer used to need `isSiteAdmin` on top; that separate tier no
//     longer exists in `dispatch/bus.rs` and the gate moved to `canAdmin`.
//  9. No quota UI: `BusQuotaGetRequest`/`QuotaSetRequest` are wired in
//     `codec.js`, but `SPEC.md` (§4, the mockup map) has no quota screen or
//     "Limity org" card in any of the 8 accepted mockups — deliberately not
//     built here to avoid inventing UI the mockups never asked for.
//  10. M2 (PLAN-M2.md §1f, mockup m06): new M06 "Replikacja i failover" view
//      (`busReplicaListRequest`/`ReplicaListResponse{nodes,partitions,
//      failovers}`), "Przenieś lidera" (`busLeaderTransferRequest`) and
//      "Zmień repliki" (`busReassignRequest`) — both gated `canAdmin()`.
//      They were on `isSiteAdmin()` while `dispatch/bus.rs` still had a
//      separate `bus_dispatch_admin` `#[policy(Admin)]` tier; that tier was
//      removed and both handlers now open with `gate_admin` (`bus.admin` in
//      the instance matrix AND the `org.admin` role), which is what
//      `can_admin` reports. NOT built: a real "ISR shrink/expand" HISTORY timeline —
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
//      `SUM/mockups/tentabus-app-20260903/SPEC.md`): the groups/DLQ/
//      replication tables — this wave only threads the instance id
//      through, it does not redraw any of those existing views.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, formatBytes, fmtCompact } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';
import { setAttr, setText, patchHtml, setClass } from '/js/lib/dom-patch.js';
import { fmtCount, fmtElapsed, fmtRetention, loadErrorKind } from '/js/modules/tentabus/format.js';
import { MAIN_TABS, DEFAULT_TAB, parseRoute, routeParams } from '/js/modules/tentabus/routes.js';
import { shellCounts, userTopics, userRate, nodeRows } from '/js/modules/tentabus/model.js';
import { laggingReplicas, isLagging, lagSeriesKey } from '/js/modules/tentabus/alerts.js';
import { drawOverview, pushOverviewSample, CHART_WINDOW_SECS } from '/js/modules/tentabus/overview.js';
import { drawSchemas } from '/js/modules/tentabus/schemas.js';
import { drawTopics } from '/js/modules/tentabus/topics.js';
import { openTopicCreator } from '/js/modules/tentabus/topic-creator.js';
import { openTopicDelete } from '/js/modules/tentabus/topic-delete.js';
import { openMessagePreview } from '/js/modules/tentabus/message-preview.js';
import { bytesToPreviewText, headerText } from '/js/modules/tentabus/payload.js';
import { confirmDialog } from '/js/lib/confirm-dialog.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-button.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
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
const REPLICA_POLL_MS = 10_000;
const TABS = MAIN_TABS;
const TAB_ICONS = { overview: 'gauge', topics: 'share', groups: 'users', dlq: 'inbox', schemas: 'file-code', replication: 'branch' };
// The mutually-exclusive views `#tb-panel` can show (six tabs + topic
// detail) — each gets its OWN persistent container (`ensureViewContainer`)
// so switching between them shows/hides existing DOM instead of tearing it
// down and rebuilding it, keeping scroll position, in-progress search text,
// table sort and focus intact across a tab switch.
const VIEW_SLOTS = ['overview', 'topics', 'groups', 'dlq', 'schemas', 'detail', 'replication'];
const DLQ_RETRY_ALL_MAX = 500;
const COMMIT_MODES = ['auto_after_success', 'explicit', 'at_most_once'];
// Rolling in-memory window for the topic detail's live chart — there is no
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

// The topic detail's "(polityka jawna)" secondary label (KRYTYK-M1-R5.md b.7): a
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

// The broker's own `__*` topics (the unprocessed-message stores themselves,
// metrics) are never a source to pick.
function dlqSourceTopicOptions(topics) {
  return (Array.isArray(topics) ? topics : [])
    .filter((t) => !(t.isDlq ?? t.is_dlq) && !String(t.name || '').startsWith('__'))
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
// A kept choice must still name a listed topic (an address can carry a stale
// or foreign one); the default is the topic with the most unprocessed
// messages in the stats snapshot, then the first topic.
function resolveDlqEntrySource(currentSource, topics, statsTopics = []) {
  const options = dlqSourceTopicOptions(topics);
  if (currentSource && (!options.length || options.some((o) => o.value === currentSource))) return currentSource;
  const depth = new Map((statsTopics || []).map((t) => [t.topic, Number(t.dlqDepth) || 0]));
  const best = options.reduce((top, o) => (top == null || (depth.get(o.value) || 0) > (depth.get(top.value) || 0) ? o : top), null);
  return best ? best.value : '';
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
// the same silent all-hidden failure. `isSiteAdmin` is still decoded here —
// the wire field has not been retired — but nothing in this module gates on
// it any more; see `canAdmin`'s own comment.
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

// Ring-buffer append for the "live last N samples" chart (the topic detail
// overview): keeps at most `maxLen` points, oldest evicted first,
// so the series scrolls left sample by sample instead of resetting to empty
// and redrawing from zero. Mutates and returns `arr` (the caller's
// long-lived series array) rather than allocating a new one every poll.
function pushWindowSample(arr, point, maxLen) {
  arr.push(point);
  if (arr.length > maxLen) arr.splice(0, arr.length - maxLen);
  return arr;
}

// Key-based diff between two row-array snapshots (the M04
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
  tab: DEFAULT_TAB,
  view: null, // null | { kind: 'topic-detail', name }
  detailTab: 'overview',

  topics: [],
  topicsLoaded: false,
  // The last failed topic-list load (Topiki shows it), `null` after a success.
  topicsError: null,
  // "Utworzono topik …" / "Usunięto topik …" above the list, until the tab is left.
  topicsNotice: null,

  stats: null,
  statsTimer: null,

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
    groupsTableRows: null,
    nodeCards: null, roleMatrix: null, failoverKeys: null,
  },

  // What the frame (header card, tab counters) and Przegląd read beside the
  // stats snapshot — see `freshShellState`.
  shell: freshShellState(),
};

// `null` = that source has not answered yet (the figure is left out), never
// a guessed zero.
function freshShellState() {
  return {
    instances: [],
    version: null,
    subjects: null,
    subjectsError: null,
    statsAt: 0,
    statsError: null,
    ratePoints: [],
    nodes: null,
    replicaTopics: null,
    replicaLags: [],
    lagSeries: new Map(),
    replicaTimer: null,
  };
}

// `canAdmin` gates EVERY mutating admin action in this module: topic CRUD,
// pause/resume, DLQ retry/discard, offset reset, ACL writes, partition
// reassignment and leader transfer. It fails closed to `false` before the
// first `busCapabilitiesRequest` resolves.
//
// The four heaviest of those used to be gated on `isSiteAdmin` instead, back
// when `dispatch/bus.rs` registered them on a separate, coarser
// `#[policy(Admin)]` tier (`bus_dispatch_admin`). That tier is gone: all
// eleven former admin variants now sit on the plain `UserSession` dispatch and
// every one of the four opens with `gate_admin`, which is `bus.admin` in the
// instance permission matrix AND the `org.admin` role — exactly what
// `capabilities_v1` folds into `can_admin`. Keeping the old gate here hid four
// working controls from the delegated org operator the double lock was built
// for, and showed them to a site admin acting in an org where `gate_admin`
// would refuse. `isSiteAdmin` is deliberately not read any more; the wire field
// survives for compatibility and no handler consults it either.
function canAdmin() {
  return state.capabilities?.canAdmin === true;
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
    // The URL named an instance that does not exist (uninstalled since the
    // link was made, or simply wrong). Falling through to the
    // single-enabled-instance shortcut below would silently open a
    // DIFFERENT instance under the requested one's URL — the critic hit
    // exactly this via a sidebar entry left behind after an uninstall, and
    // saw `prod`'s data under `?instance=<test's id>`. An explicit request
    // that cannot be honoured must never be answered with another
    // instance's data, so stop here regardless of how many are enabled.
    return { target: null, instances, unknownRequestedId: requestedId };
  }
  const enabled = instances.filter((a) => a.enabled);
  if (enabled.length === 1) return { target: enabled[0], instances };
  return { target: null, instances };
}

// Shown only when the address names no instance (or one that does not
// exist) and there is not exactly one to open: the same header card as the
// screen itself, then one row per instance. The screen's own picker takes
// over as soon as one is open.
function renderInstanceGate(instances, unknownRequestedId = null) {
  const root = byId('tb-root');
  if (!root) return;
  const enabled = instances.filter((a) => a.enabled);
  let hint = T('instance_picker_hint');
  if (unknownRequestedId) hint = T('instance_picker_unknown', { id: unknownRequestedId });
  else if (!enabled.length) hint = T('instance_picker_empty');
  const body = enabled.length === 0
    ? `<tf-empty-state icon="apps" title="${escapeAttr(T('title'))}" message="${escapeAttr(hint)}">
        <tf-button variant="secondary" id="tb-instance-goto-apps">${escapeHtml(I18n.t('nav.apps_home'))}</tf-button>
      </tf-empty-state>`
    : `<div class="tb-instance-list">${enabled.map((a) => `
        <div class="tb-instance-row" role="link" tabindex="0" data-instance="${escapeAttr(a.addonId)}">
          <span class="tb-instance-ico">${sprite('broadcast')}</span>
          <span class="tb-instance-row-title">${escapeHtml(a.title)}</span>
          ${sprite('chevron-right')}
        </div>`).join('')}
      </div>`;
  root.innerHTML = `
    <tf-breadcrumb class="tb-crumbs"><tf-breadcrumb-item current>${escapeHtml(T('title'))}</tf-breadcrumb-item></tf-breadcrumb>
    <div class="tf-detail-header tb-app-head">
      <div class="big-ico">${sprite('broadcast')}</div>
      <div class="d-meta">
        <div class="d-name">${escapeHtml(T('title'))}</div>
        <div class="d-sub">${escapeHtml(T('subtitle'))}</div>
      </div>
    </div>
    <div class="section-card">
      ${enabled.length ? `<div class="section-sub">${escapeHtml(hint)}</div>` : ''}
      ${body}
    </div>
  `;
  root.querySelector('#tb-instance-goto-apps')?.addEventListener('click', () => Router.navigate('apps-home'));
  const open = (el) => Router.navigate('tentabus', { instance: el.dataset.instance });
  root.querySelectorAll('.tb-instance-row').forEach((el) => {
    el.addEventListener('click', () => open(el));
    el.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); open(el); } });
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
    const route = parseRoute(params);
    const { target, instances, unknownRequestedId } = await resolveInstanceGate(route.instance);
    if (!target) {
      state.instanceId = null;
      state.instanceLabel = '';
      renderInstanceGate(instances, unknownRequestedId || null);
      return;
    }
    state.instanceId = target.addonId;
    state.instanceLabel = target.title;
    state.shell.instances = instances.filter((a) => a.enabled || a.addonId === target.addonId);
    state.tab = route.tab;
    if (route.dlqTopic) state.dlqSource = route.dlqTopic;

    try {
      state.capabilities = unwrapCapabilities(await ApiBinary.one('busCapabilitiesRequest', { instanceId: requireInstanceId(state.instanceId) }));
    } catch {
      state.capabilities = NO_CAPABILITIES;
    }
    const root = byId('tb-root');
    if (!root) return;
    root.innerHTML = shellHtml();
    wireShell(root);

    renderPanel();
    if (route.topic) openTopicDetail(route.topic);
    else if (route.group && route.groupTopic) openGroupDetail(route.group, route.groupTopic);
    loadShellMeta();
    startStatsPolling();
    // Task 3: groups load together with topics so every counter reads the
    // same lists regardless of which tab is open first.
    await Promise.all([loadTopics(), loadGroups()]);
  },

  unmount() {
    // Stopping the polls BEFORE anything else is what keeps one instance's
    // numbers out of the next mount: a leaked timer would keep firing against
    // `state.instanceId`, which the lines below repoint.
    stopStatsPolling();
    state.instanceId = null;
    state.instanceLabel = '';
    state.capabilities = NO_CAPABILITIES;
    state.tab = DEFAULT_TAB;
    state.view = null;
    state.detailTab = 'overview';
    state.topics = [];
    state.topicsLoaded = false;
    state.topicsError = null;
    state.topicsNotice = null;
    state.stats = null;
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
    state.shell = freshShellState();
    state.dom = {
      groupsTableRows: null,
      nodeCards: null, roleMatrix: null, failoverKeys: null,
    };
  },
};

// =============================================================================
// Screen shell (T01 frame): breadcrumb, the TentaBus header card, the six
// underlined main tabs with their counters, and the tab body. Built ONCE per
// mount; every later change patches it in place (`paintShell`), so a poll
// never rebuilds the header, the tab strip or the instance picker.
// =============================================================================

function shellHtml() {
  return `
    <tf-breadcrumb class="tb-crumbs" id="tb-crumbs"></tf-breadcrumb>
    <div class="tf-detail-header tb-app-head">
      <div class="big-ico">${sprite('broadcast')}</div>
      <div class="d-meta">
        <div class="d-name">${escapeHtml(T('title'))} <span id="tb-head-chips" class="tb-head-chips"></span></div>
        <div class="d-sub" id="tb-head-sub"></div>
        <div class="d-badges" id="tb-head-badges"></div>
      </div>
      <div class="d-actions">
        <tf-select id="tb-instance-select" prefix="${escapeAttr(T('shell.instance'))}"></tf-select>
        <tf-button variant="secondary" icon="refresh" id="tb-refresh">${escapeHtml(T('shell.refresh'))}</tf-button>
      </div>
    </div>
    <tf-tabs id="tb-tabs" variant="underline" scroll-align="center" value="${escapeAttr(state.tab)}">
      ${MAIN_TABS.map((id) => `<tf-tab id="${id}" icon="${TAB_ICONS[id]}">${escapeHtml(T(`shell.tabs.${id}`))}</tf-tab>`).join('')}
    </tf-tabs>
    <div id="tb-panel" class="tb-panel"></div>
  `;
}

function wireShell(root) {
  root.querySelector('#tb-tabs')?.addEventListener('change', (e) => {
    const id = e.detail?.value;
    if (id && TABS.includes(id)) setTab(id);
  });
  const select = root.querySelector('#tb-instance-select');
  select.setOptions(state.shell.instances.map((a) => ({ value: a.addonId, label: a.title })), state.instanceId);
  select.addEventListener('change', (e) => {
    const next = e.detail?.value;
    if (next && next !== state.instanceId) Router.navigate('tentabus', { instance: next });
  });
  root.querySelector('#tb-refresh')?.addEventListener('click', () => refreshAll());
  const crumbs = root.querySelector('#tb-crumbs');
  crumbs.addEventListener('click', (e) => {
    const link = e.target.closest('a.tf-breadcrumb-item');
    if (!link) return;
    e.preventDefault();
    const links = [...crumbs.querySelectorAll('a.tf-breadcrumb-item')];
    const acts = [...crumbs.querySelectorAll('tf-breadcrumb-item')].filter((i) => !i.hasAttribute('current')).map((i) => i.dataset.crumb || '');
    const act = acts[links.indexOf(link)];
    if (act === 'overview') setTab('overview');
    else if (act && TABS.includes(act)) setTab(act);
  });
}

// The one breadcrumb: TentaBus › <instance> › <tab> › <topic>. Rewritten only
// when its items change (navigation), never by a poll.
function paintCrumbs() {
  const bar = byId('tb-crumbs');
  if (!bar) return;
  const href = (params) => `#/tentabus?${new URLSearchParams(routeParams({ instance: state.instanceId, ...params })).toString()}`;
  const items = [{ label: T('title'), act: 'overview', href: href({ tab: DEFAULT_TAB }) }];
  const onOverview = state.tab === DEFAULT_TAB && !state.view;
  items.push(onOverview ? { label: state.instanceLabel } : { label: state.instanceLabel, act: 'overview', href: href({ tab: DEFAULT_TAB }) });
  if (!onOverview) {
    const tabLabel = T(`shell.tabs.${state.tab}`);
    if (state.view?.kind === 'topic-detail') {
      items.push({ label: tabLabel, act: 'topics', href: href({ tab: 'topics' }) });
      items.push({ label: state.view.name });
    } else {
      items.push({ label: tabLabel });
    }
  }
  const html = items.map((it) => (it.act
    ? `<tf-breadcrumb-item href="${escapeAttr(it.href)}" data-crumb="${escapeAttr(it.act)}">${escapeHtml(it.label)}</tf-breadcrumb-item>`
    : `<tf-breadcrumb-item current>${escapeHtml(it.label)}</tf-breadcrumb-item>`)).join('');
  if (bar.__tbCrumbs === html) return;
  bar.__tbCrumbs = html;
  bar.querySelectorAll('tf-breadcrumb-item').forEach((i) => i.remove());
  const tpl = document.createElement('template');
  tpl.innerHTML = html;
  bar.insertBefore(tpl.content, bar.firstChild);
  if (bar._nav && typeof bar._render === 'function') bar._render();
}

// The address names the view (instance, tab, open topic or consumer) through
// the router's own `replaceParams`: no history entry per click, and the
// router's notion of the current params stays true for a language repaint.
function syncLocation() {
  if (!state.instanceId) return;
  const gd = state.tab === 'groups' ? state.groupDetail : null;
  Router.replaceParams(routeParams({
    instance: state.instanceId,
    tab: state.tab,
    topic: state.view?.kind === 'topic-detail' ? state.view.name : null,
    group: gd?.group || null,
    groupTopic: gd?.topic || null,
    dlqTopic: state.tab === 'dlq' ? state.dlqSource || null : null,
  }));
}

// Header card, tab counters and the stale state — everything a poll moves in
// the frame. Each figure is left out until its source has answered.
function paintShell() {
  const root = byId('tb-root');
  if (!root || !byId('tb-head-chips')) return;
  const sh = state.shell;
  const counts = shellCounts({ stats: state.stats, topicList: state.topicsLoaded ? state.topics : null, subjects: sh.subjects, nodes: sh.nodes });
  const errorKind = sh.statsError ? loadErrorKind(sh.statsError) : null;
  const stale = Boolean(state.stats && errorKind);
  let status;
  if (errorKind === 'denied') status = { tone: 'warn', label: T('shell.status_denied') };
  else if (errorKind) status = { tone: 'err', label: T('shell.status_lost') };
  else if (state.stats) status = { tone: 'ok', label: T('shell.status_ok') };
  else status = { tone: 'neutral', label: T('shell.loading') };
  patchHtml(byId('tb-head-chips'), [
    `<tf-chip size="sm" variant="outline" dot data-role="status"></tf-chip>`,
    counts.nodes != null ? `<tf-chip size="sm" variant="outline" status="info" icon="branch" data-role="nodes"></tf-chip>` : '',
    sh.version ? `<tf-chip size="sm" variant="outline" status="neutral" data-role="version"></tf-chip>` : '',
  ].join(''));
  const chips = byId('tb-head-chips');
  setAttr(chips.querySelector('[data-role="status"]'), 'status', status.tone);
  setAttr(chips.querySelector('[data-role="status"]'), 'label', status.label);
  setAttr(chips.querySelector('[data-role="nodes"]'), 'label', counts.nodes != null ? T('shell.chip_nodes', { count: fmtCount(counts.nodes), n: counts.nodes }) : null);
  setAttr(chips.querySelector('[data-role="version"]'), 'label', sh.version ? T('shell.chip_version', { v: sh.version }) : null);

  const now = Date.now();
  const age = sh.statsAt ? fmtElapsed(now - sh.statsAt) : null;
  setText(byId('tb-head-sub'), [
    T('shell.meta_instance', { name: state.instanceLabel }),
    age ? T(stale ? 'shell.meta_stale' : 'shell.meta_refreshed', { ago: age }) : null,
  ].filter(Boolean).join(' · '));

  const badges = byId('tb-head-badges');
  patchHtml(badges, [
    counts.topics != null ? '<tf-chip size="sm" variant="outline" status="accent" icon="share" data-role="topics"></tf-chip>' : '',
    counts.dlq != null ? '<tf-chip size="sm" variant="outline" status="neutral" icon="inbox" data-role="dlq"></tf-chip>' : '',
    counts.schemas != null ? '<tf-chip size="sm" variant="outline" status="neutral" icon="file-code" data-role="schemas"></tf-chip>' : '',
    sh.nodes?.length ? '<tf-chip size="sm" variant="outline" status="info" icon="cpu" data-role="node-list"></tf-chip>' : '',
  ].join(''));
  setAttr(badges.querySelector('[data-role="topics"]'), 'label', counts.topics != null ? T('shell.badge_topics', { count: fmtCount(counts.topics), n: counts.topics }) : null);
  const dlqBadge = badges.querySelector('[data-role="dlq"]');
  setAttr(dlqBadge, 'label', counts.dlq != null ? T('shell.badge_dlq', { count: fmtCount(counts.dlq), n: counts.dlq }) : null);
  setAttr(dlqBadge, 'status', counts.dlq ? 'warn' : 'neutral');
  setAttr(badges.querySelector('[data-role="schemas"]'), 'label', counts.schemas != null ? T('shell.badge_schemas', { count: fmtCount(counts.schemas), n: counts.schemas }) : null);
  setAttr(badges.querySelector('[data-role="node-list"]'), 'label', sh.nodes?.length ? T('shell.badge_nodes', { list: sh.nodes.map((n) => n.label || n.nodeId).join(' · ') }) : null);
  setClass(badges, 'is-stale', stale);

  const select = byId('tb-instance-select');
  setAttr(select, 'dot', status.tone === 'neutral' ? null : status.tone);

  const tabs = byId('tb-tabs');
  if (tabs) {
    setClass(tabs, 'is-stale', stale);
    const tabCount = { topics: counts.topics, groups: counts.groups, dlq: counts.dlq, schemas: counts.schemas, replication: counts.nodes };
    for (const [id, value] of Object.entries(tabCount)) {
      setAttr(tabs.querySelector(`tf-tab#${id}`), 'count', value != null ? fmtCount(value) : null);
    }
  }
}

// Navigation, crumbs and the address follow every view change; the header
// counters follow every data change.
function paintFrame() {
  const tabs = byId('tb-tabs');
  if (tabs && tabs.getAttribute('value') !== state.tab) tabs.value = state.tab;
  paintCrumbs();
  syncLocation();
  paintShell();
}

// Version (from the platform's addon registry: the bus does not know its own
// package version) and the message-pattern list behind the "wzory" counter.
async function loadShellMeta() {
  const instanceId = state.instanceId;
  ApiBinary.list('addonsListRequest', { arrayKey: 'addons' }).then((addons) => {
    if (state.instanceId !== instanceId) return;
    const row = (addons || []).find((a) => a.addonId === instanceId);
    state.shell.version = row?.version ? String(row.version) : null;
    paintShell();
  }, () => {});
  await loadSubjects();
}

async function loadSubjects() {
  const instanceId = state.instanceId;
  try {
    const subjects = await ApiBinary.list('busSchemaSubjectListRequest', { arrayKey: 'subjects', payload: { instanceId: requireInstanceId(instanceId) } });
    if (state.instanceId !== instanceId) return;
    state.shell.subjects = subjects || [];
    state.shell.subjectsError = null;
  } catch (err) {
    if (state.instanceId !== instanceId) return;
    state.shell.subjectsError = err;
  }
  paintShell();
  if (state.tab === 'schemas' && !state.view) renderPanel();
}

// "Odśwież": asks every source again and repaints the open tab.
async function refreshAll() {
  refreshStats();
  refreshReplicas();
  loadSubjects();
  await Promise.all([loadTopics(), loadGroups()]);
  if (state.view?.kind === 'topic-detail') loadTopicDetail(state.view.name);
  else if (state.tab === 'dlq') loadDlqRecords(true);
  else if (state.tab === 'replication') { state.repl.loaded = false; renderPanel(); }
  else if (state.tab === 'groups' && state.groupDetail) openGroupDetail(state.groupDetail.group, state.groupDetail.topic);
}

// What Przegląd and Wzory wiadomości read, and where their buttons lead.
const tabContext = {
  view() {
    const sh = state.shell;
    return {
      stats: state.stats,
      error: sh.statsError,
      errorKind: sh.statsError ? loadErrorKind(sh.statsError) : null,
      topicList: state.topics,
      nodes: sh.nodes,
      replicaTopics: sh.replicaTopics,
      replicaLags: sh.replicaLags,
      lagSeries: sh.lagSeries,
      instanceLabel: state.instanceLabel,
      stale: Boolean(state.stats && sh.statsError),
      ratePoints: sh.ratePoints,
      nowMs: Date.now(),
    };
  },
  go(action) {
    if (action.kind === 'tab' && TABS.includes(action.tab)) setTab(action.tab);
    else if (action.kind === 'topic') { setTab('topics'); openTopicDetail(action.topic); }
    else if (action.kind === 'group') { setTab('groups'); openGroupDetail(action.group, action.topic); }
    else if (action.kind === 'dlq') {
      // Chosen BEFORE the tab opens, so the tab's own default pick does not
      // start a second, racing load of another topic.
      if (action.topic && action.topic !== state.dlqSource) {
        state.dlqSource = action.topic;
        state.dlqRecords = null;
        state.dlqPartitions = [];
      }
      setTab('dlq');
    }
    else if (action.kind === 'retry') refreshAll();
  },
};

const schemasContext = {
  view() {
    const sh = state.shell;
    return {
      subjects: sh.subjects,
      error: sh.subjectsError,
      errorKind: sh.subjectsError ? loadErrorKind(sh.subjectsError) : null,
      instanceLabel: state.instanceLabel,
    };
  },
  go(action) {
    if (action.kind === 'retry') { state.shell.subjectsError = null; renderPanel(); loadSubjects(); }
  },
};

function setTab(id) {
  if (state.view) state.view = null;
  if (id !== 'groups') state.groupDetail = null;
  if (id !== 'topics') state.topicsNotice = null;
  state.tab = id;
  renderPanel();
  if (id === 'groups' && !state.groupsLoaded) loadGroups();
  if (id === 'dlq') ensureDlqTabReady();
  // 'replication' needs no entry here — `renderPanel()` above already ran
  // `renderReplicationTab`, which triggers its own load.
}

// Persistent per-view container inside `#tb-panel`, keyed by `VIEW_SLOTS`
// entry (`data-tb-view-slot`, distinct from `ensureSkeleton`'s own
// `data-tb-view` marker on the SAME element — one records "which view is this
// container for" and never changes once created, the other records "which
// skeleton variant is currently built inside it"). Built once, on first
// visit; never removed for the life of the mount.
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
  paintFrame();
  if (!activeEl) return;
  if (activeKey === 'overview') { drawOverview(activeEl, tabContext); return; }
  if (activeKey === 'schemas') { drawSchemas(activeEl, schemasContext); return; }
  if (activeKey === 'detail') { renderTopicDetail(activeEl); return; }
  if (activeKey === 'topics') { drawTopics(activeEl, topicsContext); return; }
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
// Polling — BusStatsSnapshotRequest every 3 s (the header, the tab counters,
// Przegląd and the legacy tab strips all read it) and ReplicaListRequest every
// 10 s (node state and lagging replicas). Plain polls, not push
// subscriptions; started once in mount(), stopped in unmount(). A failed poll
// keeps the last data on screen and turns the header to "Brak połączenia"
// with the age of that data (T12) — it never blanks the numbers.
// =============================================================================

function startStatsPolling() {
  stopStatsPolling();
  refreshStats();
  refreshReplicas();
  state.statsTimer = setInterval(refreshStats, STATS_POLL_MS);
  state.shell.replicaTimer = setInterval(refreshReplicas, REPLICA_POLL_MS);
}

function stopStatsPolling() {
  if (state.statsTimer) clearInterval(state.statsTimer);
  state.statsTimer = null;
  if (state.shell.replicaTimer) clearInterval(state.shell.replicaTimer);
  state.shell.replicaTimer = null;
}

async function refreshStats() {
  const instanceId = state.instanceId;
  if (!instanceId) return;
  try {
    const stats = await ApiBinary.one('busStatsSnapshotRequest', { instanceId: requireInstanceId(instanceId) });
    if (state.instanceId !== instanceId) return;
    state.stats = stats;
    state.shell.statsAt = Date.now();
    state.shell.statsError = null;
  } catch (err) {
    if (state.instanceId !== instanceId) return;
    state.shell.statsError = err;
    paintShell();
    if (state.tab === 'overview' && !state.view) renderPanel();
    return;
  }
  ensureDlqTabReady();
  const rate = userRate(state.stats);
  pushRateSample(state.shell.ratePoints, state.shell.statsAt, rate);
  paintShell();
  if (state.tab === 'overview' && !state.view) {
    const body = document.querySelector('#tb-panel > [data-tb-view-slot="overview"]');
    pushOverviewSample(body, rate, state.shell.statsAt);
    renderPanel();
  }
  if (state.tab === 'topics' && !state.view) renderPanel();
  if (state.tab === 'groups' && !state.view && state.groupsLoaded) paintGroupsTable();
  // M06 patches its own DOM in place on the same cadence while visible.
  if (state.tab === 'replication' && !state.view) {
    pollReplication();
  }
  if (state.view?.kind === 'topic-detail' && state.detail?.topic) {
    // Sample the OPEN topic's own series every poll so switching back to its
    // overview does not lose the window already collected.
    const ts = findTopicStats(state.stats?.topics, state.detail.topic.name);
    if (ts && state.detailChartSeries) pushChartSample(state.detailChartSeries, ts.msgsInPerSec, ts.bytesInPerSec, ts.totalLag);
    if (state.detailTab === 'overview') renderDetailBody();
  }
}

// Keeps the live-chart window as epoch-ms points, so a chart built later
// (Przegląd opened after another tab) starts from what the screen already saw.
function pushRateSample(points, atMs, rate) {
  points.push({ x: atMs, y: Number(rate) || 0 });
  const floor = atMs - (CHART_WINDOW_SECS + STATS_POLL_MS / 1000) * 1000;
  while (points.length && points[0].x < floor) points.shift();
}

// Node state for the header and Przegląd, and which replicas trail their
// leader. The whole-instance answer names the nodes; the per-topic answers
// give the partitions of the reader's topics (the wire partition carries no
// topic name, and the node summary also counts the broker's own `__*`
// topics), from which Przegląd counts leaders, copies and lagging replicas.
// The last few lag samples of every consumer the overview flags as falling
// behind — whether its backlog still grows ("rośnie od") or only waits
// ("czeka od") is read from what the node measured, not guessed.
async function refreshLagSeries(instanceId) {
  const now = Date.now();
  const flagged = (state.stats?.groups || []).filter((g) => isLagging(g, now));
  const next = new Map();
  await Promise.all(flagged.map((g) => ApiBinary.one('busLagHistoryRequest', {
    instanceId: requireInstanceId(instanceId), group: g.group, topic: g.topic, sinceMs: now - 10 * 60_000,
  }).then((r) => {
    const series = (r?.groups || []).find((x) => x.group === g.group && x.topic === g.topic);
    if (series) next.set(lagSeriesKey(g.group, g.topic), series.samples || []);
  }, () => {})));
  if (state.instanceId === instanceId) state.shell.lagSeries = next;
}

async function refreshReplicas() {
  const instanceId = state.instanceId;
  if (!instanceId || !state.topicsLoaded) return;
  let all;
  try {
    all = await ApiBinary.one('busReplicaListRequest', buildReplicaListRequest(instanceId, ''));
  } catch {
    return;
  }
  const names = state.topics.map((t) => t.name);
  const perTopic = await Promise.all(names.map((topic) => ApiBinary.one('busReplicaListRequest', buildReplicaListRequest(instanceId, topic))
    .then((r) => ({ topic, partitions: r?.partitions || [] }), () => ({ topic, partitions: [] }))));
  if (state.instanceId !== instanceId) return;
  const nodes = all?.nodes || [];
  state.shell.nodes = nodes;
  state.shell.replicaTopics = perTopic;
  state.shell.replicaLags = laggingReplicas(perTopic, nodes);
  await refreshLagSeries(instanceId);
  if (state.instanceId !== instanceId) return;
  paintShell();
  if (state.tab === 'overview' && !state.view) renderPanel();
}

// Appends one sample to a rolling `{msgsIn, bytesIn, lag}` window, trimmed to
// `MAX_CHART_POINTS` via `pushWindowSample` — the "live last N minutes"
// replacement for the mockup's unavailable 24h history (module-doc gap #1).
// `x` is a plain HH:MM:SS label (category axis), not an epoch, since
// `tf-line-chart`'s category scale expects display-ready ticks.
function pushChartSample(series, msgsIn, bytesIn, lag) {
  const x = new Date().toLocaleTimeString(undefined, { hour12: false });
  const push = (arr, y) => pushWindowSample(arr, { x, y: Number(y) || 0 }, MAX_CHART_POINTS);
  push(series.msgsIn, msgsIn);
  push(series.bytesIn, bytesIn);
  push(series.lag, lag);
}

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// The "live window" line chart of
// M03's per-topic overview (fed by `pushChartSample` above) — msgs/s in
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
// Topiki (T02): the list lives in modules/tentabus/topics.js, the creator,
// the delete window and the message preview in their own modules; the
// shell loads the data and says where each move leads.
// =============================================================================

async function loadTopics() {
  const instanceId = state.instanceId;
  try {
    // The broker's own `__*` topics (unprocessed-message stores, metrics) are
    // never shown as topics: every list, count and picker reads this one.
    const topics = userTopics(await ApiBinary.list('busTopicListRequest', { arrayKey: 'topics', payload: { instanceId: requireInstanceId(instanceId) } }));
    if (state.instanceId !== instanceId) return;
    state.topics = topics;
    state.topicsError = null;
  } catch (err) {
    if (state.instanceId !== instanceId) return;
    // The last list stays for the other tabs; Topiki shows the failure itself.
    state.topicsError = err;
  }
  state.topicsLoaded = true;
  if (state.tab === 'topics' && !state.view) renderPanel();
  paintShell();
  refreshReplicas();
  if (state.tab === 'overview' && !state.view) renderPanel();
  // Covers the race where a user switches to the DLQ tab BEFORE this initial
  // `loadTopics()` (kicked off in parallel from `mount()`) resolves: `setTab`'s
  // own `ensureDlqTabReady()` call ran too early to see any topics yet, so the
  // tab would otherwise sit on its loading placeholder forever once topics
  // finally arrive. Calling the SAME single-source-of-truth helper here (not
  // a second, divergent code path) keeps `state.dlqSource` deterministic
  // regardless of which of the two triggers fires first (R3-1).
  if (state.tab === 'dlq') ensureDlqTabReady();
}

const topicsContext = {
  view() {
    return {
      topics: state.topicsLoaded && !state.topicsError ? state.topics : null,
      error: state.topicsError,
      errorKind: state.topicsError ? loadErrorKind(state.topicsError) : null,
      stats: state.stats,
      instanceLabel: state.instanceLabel,
      canAdmin: canAdmin(),
      notice: state.topicsNotice,
      nowMs: Date.now(),
    };
  },
  go(action) {
    if (action.kind === 'open') openTopicDetail(action.topic);
    else if (action.kind === 'preview') openTopicPreview(action.topic);
    else if (action.kind === 'delete') openTopicDeleteWindow(action.topic);
    else if (action.kind === 'create') openCreator();
    else if (action.kind === 'retry') { state.topicsError = null; state.topicsLoaded = false; renderPanel(); loadTopics(); }
  },
};

const describeBusError = (err) => mapBusErrorMessage(err?.message, T);

function topicByName(name) {
  return state.topics.find((t) => t.name === name) || null;
}

function openCreator() {
  if (!canAdmin()) return;
  const instanceId = state.instanceId;
  openTopicCreator({
    instanceId: requireInstanceId(instanceId),
    instanceLabel: state.instanceLabel,
    capabilities: state.capabilities,
    subjects: state.shell.subjectsError ? null : state.shell.subjects,
    reloadSubjects: async () => {
      await loadSubjects();
      if (state.shell.subjectsError) throw state.shell.subjectsError;
      return state.shell.subjects || [];
    },
    existingNames: state.topics.map((t) => t.name),
    create: (request) => ApiBinary.action('busTopicCreateRequest', request),
    describeError: describeBusError,
    onCreated: async ({ name, schemaId, plan }) => {
      if (state.instanceId !== instanceId) return;
      const text = schemaId
        ? T('topics.created_text_schema', { name: schemaId })
        : T(`topics.created_text_${plan.kind === 'single' ? 'single' : 'plain'}`);
      state.topicsNotice = { tone: 'success', title: T('topics.created_title', { name }), text };
      await loadTopics();
      refreshStats();
    },
  });
}

function openTopicPreview(name) {
  const instanceId = state.instanceId;
  const topic = topicByName(name);
  openMessagePreview({
    instanceId: requireInstanceId(instanceId),
    topic: name,
    partitionCount: topic?.partitions ?? state.detail?.topic?.partitions ?? 1,
    browse: (request) => ApiBinary.one('busMessagesBrowseRequest', request),
    loadPartitions: async () => (await ApiBinary.one('busTopicDetailRequest', { instanceId, name }))?.partitions || [],
    describeError: describeBusError,
  });
}

function openTopicDeleteWindow(name) {
  if (!canAdmin()) return;
  const instanceId = state.instanceId;
  const topic = topicByName(name) || state.detail?.topic || { name, partitions: 0 };
  const consumers = (state.stats?.groups || []).filter((g) => g.topic === name && !isInternalGroupId(g.group)).map((g) => g.group);
  openTopicDelete({
    topic,
    stats: findTopicStats(state.stats?.topics, name),
    consumers,
    loadCounts: async () => {
      const [acl, policies] = await Promise.allSettled([
        ApiBinary.one('busAclListRequest', { instanceId, topic: name }),
        ApiBinary.one('busFieldPolicyListRequest', { instanceId, topic: name }),
      ]);
      return {
        aclCount: acl.status === 'fulfilled' ? (acl.value?.entries || []).length : null,
        policyCount: policies.status === 'fulfilled' ? (policies.value?.policies || []).length : null,
      };
    },
    remove: () => ApiBinary.action('busTopicDeleteRequest', { instanceId: requireInstanceId(instanceId), name }),
    describeError: describeBusError,
    onDeleted: async () => {
      if (state.instanceId !== instanceId) return;
      state.topicsNotice = { tone: 'success', title: T('topics.deleted_title', { name }), text: T('topics.deleted_text') };
      if (state.view?.name === name) state.view = null;
      state.tab = 'topics';
      renderPanel();
      await loadTopics();
      refreshStats();
    },
  });
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
  return { status, variant: 'outline', label: T(`env_${env}`) || env };
}

// Owner decision B: the M03 config tab renders
// a chip for a topic's durability class — critical highlighted (err
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
  const chip = `<span class="tf-chip tf-chip--outline ${status}"${title ? ` title="${title}"` : ''}>${escapeHtml(label)}</span>`;
  if (!shouldShowDurabilityExplicitLabel(topic)) return chip;
  return `${chip} <span class="tb-field-hint tb-durability-explicit">${escapeHtml(T('durability_class_explicit_suffix'))}</span>`;
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

function closeModal(modal) {
  modal.removeAttribute('open');
  setTimeout(() => modal.remove(), 300);
}

// =============================================================================
// Modal focus trap (`<tf-modal>`, tentaflow-core/www/js/components/tf-modal.js,
// has none — Tab cycles out into the page behind the dialog and focus never
// moves into the dialog on open, WCAG 2.1.1/2.4.3). `tf-modal.js` is a
// shared component outside this file's change scope, so every dialog THIS
// module builds traps focus itself instead: `openOffsetResetModal`,
// `openLeaderTransferModal` and `openReassignModal` call `trapModalFocus`
// directly.
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
  // (`_dismiss()`); every close button THIS module wires (offset-reset,
  // leader-transfer and reassign Cancel/confirm)
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
    byId('tb-detail-preview')?.addEventListener('click', () => openTopicPreview(topic.name));
    byId('tb-detail-delete')?.addEventListener('click', () => openTopicDeleteWindow(topic.name));
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
  return `<tf-chip variant="outline" status="${escapeAttr(chip.status)}">${escapeHtml(chip.label)}</tf-chip>`;
}

function heroHtml(topic, groups) {
  return `
    <div class="tb-hero">
      <div class="tb-hero-ident">
        <div class="tb-hero-name">${escapeHtml(topic.name)}${topic.name.startsWith('__dlq.') ? ` <tf-chip variant="outline" status="warn">${escapeHtml(T('badge_dlq'))}</tf-chip>` : ''}</div>
        <div class="tb-hero-meta">
          ${chipHtml(envChip(topic.environment))}
          <tf-chip variant="outline" status="info">${escapeHtml(topic.delivery)}</tf-chip>
          <tf-chip variant="outline" status="info">${escapeHtml(topic.acks)}</tf-chip>
        </div>
      </div>
      <div class="tb-mini-kpis">
        <div class="tb-mk"><b>${topic.partitions}</b><span>${escapeHtml(T('detail_mk_partitions'))}</span></div>
        <div class="tb-mk"><b>${sumGroupLag(groups)}</b><span>${escapeHtml(T('detail_mk_groups_lag'))}</span></div>
      </div>
      <div class="tb-head-actions">
        <tf-button variant="ghost" icon="eye" id="tb-detail-preview">${escapeHtml(T('detail_preview_messages'))}</tf-button>
        ${canAdmin() ? `<tf-button variant="danger" icon="trash" id="tb-detail-delete">${escapeHtml(T('detail_delete'))}</tf-button>` : ''}
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
        <td class="mono">${p.leaderEpoch != null ? escapeHtml(T('replication.epoch_value', { n: p.leaderEpoch })) : '—'}</td>
        <td>${isrCount != null && replicaCount != null ? `${isrCount}/${replicaCount}` : '—'}${degraded ? ` <span class="tf-chip tf-chip--outline warn">${escapeHtml(T('detail_isr_degraded'))}</span>` : ''}</td>
        <td class="mono">${p.earliestOffset}</td>
        <td class="mono">${p.logEndOffset}</td>
        <td class="mono">${p.highWatermark != null ? p.highWatermark : '—'}</td>
        <td class="mono">${p.highWatermark != null ? lag : '—'}</td>
        <td>${formatBytes(p.sizeBytes)}</td>
        <td>${p.segments}</td>
        <td>${reasonKey ? `<span class="tf-chip tf-chip--outline err" title="${escapeAttr(T(reasonKey))}">${escapeHtml(T('detail_partition_unavailable'))}</span>` : ''}</td>
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
    ['retention_ms', fmtRetention(topic.retentionMs)],
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
  const admin = canAdmin();
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

// Per-partition summary chips above the unprocessed-message table: which
// message numbers each partition still keeps.
function partitionSummaryHtml(partitions) {
  if (!partitions?.length) return '';
  const chips = partitions.map((p) => {
    const first = Number(p.earliestOffset) || 0;
    const next = Number(p.highWatermark) || 0;
    const label = next > first
      ? T('partitions_summary_chip', { partition: p.partition, from: fmtCount(first), to: fmtCount(next - 1) })
      : T('partitions_summary_chip_empty', { partition: p.partition });
    return `<span class="tf-chip tf-chip--outline info" title="${escapeAttr(T('partitions_summary_chip_title', { partition: p.partition, next: fmtCount(next) }))}">${escapeHtml(label)}</span>`;
  }).join('');
  return `<div class="tb-partition-summary">${chips}</div>`;
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
          <tf-column key="waiting" label="${escapeAttr(T('groups_col_waiting'))}" renderer="num"></tf-column>
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
  table.rowActions = (row, idx, currentRow) => {
    if (!canAdmin()) return null;
    // The actions cell can be kept across re-renders, so pause/resume must act
    // on the row sitting in this slot at click time.
    const live = () => currentRow?.() ?? row;
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', 'ghost');
    btn.setAttribute('size', 'sm');
    btn.setAttribute('icon', row.paused ? 'play' : 'pause');
    btn.title = T(row.paused ? 'groups_action_resume' : 'groups_action_pause');
    btn.addEventListener('click', (e) => { e.stopPropagation(); toggleGroupPause(live()); });
    return btn;
  };
  table.addEventListener('row-click', (e) => openGroupDetail(e.detail.row.group, e.detail.row.topic));
}

function paintGroupsTable() {
  const table = byId('tb-groups-table');
  if (!table) return;
  const visibleGroups = filterVisibleGroups(state.groups);
  const liveLag = new Map((state.stats?.groups || []).map((g) => [`${g.group}\u0000${g.topic}`, g.lagTotal]));
  const fmtWaiting = (v) => (v == null ? '—' : fmtCount(v));
  const rows = visibleGroups.map((g) => ({
    group: g.group,
    topic: g.topic,
    // Chosen by the consumer's program when it connects: shown, never edited.
    commitMode: COMMIT_MODES.includes(g.commitMode) ? T(`groups_commit_${g.commitMode}`) : String(g.commitMode || '—'),
    // The live snapshot's figure (what Przegląd counts), else the list's own.
    // Unknown (`null`) is not zero: a lag this node cannot measure prints "—".
    waiting: fmtWaiting(liveLag.has(`${g.group}\u0000${g.topic}`) ? liveLag.get(`${g.group}\u0000${g.topic}`) : g.lagTotal),
    state: { status: g.paused ? 'warn' : 'ok', variant: 'outline', label: T(g.paused ? 'groups_state_paused' : 'groups_state_active') },
    paused: g.paused,
  }));
  // A no-op-poll gate (this table is not on the
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
  paintFrame();
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
        <td>${canAdmin() ? `<tf-button variant="ghost" size="sm" icon="rotate" class="tb-reset-offset" data-partition="${p.partition}">${escapeHtml(T('group_detail_reset_offset'))}</tf-button>` : ''}</td>
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
        ${canAdmin() ? '' : `<div class="tb-gap-note">${sprite('info')}${escapeHtml(T('group_detail_admin_required'))}</div>`}
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
  if (canAdmin()) {
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
    <div class="tb-toolbar" id="tb-dlq-toolbar" hidden>
      <tf-select id="tb-dlq-source" label="${escapeAttr(T('dlq_source_label'))}"></tf-select>
      <span class="tb-spacer"></span>
      ${canAdmin() ? `<tf-button variant="danger" icon="rotate" id="tb-dlq-retry-all" hidden>${escapeHtml(T('dlq_retry_all'))}</tf-button>` : ''}
    </div>
    <div class="tb-card">
      <div class="tb-c-body" id="tb-dlq-body"></div>
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
  // No topic, nothing to pick: the toolbar (and its card) goes away.
  const toolbar = byId('tb-dlq-toolbar');
  if (toolbar) toolbar.hidden = !(state.topicsLoaded && options.length > 0);
}

// "Ponów wszystkie" only when it has something to retry in the chosen topic.
function paintDlqRetryAll() {
  const btn = byId('tb-dlq-retry-all');
  if (btn) btn.hidden = !(state.dlqSource && (state.dlqRecords || []).length > 0);
}

// R3-1: the only function that ACTS on `resolveDlqEntrySource`'s answer —
// called from exactly two entry points (`setTab` on tab switch,
// `loadTopics` for the "topics arrived after the tab was already open" race)
// so there is no third path that can silently disagree with it. Idempotent:
// re-entering the tab with an already-loaded source is a no-op.
function ensureDlqTabReady() {
  if (state.tab !== 'dlq' || state.view) return;
  // The default choice needs both the topic list and the snapshot's counts;
  // `loadTopics` and the first stats poll each call back in here.
  if (!state.topicsLoaded || !state.stats) return;
  paintDlqSourceOptions();
  const next = resolveDlqEntrySource(state.dlqSource, state.topics, state.stats.topics);
  if (next !== state.dlqSource) {
    selectDlqSource(next);
    return;
  }
  // With no source (no topics) the load settles on "nothing to show" at once.
  if (state.dlqRecords == null && !state.dlqLoading) loadDlqRecords(true);
}

function selectDlqSource(topicName) {
  state.dlqSource = topicName;
  state.dlqRecords = null;
  state.dlqPartitions = [];
  paintDlqSourceOptions();
  syncLocation();
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
  paintDlqRetryAll();
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
    const empty = state.dlqSource ? T('dlq_empty_for_topic') : T('dlq_all_processed_sub');
    host.innerHTML = `${state.dlqSource ? partitionSummaryHtml(state.dlqPartitions) : ''}
      <tf-empty-state badge icon="inbox" title="${escapeAttr(T('dlq_all_processed_title'))}" message="${escapeAttr(empty)}"></tf-empty-state>`;
    return;
  }
  const admin = canAdmin();
  // One tf-table: on a phone it turns into cards, so every row's actions stay
  // reachable; the error text and the attempts give way first.
  const rows = state.dlqRecords.map((r, idx) => {
    const reason = headerText(r.headers, 'dlq.reason') || 'unknown';
    const errorMsg = headerText(r.headers, 'dlq.error_message') || '';
    return {
      when: msToDate(r.timestampMs),
      source: T('dlq_source_cell', { partition: r.partition, number: fmtCount(r.offset) }),
      reason: { status: DLQ_REASON_TONE[reason] || 'info', variant: 'outline', label: T(`dlq_reason_${reason}`) },
      attempts: headerText(r.headers, 'dlq.attempts') || '—',
      error: errorMsg,
      _idx: idx,
      _key: `${r.partition}:${r.offset}`,
    };
  });
  host.innerHTML = `
    ${admin ? '' : `<div class="tb-gap-note">${sprite('info')}${escapeHtml(T('dlq_admin_required'))}</div>`}
    ${partitionSummaryHtml(state.dlqPartitions)}
    <tf-table id="tb-dlq-table">
      <tf-column key="when" label="${escapeAttr(T('dlq_col_timestamp'))}" nowrap></tf-column>
      <tf-column key="source" label="${escapeAttr(T('dlq_col_source'))}" nowrap></tf-column>
      <tf-column key="reason" label="${escapeAttr(T('dlq_col_reason'))}" renderer="chip"></tf-column>
      <tf-column key="attempts" label="${escapeAttr(T('dlq_col_attempts'))}" hide-below="900"></tf-column>
      <tf-column key="error" label="${escapeAttr(T('dlq_col_error'))}" hide-below="1200" fill></tf-column>
    </tf-table>
    <div id="tb-dlq-detail"></div>
    ${state.dlqHasMore ? `<tf-button variant="secondary" id="tb-dlq-more" style="margin-top:10px">${escapeHtml(T('preview_load_more'))}</tf-button>` : ''}
  `;
  const table = host.querySelector('#tb-dlq-table');
  table.rowActionsKey = (row) => `${row._key}:${row._idx}:${admin}`;
  table.rowActions = (row) => {
    const wrap = document.createElement('div');
    wrap.className = 'tf-table__row-actions';
    const button = (icon, label, act) => {
      const btn = document.createElement('tf-button');
      btn.setAttribute('variant', 'ghost');
      btn.setAttribute('size', 'sm');
      btn.setAttribute('icon', icon);
      btn.dataset.act = act;
      btn.textContent = label;
      btn.addEventListener('click', (e) => {
        e.stopPropagation();
        const record = state.dlqRecords[row._idx];
        if (act === 'view') toggleDlqDetail(host, row._idx);
        else if (act === 'retry') dlqRetry(record);
        else confirmDlqDiscard(record);
      });
      wrap.appendChild(btn);
    };
    button('eye', T('preview_action_view'), 'view');
    if (admin) {
      button('rotate', T('dlq_action_retry'), 'retry');
      button('close', T('dlq_action_discard'), 'discard');
    }
    return wrap;
  };
  table.rows = rows;
  host.querySelector('#tb-dlq-more')?.addEventListener('click', () => loadDlqRecords(false));
}

// "Szczegóły" of one message: its envelope headers and the start of its
// payload, under the table (a second click on the same row closes it).
function toggleDlqDetail(host, idx) {
  const panel = host.querySelector('#tb-dlq-detail');
  if (!panel) return;
  if (panel.dataset.idx === String(idx)) {
    panel.dataset.idx = '';
    panel.innerHTML = '';
    return;
  }
  const record = state.dlqRecords[idx];
  const allHeaders = (record.headers || [])
    .map((h) => [h.key, formatHeaderValue(h.key, bytesToPreviewText(h.value, 512))]);
  panel.dataset.idx = String(idx);
  panel.innerHTML = `
    <div class="tb-dlq-detail">
      <div>
        <strong>${escapeHtml(T('dlq_source_cell', { partition: record.partition, number: fmtCount(record.offset) }))}</strong>
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
  const ok = await confirmDialog({
    title: T('dlq_discard_confirm_title'),
    lead: T('dlq_discard_confirm_body'),
    confirmLabel: T('dlq_action_discard'),
    cancelLabel: T('common_cancel'),
    variant: 'danger',
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
  const ok = await confirmDialog({
    title: T('dlq_retry_all_confirm_title'),
    lead: T('dlq_retry_all_confirm_body', { topic: state.dlqSource, max: DLQ_RETRY_ALL_MAX }),
    confirmLabel: T('dlq_retry_all'),
    cancelLabel: T('common_cancel'),
    variant: 'primary',
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
        <div class="tb-c-head"><h3>${escapeHtml(T('replication.lag_state_title'))}</h3><div class="tb-hint">${escapeHtml(T('replication.lag_state_hint'))}</div></div>
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

// Counts per node over the reader's topics — the same `nodeRows` Przegląd
// uses, so both screens say the same. "Wszystkie topiki" reads the shell's
// per-topic snapshots; a chosen topic reads its own answer. The node summary
// on the wire also counts the broker's `__*` topics, so it only names nodes.
function nodeCardRows(data) {
  const nodes = Array.isArray(data?.nodes) ? data.nodes : [];
  const partitions = Array.isArray(data?.partitions) ? data.partitions : [];
  const perTopic = state.repl.topic
    ? [{ topic: state.repl.topic, partitions }]
    : state.shell.replicaTopics;
  const counts = new Map((nodeRows(nodes, perTopic) || []).map((r) => [r.nodeId, r]));
  return nodes.map((n) => {
    const c = counts.get(n.nodeId);
    return {
      _key: n.nodeId,
      nodeId: n.nodeId,
      label: n.label || n.nodeId,
      environment: n.environment,
      isLocal: !!n.isLocal,
      reachable: n.reachable !== false,
      lastHeartbeatMsAgo: n.lastHeartbeatMsAgo,
      leaderCount: c ? c.leads : null,
      followerCount: c ? c.holds : null,
      isrCount: c ? c.inSync : null,
      degraded: nodeDegradedReason(n, partitions),
    };
  });
}

// The node this screen runs on has no "last signal" worth printing — it is
// the one answering.
function nodeCardSubText(r) {
  if (!r.reachable) return T('replication.node_unreachable');
  const heartbeat = r.isLocal ? '' : T('replication.node_heartbeat', { ms: fmtCompact(Number(r.lastHeartbeatMsAgo) || 0) });
  if (!r.degraded || r.degraded.kind !== 'lagging') return heartbeat;
  const lag = r.degraded.lag || {};
  return [heartbeat, T('replication.node_lagging_note', {
    partition: r.degraded.partition,
    reason: lag.reason || T('replication.node_lagging_reason_unknown'),
  })].filter(Boolean).join(' · ');
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
        ${r.isLocal ? `<span class="tf-chip tf-chip--outline info">${escapeHtml(T('replication.node_local_badge'))}</span>` : ''}
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
  const actions = canAdmin() ? `
    <td class="tb-row-actions">
      <tf-button variant="ghost" size="sm" class="tb-repl-transfer-leader" data-partition="${row.partition}">${escapeHtml(T('replication.action_transfer_leader'))}</tf-button>
      <tf-button variant="ghost" size="sm" class="tb-repl-reassign" data-partition="${row.partition}">${escapeHtml(T('replication.action_reassign'))}</tf-button>
    </td>` : '';
  return `
    <tr class="${row.unavailableReason ? 'tb-row-unavailable' : ''}" id="tb-repl-row-${key}">
      <td>
        ${escapeHtml(T('partition_label', { n: row.partition }))}
        ${reasonKey ? `<div class="tf-chip tf-chip--outline warn tb-role-unavailable-chip">${escapeHtml(T(reasonKey))}</div>` : ''}
      </td>
      ${cells}
      <td class="mono" id="tb-repl-epoch-${key}">${escapeHtml(T('replication.epoch_value', { n: row.leaderEpoch }))}</td>
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
        ${canAdmin() ? `<th>${escapeHtml(T('replication.matrix_col_actions'))}</th>` : ''}
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
  patchText(tr.querySelector(`#tb-repl-epoch-${key}`), T('replication.epoch_value', { n: row.leaderEpoch }));
}

function wireRoleMatrixActions(body, topic) {
  body.querySelectorAll('.tb-repl-transfer-leader').forEach((btn) => {
    btn.addEventListener('click', () => openLeaderTransferModal(topic, Number(btn.dataset.partition)));
  });
  body.querySelectorAll('.tb-repl-reassign').forEach((btn) => {
    btn.addEventListener('click', () => openReassignModal(topic, Number(btn.dataset.partition)));
  });
}

// Without a chosen topic a card asks for one — unless there is none to choose.
function replNoTopicKey(pickKey) {
  return state.topicsLoaded && state.topics.length === 0 ? 'replication.no_topics' : pickKey;
}

function paintReplMatrix() {
  const titleEl = byId('tb-repl-matrix-title');
  const body = byId('tb-repl-matrix-body');
  if (!body) return;
  const topic = state.repl.topic;
  if (!topic) {
    patchText(titleEl, T('replication.matrix_title_generic'));
    body.innerHTML = `<div class="tb-state tb-empty">${escapeHtml(T(replNoTopicKey('replication.matrix_select_topic')))}</div>`;
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
    host.innerHTML = `<div class="tb-state tb-empty">${escapeHtml(T(replNoTopicKey('replication.lag_state_select_topic')))}</div>`;
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
      <div class="tb-lag-item-head">${escapeHtml(T('replication.partition_on_node', { partition: it.partition, node: nodeLabelById(nodes, it.nodeId) }))}</div>
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
      <td class="mono">${escapeHtml(e.topic)} · ${escapeHtml(T('partition_label', { n: e.partition }))}</td>
      <td><span class="tf-chip tf-chip--outline">${escapeHtml(T('replication.epoch_change', { from: e.fromEpoch, to: e.toEpoch }))}</span></td>
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
  renderPanel();
}

export default TentaBusScreen;
