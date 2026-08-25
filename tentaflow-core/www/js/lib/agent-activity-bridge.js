// =============================================================================
// File: lib/agent-activity-bridge.js — wires a <tf-agent-activity> widget to the
//   run-events stream + reply/cancel protocol (Harness plan §3.9 / §3.11 C).
//   Used by BOTH chat.js and chat-audio.js: subscribe on chat mount with
//   scope=session, route AgentRunEvent frames into the widget, and forward the
//   widget's reply/permission/cancel CustomEvents to the binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { toast } from '/js/utils.js';

// Maps i18n keys → the flat label dict the (i18n-agnostic) component consumes.
export function activityLabels() {
  const t = (k, fb) => I18n.t(k) || fb;
  return {
    background_one: t('agent_activity.background_one', '{n} w tle'),
    background_many: t('agent_activity.background_many', '{n} w tle'),
    iteration: t('agent_activity.iteration', 'iteracja'),
    idle: t('agent_activity.idle', 'bezczynny'),
    runs_title: t('agent_activity.runs_title', 'Aktywne przebiegi'),
    no_runs: t('agent_activity.no_runs', 'Brak aktywnych przebiegów'),
    timeline_title: t('agent_activity.timeline_title', 'Oś czasu'),
    no_steps: t('agent_activity.no_steps', 'Brak kroków'),
    cancel: t('agent_activity.cancel', 'Anuluj'),
    elapsed: t('agent_activity.elapsed', 'czas'),
    tokens: t('agent_activity.tokens', 'tokeny'),
    asks: t('agent_activity.asks', 'pyta…'),
    question_send: t('agent_activity.question_send', 'Wyślij'),
    question_placeholder: t('agent_activity.question_placeholder', 'Wpisz odpowiedź…'),
    perm_wants: t('agent_activity.perm_wants', 'chce użyć narzędzia'),
    perm_of: t('agent_activity.perm_of', 'addonu'),
    perm_deny: t('agent_activity.perm_deny', 'Odmów'),
    perm_allow_once: t('agent_activity.perm_allow_once', 'Zezwól raz'),
    perm_allow_run: t('agent_activity.perm_allow_run', 'Zezwól na przebieg'),
    perm_always: t('agent_activity.perm_always', 'Zawsze'),
    back: t('agent_activity.back', 'Wstecz'),
    step_node: t('agent_activity.step_node', 'węzeł'),
    step_iteration: t('agent_activity.step_iteration', 'iteracja'),
    step_tool: t('agent_activity.step_tool', 'narzędzie'),
    step_compaction: t('agent_activity.step_compaction', 'kompakcja kontekstu'),
    step_router: t('agent_activity.step_router', 'router'),
    step_child: t('agent_activity.step_child', 'subagent'),
    step_question: t('agent_activity.step_question', 'pytanie'),
    step_permission: t('agent_activity.step_permission', 'uprawnienie'),
    step_resolved: t('agent_activity.step_resolved', 'rozwiązano'),
  };
}

// Wire a widget to a session scope. Returns a teardown function that unsubscribes
// the stream and detaches listeners. Re-call on session change.
/// One-line "what is the model doing right now", built from the raw
/// AgentRunEvent stream. Shared so the chat and the agent playground say the
/// same thing about the same event instead of each inventing its own wording.
///
/// Spawns are counted rather than listed: an agent that delegates five tasks in
/// one iteration should read as "Odpalam 5 agentów", not five separate lines
/// racing each other.
export function activityStatusText(event, labels = activityLabels()) {
  if (!event || !event.kind) return '';
  const l = labels;
  switch (event.kind) {
    case 'iteration_started':
      return event.max
        ? `${l.step_iteration} ${event.n}/${event.max}`
        : `${l.step_iteration} ${event.n}`;
    case 'tool_call_started':
      return `${l.step_tool} · ${event.name ?? ''}`.trim();
    case 'child_spawned': {
      const count = Number(event.count ?? 0);
      return count > 1
        ? t('agent_activity.status_spawn_many', 'Odpalam {count} agentów').replace('{count}', String(count))
        : `${l.step_child} · ${event.agent ?? ''}`.trim();
    }
    case 'user_question':
      return l.step_question;
    case 'permission_request':
      return l.step_permission;
    case 'compaction':
      return l.step_compaction;
    default:
      return '';
  }
}

/// `opts.onStatus` receives the one-line "what is happening now" for every
/// event that has one. The chat feeds it into the streaming bubble; the widget
/// keeps its own timeline either way.
export function attachAgentActivity(widget, sessionId, opts = {}) {
  if (!widget || !sessionId) return () => {};

  widget.labels = activityLabels();

  // Forward the widget's interaction events to the binary protocol.
  const onReply = async (e) => {
    const { runId, interactionId, answer } = e.detail || {};
    try {
      await ApiBinary.action('agentRunReplyRequest', {
        runId,
        questionId: interactionId,
        answer,
      });
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message ?? err}`, 'error');
    }
  };
  const onPermission = async (e) => {
    const { runId, interactionId, decision } = e.detail || {};
    try {
      await ApiBinary.action('agentPermissionReplyRequest', {
        runId,
        requestId: interactionId,
        decision,
      });
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message ?? err}`, 'error');
    }
  };
  const onCancel = async (e) => {
    const { runId } = e.detail || {};
    try {
      const resp = await ApiBinary.action('agentRunCancelRequest', { runId });
      if (resp && (resp.cancelled === true || resp.cancelled === 'true')) {
        widget.setRunStatus(runId, 'cancelled');
      }
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message ?? err}`, 'error');
    }
  };

  widget.addEventListener('agent-reply', onReply);
  widget.addEventListener('agent-permission', onPermission);
  widget.addEventListener('agent-cancel', onCancel);

  // Subscribe to the session's run-event stream. Each AgentRunEvent chunk is fed
  // to the widget; the stream stays open until teardown / disconnect.
  let unsubscribe = null;
  // A ChildFinished is mirrored to both the child's and the parent's scope, so a
  // widget subscribed to both would toast twice — track the run ids we toasted.
  const toastedRuns = new Set();
  ApiBinary.subscribe(
    'agentRunEventsSubscribeRequest',
    { scopeKind: 'session', scopeId: sessionId },
    {
      onChunk: (body) => {
        if (!body || body.variant !== 'AgentRunEvent') return;
        widget.applyEvent(body);
        if (typeof opts.onStatus === 'function') {
          const status = activityStatusText(body);
          if (status) opts.onStatus(status, body);
        }
        // A successfully finished background child → a subtle toast (§3.9).
        // Only `completed` claims completion; a failed/cancelled child must not.
        if (body.kind === 'child_finished' && (body.run_id || body.runId)) {
          const runId = body.run_id || body.runId;
          if (body.status === 'completed' && !toastedRuns.has(runId)) {
            toastedRuns.add(runId);
            const name = body.agent || runId.slice(0, 8);
            toast(I18n.t('agent_activity.finished_toast', { agent: name }) || `${name} ukończył`, 'info');
          }
        }
      },
      onError: () => {
        // Transient: the UI reconciles from RunDetail on the next mount. Do not
        // spam the operator with a toast for a backgrounded subscription.
      },
      onEnd: () => {
        unsubscribe = null;
      },
    },
  ).then((unsub) => {
    unsubscribe = unsub;
  }).catch(() => {});

  return () => {
    widget.removeEventListener('agent-reply', onReply);
    widget.removeEventListener('agent-permission', onPermission);
    widget.removeEventListener('agent-cancel', onCancel);
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
  };
}
