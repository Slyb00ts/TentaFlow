// ===== File: modules/tentanas/feature-words.js — the Environment rows and the Elastic capabilities in the reader's language =====
//
// The node sends each feature row's detail twice: its own English sentence
// (`detail`, a tooltip at most) and the same thing as codes with parameters
// (`NasEnvironment.featureReasons[id]`, `NasElasticCapabilities.reasons`).
// These maps word the codes. As everywhere (`wordReasons`), a row is worded
// only when EVERY part has words; otherwise the node's sentence is shown as
// it is, which is truthful if not translated.

import { T, wordReasons, nodeTextTitle } from '/js/modules/tentanas/format.js';
import { KERNEL_SUPPORT_WORDS } from '/js/modules/tentanas/target-wizard.js';

const need = (...values) => values.every((v) => typeof v === 'string' && v !== '');

export const FEATURE_DETAIL_WORDS = new Map([
  ...KERNEL_SUPPORT_WORDS,
  ['feature_binaries_missing', (p) => (need(p.binaries) ? T('env.detail.feature_binaries_missing', { binaries: p.binaries }) : null)],
  ['feature_version_too_low', (p) => (need(p.found, p.required) ? T('env.detail.feature_version_too_low', { found: p.found, required: p.required }) : null)],
  ['feature_module_not_loaded', (p) => (need(p.module) ? T('env.detail.feature_module_not_loaded', { module: p.module }) : null)],
  ['snapraid_killed', (p) => (need(p.signal) ? T('env.detail.snapraid_killed_signal', { signal: p.signal }) : T('env.detail.snapraid_killed'))],
  ['snapraid_probe_unconfirmed', (p) => (need(p.code) ? T('env.detail.snapraid_probe_unconfirmed', { code: p.code }) : null)],
  // The error itself may name a path; it stays in the tooltip.
  ['snapraid_probe_failed', () => T('env.detail.snapraid_probe_failed')],
  ['snapraid_vanished', () => T('env.detail.snapraid_vanished')],
  ['rdma_no_device', (p) => (need(p.path) ? T('env.detail.rdma_no_device', { path: p.path }) : null)],
  // Device and interface names are what the admin reads on the hardware:
  // they are shown as the node measured them.
  ['rdma_devices', (p) => (need(p.devices) ? p.devices : null)],
  ['ksmbd_no_interface', () => T('env.detail.ksmbd_no_interface')],
  ['ksmbd_exposed', (p) => (need(p.interfaces) ? T('env.detail.ksmbd_exposed', { interfaces: p.interfaces }) : null)],
  ['ksmbd_tools_missing', (p) => (need(p.tools, p.listener) ? T('env.detail.ksmbd_tools_missing', { tools: p.tools, listener: p.listener }) : null)],
  ['ksmbd_listener', (p) => (need(p.listener) ? p.listener : null)],
  ['ksmbd_experimental', () => T('env.detail.ksmbd_experimental')],
  ['module_loaded', (p) => (need(p.module) ? T('env.detail.module_loaded', { module: p.module }) : null)],
  ['module_on_demand', (p) => (need(p.module) ? T('env.detail.module_on_demand', { module: p.module }) : null)],
  ['module_absent', (p) => (need(p.module) ? T('env.detail.module_absent', { module: p.module }) : null)],
]);

// One Environment row's detail: `{ text, title }`. `text` is the worded
// reasons, or the node's sentence when this build cannot word them all (or
// the node is too old to send any); `title` is the node's sentence, scrubbed
// of ids, only when the text is not already that sentence.
export function featureDetail(feature, featureReasons) {
  const detail = String(feature?.detail || '');
  const reasons = featureReasons && typeof featureReasons === 'object' ? featureReasons[feature?.id] : null;
  const worded = wordReasons(reasons, FEATURE_DETAIL_WORDS);
  if (!worded) return { text: detail, title: '' };
  return { text: worded, title: nodeTextTitle(detail) };
}

// A tool's name as the Environment tab shows it ("SnapRAID"), never the
// feature id alone when the table has a word for it.
const toolName = (tool) => {
  const key = 'feature.' + tool;
  const words = T(key);
  return words === 'tentanas.' + key ? tool : words;
};

const CAPABILITY_WORDS = new Map([
  ['elastic_tool_unavailable', (p) => {
    if (!need(p.tool, p.status)) return null;
    const key = 'feature_status.' + p.status;
    const status = T(key);
    return status === 'tentanas.' + key ? null : T('wizard_pool.elastic_reason.tool_unavailable', { tool: toolName(p.tool), status });
  }],
  ['elastic_tool_not_probed', (p) => (need(p.tool) ? T('wizard_pool.elastic_reason.tool_not_probed', { tool: toolName(p.tool) }) : null)],
  ['elastic_no_mkfs', () => T('wizard_pool.elastic_reason.no_mkfs')],
]);

// Why this node cannot run an Elastic Array, `{ text, title }` like
// `featureDetail`. Empty text when the node gave no reason at all.
export function elasticCapabilitiesDetail(capabilities) {
  const detail = String(capabilities?.detail || '');
  const worded = wordReasons(capabilities?.reasons, CAPABILITY_WORDS);
  if (!worded) return { text: detail ? nodeTextTitle(detail) : '', title: '' };
  return { text: worded, title: nodeTextTitle(detail) };
}
