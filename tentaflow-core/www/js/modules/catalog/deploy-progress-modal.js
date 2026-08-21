// =============================================================================
// File: modules/catalog/deploy-progress-modal.js
// Opis: Live progress modal dla deploymentu silnika. Subscribes via binary
//       protocol (deploymentLogStreamRequest) — replay log_tail z DB, potem
//       live chunki z runnera. Pokazuje pasek postępu, fazę i ogon build
//       outputu. Strumień jest odtwarzalny: każdy terminal frame który NIE jest
//       DeploymentStreamEnd (pad socketu, bezciałowy end serwera) oznacza
//       przerwanie, nie koniec deployu — modal resubskrybuje i odzyskuje
//       zgubione linie z replayu log_tail. Po prawdziwym StreamEnd emituje
//       toast i wypełnia status (sukces/błąd).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeHtml, toast } from '/js/utils.js';

// 2000 linii ≈ kilkaset kB docker buildu — dolny ogon, który przeglądarka
// przerysowuje bez zadławienia. Starsze linie zostają w `deployments.log_tail`.
const LOG_BUFFER_LIMIT = 2000;
// Watchdog pyta o status deploymentu w tym rytmie i po tym porównuje, czy
// serwer dopisał do log_tail nie dostarczając nam ani jednego chunka.
const WATCHDOG_INTERVAL_MS = 15_000;
// Backoff resubskrypcji (pad transportu, przerwany stream). Reset po każdym
// odebranym chunku i po każdym udanym reconnect socketu.
const RESUBSCRIBE_BASE_MS = 1_000;
const RESUBSCRIBE_MAX_MS = 15_000;
const RESUBSCRIBE_MAX_ATTEMPTS = 12;
// Piksele od dołu, w których traktujemy scroll jako "przyklejony do końca".
const STICKY_BOTTOM_PX = 24;

const TERMINAL_STATUSES = ['success', 'failed', 'interrupted', 'cancelled'];

