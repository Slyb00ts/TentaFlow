// =============================================================================
// File: modules/tentabus.js — the TentaBus screen shell (SUM/tentabus/
// PLAN-UI-20260923 U0–U2, mockups SUM/mockups/tentabus-20260923): breadcrumb,
// the TentaBus header card with the instance picker, the six underlined main
// tabs (Przegląd / Topiki / Odbiorcy / Nieprzetworzone / Wzory wiadomości /
// Kopie i nody) with their counters, the address (`#/tentabus?instance=…&tab=
// …&topic=…&section=…&group=…`, see modules/tentabus/routes.js) and the
// polling every tab reads. Przegląd, Topiki with a topic's page (Stan,
// Ustawienia, Partycje i kopie), Wzory wiadomości and Kopie i nody live in
// modules/tentabus/*. Odbiorcy and Nieprzetworzone below are the M1/M2 views
// (consumer groups with offset reset, unprocessed messages per topic) until
// their U3–U4 packages replace them.
//
// Every request names its instance (`BusEnvelope.instance_id`): `mount`
// resolves `state.instanceId` from `?instance=` (or the same-screen instance
// gate, never a guess) and every request goes through `requireInstanceId`.
// Controls that change something are gated on the server's own answer —
// `BusCapabilitiesRequest` for the instance, `TopicDetailResponse.access` on
// a topic's page — and fail closed until it arrives.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';
import { setAttr, setText, patchHtml, setClass } from '/js/lib/dom-patch.js';
import { fmtCount, fmtElapsed, loadErrorKind } from '/js/modules/tentabus/format.js';
import { MAIN_TABS, DEFAULT_TAB, DEFAULT_SECTION, TOPIC_SECTIONS, parseRoute, routeParams } from '/js/modules/tentabus/routes.js';
import { shellCounts, userTopics, userRate } from '/js/modules/tentabus/model.js';
import { laggingReplicas, isLagging, lagSeriesKey } from '/js/modules/tentabus/alerts.js';
import { drawOverview, pushOverviewSample, CHART_WINDOW_SECS } from '/js/modules/tentabus/overview.js';
import { drawSchemas } from '/js/modules/tentabus/schemas.js';
import { drawTopics } from '/js/modules/tentabus/topics.js';
import { openTopicCreator } from '/js/modules/tentabus/topic-creator.js';
import { openTopicDelete } from '/js/modules/tentabus/topic-delete.js';
import { openMessagePreview } from '/js/modules/tentabus/message-preview.js';
import { drawTopicDetail, effectiveSection, topicDetailLoader } from '/js/modules/tentabus/topic-detail.js';
import { openSettingsWindow } from '/js/modules/tentabus/topic-settings.js';
import { openLeaderTransfer, transferChoices } from '/js/modules/tentabus/partitions.js';
import { drawReplication } from '/js/modules/tentabus/replication.js';
import { bytesToPreviewText, headerText } from '/js/modules/tentabus/payload.js';
import { confirmDialog } from '/js/lib/confirm-dialog.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-button.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
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
// Incremental repaint: a poll only swaps values, it never re-renders a table
// whose rows did not change. Unit-tested from `tentabus.request-builders.test.js`.
// =============================================================================

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

// =============================================================================
// Replication requests (`ReplicaListResponse{nodes,partitions,failovers}`,
// `LeaderTransferRequest`).
// =============================================================================

// `topic` omitted (`undefined`, not `''`) asks for the whole instance — the
// node roster and the leadership changes; a topic name adds its partitions.
function buildReplicaListRequest(instanceId, topic) {
  return { instanceId: requireInstanceId(instanceId), topic: topic || undefined };
}

// "Przenieś prowadzenie" request builder.
function buildLeaderTransferRequest(instanceId, topic, partition, targetNodeId) {
  return { instanceId: requireInstanceId(instanceId), topic, partition: Number(partition), targetNodeId };
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
  view: null, // null | { kind: 'topic-detail', name, section }

  topics: [],
  topicsLoaded: false,
  // The last failed topic-list load (Topiki shows it), `null` after a success.
  topicsError: null,
  // "Utworzono topik …" / "Usunięto topik …" above the list, until the tab is left.
  topicsNotice: null,

  stats: null,
  statsTimer: null,

  // `TopicDetailResponse` of the open topic (config, partitions, access,
  // administrators), `null` until it answers; the failed load, if any.
  detail: null,
  detailError: null,
  // "Zapisano …" / "Przeniesiono prowadzenie" over the section it concerns
  // (`{ section, title, text }`), until the reader moves to another section.
  detailNotice: null,
  // Partitions whose leadership was moved from this page: marked "zmieniono
  // przed chwilą" and not offered again until the reader refreshes. Numbers
  // on a topic's page, `topic:partition` on Kopie i nody.
  justMoved: new Set(),
  replicationNotice: null,

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

  // Last-painted rows of the consumer table, diffed against the next poll's
  // (`diffRowsByKey`) so an unchanged poll leaves the table alone.
  dom: {
    groupsTableRows: null,
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
    failovers: [],
    replicaLags: [],
    lagSeries: new Map(),
    replicaPoll: null,
  };
}

