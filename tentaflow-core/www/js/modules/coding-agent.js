// ============ File: coding-agent.js — Agent account login, access and relocation over node-routed RPC. ============

import '/js/components/tf-tabs.js';
import { I18n } from '/js/i18n.js';
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

export async function findDeployedAgent(deployId, nodeId) {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const services = await ApiBinary.list('serviceListRequest', { arrayKey: 'services' });
    const service = services.find((item) =>
      (item.activeDeployId || item.active_deploy_id || item.lastDeployId || item.last_deploy_id) === deployId
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
  if (status.credential_present && status.status === 'credentials_present_unverified') {
    toast(I18n.t('agent_accounts.credentials_saved'), 'info');
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
      const sameFlow = (auth.login_flow_id || auth.loginFlowId) === flowId;
      if (sameFlow && auth.login_completed === false && auth.status === 'login_failed') {
        throw new Error(I18n.t('agent_accounts.login_failed'));
      }
      if (sameFlow && auth.login_completed === true && auth.credential_present && !auth.authenticated) {
        toast(I18n.t('agent_accounts.credentials_saved'), 'info');
        close();
        return;
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

export async function openAgentAccount(service) {
  const [account, access, users] = await Promise.all([
    agentRequest(service, 'account.access'),
    agentRequest(service, 'account.grants.list'),
    ApiBinary.list('usersListRequest', { arrayKey: 'users' }),
  ]);
  const t = (key) => I18n.t(`agent_accounts.${key}`);
  const win = document.createElement('tf-window');
  win.setAttribute('title', t('title'));
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '600');
  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <tf-tabs data-account-tabs value="access">
      <tf-tab id="access" label="${escapeAttr(t('access'))}" icon="users"></tf-tab>
      <tf-tab id="move" label="${escapeAttr(t('move_title'))}" icon="arrow-right"></tf-tab>
    </tf-tabs>
    <section data-account-access>
    <tf-input data-account-name label="${escapeAttr(t('name'))}" value="${escapeAttr(account.display_name)}"></tf-input>
    <p class="text-muted">${escapeHtml(t('hint'))}</p>
    <tf-select data-account-user label="${escapeAttr(t('user'))}"></tf-select>
    <tf-button variant="outline" data-account-grant>${escapeHtml(t('grant'))}</tf-button>
    <tf-table data-account-grants actions-label="${escapeAttr(t('access'))}">
      <tf-column key="name" label="${escapeAttr(t('user'))}"></tf-column>
    </tf-table>
    <p data-account-error role="alert" hidden></p>
    </section>
    <section data-account-move></section>`;
  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.innerHTML = `<tf-button variant="primary" data-account-save>${escapeHtml(t('save'))}</tf-button>`;
  win.append(body, footer);
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);
  win.addEventListener('close-request', () => backdrop.remove());
  body.style.display = 'grid';
  body.style.gridTemplateColumns = 'minmax(0, 1fr)';
  body.style.gap = '16px';
  const accessPane = body.querySelector('[data-account-access]');
  const movePane = body.querySelector('[data-account-move]');
  accessPane.style.display = 'grid';
  accessPane.style.gap = '16px';
  movePane.style.display = 'none';
  body.querySelector('[data-account-tabs]').addEventListener('change', (event) => {
    const moving = event.detail?.value === 'move';
    accessPane.style.display = moving ? 'none' : 'grid';
    movePane.style.display = moving ? 'grid' : 'none';
    if (moving) footer.remove();
    else win.appendChild(footer);
  });
  let grants = access.grants || [];
  const error = body.querySelector('[data-account-error]');
  const userId = (user) => String(user.userId ?? user.user_id ?? user.id ?? '');
  const userName = (user) => String(user.displayName ?? user.display_name ?? user.username ?? user.email ?? userId(user));
  const paint = () => {
    const granted = new Set(grants.map((grant) => grant.user_id));
    const candidates = users.filter((user) => !granted.has(userId(user)))
      .map((user) => ({ value: userId(user), label: userName(user) }));
    const picker = body.querySelector('[data-account-user]');
    picker.setOptions(candidates.length ? candidates : [{ value: '', label: t('all_granted') }]);
    picker.toggleAttribute('disabled', !candidates.length);
    body.querySelector('[data-account-grant]').toggleAttribute('disabled', !candidates.length);
    const table = body.querySelector('[data-account-grants]');
    table.rowActions = (row) => {
      const button = document.createElement('tf-button');
      button.setAttribute('variant', 'ghost');
      button.setAttribute('size', 'sm');
      button.textContent = t('revoke');
      button.addEventListener('click', () => changeGrant(row.id, false, button));
      return button;
    };
    table.rows = grants.map((grant) => ({ id: grant.user_id,
      name: userName(users.find((user) => userId(user) === grant.user_id) || { user_id: grant.user_id }) }));
  };
  const changeGrant = async (id, canUse, button) => {
    if (!id) return;
    button.setAttribute('disabled', '');
    error.hidden = true;
    try {
      const response = await agentRequest(service, 'account.grants.set', { user_id: id, can_use: canUse });
      grants = response.grants || [];
      paint();
    } catch (err) {
      error.textContent = err.message;
      error.hidden = false;
    } finally {
      button.removeAttribute('disabled');
      paint();
    }
  };
  body.querySelector('[data-account-grant]').addEventListener('click', (event) =>
    changeGrant(body.querySelector('[data-account-user]').value, true, event.currentTarget));
  footer.querySelector('[data-account-save]').addEventListener('click', async (event) => {
    const button = event.currentTarget;
    button.setAttribute('disabled', '');
    error.hidden = true;
    try {
      await agentRequest(service, 'account.rename', { display_name: String(body.querySelector('[data-account-name]').value || '').trim() });
      toast(t('saved'), 'success');
      win.close();
    } catch (err) {
      error.textContent = err.message;
      error.hidden = false;
    } finally { button.removeAttribute('disabled'); }
  });
  paint();
  mountAccountMove(body.querySelector('[data-account-move]'), service, account);
}

function mountAccountMove(host, service, account) {
  const t = (key) => I18n.t(`agent_accounts.${key}`);
  const engine = service.engineId || service.engine_id;
  host.style.gap = '12px';
  host.style.gridTemplateColumns = 'minmax(0, 1fr)';
  host.style.overflowWrap = 'anywhere';
  if (!['codex', 'claude-code'].includes(engine) || !account.account_id) {
    host.innerHTML = `<h3>${escapeHtml(account.display_name)}</h3><p class="text-muted">${escapeHtml(t('move_unavailable'))}</p>`;
    return;
  }
  host.innerHTML = `<h3>${escapeHtml(account.display_name)}</h3>
    <p class="text-muted">${escapeHtml(t('move_hint'))}</p>
    <tf-select data-move-target label="${escapeAttr(t('move_target'))}" disabled></tf-select>
    <p data-move-status role="status">${escapeHtml(t('move_loading'))}</p>
    <p data-move-error role="alert" hidden></p>
    <div><tf-button data-move-start variant="outline" disabled>${escapeHtml(t('move_start'))}</tf-button>
      <tf-button data-move-refresh variant="ghost" icon="refresh">${escapeHtml(t('move_refresh'))}</tf-button></div>`;
  const picker = host.querySelector('[data-move-target]');
  const start = host.querySelector('[data-move-start]');
  const refresh = host.querySelector('[data-move-refresh]');
  const status = host.querySelector('[data-move-status]');
  const error = host.querySelector('[data-move-error]');
  let move = null;
  let canUse = account.can_use === true;
  let nodes = [];
  let timer;
  let busy = false;
  const nodeId = (node) => String(node.nodeId ?? node.node_id ?? node.id ?? '');
  const nodeLabel = (id) => {
    const hostname = nodes.find((node) => nodeId(node) === id)?.hostname;
    return hostname ? `${hostname} (${id.slice(0, 12)})` : id.slice(0, 12);
  };
  const source = String(service.nodeId || service.node_id);
  const phases = new Set(['none', 'source_frozen', 'source_retired', 'source_complete', 'target_staged', 'target_active']);
  const canStartMove = () => move?.phase === 'none' || (move?.phase === 'target_active' && canUse);
  const paint = () => {
    const known = phases.has(move?.phase);
    const fresh = canStartMove();
    const retry = !!move?.error && String(move?.phase).startsWith('source_') && move.phase !== 'source_complete';
    const target = fresh ? null : move?.target_node_id;
    picker.toggleAttribute('disabled', busy || !fresh || !nodes.length);
    start.toggleAttribute('disabled', busy || !known || (!fresh && !retry) || !(target || picker.value));
    start.textContent = t(retry ? 'move_retry' : 'move_start');
    refresh.toggleAttribute('disabled', busy);
    if (known) status.textContent = t(`move_${move.phase}`) + (target ? ` · ${nodeLabel(target)}` : '');
    else status.textContent = t('move_loading');
    if (fresh && !nodes.length) status.textContent = t('move_no_targets');
    if (move?.error) {
      error.textContent = `${t('move_failed')} ${move.error}`;
      error.hidden = false;
    }
  };
  const load = async () => {
    if (busy || !host.isConnected) return;
    clearTimeout(timer);
    busy = true;
    error.hidden = true;
    paint();
    try {
      const [state, available, access] = await Promise.all([
        agentRequest(service, 'account.move.status'),
        ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }),
        agentRequest(service, 'account.access'),
      ]);
      move = state;
      canUse = access.can_use === true;
      nodes = available.filter((node) => nodeId(node) !== source
        && ((node.isLocal ?? node.is_local) || node.source === 'trusted'));
      const fresh = canStartMove();
      const selected = fresh ? (picker.value === source ? '' : picker.value) : move.target_node_id || picker.value;
      const options = nodes.map((node) => ({ value: nodeId(node), label: nodeLabel(nodeId(node)) }));
      if (!fresh && move.target_node_id && !nodes.some((node) => nodeId(node) === move.target_node_id)) {
        options.push({ value: move.target_node_id, label: nodeLabel(move.target_node_id) });
      }
      if (fresh && options.length) options.unshift({ value: '', label: t('move_target'), disabled: true });
      picker.setOptions(options.length ? options : [{ value: '', label: t('move_no_targets') }], selected);
    } catch (err) {
      move = null;
      error.textContent = err.message;
      error.hidden = false;
    } finally {
      busy = false;
      paint();
      if (host.isConnected && move && !['none', 'source_complete', 'target_active'].includes(move.phase)) {
        timer = setTimeout(load, 5000);
      }
    }
  };
  picker.addEventListener('change', paint);
  refresh.addEventListener('click', load);
  start.addEventListener('click', async () => {
    if (busy) return;
    const target = canStartMove() ? picker.value : move?.target_node_id || picker.value;
    if (!target) return;
    clearTimeout(timer);
    busy = true;
    error.hidden = true;
    paint();
    try {
      move = await agentRequest(service, 'account.move', { target_node_id: target });
    } catch (err) {
      error.textContent = err.message;
      error.hidden = false;
    } finally {
      busy = false;
      paint();
      if (host.isConnected && move?.phase !== 'none') timer = setTimeout(load, 2000);
    }
  });
  host.closest('tf-window')?.addEventListener('close-request', () => clearTimeout(timer));
  load();
}