export function openDeployProgressModal({ deployId, engineId, deployMethod, nodeId }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', `${I18n.t('deploy.progress_title')}: ${engineId}`);
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '560');
  win.setAttribute('width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = renderBodyInitial({ deployId, engineId, deployMethod });
  win.appendChild(body);

  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.style.cssText = 'display:flex;gap:8px;justify-content:flex-end;padding:8px;';
  footer.innerHTML = `
    <tf-button variant="ghost" data-background-btn>${escapeHtml(I18n.t('deploy.continue_background'))}</tf-button>
    <tf-button variant="ghost" data-close-btn>${escapeHtml(I18n.t('common.close'))}</tf-button>
  `;
  win.appendChild(footer);

  document.body.appendChild(win);

  let unsubscribe = null;
  let lifecycleOff = null;
  let watchdogTimer = null;
  let resubscribeTimer = null;
  let logLines = [];
  let finished = false;
  let disposed = false;
  // Ustawiane tylko na czas otwierania strumienia — watchdog nie wtrąca się
  // w trwającą (re)subskrypcję.
  let subscribing = false;
  let chipStatus = 'deploying';
  // Rośnie przy każdej (re)subskrypcji — callbacki starej subskrypcji, które
  // dobiegną po zamianie, są po tym rozpoznawane i ignorowane.
  let streamEpoch = 0;
  let resubscribeAttempt = 0;
  // Pierwszy chunk nowej subskrypcji zaczyna replay z log_tail, więc podmienia
  // bufor zamiast go dokleić — bez tego reconnect dublowałby cały log.
  let awaitingReplaySwap = false;
  let lastChunkAt = 0;
  let watchdogTailLen = -1;
  let renderScheduled = false;
  let stickToBottom = true;

  const logBox = () => body.querySelector('[data-log-box]');
  const progressBar = () => body.querySelector('[data-progress-bar]');
  const progressLabel = () => body.querySelector('[data-progress-label]');
  const phaseLabel = () => body.querySelector('[data-phase-label]');
  const statusChip = () => body.querySelector('[data-status-chip]');
  const errorBox = () => body.querySelector('[data-error-box]');

  logBox()?.addEventListener('scroll', () => {
    const box = logBox();
    if (!box) return;
    stickToBottom = box.scrollHeight - box.scrollTop - box.clientHeight < STICKY_BOTTOM_PX;
  });

  // ---------------------------------------------------------------- rendering

  function renderLog() {
    renderScheduled = false;
    const box = logBox();
    if (!box) return;
    box.innerHTML = logLines.join('\n');
    if (stickToBottom) box.scrollTop = box.scrollHeight;
  }

  // Burst docker buildu to setki linii na klatkę; jeden zapis textContent per
  // animation frame zamiast per linia trzyma okno responsywne przy 200 kB logu.
  function scheduleRender() {
    if (renderScheduled) return;
    renderScheduled = true;
    requestAnimationFrame(renderLog);
  }

  /**
   * `step` wyroznia linie niosace postep (kroki builda, fazy) — to naturalne
   * naglowki logu. Escape i klasa liczone raz przy dopisaniu, bo render sklada
   * caly bufor od nowa na kazdej klatce.
   */
  function appendLine(raw, { step = false } = {}) {
    if (raw == null) return;
    const cls = step ? 'deploy-log-line is-step' : 'deploy-log-line';
    for (const part of String(raw).split('\n')) {
      const cleaned = cleanLogLine(part);
      if (!cleaned) continue;
      logLines.push(`<span class="${cls}">${escapeHtml(cleaned)}</span>`);
    }
    if (logLines.length > LOG_BUFFER_LIMIT) {
      logLines.splice(0, logLines.length - LOG_BUFFER_LIMIT);
    }
    scheduleRender();
  }

  function applyProgress(pct) {
    const value = Math.max(0, Math.min(100, Number(pct) || 0));
    const bar = progressBar();
    const label = progressLabel();
    if (bar) bar.style.width = `${value}%`;
    if (label) label.textContent = `${value}%`;
  }

  function applyPhase(phase) {
    const el = phaseLabel();
    if (el) el.textContent = phase || '';
  }

  function applyStatus(status) {
    const chip = statusChip();
    if (!chip) return;
    chip.setAttribute('status', statusToChipVariant(status));
    if (status === 'deploying' || status === 'reconnecting') {
      chip.setAttribute('dot', '');
    } else {
      chip.removeAttribute('dot');
    }
    chip.textContent = I18n.t(`deploy.status_${status}`) || status;
    chipStatus = status;
  }

  function showError(message) {
    const box = errorBox();
    if (!box) return;
    box.textContent = message || I18n.t('deploy.err_generic');
    box.hidden = false;
  }

  // ------------------------------------------------------------- stream logic

  function applyDeploymentSummary(summary) {
    if (!summary) return;
    const status = summary.status || 'deploying';
    applyStatus(status);
    applyPhase(summary.phase || '');
    applyProgress(summary.progressPct);
    if (summary.logTail && logLines.length === 0) {
      appendLine(summary.logTail);
    }
    if (TERMINAL_STATUSES.includes(status) && status !== 'success') {
      showError(summary.errorMessage);
    }
  }

  function onChunk(chunk) {
    if (!chunk || chunk.variant !== 'DeploymentStreamChunk') return;
    if (chunk.deployId && chunk.deployId !== deployId) return;
    lastChunkAt = Date.now();
    resubscribeAttempt = 0;
    if (awaitingReplaySwap) {
      awaitingReplaySwap = false;
      logLines = [];
    }
    // Backend zapisuje `line` kazdego kinda do log_tail dokladnie raz, a krok
    // builda leci ALBO jako `progress`, ALBO jako `info` (nigdy oba, patrz
    // services/deploy/docker.rs). Dlatego front renderuje tresc doslownie i nic
    // nie dokleja: inaczej replay z log_tail (same `log`) rozjechalby sie z
    // widokiem live. Faza bez tresci nie zostawia sladu w logu.
    if (chunk.kind === 'progress') {
      applyProgress(chunk.progressPct);
      if (chunk.phase) applyPhase(chunk.phase);
      appendLine(chunk.line, { step: true });
    } else if (chunk.kind === 'phase') {
      applyPhase(chunk.phase || chunk.line);
      applyProgress(chunk.progressPct);
      appendLine(chunk.line, { step: true });
    } else {
      appendLine(chunk.line);
    }
    // Chunk po przerwie = strumień znowu żyje; status z DB odświeża watchdog.
    if (chipStatus === 'reconnecting') applyStatus('deploying');
  }

  /**
   * Każdy terminal frame subskrypcji. Tylko DeploymentStreamEnd jest werdyktem
   * deployu; `StreamClosed` (syntetyczny end z BinaryWsClient po padzie
   * transportu) i bezciałowy end serwera (zamknięty log_bus) znaczą "stream
   * ucięty" — deploy może dalej trwać, więc wracamy po niego resubskrypcją.
   */
  function onEnd(endBody) {
    unsubscribe = null;
    if (endBody && endBody.variant === 'DeploymentStreamEnd') {
      finish({
        status: endBody.finalStatus || 'ended',
        errorMessage: endBody.errorMessage,
        durationMs: endBody.durationMs,
      });
      return;
    }
    scheduleResubscribe();
  }

  function onStreamError(err) {
    unsubscribe = null;
    appendLine(`[stream] ${err?.message || I18n.t('deploy.stream_lost')}`);
    scheduleResubscribe();
  }

  function finish({ status, errorMessage, durationMs }) {
    if (finished) return;
    finished = true;
    stopTimers();
    dropSubscription();
    applyProgress(100);
    applyStatus(status || 'ended');
    if (status === 'failed' || status === 'interrupted' || status === 'cancelled') {
      showError(errorMessage);
      toast(`${I18n.t('deploy.failed')}: ${errorMessage || ''}`, 'error');
      return;
    }
    if (status === 'success') {
      appendLine(`— ${I18n.t('deploy.success')} (${durationMs || 0} ms)`, { step: true });
      toast(I18n.t('deploy.success'), 'success');
      if (engineId === 'codex' || engineId === 'claude-code') {
        import('/js/modules/coding-agent.js').then(async (module) => {
          const service = await module.findDeployedAgent(engineId, nodeId);
          await module.openAgentLogin(service);
        }).catch((error) => toast(error.message || String(error), 'error'));
      }
    }
  }

  function dropSubscription() {
    if (!unsubscribe) return;
    try {
      unsubscribe();
    } catch (_) {
      /* stream już zamknięty po stronie serwera */
    }
    unsubscribe = null;
  }

  function scheduleResubscribe() {
    if (finished || disposed || resubscribeTimer) return;
    if (resubscribeAttempt >= RESUBSCRIBE_MAX_ATTEMPTS) {
      applyStatus('ended');
      showError(I18n.t('deploy.stream_lost'));
      return;
    }
    const delay = Math.min(
      RESUBSCRIBE_BASE_MS * 2 ** resubscribeAttempt,
      RESUBSCRIBE_MAX_MS,
    );
    resubscribeAttempt += 1;
    applyStatus('reconnecting');
    resubscribeTimer = setTimeout(() => {
      resubscribeTimer = null;
      openStream({ resumed: true });
    }, delay);
  }

  /**
   * Otwiera (lub odtwarza) strumień. Zawsze zaczyna od statusu z DB: jeśli
   * deployment jest już terminalny, kończymy bez subskrypcji — dzięki temu
   * StreamEnd przespany w czasie rozłączenia nie ginie. Inaczej subskrybujemy
   * z `replayTail`, co dociąga wszystko, co ominęliśmy.
   */
  async function openStream({ resumed = false } = {}) {
    if (finished || disposed) return;
    // Nie ma blokady na "już otwieram": nowsze wywołanie ma wygrać, inaczej
    // reconnect przespałby się na zawieszonym requeście starszego.
    subscribing = true;
    const epoch = ++streamEpoch;
    dropSubscription();
    try {
      const status = await ApiBinary.one('deploymentStatusRequest', { deployId });
      if (epoch !== streamEpoch || finished || disposed) return;
      const summary = status?.deployment;
      applyDeploymentSummary(summary);
      if (summary && TERMINAL_STATUSES.includes(summary.status)) {
        finish({ status: summary.status, errorMessage: summary.errorMessage, durationMs: 0 });
        return;
      }
      watchdogTailLen = summary?.logTail ? String(summary.logTail).length : 0;
      awaitingReplaySwap = true;
      const unsub = await ApiBinary.subscribe(
        'deploymentLogStreamRequest',
        { deployId, replayTail: true },
        {
          onChunk: (chunkBody) => { if (epoch === streamEpoch) onChunk(chunkBody); },
          onEnd: (endBody) => { if (epoch === streamEpoch) onEnd(endBody); },
          onError: (err) => { if (epoch === streamEpoch) onStreamError(err); },
        },
      );
      if (epoch !== streamEpoch || finished || disposed) {
        try { unsub(); } catch (_) { /* ignore */ }
        return;
      }
      unsubscribe = unsub;
      lastChunkAt = Date.now();
      applyStatus(summary?.status || 'deploying');
      if (resumed) appendLine(`— ${I18n.t('deploy.stream_resumed')}`, { step: true });
    } catch (err) {
      if (epoch === streamEpoch && !finished && !disposed) {
        awaitingReplaySwap = false;
        appendLine(`[stream] ${err?.message || ''}`);
        scheduleResubscribe();
      }
    } finally {
      if (epoch === streamEpoch) subscribing = false;
    }
  }

  /**
   * Wykrywa strumień, który umarł bez terminal frame'u (serwer zrywa handler
   * przy backpressure kanału subskrypcji). Sygnał jest jednoznaczny: log_tail
   * w DB przyrósł, a do nas od ostatniego ticku nie dotarł ani jeden chunk.
   */
  async function watchdogTick() {
    if (finished || disposed || subscribing) return;
    let summary = null;
    try {
      const status = await ApiBinary.one('deploymentStatusRequest', { deployId });
      summary = status?.deployment;
    } catch (_) {
      return; // transport w dołku — lifecycle 'open' wznowi
    }
    if (finished || disposed || !summary) return;
    applyPhase(summary.phase || '');
    applyProgress(summary.progressPct);
    if (TERMINAL_STATUSES.includes(summary.status)) {
      finish({ status: summary.status, errorMessage: summary.errorMessage, durationMs: 0 });
      return;
    }
    const tailLen = summary.logTail ? String(summary.logTail).length : 0;
    const grew = watchdogTailLen >= 0 && tailLen > watchdogTailLen;
    watchdogTailLen = tailLen;
    if (grew && Date.now() - lastChunkAt > WATCHDOG_INTERVAL_MS) {
      resubscribeAttempt = 0;
      openStream({ resumed: true });
    }
  }

  function stopTimers() {
    if (watchdogTimer) {
      clearInterval(watchdogTimer);
      watchdogTimer = null;
    }
    if (resubscribeTimer) {
      clearTimeout(resubscribeTimer);
      resubscribeTimer = null;
    }
  }

  // Serwer trzyma subskrypcje per-socket, więc po reconnekcie stare
  // correlation_id jest martwe — bez tego okno zamierało na resztę deployu.
  lifecycleOff = ApiBinary.onLifecycle((ev) => {
    if (finished || disposed) return;
    if (ev.type === 'disconnected' || ev.type === 'close') {
      dropSubscription();
      applyStatus('reconnecting');
    } else if (ev.type === 'open') {
      resubscribeAttempt = 0;
      if (resubscribeTimer) {
        clearTimeout(resubscribeTimer);
        resubscribeTimer = null;
      }
      openStream({ resumed: true });
    }
  });

  watchdogTimer = setInterval(watchdogTick, WATCHDOG_INTERVAL_MS);
  openStream();

  const dispose = () => {
    if (disposed) return;
    disposed = true;
    stopTimers();
    streamEpoch += 1;
    dropSubscription();
    if (lifecycleOff) {
      lifecycleOff();
      lifecycleOff = null;
    }
  };

  const closeModal = () => {
    dispose();
    win.close(true);
  };
  footer.querySelector('[data-close-btn]')?.addEventListener('click', closeModal);
  // "Kontynuuj w tle" — deploy leci dalej na serwerze, znika tylko podgląd.
  footer.querySelector('[data-background-btn]')?.addEventListener('click', closeModal);
  // tf-window emituje 'close-request' zanim usunie sam siebie (przycisk X, ESC).
  win.addEventListener('close-request', dispose);
}

