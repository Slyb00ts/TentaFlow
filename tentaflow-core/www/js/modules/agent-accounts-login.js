// =============================================================================
// File: modules/agent-accounts-login.js — the provider sign-in window (A02) and
//       the state machine behind it.
//
//       One window, three entry points: an administrator on a shared account
//       (A01/A03), the owner of a personal one (U01), and the console's "connect
//       an account" card (C01), which calls `openLoginWizard` the same way.
//
//       Core runs the vendor CLI in a terminal ON A NODE and keeps the flow;
//       the browser only shows what that terminal printed, types the code back
//       and polls for the outcome. That is why closing this window CANCELS the
//       sign-in instead of leaving a CLI holding a PTY on the node: nothing here
//       owns the flow, so nothing here may abandon it.
//
//       No credential material ever reaches this file — `LoginStatusResponse`
//       carries a state, a provider identity and a message key, and the
//       material goes from the bridge into the store on the node.
// =============================================================================

import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
import { TfWindow } from '/js/components/tf-window.js';
import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml, toast } from '/js/utils.js';
import { AgentAccounts, T, describeError, engineName, osLabel } from '/js/modules/agent-accounts.js';

/** How often a started sign-in is polled. The person is at the provider for most of it. */
export const LOGIN_POLL_MS = 1500;

/** States a flow can end in; nothing is polled or cancelled afterwards. */
const TERMINAL = new Set(['succeeded', 'failed', 'cancelled']);

/**
 * The sign-in as a state machine, with no DOM in it.
 *
 * `api` is the request layer (`AgentAccounts`); the window passes the real one.
 * Every transition ends in `notify()`, so a renderer subscribes once and never
 * has to ask what happened.
 */
export class LoginFlow {
  constructor(api = AgentAccounts, { pollMs = LOGIN_POLL_MS } = {}) {
    this.api = api;
    this.pollMs = pollMs;
    this.state = 'idle';
    this.loginId = null;
    this.nodeId = null;
    this.verificationUrl = '';
    this.instructionKey = '';
    this.expiresAt = '';
    this.providerSubject = null;
    this.planLabel = null;
    this.message = '';
    this.busy = false;
    this.listener = null;
    this.timer = null;
    this.pollFailures = 0;
    this.disposed = false;
  }

  onChange(listener) {
    this.listener = listener;
  }

  get finished() {
    return TERMINAL.has(this.state);
  }

  notify() {
    this.listener?.(this);
  }

  /** Starts the CLI's sign-in. `nodeId` null lets Core pick the account's home node. */
  async start({ accountId, nodeId = null }) {
    if (this.busy || this.loginId) return;
    this.busy = true;
    this.state = 'starting';
    this.message = '';
    this.notify();
    try {
      const started = await this.api.loginStart({ accountId, nodeId });
      if (this.disposed) return;
      this.loginId = started.login_id ?? started.loginId ?? '';
      this.nodeId = started.node_id ?? started.nodeId ?? nodeId;
      this.verificationUrl = started.verification_url ?? started.verificationUrl ?? '';
      this.instructionKey = started.instruction_key ?? started.instructionKey ?? '';
      this.expiresAt = started.expires_at ?? started.expiresAt ?? '';
      this.state = 'awaiting_input';
      this.schedulePoll();
    } catch (error) {
      if (this.disposed) return;
      this.state = 'failed';
      this.message = describeError(error).message;
    } finally {
      this.busy = false;
      this.notify();
    }
  }

  /** Types the code the provider showed into the sign-in terminal. */
  async submit(value) {
    const code = String(value ?? '').trim();
    if (!this.loginId || this.busy || this.finished || !code) return;
    this.busy = true;
    this.message = '';
    this.notify();
    try {
      await this.api.loginInput(this.loginId, code);
      if (this.disposed) return;
      // The node moves the flow to `verifying` itself; showing it here keeps the
      // button disabled until the next poll answers instead of inviting a second
      // submission of the same code.
      this.state = 'verifying';
    } catch (error) {
      if (this.disposed) return;
      this.message = describeError(error).message;
    } finally {
      this.busy = false;
      this.notify();
    }
  }

