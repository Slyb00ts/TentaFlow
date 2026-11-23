// ===== File: modules/tentabus/consumer-sla.js — what a flow's SLA policy adds to a consumer's page =====
//
// A consumer of the bus is read by a flow (`bus_consume` node) under the group
// `k:<service account>.<group>`. The policy belongs to that flow, so the page
// finds the flow whose node reads this group and topic and shows, from the
// flow's consumer status, how many units run at once ("Równoległość 3/4") and
// until when the policy holds the reading back; Ustawienia shows the policy's
// values and leads to it. Nothing here is drawn: it only decides what the
// page says; consumer-detail.js paints it in place on every poll.

import { escapeAttr, escapeHtml } from '/js/utils.js';
import { T, fmtCount } from '/js/modules/tentabus/format.js';
import { bounded, clockText, durationText, leadingReading, windowPer, T as slaT } from '/js/modules/sla/model.js';

/** The `bus_consume` configs of a flow's graph: `{ instanceId, topic, group, serviceAccount }` for each node. */
export function consumeConfigs(graphJson) {
  let graph;
  try {
    graph = typeof graphJson === 'string' ? JSON.parse(graphJson) : graphJson;
  } catch {
    return [];
  }
  return (graph?.nodes ?? [])
    .filter((node) => node?.type === 'bus_consume' && node.config)
    .map((node) => ({
      instanceId: String(node.config.instance_id ?? ''),
      topic: String(node.config.topic ?? ''),
      group: String(node.config.group ?? ''),
      serviceAccount: String(node.config.service_account ?? ''),
    }));
}

/** The group the broker sees for a node's config: the key's namespace plus the plain group. */
export const effectiveGroup = (config) => (config.serviceAccount ? `k:${config.serviceAccount}.${config.group}` : '');

/** Whether `graphJson` has a node reading `topic` of the bus instance `instanceId` as the broker group `group`. */
export function readsAs(graphJson, { group, topic, instanceId }) {
  return consumeConfigs(graphJson).some((config) => config.topic === topic
    && config.instanceId === instanceId
    && effectiveGroup(config) === group);
}

const NODE_DEADLINE_MS = 4000;

/**
 * Finds the flow that reads `group` on `topic` of the bus instance `instanceId` and what the policy
 * says about it. `call(kind, payload, options)` is the transport, `flows(options)` reads the payload-less flow list, `nodes()` the mesh nodes; `cache`
 * (a Map kept by the page) remembers each flow's graph, which is read once, not once per poll.
 * The flow's consumer is read from every node on its own short deadline and the one that
 * runs is used, as the "Stan SLA" card does. Answers `null` when no flow reads as this consumer
 * (an external program does).
 */
export async function loadConsumerSla({ group, topic, instanceId, call, flows, cache, nodes = async () => [], deadlineMs = NODE_DEADLINE_MS }) {
  const list = await flows({});
  const candidates = (list.flows ?? []).filter((flow) => flow.busConsumer);
  let match = null;
  for (const flow of candidates) {
    if (!cache.has(flow.id)) {
      const detail = await call('flowDetailRequest', { flowId: flow.id }).catch(() => null);
      if (!detail) continue;
      cache.set(flow.id, detail.graphJson);
    }
    if (readsAs(cache.get(flow.id), { group, topic, instanceId })) { match = flow; break; }
  }
  if (!match) return null;
  const flow = { id: match.id, name: match.name };
  const elsewhere = Boolean(match.slaBrief?.governedElsewhere);
  const [policyResponse, status, consumer] = await Promise.all([
    match.slaPolicyId && !elsewhere ? bounded(call('slaPolicyGetRequest', { policyId: match.slaPolicyId }), deadlineMs) : Promise.resolve(null),
    match.slaPolicyId && !elsewhere ? bounded(call('slaStatusRequest', { flowId: match.id }).then((r) => r.statuses?.[0] ?? null), deadlineMs) : Promise.resolve(null),
    runningConsumer({ flowId: match.id, local: match.busConsumer, flows, nodes, deadlineMs }),
  ]);
  return {
    flow,
    consumer,
    // The reading of the node this page is open on: the one that says "not the leader of the topic here".
    local: match.busConsumer,
    elsewhere,
    policy: policyResponse?.policy ?? null,
    policyError: Boolean(match.slaPolicyId) && !elsewhere && !policyResponse?.policy,
    status,
  };
}

async function runningConsumer({ flowId, local, flows, nodes, deadlineMs }) {
  const known = (await bounded(nodes(), deadlineMs)) ?? [];
  if (known.length < 2) return local;
  const readings = await Promise.all(known.map(async (node) => {
    const consumer = node.isLocal
      ? local
      : await bounded(
        flows({ targetNodeId: node.nodeId, timeoutMs: deadlineMs }).then((r) => r.flows?.find((f) => f.id === flowId)?.busConsumer ?? null),
        deadlineMs,
      );
    return { node, consumer };
  }));
  return leadingReading(readings)?.consumer ?? local;
}

/** Until when the policy holds the reading back, in epoch milliseconds; `null` when it does not. */
export function throttledUntilMs(sla, nowMs) {
  const until = sla?.consumer?.throttledUntil;
  return until != null && Number(until) > nowMs ? Number(until) : null;
}