/**
 * Docker/BuildKit sypie sekwencjami SGR i przerysowuje paski postępu przez CR.
 * W <pre> jedno i drugie jest śmieciem, więc zostawiamy ostatni stan linii bez
 * kodów sterujących.
 */
function cleanLogLine(raw) {
  const repaints = String(raw).split('\r').filter((part) => part !== '');
  const text = repaints.length > 0 ? repaints[repaints.length - 1] : '';
  const stripped = text
    // OSC: ESC ] ... (BEL | ESC \)
    .replace(/\u001B\][\s\S]*?(?:\u0007|\u001B\\)/g, '')
    // CSI: ESC [ params intermediates final
    .replace(/\u001B\[[0-?]*[ -/]*[@-~]/g, '')
    // pozostale dwuznakowe escape'y (ESC ( B, ESC = , ...)
    .replace(/\u001B[@-Z\\-_]/g, '')
    // gole znaki sterujace (bez tabulacji)
    .replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, '')
    // BuildKit right-aligns the step duration with a long run of spaces. The log
    // box renders with `white-space: pre-wrap`, so that run survives the wrap and
    // tears a blank hole across the line. Leading indentation (tracebacks) stays.
    .replace(/(\S) {3,}(?=\S)/g, '$1 ');
  return stripped.trimEnd();
}

