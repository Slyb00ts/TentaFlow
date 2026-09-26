// ===== File: modules/tentabus/routes.js — the TentaBus address: which instance, tab and object the screen shows =====
//
// `#/tentabus?instance=…&tab=…&topic=…&section=…&group=…&source=…` is written with the router's
// own `replaceParams` (no history entry per click, no second router) and read
// back by `mount(params)`, so a reload or a pasted link reopens the same view.
// Pure functions: the screen owns the state, this file only translates it.

export const MAIN_TABS = ['overview', 'topics', 'groups', 'dlq', 'schemas', 'replication'];
export const DEFAULT_TAB = 'overview';
/** The sections of a topic's page, in the order of its menu. */
export const TOPIC_SECTIONS = ['state', 'settings', 'partitions'];
export const DEFAULT_SECTION = 'state';

/**
 * The view a hash names. An unknown tab falls back to the overview; a topic
 * belongs to the Topiki tab and a consumer to Odbiorcy, whatever `tab` says,
 * so a hand-edited link cannot open a topic under the wrong tab. A section
 * belongs to a topic; an unknown one opens the topic's first section.
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
    section: topic ? (TOPIC_SECTIONS.includes(params.section) ? params.section : DEFAULT_SECTION) : null,
    group: topic ? null : group,
    groupTopic: !topic && group && params.gtopic ? String(params.gtopic) : null,
    dlqTopic: tab === 'dlq' && params.source ? String(params.source) : null,
  };
}

/** Router params for a view; defaults are left out so the address stays short. */
export function routeParams({ instance, tab, topic = null, section = null, group = null, groupTopic = null, dlqTopic = null }) {
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
    return out;
  }
  if (tab && tab !== DEFAULT_TAB) out.tab = tab;
  if (tab === 'dlq' && dlqTopic) out.source = dlqTopic;
  return out;
}