  /**
   * Abandons the sign-in. The account keeps whatever credential it had, and the
   * node closes the terminal — a cancel that never reached it would leave a CLI
   * running for the flow's whole ten-minute deadline.
   */
  async cancel() {
    this.stopPolling();
    const loginId = this.loginId;
    const wasRunning = !this.finished && Boolean(loginId);
    this.state = 'cancelled';
    this.notify();
    if (!wasRunning) return;
    try {
      await this.api.loginCancel(loginId);
    } catch (error) {
      // The flow may already be gone (it expired, or the node swept it); the
      // sign-in is over either way, so this is reported and not retried.
      console.warn('[agent-accounts] the sign-in was not confirmed cancelled:', describeError(error).message);
    }
  }

  stopPolling() {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  /** Stops every timer; the flow itself is left where it is. */
  dispose() {
    this.disposed = true;
    this.stopPolling();
    this.listener = null;
  }

  schedulePoll() {
    this.stopPolling();
    if (this.disposed || this.finished || !this.loginId) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.poll();
    }, this.pollMs);
  }

  async poll() {
    if (this.disposed || this.finished || !this.loginId) return;
    let snapshot;
    try {
      snapshot = await this.api.loginStatus(this.loginId);
      this.pollFailures = 0;
    } catch (error) {
      if (this.disposed || this.finished) return;
      const { code, message } = describeError(error);
      // The node knows of no such flow: it expired, it was cancelled elsewhere,
      // or Core restarted under it. There is nothing left to poll.
      if (code === 'NotFound') {
        this.state = 'failed';
        this.message = T('login.lost');
        this.notify();
        return;
      }
      this.pollFailures += 1;
      if (this.pollFailures >= 5) {
        this.state = 'failed';
        this.message = message;
        this.notify();
        return;
      }
      this.schedulePoll();
      return;
    }
    if (this.disposed || this.finished) return;
    const state = String(snapshot.state ?? '');
    const messageKey = snapshot.message_key ?? snapshot.messageKey ?? null;
    this.providerSubject = snapshot.provider_subject ?? snapshot.providerSubject ?? this.providerSubject;
    this.planLabel = snapshot.plan_label ?? snapshot.planLabel ?? this.planLabel;
    this.message = messageKey ? I18n.t(messageKey) : '';
    if (state === 'succeeded' || state === 'failed') {
      this.state = state;
      this.notify();
      return;
    }
    this.state = state || this.state;
    this.notify();
    this.schedulePoll();
  }
}

// =============================================================================
// The window
// =============================================================================

/**
 * Only an address a browser can open. The URL comes from a terminal transcript,
 * which is the one thing on this screen the node did not author, so nothing but
 * http(s) is ever turned into a link or handed to `window.open`.
 */
export function safeHttpUrl(raw) {
  try {
    const url = new URL(String(raw ?? ''));
    return url.protocol === 'https:' || url.protocol === 'http:' ? url.href : '';
  } catch {
    return '';
  }
}

/**
 * Opens A02 for one account.
 *
 * `nodes` is the runtime matrix when the caller has one (the admin screens); a
 * user signing their own account in has no right to read it, so the window
 * simply omits the picker and lets Core choose the node. `onFinished` is called
 * with the final state, so the screen behind can reload itself.
 */
