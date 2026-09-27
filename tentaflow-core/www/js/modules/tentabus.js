// =============================================================================
// File: modules/tentabus.js — the TentaBus screen shell (SUM/tentabus/
// PLAN-UI-20260923 U0–U2, mockups SUM/mockups/tentabus-20260923): breadcrumb,
// the TentaBus header card with the instance picker, the six underlined main
// tabs (Przegląd / Topiki / Odbiorcy / Nieprzetworzone / Wzory wiadomości /
// Kopie i nody) with their counters, the address (`#/tentabus?instance=…&tab=
// …&topic=…&section=…&group=…&gtopic=…`, see modules/tentabus/routes.js) and
// the polling every tab reads. Przegląd, Topiki with a topic's page (Stan,
// Ustawienia, Nieprzetworzone, Partycje i kopie), Odbiorcy with a consumer's
// page (Stan, Miejsce czytania, Ustawienia), Nieprzetworzone wiadomości,
// Wzory wiadomości and Kopie i nody live in modules/tentabus/*.
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
import { MAIN_TABS, DEFAULT_TAB, DEFAULT_SECTION, TOPIC_SECTIONS, CONSUMER_SECTIONS, parseRoute, routeParams } from '/js/modules/tentabus/routes.js';
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
import { drawConsumers, consumerKey } from '/js/modules/tentabus/consumers.js';
import { drawConsumerDetail, positionRows, consumerDlq } from '/js/modules/tentabus/consumer-detail.js';
import { openOffsetMove, movedText } from '/js/modules/tentabus/offset-move.js';
import { drawUnprocessed, unprocessedRecord, unprocessedTopics, mergeNewest, retryAllPlan, topicConsumers, messageName, receiversText, UNPROCESSED_PAGE, LIST_STEP } from '/js/modules/tentabus/unprocessed.js';
import { openUnprocessedView, openRetryOne, openDiscardOne, openRetryAll } from '/js/modules/tentabus/unprocessed-windows.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-button.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-select.js';
import '/js/components/tf-chip.js';
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
// The mutually-exclusive views `#tb-panel` can show (six tabs, a topic's
// page, a consumer's page) — each gets its OWN persistent container (`ensureViewContainer`)
// so switching between them shows/hides existing DOM instead of tearing it
// down and rebuilding it, keeping scroll position, in-progress search text,
// table sort and focus intact across a tab switch.
const VIEW_SLOTS = ['overview', 'topics', 'groups', 'dlq', 'schemas', 'detail', 'consumer', 'replication'];
// "Ponów wszystkie" republishes up to 500 messages one by one on the server.
const RETRY_ALL_TIMEOUT_MS = 120_000;
// =============================================================================
// Pure helpers — no DOM, no ApiBinary. Unit-tested from
// `tentabus.request-builders.test.js` by brace-extraction (services.js-style),
// since this module pulls in DOM-only custom-element imports at load time.
// =============================================================================

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

// Looks up one topic's row in `BusStatsSnapshotWire.topics` by name — `null`
// when the snapshot has not loaded yet or predates this topic (a brand-new
// topic can lag one poll behind `topics[]`, tor U task 3).
function findTopicStats(statsTopics, name) {
  return (Array.isArray(statsTopics) ? statsTopics : []).find((t) => t.topic === name) || null;
}

// The broker's internal `tf-*` consumer groups (the read-only probe) are
// not something an operator manages; the server already hides them and the
// topic's delete window leaves them out again.
function isInternalGroupId(groupId) {
  return typeof groupId === 'string' && groupId.startsWith('tf-');
}

// Per-partition paging cursor for the NEXT `DlqList` page
// (tor U task 1/2's `partitions[]` + `fromOffsets`): only partitions that
// reported `hasMore` carry a cursor forward, at their own `nextOffset` — a
// partition that already reached its high watermark is simply omitted, not
// re-sent with a stale offset.
function buildFromOffsetsForNextPage(partitions) {
  return (Array.isArray(partitions) ? partitions : [])
    .filter((p) => p.hasMore)
    .map((p) => ({ partition: p.partition, offset: p.nextOffset }));
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
// disabled every one of the translated `tentabus.errors.*` codes (5 locales)
// plus the "no unprocessed message yet" branch of the unprocessed lists.
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
// edit/delete and moving leadership for EVERY
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
  view: null, // null | { kind: 'topic-detail', name, section } | { kind: 'consumer-detail', group, topic, section }

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

  // `GroupListResponse.groups`, `null` until it answers, and the failed load.
  groups: null,
  groupsError: null,
  // The open consumer's page: `{ detail, topicDetail, dlq, samples }` once
  // its first answer landed, the failed load, the note over the section it
  // concerns, the partitions whose reading place was just moved (marked and
  // not offered again until "Odśwież"), and a pause/resume in flight.
  consumer: null,
  consumerError: null,
  consumerNotice: null,
  consumerMoved: new Set(),
  consumerBusy: false,

  // Nieprzetworzone wiadomości — see `freshUnprocessed`.
  unprocessed: freshUnprocessed(),

  // What the frame (header card, tab counters) and Przegląd read beside the
  // stats snapshot — see `freshShellState`.
  shell: freshShellState(),
};

