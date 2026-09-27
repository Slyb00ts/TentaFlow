// ===== File: modules/tentabus/routes.js — the TentaBus address: which instance, tab and object the screen shows =====
//
// `#/tentabus?instance=…&tab=…&topic=…&section=…&group=…&gtopic=…` is written with the router's
// own `replaceParams` (no history entry per click, no second router) and read
// back by `mount(params)`, so a reload or a pasted link reopens the same view.
// Pure functions: the screen owns the state, this file only translates it.

export const MAIN_TABS = ['overview', 'topics', 'groups', 'dlq', 'schemas', 'replication'];
export const DEFAULT_TAB = 'overview';
/** The sections of a topic's page, in the order of its menu. */
export const TOPIC_SECTIONS = ['state', 'settings', 'dlq', 'partitions'];
export const DEFAULT_SECTION = 'state';
/** The sections of a consumer's page, in the order of its menu; it opens on the first. */
export const CONSUMER_SECTIONS = ['state', 'position', 'settings'];

/**
 * The view a hash names. An unknown tab falls back to the overview; a topic
 * belongs to the Topiki tab and a consumer to Odbiorcy, whatever `tab` says,
 * so a hand-edited link cannot open a topic under the wrong tab. A section
 * belongs to the open topic or consumer; an unknown one opens its first section.
 */
export function parseRoute(params = {}) {
  const topic = params.topic ? String(params.topic) : null;
  const group = params.group ? String(params.group) : null;
  let tab = MAIN_TABS.includes(params.tab) ? params.tab : DEFAULT_TAB;
  if (topic) tab = 'topics';
  else if (group) tab = 'groups';
  return {
    instance: params.instance ? String(params.instance) : null,
    tab,
    topic,
    section: sectionOf(topic ? TOPIC_SECTIONS : group ? CONSUMER_SECTIONS : null, params.section),
    group: topic ? null : group,
    groupTopic: !topic && group && params.gtopic ? String(params.gtopic) : null,
  };
}

function sectionOf(sections, asked) {
  if (!sections) return null;
  return sections.includes(asked) ? asked : sections[0];
}

/** Router params for a view; defaults are left out so the address stays short. */
export function routeParams({ instance, tab, topic = null, section = null, group = null, groupTopic = null }) {
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
  if (tab && tab !== DEFAULT_TAB) out.tab = tab;
  return out;
}