export function openLoginWizard({
  accountId,
  accountName = '',
  engineId = '',
  engines = [],
  nodes = [],
  homeNodeId = null,
  onFinished = null,
} = {}) {
  const flow = new LoginFlow();
  const eligible = nodes.filter((node) => node.receives_accounts ?? node.receivesAccounts);
  const body = document.createElement('div');
  body.className = 'aa-login';
  body.innerHTML = `
    <ol class="aa-login-steps">
      <li class="aa-login-step" data-step="start">
        <span class="aa-login-num">1</span>
        <div class="aa-login-body">
          <h5>${escapeHtml(T('login.step_start'))}</h5>
          <p class="aa-hint">${escapeHtml(T('login.step_start_hint'))}</p>
          ${eligible.length ? `
            <tf-select data-field="node" label="${escapeAttr(T('login.field_node'))}"></tf-select>` : ''}
          <div class="aa-login-actions">
            <tf-button variant="primary" data-act="start">${escapeHtml(T('login.action_start'))}</tf-button>
          </div>
        </div>
      </li>
      <li class="aa-login-step" data-step="open">
        <span class="aa-login-num">2</span>
        <div class="aa-login-body">
          <h5>${escapeHtml(T('login.step_open'))}</h5>
          <div class="aa-login-link" data-link hidden>
            <a data-url target="_blank" rel="noopener noreferrer"></a>
            <tf-button variant="secondary" size="sm" data-act="copy">${escapeHtml(T('login.action_copy'))}</tf-button>
            <tf-button variant="primary" size="sm" data-act="open">${escapeHtml(T('login.action_open'))}</tf-button>
          </div>
          <p class="aa-hint" data-instruction></p>
        </div>
      </li>
      <li class="aa-login-step" data-step="code">
        <span class="aa-login-num">3</span>
        <div class="aa-login-body">
          <h5>${escapeHtml(T('login.step_code'))}</h5>
          <div class="aa-login-actions">
            <tf-input data-field="code" autocomplete="off" spellcheck="false"
              placeholder="${escapeAttr(T('login.field_code_placeholder'))}"></tf-input>
            <tf-button variant="primary" data-act="submit">${escapeHtml(T('login.action_submit'))}</tf-button>
          </div>
        </div>
      </li>
      <li class="aa-login-step" data-step="result">
        <span class="aa-login-num">4</span>
        <div class="aa-login-body">
          <h5>${escapeHtml(T('login.step_result'))}</h5>
          <p class="aa-hint" data-result>${escapeHtml(T('login.step_result_hint'))}</p>
        </div>
      </li>
    </ol>
    <p class="aa-error" role="alert" data-error hidden></p>`;

  const footer = document.createElement('div');
  footer.innerHTML = `
    <tf-button variant="ghost" data-act="cancel">${escapeHtml(T('login.action_cancel'))}</tf-button>
    <tf-button variant="primary" data-action="close">${escapeHtml(I18n.t('common.close'))}</tf-button>`;

  const closed = TfWindow.open({
    title: T('login.title'),
    subtitle: [engineName(engineId, engines), accountName].filter(Boolean).join(' · '),
    icon: 'key',
    width: 720,
    modal: true,
    buttons: 'close',
    body,
    footer,
  });

  const field = (name) => body.querySelector(`[data-field="${name}"]`);
  const act = (name) => body.querySelector(`[data-act="${name}"]`) ?? footer.querySelector(`[data-act="${name}"]`);
  const error = body.querySelector('[data-error]');
  const link = body.querySelector('[data-link]');
  const anchor = body.querySelector('[data-url]');

  const picker = field('node');
  if (picker) {
    const preferred = eligible.some((node) => (node.node_id ?? node.nodeId) === homeNodeId)
      ? homeNodeId
      : (eligible[0].node_id ?? eligible[0].nodeId);
    picker.setOptions(
      eligible.map((node) => {
        const id = node.node_id ?? node.nodeId;
        return { value: id, label: `${node.node_name ?? node.nodeName ?? id} · ${osLabel(node.os)}` };
      }),
      preferred,
    );
  }

  const stepState = () => {
    const started = Boolean(flow.loginId);
    return {
      start: flow.state === 'idle' || flow.state === 'starting' ? 'now' : 'done',
      open: !started ? 'todo' : (flow.finished ? 'done' : 'now'),
      code: !started ? 'todo' : (flow.state === 'awaiting_input' ? 'now' : (flow.finished ? 'done' : 'now')),
      result: flow.finished ? 'now' : 'todo',
    };
  };

  const paint = () => {
    const steps = stepState();
    for (const [name, mark] of Object.entries(steps)) {
      const step = body.querySelector(`[data-step="${name}"]`);
      step.classList.toggle('is-now', mark === 'now');
      step.classList.toggle('is-done', mark === 'done');
    }
    const url = safeHttpUrl(flow.verificationUrl);
    link.hidden = !url;
    if (url) {
      anchor.textContent = url;
      anchor.href = url;
    }
    body.querySelector('[data-instruction]').textContent = flow.instructionKey
      ? I18n.t(flow.instructionKey)
      : T('login.step_open_hint');

    act('start').hidden = Boolean(flow.loginId);
    act('start').toggleAttribute('disabled', flow.busy);
    if (picker) picker.toggleAttribute('disabled', Boolean(flow.loginId) || flow.busy);

    const canType = flow.state === 'awaiting_input' || flow.state === 'awaiting_open';
    field('code').toggleAttribute('disabled', !canType || flow.busy);
    act('submit').toggleAttribute('disabled', !canType || flow.busy);
    act('cancel').hidden = flow.finished;
    act('cancel').toggleAttribute('disabled', flow.busy);

    const result = body.querySelector('[data-result]');
    if (flow.state === 'succeeded') {
      const identity = [flow.providerSubject, flow.planLabel].filter(Boolean).join(' · ');
      result.textContent = identity ? T('login.result_ok_as', { who: identity }) : T('login.result_ok');
    } else if (flow.state === 'failed') {
      result.textContent = flow.message || T('login.result_failed');
    } else if (flow.state === 'verifying') {
      result.textContent = T('login.result_verifying');
    } else {
      result.textContent = T('login.step_result_hint');
    }

    // One line for everything that went wrong: a refused start, a code the
    // terminal would not take, and the outcome of a sign-in that failed.
    const problem = flow.state === 'failed' ? (flow.message || T('login.result_failed')) : flow.message;
    error.textContent = problem;
    error.hidden = !problem;
  };

  flow.onChange(paint);

  act('start').addEventListener('click', () => {
    flow.start({ accountId, nodeId: picker ? picker.value || null : null });
  });
  const submit = () => {
    const input = field('code');
    const value = String(input.value || '').trim();
    if (!value) return;
    input.value = '';
    flow.submit(value);
  };
  act('submit').addEventListener('click', submit);
  field('code').addEventListener('keydown', (event) => {
    if (event.key === 'Enter') submit();
  });
  body.querySelector('[data-act="copy"]').addEventListener('click', async () => {
    const url = safeHttpUrl(flow.verificationUrl);
    if (!url) return;
    try {
      await navigator.clipboard.writeText(url);
      toast(T('login.copied'), 'success');
    } catch {
      // A denied clipboard is not a failed sign-in: the address is on screen as
      // a link the person can copy by hand.
      toast(T('login.copy_failed'), 'warning');
    }
  });
  body.querySelector('[data-act="open"]').addEventListener('click', () => {
    const url = safeHttpUrl(flow.verificationUrl);
    if (url) window.open(url, '_blank', 'noopener,noreferrer');
  });
  act('cancel').addEventListener('click', () => {
    flow.cancel().finally(() => body.closest('tf-window')?.close());
  });

  // Closing the window ends the sign-in: the flow lives on a node, and a person
  // who closed this window is not going to finish it.
  closed.then(() => {
    const outcome = flow.state;
    if (!flow.finished) flow.cancel();
    flow.dispose();
    onFinished?.(outcome);
  });

  paint();
  return flow;
}
