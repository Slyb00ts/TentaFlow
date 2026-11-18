// ===== File: modules/tentabus/routes.js — the TentaBus address: which instance, tab and object the screen shows =====
//
// `#/tentabus?instance=…&tab=…&topic=…&section=…&group=…&gtopic=…&subject=…` is written with the router's
// own `replaceParams` (no history entry per click, no second router) and read
// back by `mount(params)`, so a reload or a pasted link reopens the same view.
// `&corr=…&partition=…&offset=…` under a topic is a one-way link from the Events
// screen: it opens that topic's message preview on a record, and is not written back.
// Pure functions: the screen owns the state, this file only translates it.

export const MAIN_TABS = ['overview', 'topics', 'groups', 'dlq', 'schemas', 'replication'];
export const DEFAULT_TAB = 'overview';
/** The sections of a topic's page, in the order of its menu. */
export const TOPIC_SECTIONS = ['state', 'settings', 'access', 'hiding', 'dlq', 'partitions'];
export const DEFAULT_SECTION = 'state';
/** The sections of a consumer's page, in the order of its menu; it opens on the first. */
export const CONSUMER_SECTIONS = ['state', 'position', 'settings'];

/**
 * The view a hash names. An unknown tab falls back to the overview; a topic
 * belongs to the Topiki tab, a consumer to Odbiorcy and a message pattern
 * (`subject`) to Wzory wiadomości, whatever `tab` says,
 * so a hand-edited link cannot open a topic under the wrong tab. A section
 * belongs to the open topic or consumer; an unknown one opens its first section.
 */
export function parseRoute(params = {}) {
  const topic = params.topic ? String(params.topic) : null;
  const group = params.group ? String(params.group) : null;
  const subject = !topic && !group && params.subject ? String(params.subject) : null;
  let tab = MAIN_TABS.includes(params.tab) ? params.tab : DEFAULT_TAB;
  if (topic) tab = 'topics';
  else if (group) tab = 'groups';
  else if (subject) tab = 'schemas';
  return {
    ...previewOf(topic, params),
    instance: params.instance ? String(params.instance) : null,
    tab,
    topic,
    section: sectionOf(topic ? TOPIC_SECTIONS : group ? CONSUMER_SECTIONS : null, params.section),
    group: topic ? null : group,
    groupTopic: !topic && group && params.gtopic ? String(params.gtopic) : null,
    subject,
  };
}

/**
 * The deep link into a topic's message preview (`corr`, `partition`, `offset`),
 * as the Events screen writes it: only the parts that are well formed, and only
 * under a topic. A malformed number is dropped rather than guessed, so a
 * hand-edited link opens the preview without it. Absent parts leave their key
 * out, so an ordinary address parses exactly as before.
 */
function previewOf(topic, params) {
  const out = {};
  if (!topic) return out;
  if (typeof params.corr === 'string' && params.corr.trim() !== '') out.corr = params.corr.trim();
  for (const key of ['partition', 'offset']) {
    const raw = params[key];
    if ((typeof raw === 'string' || typeof raw === 'number') && /^\d{1,15}$/.test(String(raw))) out[key] = Number(raw);
  }
  return out;
}

/**
 * The params of a parsed route that must survive choosing an instance: the
 * topic and the preview deep link. Without them the instance picker would
 * drop a link from Events that carries no instance.
 */
export function carriedParams(route) {
  const out = {};
  if (!route.topic) return out;
  out.topic = route.topic;
  for (const key of ['corr', 'partition', 'offset']) if (route[key] != null) out[key] = route[key];
  return out;
}

function sectionOf(sections, asked) {
  if (!sections) return null;
  return sections.includes(asked) ? asked : sections[0];
}

/** Router params for a view; defaults are left out so the address stays short. */
export function routeParams({ instance, tab, topic = null, section = null, group = null, groupTopic = null, subject = null }) {
  const out = {};
  if (instance) out.instance = instance;
  if (topic) {
    out.tab = 'topics';
    out.topic = topic;
    if (section && section !== DEFAULT_SECTION) out.section = section;
    return out;
  }
  if (group) {
    out.tab = 'groups';
    out.group = group;
    if (groupTopic) out.gtopic = groupTopic;
    if (section && section !== CONSUMER_SECTIONS[0]) out.section = section;
    return out;
  }
  if (subject) {
    out.tab = 'schemas';
    out.subject = subject;
    return out;
  }
  if (tab && tab !== DEFAULT_TAB) out.tab = tab;
  return out;
}