// `canAdmin` gates every instance-level change: creating and deleting topics,
// pause/resume, retry/discard of unprocessed messages, offset reset and moving
// a partition's leadership from Kopie i nody. It is `bus.admin` in the
// instance matrix AND the org Admin role — what every such handler's
// `gate_admin` checks — and fails closed before `busCapabilitiesRequest`
// answers. A topic's page gates on the topic's own `access.canAdmin`, which
// also honours the topic's ACL.
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
    if (route.topic) openTopicDetail(route.topic, route.section);
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
    state.topics = [];
    state.topicsLoaded = false;
    state.topicsError = null;
    state.topicsNotice = null;
    state.stats = null;
    state.detail = null;
    state.detailError = null;
    state.detailNotice = null;
    state.justMoved = new Set();
    state.replicationNotice = null;
    state.groups = [];
    state.groupsLoaded = false;
    state.groupDetail = null;
    state.dlqSource = '';
    state.dlqRecords = null;
    state.dlqPartitions = [];
    state.dlqLoading = false;
    state.dlqError = null;
    state.shell = freshShellState();
    state.dom = { groupsTableRows: null };
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

// The address names the view (instance, tab, open topic and its section, or
// consumer) through
// the router's own `replaceParams`: no history entry per click, and the
// router's notion of the current params stays true for a language repaint.
function syncLocation() {
  if (!state.instanceId) return;
  // An address the reader has just typed or pasted, which the router has not
  // handled yet, differs from the one it last wrote: a repaint must not write
  // over it, or the router never sees the move.
  const key = (params) => new URLSearchParams(Object.entries(params || {}).sort()).toString();
  const inBar = Router.fromHash();
  if (inBar && key(inBar.params) !== key(Router.currentParams())) return;
  const gd = state.tab === 'groups' ? state.groupDetail : null;
  Router.replaceParams(routeParams({
    instance: state.instanceId,
    tab: state.tab,
    topic: state.view?.kind === 'topic-detail' ? state.view.name : null,
    section: state.view?.kind === 'topic-detail' ? currentSection() : null,
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
// A moved leadership can be moved again from here on: the next replica
// answer shows where it went.
async function refreshAll() {
  state.justMoved = new Set();
  refreshStats();
  refreshReplicas();
  loadSubjects();
  await Promise.all([loadTopics(), loadGroups()]);
  if (state.view?.kind === 'topic-detail') loadTopicDetail(state.view.name);
  else if (state.tab === 'dlq') loadDlqRecords(true);
  else if (state.tab === 'groups' && state.groupDetail) openGroupDetail(state.groupDetail.group, state.groupDetail.topic);
  renderPanel();
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
    else if (action.kind === 'topic') { setTab('topics'); openTopicDetail(action.topic, DEFAULT_SECTION); }
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
  if (state.view) closeTopicDetail();
  if (id !== 'groups') state.groupDetail = null;
  if (id !== 'topics') state.topicsNotice = null;
  if (id !== 'replication') { state.replicationNotice = null; state.justMoved = new Set(); }
  state.tab = id;
  renderPanel();
  if (id === 'groups' && !state.groupsLoaded) loadGroups();
  if (id === 'dlq') ensureDlqTabReady();
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
  if (activeKey === 'detail') { drawTopicDetail(activeEl, detailContext); return; }
  if (activeKey === 'topics') { drawTopics(activeEl, topicsContext); return; }
  if (activeKey === 'groups') { renderGroupsTab(activeEl); return; }
  if (activeKey === 'dlq') { renderDlqTab(activeEl); return; }
  if (activeKey === 'replication') { drawReplication(activeEl, replicationContext); return; }
}

// Rebuilds `panel`'s skeleton only when switching CONTEXT within a view
// (preserves focus/scroll/typed-but-not-yet-debounced input across data
// refreshes that call back into the same view's paint function).
function ensureSkeleton(panel, viewId, buildFn) {
  if (panel.dataset.tbView === viewId) return false;
  panel.innerHTML = buildFn();
  panel.dataset.tbView = viewId;
  return true;
}

// =============================================================================
// Polling — BusStatsSnapshotRequest every 3 s (the header, the tab counters,
// Przegląd and the legacy tab strips all read it) and ReplicaListRequest 10 s
// after the previous cycle answered (node state and lagging replicas). Plain polls, not push
// subscriptions; started once in mount(), stopped in unmount(). A failed poll
// keeps the last data on screen and turns the header to "Brak połączenia"
// with the age of that data (T12) — it never blanks the numbers.
// =============================================================================

function startStatsPolling() {
  stopStatsPolling();
  refreshStats();
  refreshReplicas();
  state.statsTimer = setInterval(refreshStats, STATS_POLL_MS);
  // Chained, not an interval: one cycle asks once per topic, and a slow
  // node must not have the next cycle start before the last one answered.
  const poll = { timer: null };
  state.shell.replicaPoll = poll;
  const next = () => {
    poll.timer = setTimeout(async () => {
      try {
        await refreshReplicas();
      } finally {
        if (state.shell.replicaPoll === poll) next();
      }
    }, REPLICA_POLL_MS);
  };
  next();
}

function stopStatsPolling() {
  if (state.statsTimer) clearInterval(state.statsTimer);
  state.statsTimer = null;
  if (state.shell.replicaPoll) clearTimeout(state.shell.replicaPoll.timer);
  state.shell.replicaPoll = null;
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
  if (state.view?.kind === 'topic-detail') renderPanel();
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
  state.shell.failovers = all?.failovers || [];
  state.shell.replicaLags = laggingReplicas(perTopic, nodes);
  await refreshLagSeries(instanceId);
  if (state.instanceId !== instanceId) return;
  paintShell();
  if (state.view?.kind === 'topic-detail') {
    // The page's partition numbers and sizes move with the log; they come
    // with the topic's own answer, asked again on the replica cadence.
    loadTopicDetail(state.view.name);
    return;
  }
  if (!state.view && (state.tab === 'overview' || state.tab === 'replication')) renderPanel();
}

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
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
      if (state.view?.name === name) closeTopicDetail();
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
// control in the consumer table but never open a group's detail panel. This is a progressive-enhancement layer added from
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
// module builds traps focus itself instead: `openOffsetResetModal` calls
// `trapModalFocus` directly.
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
  // (`_dismiss()`); every close button THIS module wires (offset-reset
  // Cancel/confirm) calls `closeModal()` directly, which just removes the `open`
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
// A topic's page (Topiki › <topic>): its sections, the settings windows and
// moving a partition's leadership. The page itself is modules/tentabus/
// topic-detail.js; the shell loads the topic and says where each move leads.
// =============================================================================

function openTopicDetail(name, section = DEFAULT_SECTION) {
  if (state.view?.name !== name) {
    state.detail = null;
    state.detailError = null;
    state.detailNotice = null;
    state.justMoved = new Set();
  }
  state.view = { kind: 'topic-detail', name, section: TOPIC_SECTIONS.includes(section) ? section : DEFAULT_SECTION };
  renderPanel();
  loadTopicDetail(name);
}

function closeTopicDetail() {
  state.view = null;
  state.detail = null;
  state.detailError = null;
  state.detailNotice = null;
  state.justMoved = new Set();
}

// A failed reload keeps the page it already shows (the header says the data
// is old); only a topic never loaded, or one deleted since, shows the failure.
const loadTopicDetail = topicDetailLoader({
  fetch: (instanceId, name) => ApiBinary.one('busTopicDetailRequest', { instanceId: requireInstanceId(instanceId), name }),
  context: () => ({ instanceId: state.instanceId, name: state.view?.name }),
  apply({ detail, error }) {
    if (!error) {
      state.detail = detail;
      state.detailError = null;
    } else {
      const missing = busErrorCode(error?.message) === 'topic_not_found';
      // A failed reload keeps the page it had; only "gone" replaces it.
      if (state.detail && !missing) return;
      if (missing) state.detail = null;
      state.detailError = error;
    }
    renderPanel();
  },
});

/** The section the open topic's page shows (a closed one falls back). */
function currentSection() {
  // Until the topic answers (or when it is gone) nothing is known to be
  // closed, so the address keeps the section that was asked for.
  if (!state.detail) return state.view.section;
  return effectiveSection(state.view.section, state.detail.access);
}

const detailContext = {
  view() {
    const sh = state.shell;
    return {
      name: state.view.name,
      detail: state.detail,
      error: state.detailError,
      errorKind: state.detailError ? loadErrorKind(state.detailError) : null,
      section: state.view.section,
      stats: state.stats,
      subjects: sh.subjectsError ? null : sh.subjects,
      capabilities: state.capabilities,
      nodes: sh.nodes || [],
      replicaTopics: sh.replicaTopics,
      replicaLags: sh.replicaLags,
      lagSeries: sh.lagSeries,
      notice: state.detailNotice,
      justMoved: state.justMoved,
      instanceLabel: state.instanceLabel,
      nowMs: Date.now(),
    };
  },
  go(action) {
    const name = state.view?.name;
    if (!name) return;
    if (action.kind === 'back') { setTab('topics'); return; }
    if (action.kind === 'section') {
      if (!TOPIC_SECTIONS.includes(action.section) || action.section === state.view.section) return;
      state.view.section = action.section;
      state.detailNotice = null;
      renderPanel();
    } else if (action.kind === 'preview') openTopicPreview(name);
    else if (action.kind === 'delete') openTopicDeleteWindow(name);
    else if (action.kind === 'change') openTopicSettings(name, action.card);
    else if (action.kind === 'transfer') openPartitionTransfer(name, action.partition, 'topic');
    else if (action.kind === 'group') { setTab('groups'); openGroupDetail(action.group, name); }
    else if (action.kind === 'dlq') tabContext.go({ kind: 'dlq', topic: name });
    else if (action.kind === 'retry') { state.detailError = null; renderPanel(); loadTopicDetail(name); }
  },
};

function openTopicSettings(name, card) {
  const detail = state.detail;
  if (!detail?.access?.canAdmin) return;
  const instanceId = state.instanceId;
  openSettingsWindow(card, {
    instanceId: requireInstanceId(instanceId),
    view: { ...detailContext.view(), topic: detail.topic, partitions: detail.partitions || [] },
    update: (request) => ApiBinary.action('busTopicUpdateRequest', request),
    describeError: describeBusError,
    onSaved: async (notice) => {
      if (state.instanceId !== instanceId || state.view?.name !== name) return;
      state.detailNotice = { section: 'settings', tone: 'success', title: notice.title, text: notice.text };
      await loadTopicDetail(name);
      loadTopics();
      if (card === 'write') refreshReplicas();
    },
  });
}

// "Przenieś prowadzenie" from a topic's Partycje i kopie (`from = 'topic'`)
// or from Kopie i nody (`from = 'replication'`): the same window over the
// replica list the shell polls; the result lands as a note where it started.
function openPartitionTransfer(topic, partition, from) {
  const allowed = from === 'topic' ? state.detail?.access?.canAdmin === true : canAdmin();
  if (!allowed) return;
  const replica = (state.shell.replicaTopics || []).find((r) => r.topic === topic)?.partitions?.find((p) => Number(p.partition) === Number(partition));
  if (!replica) return;
  const instanceId = state.instanceId;
  openLeaderTransfer({
    topic,
    partition,
    choices: transferChoices(replica, state.shell.nodes || []),
    transfer: (target) => ApiBinary.action('busLeaderTransferRequest', buildLeaderTransferRequest(instanceId, topic, partition, target)),
    describeError: describeBusError,
    onDone: async ({ label }) => {
      if (state.instanceId !== instanceId) return;
      const text = T('partitions.moved_text', { partition: fmtCount(partition), topic, node: label });
      if (from === 'topic' && state.view?.name === topic) {
        state.justMoved.add(Number(partition));
        state.detailNotice = { section: 'partitions', tone: 'success', title: T('partitions.moved_title'), text };
      } else if (from === 'replication') {
        state.justMoved.add(`${topic}:${partition}`);
        state.replicationNotice = { title: T('partitions.moved_title'), text };
      }
      renderPanel();
      await refreshReplicas();
    },
  });
}

// =============================================================================
// Kopie i nody (T10): modules/tentabus/replication.js over the replica lists
// the shell polls.
// =============================================================================

const replicationContext = {
  view() {
    const sh = state.shell;
    return {
      nodes: sh.nodes,
      replicaTopics: sh.replicaTopics,
      failovers: sh.failovers,
      topics: state.topics,
      canAdmin: canAdmin(),
      notice: state.replicationNotice,
      justMoved: state.justMoved,
      nowMs: Date.now(),
    };
  },
  go(action) {
    if (action.kind === 'topic') { setTab('topics'); openTopicDetail(action.topic, 'partitions'); }
    else if (action.kind === 'transfer') openPartitionTransfer(action.topic, action.partition, 'replication');
  },
};

function chipHtml(chip) {
  return `<tf-chip variant="outline" status="${escapeAttr(chip.status)}">${escapeHtml(chip.label)}</tf-chip>`;
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

export default TentaBusScreen;