// The unprocessed messages the tab and a topic's section show: per topic its
// newest pages (`null` while the first one loads; `{ records, hasMore,
// partitions, error }`), and what the reader may do there (`{ canAdmin,
// adminLabels, maxAttempts }` from the topic's own answer); the rows each
// list shows, and the note over the tab.
function freshUnprocessed() {
  return { byTopic: new Map(), access: new Map(), asked: new Map(), shown: LIST_STEP, sectionShown: LIST_STEP, notice: null };
}

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

// `canAdmin` gates every instance-level change: creating and deleting topics
// and moving a partition's leadership from Kopie i nody. It is `bus.admin` in the instance matrix AND the org
// Admin role — what every such handler's `gate_admin` checks — and fails
// closed before `busCapabilitiesRequest` answers. A topic's page, a consumer
// (pause, resume, moving its reading place), its list row and the retry and
// discard of a topic's unprocessed messages gate on the topic's own
// administration instead, which also honours the topic's ACL.
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
    else if (route.group && route.groupTopic) openConsumer(route.group, route.groupTopic, route.section);
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
    state.groups = null;
    state.groupsError = null;
    closeConsumer();
    state.unprocessed = freshUnprocessed();
    unprocessedTurns.clear();
    unprocessedLoads.clear();
    state.shell = freshShellState();
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
    } else if (state.view?.kind === 'consumer-detail') {
      items.push({ label: tabLabel, act: 'groups', href: href({ tab: 'groups' }) });
      items.push({ label: state.view.group });
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
  const consumer = state.view?.kind === 'consumer-detail' ? state.view : null;
  let section = null;
  if (state.view?.kind === 'topic-detail') section = currentSection();
  else if (consumer) section = consumer.section;
  Router.replaceParams(routeParams({
    instance: state.instanceId,
    tab: state.tab,
    topic: state.view?.kind === 'topic-detail' ? state.view.name : null,
    section,
    group: consumer?.group || null,
    groupTopic: consumer?.topic || null,
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
  else if (state.view?.kind === 'consumer-detail') {
    state.consumerMoved = new Set();
    consumerDlqDue = true;
    loadConsumer(consumerKey(state.view.group, state.view.topic));
  }
  // Every unprocessed list is asked again, the open one first.
  state.unprocessed.asked = new Map();
  ensureUnprocessed();
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
    else if (action.kind === 'group') openConsumer(action.group, action.topic);
    else if (action.kind === 'dlq') {
      // A topic's unprocessed messages are dealt with in its own section.
      if (action.topic) { setTab('topics'); openTopicDetail(action.topic, 'dlq'); } else setTab('dlq');
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
  if (state.view?.kind === 'topic-detail') closeTopicDetail();
  else if (state.view?.kind === 'consumer-detail') closeConsumer();
  if (id !== 'topics') state.topicsNotice = null;
  if (id !== 'replication') { state.replicationNotice = null; state.justMoved = new Set(); }
  if (id !== 'dlq') { state.unprocessed.notice = null; state.unprocessed.shown = LIST_STEP; }
  state.tab = id;
  renderPanel();
  if (id === 'groups' && state.groups == null) loadGroups();
  if (id === 'dlq') ensureUnprocessed();
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
  let activeKey = state.tab;
  if (state.view?.kind === 'topic-detail') activeKey = 'detail';
  else if (state.view?.kind === 'consumer-detail') activeKey = 'consumer';
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
  if (activeKey === 'groups') { drawConsumers(activeEl, consumersContext); return; }
  if (activeKey === 'consumer') { drawConsumerDetail(activeEl, consumerContext); return; }
  if (activeKey === 'dlq') { drawUnprocessed(activeEl, unprocessedContext); return; }
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
// Przegląd, the lists and the open pages all read it) and ReplicaListRequest 10 s
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
  ensureUnprocessed();
  const rate = userRate(state.stats);
  pushRateSample(state.shell.ratePoints, state.shell.statsAt, rate);
  paintShell();
  if (state.tab === 'overview' && !state.view) {
    const body = document.querySelector('#tb-panel > [data-tb-view-slot="overview"]');
    pushOverviewSample(body, rate, state.shell.statsAt);
    renderPanel();
  }
  if ((state.tab === 'topics' || state.tab === 'dlq') && !state.view) renderPanel();
  if (state.tab === 'groups' && !state.view) {
    // A consumer that connected since the list answered shows up in the
    // snapshot first: the list is asked again so its row can be drawn.
    const known = new Set((state.groups || []).map((g) => consumerKey(g.group, g.topic)));
    if (state.groups && (state.stats.groups || []).some((g) => !known.has(consumerKey(g.group, g.topic)))) loadGroups();
    renderPanel();
  }
  if (state.view) renderPanel();
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
  if (state.view?.kind === 'consumer-detail') {
    // Its reading places move with the log, asked again on the same cadence.
    loadConsumer(consumerKey(state.view.group, state.view.topic));
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
    state.unprocessed.sectionShown = LIST_STEP;
  }
  state.view = { kind: 'topic-detail', name, section: TOPIC_SECTIONS.includes(section) ? section : DEFAULT_SECTION };
  renderPanel();
  loadTopicDetail(name);
  ensureUnprocessed();
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
      unprocessed: state.unprocessed.byTopic.get(state.view.name) ?? null,
      unprocessedShown: state.unprocessed.sectionShown,
      instanceLabel: state.instanceLabel,
      nowMs: Date.now(),
    };
  },
  go(action) {
    const name = state.view?.name;
    if (!name) return;
    if (action.kind === 'back') { setTab('topics'); return; }
    if (action.kind === 'section' || action.kind === 'dlq') {
      const section = action.kind === 'dlq' ? 'dlq' : action.section;
      if (!TOPIC_SECTIONS.includes(section) || section === state.view.section) return;
      state.view.section = section;
      state.detailNotice = null;
      state.unprocessed.sectionShown = LIST_STEP;
      renderPanel();
      ensureUnprocessed();
    } else if (action.kind === 'unp-view') openUnprocessedWindow('view', name, action.key);
    else if (action.kind === 'unp-retry') openUnprocessedWindow('retry', name, action.key);
    else if (action.kind === 'unp-discard') openUnprocessedWindow('discard', name, action.key);
    else if (action.kind === 'unp-retry-all') openUnprocessedRetryAll(name, 'topic');
    else if (action.kind === 'unp-more') showMoreUnprocessed(name);
    else if (action.kind === 'unp-reload') loadUnprocessedTopic(name);
    else if (action.kind === 'preview') openTopicPreview(name);
    else if (action.kind === 'delete') openTopicDeleteWindow(name);
    else if (action.kind === 'change') openTopicSettings(name, action.card);
    else if (action.kind === 'transfer') openPartitionTransfer(name, action.partition, 'topic');
    else if (action.kind === 'group') openConsumer(action.group, name);
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

// =============================================================================
// Odbiorcy (T05): the list lives in modules/tentabus/consumers.js, a
// consumer's page in consumer-detail.js and the "Przesuń" window in
// offset-move.js; the shell loads the data and says where each move leads.
// =============================================================================

async function loadGroups() {
  const instanceId = state.instanceId;
  try {
    const groups = await ApiBinary.list('busGroupListRequest', { arrayKey: 'groups', payload: { instanceId: requireInstanceId(instanceId) } });
    if (state.instanceId !== instanceId) return;
    state.groups = (groups || []).filter((g) => !isInternalGroupId(g.group));
    state.groupsError = null;
  } catch (err) {
    if (state.instanceId !== instanceId) return;
    // The last list stays on screen; a list never loaded shows the failure.
    state.groupsError = err;
  }
  if (state.tab === 'groups' && !state.view) renderPanel();
}

const consumersContext = {
  view() {
    return {
      groups: state.groupsError && !state.groups ? null : state.groups,
      error: state.groupsError,
      errorKind: state.groupsError ? loadErrorKind(state.groupsError) : null,
      stats: state.stats,
      instanceLabel: state.instanceLabel,
      notice: null,
      nowMs: Date.now(),
    };
  },
  go(action) {
    if (action.kind === 'open') openConsumer(action.group, action.topic);
    else if (action.kind === 'pause' || action.kind === 'resume') toggleConsumerPause(action.group, action.topic, action.kind === 'pause');
    else if (action.kind === 'retry') { state.groupsError = null; state.groups = null; renderPanel(); loadGroups(); }
  },
};

function openConsumer(group, topic, section = CONSUMER_SECTIONS[0]) {
  const same = state.view?.kind === 'consumer-detail' && state.view.group === group && state.view.topic === topic;
  if (state.view?.kind === 'topic-detail') closeTopicDetail();
  if (!same) {
    closeConsumer();
    consumerDlqDue = true;
  }
  state.tab = 'groups';
  state.view = { kind: 'consumer-detail', group, topic, section: CONSUMER_SECTIONS.includes(section) ? section : CONSUMER_SECTIONS[0] };
  renderPanel();
  loadConsumer(consumerKey(group, topic));
}

function closeConsumer() {
  if (state.view?.kind === 'consumer-detail') state.view = null;
  state.consumer = null;
  state.consumerError = null;
  state.consumerNotice = null;
  state.consumerMoved = new Set();
  state.consumerBusy = false;
}

// Everything the consumer's page shows, in one answer: the consumer itself
// (reading places, how it confirms), its topic (rights, who administers it,
// the oldest numbers kept, retries), its own unprocessed messages among the
// newest page of the topic's, and its recent lag samples (whether the backlog
// still grows). Only the consumer itself is required; the rest degrades to
// "unknown" on the page.
//
// Reading the unprocessed messages is a message read the server audits
// (`bus.messages.browse`, one row per partition), so it happens when the
// page opens and on "Odśwież" — not on every poll, where the count it gives
// would cost a stream of audit rows the reader never asked for.
let consumerDlqDue = true;

async function fetchConsumer(instanceId, group, topic) {
  const iid = requireInstanceId(instanceId);
  const [groupResp, topicDetail] = await Promise.all([
    ApiBinary.one('busGroupDetailRequest', { instanceId: iid, group, topic }),
    ApiBinary.one('busTopicDetailRequest', { instanceId: iid, name: topic }).catch(() => null),
  ]);
  const nowMs = Date.now();
  const readDlq = consumerDlqDue || !state.consumer?.dlq;
  consumerDlqDue = false;
  const [dlqResp, history] = await Promise.allSettled([
    !readDlq ? Promise.resolve(null)
      : topicDetail?.access?.canRead
      ? ApiBinary.one('busDlqListRequest', { instanceId: iid, sourceTopic: topic, limit: UNPROCESSED_PAGE, newestFirst: true })
      : Promise.reject(new Error('no read access')),
    ApiBinary.one('busLagHistoryRequest', { instanceId: iid, group, topic, sinceMs: nowMs - 10 * 60_000 }),
  ]);
  let dlq = null;
  if (!readDlq) dlq = state.consumer?.dlq ?? null;
  else if (dlqResp.status === 'fulfilled') {
    dlq = consumerDlq({ records: dlqResp.value?.records, hasMore: dlqResp.value?.hasMore, group, nowMs });
  } else if (busErrorCode(dlqResp.reason?.message) === 'topic_not_found') {
    // The topic has never had an unprocessed message: nothing is this consumer's.
    dlq = consumerDlq({ records: [], hasMore: false, group, nowMs });
  }
  const series = history.status === 'fulfilled'
    ? (history.value?.groups || []).find((x) => x.group === group && x.topic === topic)
    : null;
  return { detail: groupResp?.detail, topicDetail, dlq, samples: series?.samples || [] };
}

// A failed reload keeps the page it already shows (the header says the data
// is old); only a consumer never loaded, or one gone since, shows the failure.
const loadConsumer = topicDetailLoader({
  fetch: (instanceId, key) => {
    const [group, topic] = key.split('\u0000');
    return fetchConsumer(instanceId, group, topic);
  },
  context: () => ({
    instanceId: state.instanceId,
    name: state.view?.kind === 'consumer-detail' ? consumerKey(state.view.group, state.view.topic) : null,
  }),
  apply({ detail, error }) {
    if (!error) {
      state.consumer = detail;
      state.consumerError = null;
    } else {
      const missing = busErrorCode(error?.message) === 'group_not_found';
      if (state.consumer && !missing) return;
      if (missing) state.consumer = null;
      state.consumerError = error;
    }
    renderPanel();
  },
});

const consumerContext = {
  view() {
    const v = state.view;
    return {
      group: v.group,
      topic: v.topic,
      section: v.section,
      data: state.consumer,
      error: state.consumerError,
      errorKind: state.consumerError ? loadErrorKind(state.consumerError) : null,
      stats: state.stats,
      notice: state.consumerNotice,
      justMoved: state.consumerMoved,
      busy: state.consumerBusy,
      instanceLabel: state.instanceLabel,
      nowMs: Date.now(),
    };
  },
  go(action) {
    const v = state.view;
    if (v?.kind !== 'consumer-detail') return;
    if (action.kind === 'back') setTab('groups');
    else if (action.kind === 'section') {
      if (!CONSUMER_SECTIONS.includes(action.section) || action.section === v.section) return;
      v.section = action.section;
      state.consumerNotice = null;
      renderPanel();
    } else if (action.kind === 'pause' || action.kind === 'resume') toggleConsumerPause(v.group, v.topic, action.kind === 'pause');
    else if (action.kind === 'move') openConsumerMove(action.partition);
    else if (action.kind === 'topic') { setTab('topics'); openTopicDetail(v.topic, DEFAULT_SECTION); }
    else if (action.kind === 'topic-settings') { setTab('topics'); openTopicDetail(v.topic, 'settings'); }
    else if (action.kind === 'dlq') tabContext.go({ kind: 'dlq', topic: v.topic });
    else if (action.kind === 'retry') { state.consumerError = null; renderPanel(); loadConsumer(consumerKey(v.group, v.topic)); }
  },
};

// Where the reader stands: the consumer list, one consumer's page, or elsewhere.
function consumerSurface() {
  if (state.view?.kind === 'consumer-detail') return `page:${consumerKey(state.view.group, state.view.topic)}`;
  return state.tab === 'groups' && !state.view ? 'list' : 'other';
}

// "Wstrzymaj" / "Wznów" from a list row or the consumer's page: both end on
// the consumer's Stan with the result, the way the mockups lead — unless the
// reader has gone elsewhere while the server answered; then a toast only.
async function toggleConsumerPause(group, topic, pause) {
  if (state.consumerBusy) return;
  const instanceId = state.instanceId;
  const surface = consumerSurface();
  state.consumerBusy = true;
  renderPanel();
  try {
    await ApiBinary.action(pause ? 'busGroupPauseRequest' : 'busGroupResumeRequest', { instanceId: requireInstanceId(instanceId), group, topic });
  } catch (err) {
    state.consumerBusy = false;
    if (state.instanceId !== instanceId) return;
    toast(describeBusError(err), 'error');
    renderPanel();
    return;
  }
  state.consumerBusy = false;
  if (state.instanceId !== instanceId) return;
  // The server has answered: the page shows the new state at once instead of
  // the last poll's until the next one lands.
  for (const g of [...(state.stats?.groups || []), ...(state.groups || [])]) {
    if (g.group === group && g.topic === topic) g.paused = pause;
  }
  const title = T(pause ? 'consumer.paused_title' : 'consumer.resumed_title');
  // The pause is kept by the node that leads the topic's partitions (the
  // server refuses it anywhere else) and is not copied to the other nodes:
  // with copies, a change of leading node lets the consumer read again.
  const copies = (Number(topicByName(topic)?.replicationFactor) || 1) > 1;
  const text = [
    T(pause ? 'consumer.paused_text' : 'consumer.resumed_text', { group, topic }),
    pause && copies ? T('consumer.paused_copies') : '',
  ].filter(Boolean).join(' ');
  if (consumerSurface() === surface && surface !== 'other') {
    openConsumer(group, topic, CONSUMER_SECTIONS[0]);
    state.consumerNotice = { section: 'state', tone: 'success', title, text };
  } else {
    toast(title, 'success');
  }
  renderPanel();
  await Promise.all([refreshStats(), loadGroups()]);
}

function openConsumerMove(partition) {
  const v = state.view;
  const data = state.consumer;
  if (v?.kind !== 'consumer-detail' || !data?.topicDetail?.access?.canAdmin) return;
  const instanceId = state.instanceId;
  const { group, topic } = v;
  const key = consumerKey(group, topic);
  const live = (state.stats?.groups || []).find((g) => g.group === group && g.topic === topic);
  openOffsetMove({
    group,
    places: positionRows({ detail: data.detail, topicPartitions: data.topicDetail.partitions }),
    partition,
    paused: live ? Boolean(live.paused) : Boolean(data.detail.paused),
    replicated: (Number(data.topicDetail.topic?.replicationFactor) || 1) > 1,
    resolveTimestamp: async (p, tsMs) => (await ApiBinary.one('busOffsetForTimestampRequest', { instanceId: requireInstanceId(instanceId), topic, partition: p, tsMs }))?.offset,
    move: async (req) => (await ApiBinary.action('busOffsetResetRequest', {
      instanceId: requireInstanceId(instanceId), group, topic, partition: req.partition, mode: req.mode, offset: req.offset, tsMs: req.tsMs,
    }))?.newOffset,
    describeError: describeBusError,
    onMoved: async ({ partition: p, after }) => {
      const here = () => state.instanceId === instanceId && state.view?.kind === 'consumer-detail' && consumerKey(state.view.group, state.view.topic) === key;
      if (!here()) return;
      state.consumerMoved.add(p);
      state.consumerNotice = { section: 'position', tone: 'success', title: T('move.done_title'), text: movedText({ partition: p, after, waiting: null }) };
      renderPanel();
      await loadConsumer(key);
      refreshStats();
      if (!here()) return;
      // What waits comes from the answer after the move, not from the numbers
      // the window opened with: the consumer may have read on meanwhile.
      const now = (state.consumer?.detail?.partitions || []).find((x) => Number(x.partition) === p);
      if (now) {
        state.consumerNotice = { section: 'position', tone: 'success', title: T('move.done_title'), text: movedText({ partition: p, after, waiting: Number(now.lag) || 0 }) };
        renderPanel();
      }
    },
  });
}

// =============================================================================
// Nieprzetworzone wiadomości (T06, a topic's section): modules/tentabus/
// unprocessed.js draws them and unprocessed-windows.js holds the windows; the
// shell loads each topic's newest pages and says where each move leads.
//
// Reading unprocessed messages is a message read the server audits
// (`bus.messages.browse`), so a topic's list is asked again only when what
// the stats snapshot counts for it changed since it was asked, on "Odśwież"
// and after a change made here — never on every poll.
// =============================================================================

// Only the newest answer per topic lands: a reload started after a retry
// must not be overwritten by the page asked before it.
const unprocessedTurns = new Map();
// The load each topic has in flight, so a change can wait for the one a
// stats poll already started instead of asking a second time.
const unprocessedLoads = new Map();

/** Asks for `topic`'s newest unprocessed messages, or (`more`) the page after those loaded. */
function loadUnprocessedTopic(topic, options = {}) {
  const load = fetchUnprocessedTopic(topic, options);
  unprocessedLoads.set(topic, load);
  return load;
}

async function fetchUnprocessedTopic(topic, { more = false }) {
  const instanceId = state.instanceId;
  if (!instanceId) return;
  const turn = (unprocessedTurns.get(topic) || 0) + 1;
  unprocessedTurns.set(topic, turn);
  const u = state.unprocessed;
  const prev = u.byTopic.get(topic) || null;
  const total = Number(findTopicStats(state.stats?.topics, topic)?.dlqDepth) || 0;
  if (!more) u.asked.set(topic, total);
  if (!prev && !more) u.byTopic.set(topic, null);
  const fromOffsets = more && prev ? buildFromOffsetsForNextPage(prev.partitions) : [];
  let entry;
  try {
    const resp = await ApiBinary.one('busDlqListRequest', {
      instanceId: requireInstanceId(instanceId),
      sourceTopic: topic,
      limit: UNPROCESSED_PAGE,
      newestFirst: true,
      fromOffsets: fromOffsets.length ? fromOffsets : undefined,
    });
    const records = (resp?.records || []).map((r) => unprocessedRecord(topic, r));
    entry = { records: more && prev ? [...prev.records, ...records] : records, hasMore: Boolean(resp?.hasMore), partitions: resp?.partitions || [], error: null };
  } catch (err) {
    // A topic whose messages were all processed never needed the store.
    entry = busErrorCode(err?.message) === 'topic_not_found'
      ? { records: [], hasMore: false, partitions: [], error: null }
      : { records: prev?.records || [], hasMore: Boolean(prev?.hasMore), partitions: prev?.partitions || [], error: describeBusError(err) };
  }
  if (state.instanceId !== instanceId || unprocessedTurns.get(topic) !== turn) return;
  u.byTopic.set(topic, entry);
  renderUnprocessedViews();
}

/** What the reader may do with `topic`'s messages, from the topic's own answer. */
async function loadUnprocessedAccess(topic) {
  const instanceId = state.instanceId;
  try {
    const d = await ApiBinary.one('busTopicDetailRequest', { instanceId: requireInstanceId(instanceId), name: topic });
    if (state.instanceId !== instanceId) return;
    state.unprocessed.access.set(topic, {
      canAdmin: d?.access?.canAdmin === true,
      adminLabels: d?.adminLabels || [],
      maxAttempts: Number(d?.topic?.maxDeliveryAttempts) || null,
    });
  } catch {
    // Without the topic's answer the tile offers no change (fails closed).
    return;
  }
  renderUnprocessedViews();
}

/**
 * Loads what the open unprocessed view needs and is not up to date: on the
 * tab every topic that has unprocessed messages, on a topic's page its own.
 */
function ensureUnprocessed() {
  if (!state.instanceId || !state.stats) return;
  const u = state.unprocessed;
  const stale = (topic, total) => !u.byTopic.has(topic) || u.asked.get(topic) !== total;
  if (state.view?.kind === 'topic-detail') {
    if (currentSection() !== 'dlq') return;
    const topic = state.view.name;
    const total = Number(findTopicStats(state.stats.topics, topic)?.dlqDepth) || 0;
    if (stale(topic, total)) loadUnprocessedTopic(topic);
    return;
  }
  if (state.view || state.tab !== 'dlq') return;
  for (const { topic, total } of unprocessedTopics(state.stats)) {
    if (stale(topic, total)) loadUnprocessedTopic(topic);
    if (!u.access.has(topic)) loadUnprocessedAccess(topic);
  }
}

function renderUnprocessedViews() {
  if ((!state.view && state.tab === 'dlq') || (state.view?.kind === 'topic-detail' && currentSection() === 'dlq')) renderPanel();
}

const unprocessedContext = {
  view() {
    const sh = state.shell;
    return {
      stats: state.stats,
      byTopic: state.unprocessed.byTopic,
      access: state.unprocessed.access,
      shown: state.unprocessed.shown,
      notice: state.unprocessed.notice,
      error: sh.statsError,
      errorKind: sh.statsError ? loadErrorKind(sh.statsError) : null,
      instanceLabel: state.instanceLabel,
      nowMs: Date.now(),
    };
  },
  go(action) {
    if (action.kind === 'topic') { setTab('topics'); openTopicDetail(action.topic, 'dlq'); }
    else if (action.kind === 'retry-all') openUnprocessedRetryAll(action.topic, 'instance');
    else if (action.kind === 'more') showMoreUnprocessed(null);
    else if (action.kind === 'retry') { state.unprocessed.asked = new Map(); ensureUnprocessed(); }
  },
};

/**
 * "Wczytaj więcej" of the tab (`topic` = null) or of a topic's section: ten
 * more rows, and the next page of a topic once its loaded rows run out.
 */
function showMoreUnprocessed(topic) {
  const u = state.unprocessed;
  if (topic) {
    u.sectionShown += LIST_STEP;
    const entry = u.byTopic.get(topic);
    if (entry && entry.hasMore && entry.records.length < u.sectionShown) loadUnprocessedTopic(topic, { more: true });
  } else {
    u.shown += LIST_STEP;
    const entries = unprocessedTopics(state.stats).map(({ topic: t }) => ({ topic: t, records: u.byTopic.get(t)?.records || [], hasMore: Boolean(u.byTopic.get(t)?.hasMore) }));
    const { rows, pending } = mergeNewest(entries);
    if (pending && rows.length < u.shown) loadUnprocessedTopic(pending, { more: true });
  }
  renderPanel();
}

/** Where a change to `topic`'s messages was made: the tab or the topic's own section. */
function unprocessedSurface(topic) {
  if (state.view?.kind === 'topic-detail' && state.view.name === topic) return 'topic';
  return !state.view && state.tab === 'dlq' ? 'instance' : 'other';
}

// After a retry or discard: the note where it was made, and every counter
// (the header card, the main tab, the section menu, the tiles) from a fresh
// stats snapshot together with the topic's list asked again.
async function afterUnprocessedChange(topic, surface, notice) {
  const instanceId = state.instanceId;
  const u = state.unprocessed;
  const before = unprocessedTurns.get(topic) || 0;
  // The fresh snapshot usually starts the topic's reload itself (its count
  // moved); only a change the count does not show asks again here.
  await refreshStats();
  if (state.instanceId !== instanceId) return;
  if ((unprocessedTurns.get(topic) || 0) > before) await unprocessedLoads.get(topic);
  else await loadUnprocessedTopic(topic);
  if (state.instanceId !== instanceId) return;
  const here = unprocessedSurface(topic);
  if (here === 'topic' && surface === 'topic') state.detailNotice = { section: 'dlq', ...notice };
  else if (here === 'instance' && surface === 'instance') u.notice = notice;
  else toast(notice.title, notice.tone === 'success' ? 'success' : 'warning');
  renderPanel();
}

function unprocessedMaxAttempts(topic) {
  if (state.view?.kind === 'topic-detail' && state.view.name === topic && state.detail?.topic) return Number(state.detail.topic.maxDeliveryAttempts) || null;
  return state.unprocessed.access.get(topic)?.maxAttempts ?? null;
}

function unprocessedCanAdmin(topic) {
  if (state.view?.kind === 'topic-detail' && state.view.name === topic) return state.detail?.access?.canAdmin === true;
  return state.unprocessed.access.get(topic)?.canAdmin === true;
}

/** "Pokaż", "Ponów" or "Odrzuć" of one message of `topic` (`key` = its row). */
function openUnprocessedWindow(kind, topic, key) {
  const rec = (state.unprocessed.byTopic.get(topic)?.records || []).find((r) => r.key === key);
  if (!rec) return;
  const canAdmin = unprocessedCanAdmin(topic);
  const instanceId = state.instanceId;
  const surface = unprocessedSurface(topic);
  const maxAttempts = unprocessedMaxAttempts(topic);
  const nowMs = Date.now();
  if (kind === 'view') {
    openUnprocessedView({
      rec,
      maxAttempts,
      nowMs,
      onRetry: canAdmin && !rec.atWrite ? () => openUnprocessedWindow('retry', topic, key) : null,
      onDiscard: canAdmin ? () => openUnprocessedWindow('discard', topic, key) : null,
    });
    return;
  }
  if (!canAdmin) return;
  const coords = { instanceId: requireInstanceId(instanceId), sourceTopic: topic, partition: rec.dlqPartition, offset: rec.dlqOffset };
  const what = messageName(rec, nowMs);
  if (kind === 'retry') {
    const consumers = topicConsumers(state.stats, topic);
    openRetryOne({
      rec,
      topic,
      consumers,
      maxAttempts,
      nowMs,
      retry: async () => Number((await ApiBinary.action('busDlqRetryRequest', coords))?.accepted),
      describeError: describeBusError,
      onDone: (accepted) => afterUnprocessedChange(topic, surface, accepted === 0
        ? { tone: 'warning', title: T('unprocessed.retry.rejected_again_title'), text: T('unprocessed.retry.rejected_again_text', { what, topic }) }
        : { tone: 'success', title: T('unprocessed.retry.done_title'), text: `${T('unprocessed.retry.done_text', { what, topic })} ${receiversText(consumers)}` }),
    });
  } else {
    openDiscardOne({
      rec,
      topic,
      discard: () => ApiBinary.action('busDlqDiscardRequest', coords),
      describeError: describeBusError,
      onDone: () => afterUnprocessedChange(topic, surface, { tone: 'success', title: T('unprocessed.discard.done_title'), text: T('unprocessed.discard.done_text', { what }) }),
    });
  }
}

/** "Ponów wszystkie" of one topic, from its tile (`instance`) or its section (`topic`). */
function openUnprocessedRetryAll(topic, from) {
  if (!unprocessedCanAdmin(topic)) return;
  const entry = state.unprocessed.byTopic.get(topic);
  if (!entry) return;
  const instanceId = state.instanceId;
  const total = Number(findTopicStats(state.stats?.topics, topic)?.dlqDepth) || 0;
  const plan = retryAllPlan({ total, records: entry.records, hasMore: entry.hasMore });
  if (plan.retryable === 0) return;
  const consumers = topicConsumers(state.stats, topic);
  openRetryAll({
    topic,
    plan,
    consumers,
    maxAttempts: unprocessedMaxAttempts(topic),
    retryAll: () => ApiBinary.action('busDlqRetryAllRequest', { instanceId: requireInstanceId(instanceId), sourceTopic: topic, maxRecords: plan.batch }, { timeoutMs: RETRY_ALL_TIMEOUT_MS }),
    describeError: describeBusError,
    onDone: (resp) => {
      const retried = Number(resp?.retried) || 0;
      const failed = Number(resp?.failed) || 0;
      const text = [
        retried > 0 ? T('unprocessed.retry_all.done_text', { topic, n: retried }) : T('unprocessed.retry_all.done_none_text'),
        retried > 0 ? receiversText(consumers, retried !== 1) : '',
        failed > 0 ? T('unprocessed.retry_all.done_failed', { count: fmtCount(failed), n: failed }) : '',
      ].filter(Boolean).join(' ');
      afterUnprocessedChange(topic, from, {
        tone: failed > 0 || retried === 0 ? 'warning' : 'success',
        title: retried > 0 ? T('unprocessed.retry_all.done_title', { count: fmtCount(retried), n: retried }) : T('unprocessed.retry_all.done_none_title'),
        text,
      });
    },
  });
}

export default TentaBusScreen;