function renderBodyInitial({ deployId, engineId, deployMethod }) {
  return `
    <div class="deploy-progress">
      <div class="deploy-progress-head">
        <div>
          <div class="deploy-progress-engine">${escapeHtml(engineId)}</div>
          <div class="deploy-progress-meta">
            <span>${escapeHtml(deployMethod)}</span>
            <span>·</span>
            <code>${escapeHtml(deployId)}</code>
          </div>
        </div>
        <tf-chip data-status-chip status="warn" dot>${escapeHtml(I18n.t('deploy.status_deploying'))}</tf-chip>
      </div>
      <div class="deploy-progress-phase">
        <span data-phase-label>—</span>
        <span data-progress-label>0%</span>
      </div>
      <div class="deploy-progress-track">
        <div class="deploy-progress-bar" data-progress-bar style="width:0%"></div>
      </div>
      <pre class="deploy-log-box" data-log-box></pre>
      <div class="deploy-progress-error" data-error-box hidden></div>
    </div>
  `;
}

function statusToChipVariant(status) {
  switch (status) {
    case 'success':
      return 'success';
    case 'failed':
    case 'interrupted':
    case 'cancelled':
      return 'danger';
    default:
      return 'warn';
  }
}

// Eksport dla testów jednostkowych — czyszczenie linii jest czystą funkcją i
// jedyną częścią modalu, którą da się sprawdzić bez DOM-u i transportu.
export { cleanLogLine };
