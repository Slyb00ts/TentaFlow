// ============ File: coding-agent.js — Codex and Claude Code login/session UI over node-routed RPC. ============

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml, toast } from '/js/utils.js';

export async function agentRequest(service, operation, payload = {}) {
  const response = await ApiBinary.action('serviceAgentRequest', {
    serviceId: Number(service.id),
    nodeId: service.nodeId || service.node_id,
    operation,
    payloadJson: JSON.stringify(payload),
  });
  if (!response?.success) {
    const error = response?.error || 'Coding-agent request failed';
    if (error.includes('session_expired')) {
      throw new Error('Sesja CLI wygasła. Ponowne logowanie może wykonać administrator.');
    }
    if (error.includes('administrator_required_for_login')) {
      throw new Error('Ponowne logowanie jest dostępne wyłącznie dla administratora.');
    }
    throw new Error(error);
  }
  return JSON.parse(response.resultJson || response.result_json || '{}');
}

export async function findDeployedAgent(engineId, nodeId) {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const services = await ApiBinary.list('serviceListRequest', { arrayKey: 'services' });
    const service = services.find((item) =>
      (item.engineId || item.engine_id) === engineId
      && (!nodeId || (item.nodeId || item.node_id) === nodeId)
      && (item.status === 'running' || item.status === 'starting'));
    if (service) return service;
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error('Deployed coding-agent service was not found');
}

export async function openAgentLogin(service) {
  const status = await agentRequest(service, 'auth.status');
  if (status.authenticated) {
    // Admin-triggered login is the one moment worth paying for a real CLI
    // probe: the account may have changed, so the cached list is suspect.
    await agentRequest(service, 'models.list', { refresh: true });
    toast(`${service.displayName || service.display_name}: konto jest już zalogowane`, 'success');
    return;
  }
  const started = await agentRequest(service, 'auth.start');
  const flowId = started.flow_id || started.flowId;
  if (!flowId) throw new Error('Login process did not return a flow id');

  const win = document.createElement('tf-window');
  win.setAttribute('title', `Logowanie — ${service.displayName || service.display_name}`);
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '680');
  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <p>Logowanie działa w CLI na węźle usługi. Otwórz link pokazany poniżej i podaj kod lub token, jeśli CLI o niego poprosi.</p>
    <pre data-agent-terminal style="max-height:300px;overflow:auto;white-space:pre-wrap"></pre>
    <div data-agent-links style="display:flex;gap:8px;flex-wrap:wrap;margin:10px 0"></div>
    <tf-input data-agent-input label="Kod lub token zwrotny" autocomplete="off"></tf-input>
  `;
  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.innerHTML = '<tf-button variant="primary" data-agent-send>Wyślij do konsoli</tf-button>';
  win.append(body, footer);
  document.body.appendChild(win);

  let afterSeq = 0;
  let closed = false;
  // The login flow holds a live CLI process in a PTY. Closing the window without
  // telling the bridge would leave it running until the service restarts.
  const close = () => {
    closed = true;
    win.remove();
    agentRequest(service, 'session.close', { session_id: flowId }).catch(() => {});
  };
  win.addEventListener('close', close);
  footer.querySelector('[data-agent-send]')?.addEventListener('click', async () => {
    const input = body.querySelector('[data-agent-input]');
    const value = String(input?.value || '').trim();
    if (!value) return;
    await agentRequest(service, 'session.input', { session_id: flowId, text: `${value}\r` });
    input.value = '';
  });

  while (!closed) {
    try {
      const [events, auth] = await Promise.all([
        agentRequest(service, 'session.events', { session_id: flowId, after_seq: afterSeq }),
        agentRequest(service, 'auth.status'),
      ]);
      const terminal = body.querySelector('[data-agent-terminal]');
      const links = body.querySelector('[data-agent-links]');
      for (const event of events.events || []) {
        afterSeq = Math.max(afterSeq, Number(event.seq || 0));
        const text = String(event.data?.text || '');
        if (terminal && text) {
          terminal.textContent += text;
          terminal.scrollTop = terminal.scrollHeight;
        }
        for (const match of text.matchAll(/https?:\/\/[^\s\x1b]+/g)) {
          const url = match[0].replace(/[),.;]+$/, '');
          if (links && !links.querySelector(`[data-url="${CSS.escape(url)}"]`)) {
            links.insertAdjacentHTML('beforeend', `<tf-button variant="primary" data-url="${escapeAttr(url)}">Otwórz ${escapeHtml(new URL(url).hostname)}</tf-button>`);
            links.lastElementChild?.addEventListener('click', () => window.open(url, '_blank', 'noopener'));
          }
        }
      }
      if (auth.authenticated) {
        await agentRequest(service, 'models.list', { refresh: true });
        toast('Logowanie zakończone', 'success');
        close();
        return;
      }
    } catch (error) {
      toast(error.message || String(error), 'error');
      close();
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
}
