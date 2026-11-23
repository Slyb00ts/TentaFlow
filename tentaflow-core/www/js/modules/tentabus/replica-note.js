// ===== File: modules/tentabus/replica-note.js — the note under a view that a copy on this node answered =====
//
// A node that does not lead a topic answers reads (browse, unprocessed
// messages, a group's state, the lag) from its own copy, which may trail the
// leading node by about a second. The answer says so with `fromReplica`; the
// view then says that its newest values or messages may still be missing.
// `values` = counters and offsets, `messages` = a list of messages.

import { escapeHtml } from '/js/utils.js';
import { T } from '/js/modules/tentabus/format.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/** Whether any of the answers was served from a copy (`fromReplica`, camelCased by the codec). */
export const fromReplica = (...answers) => answers.some((a) => a?.fromReplica === true);

/** The note's markup, or `''` when the answer came from the leader. */
export function replicaNoteHtml(kind, replica) {
  if (!replica) return '';
  return `${sprite('clock')}<span>${escapeHtml(T(kind === 'messages' ? 'replica.note_messages' : 'replica.note_values'))}</span>`;
}
