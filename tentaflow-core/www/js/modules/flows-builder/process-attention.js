// ============ File: flows-builder/process-attention.js — the "Needs attention" view of the run list: open incidents of my runs and of runs of my processes ============

// A list answers one scope at a time and the people notified of an incident are
// the initiator and the definition owner, so the durable counterpart of the
// notice is the two scopes read together.
export const ATTENTION_SCOPES = ['Mine', 'Owned'];
export const ATTENTION_PAGE = 100;

export function mergeAttentionInstances(pages) {
  const byId = new Map();
  for (const page of pages) {
    for (const instance of page) byId.set(instance.instanceId, instance);
  }
  return [...byId.values()].sort((left, right) => right.updatedAtMs - left.updatedAtMs
    || (left.instanceId < right.instanceId ? 1 : -1));
}

// `request(scope)` asks one scope for its runs with an open incident. `more` is
// true when either scope holds more than one page, so the view can say it shows
// the most recent ones.
export async function loadAttentionInstances(request) {
  const answers = await Promise.all(ATTENTION_SCOPES.map((scope) => request(scope)));
  return {
    instances: mergeAttentionInstances(answers.map((answer) => answer.instances)),
    more: answers.some((answer) => answer.hasMore),
  };
}