/** The "Równoległość" tile, or `null` when the policy has no thread limit. */
export function concurrencyTile(sla) {
  const max = sla?.consumer?.maxConcurrency;
  if (max == null) return null;
  const ordering = sla.policy?.spec?.ordering;
  return {
    value: `${fmtCount(sla.consumer.inFlight ?? 0)}/${fmtCount(max)}`,
    delta: ordering == null ? '' : T(ordering === 'none' ? 'consumer.sla.kpi_delta_unordered' : 'consumer.sla.kpi_delta_ordered'),
  };
}

/** The texts of the "Wstrzymano limitem" alert. */
export function throttledTexts(sla, nowMs) {
  const until = throttledUntilMs(sla, nowMs);
  if (until == null) return null;
  const time = clockText(until, true);
  const window = sla.status?.window;
  const policy = sla.policy?.spec?.name ?? '';
  const counts = window ? { used: fmtCount(window.used), limit: fmtCount(window.limit) } : null;
  let m1;
  if (policy) m1 = counts ? T('consumer.sla.alert_throttled_window', { ...counts, policy }) : T('consumer.sla.alert_throttled_plain', { policy });
  else m1 = counts ? T('consumer.sla.alert_throttled_window_unnamed', counts) : T('consumer.sla.alert_throttled_unnamed');
  return {
    title: T('consumer.sla.alert_throttled', { time }),
    m1,
    m2: T('consumer.sla.alert_throttled_resume', { time }),
  };
}

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;
const row = (label, value, hint) => `
  <div class="tb-vrow">
    <div class="tb-vr-label">${escapeHtml(label)}</div>
    <div><div class="tb-vr-value">${escapeHtml(value)}</div>${hint ? `<div class="tb-vr-hint">${escapeHtml(hint)}</div>` : ''}</div>
  </div>`;

/** The "Polityka SLA" card of Ustawienia; empty when no flow reads as this consumer. */
export function slaSettingsHtml(sla) {
  if (!sla) return '';
  const flow = sla.flow.name;
  const head = (open) => `
    <div class="section-card-head">
      <div class="title">${sprite('shield')} ${escapeHtml(T('consumer.sla.title'))}</div>
      ${open ? `<div class="actions"><tf-button variant="secondary" size="sm" data-go="sla-policies" title="${escapeAttr(T('consumer.sla.open_title'))}">${escapeHtml(T('consumer.sla.open'))}</tf-button></div>` : ''}
    </div>`;
  const link = `<div class="tb-vr-hint tb-vr-link"><span>${escapeHtml(T('consumer.sla.set_by', { flow }))}</span>
      <tf-button variant="ghost" size="sm" icon="shield" data-go="sla-policies">${escapeHtml(T('consumer.sla.link'))}</tf-button></div>`;
  const plain = (value, hint) => `<div class="section-card" data-card="sla">${head(false)}<div class="tb-vrows">${row(T('consumer.sla.policy'), value, hint)}</div></div>`;
  if (sla.elsewhere) return plain(T('consumer.sla.elsewhere'), T('consumer.sla.elsewhere_hint', { flow }));
  if (sla.policyError) return plain(T('consumer.sla.load_error'), T('consumer.sla.load_error_hint'));
  if (!sla.policy) return plain(T('consumer.sla.none'), T('consumer.sla.none_hint', { flow }));
  const spec = sla.policy.spec;
  const rows = [`
    <div class="tb-vrow">
      <div class="tb-vr-label">${escapeHtml(T('consumer.sla.policy'))}</div>
      <div><div class="tb-vr-value">${escapeHtml(spec.name)}</div>${link}</div>
    </div>`];
  rows.push(row(
    T('consumer.sla.threads'),
    spec.maxConcurrency == null ? slaT('unlimited') : fmtCount(spec.maxConcurrency),
    spec.maxConcurrency == null ? '' : T(spec.ordering === 'none' ? 'consumer.sla.threads_unordered' : 'consumer.sla.threads_ordered'),
  ));
  if (spec.callLimit != null) {
    rows.push(row(T('consumer.sla.calls'), slaT('calls_per', { limit: fmtCount(spec.callLimit), window: windowPer(spec.callWindowSecs) }), T('consumer.sla.calls_hint')));
  }
  if (spec.timeoutMs != null) rows.push(row(T('consumer.sla.timeout'), durationText(spec.timeoutMs), ''));
  const thresholds = [];
  if (spec.latencyP95Ms != null) thresholds.push(T('consumer.sla.threshold_p95', { value: fmtCount(spec.latencyP95Ms) }));
  if (spec.errorRatePct != null) thresholds.push(T('consumer.sla.threshold_errors', { value: fmtCount(spec.errorRatePct) }));
  if (spec.callLimit != null && spec.warnAtPct != null) thresholds.push(T('consumer.sla.threshold_warn', { value: fmtCount(spec.warnAtPct) }));
  if (thresholds.length) rows.push(row(T('consumer.sla.thresholds'), thresholds.join(' · '), ''));
  return `<div class="section-card" data-card="sla">${head(true)}<div class="tb-vrows">${rows.join('')}</div></div>`;
}
