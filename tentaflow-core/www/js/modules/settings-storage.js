// =============================================================================
// File: settings-storage.js
// Purpose: Ustawienia → Magazyn danych. Lista katalogów danych węzła (ścieżki +
//          rozmiary + dysk), picker katalogu z drzewkiem i tworzeniem folderów,
//          pytanie o migrację i migracja na żywo bez restartu aplikacji.
//          Wołane z settings.js jako zakładka 'storage'.
// =============================================================================

import { byId, escapeHtml, escapeAttr, toast, formatBytes } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { TfWindow } from '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';

const CAT_ICON = {
  models_dir: 'model',
  data_dir: 'database',
  cache_dir: 'zap',
  containers_dir: 'docker',
  sync_dir: 'refresh',
  blobs_dir: 'record',
  recordings_dir: 'mic',
  addons_data_dir: 'puzzle',
  keys_dir: 'key',
  bus_dir: 'send',
};

const CAT_LABEL = {
  models_dir: 'Modele',
  data_dir: 'Baza danych',
  cache_dir: 'Cache',
  containers_dir: 'Kontenery',
  sync_dir: 'Sync Ledger',
  blobs_dir: 'Nagrania i blob-y',
  recordings_dir: 'Nagrania kamer',
  addons_data_dir: 'Dane addonów',
  keys_dir: 'Klucze',
  bus_dir: 'TentaBus',
};

const CAT_DESC = {
  models_dir: 'GGUF, cache HuggingFace, modele vision / audio / image-gen — współdzielone z kontenerami',
  data_dir: 'Główna baza platformy, paczki addonów, dokumenty prawne',
  cache_dir: 'Środowiska Python usług natywnych, cache vLLM, artefakty ML',
  containers_dir: 'Konteksty budowania obrazów Docker, definicje usług',
  sync_dir: 'Dziennik operacji synchronizacji między węzłami (Fjall)',
  blobs_dir: 'Audio z flow, nagrania rozmów, migawki',
  recordings_dir: 'Nagrania kamer (migawki i segmenty)',
  addons_data_dir: 'Bazy addonów, indeksy wektorowe, grafy wiedzy, dokumenty',
  keys_dir: 'Klucze HMAC podpisanych URL-i, klucz główny szyfrowania',
  bus_dir: 'Segmenty logu zdarzeń szyny TentaBus (partycje, indeksy offsetów, dedup)',
};

function icon(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

let overviewCache = null;

export async function loadStorageOverview() {
  overviewCache = await ApiBinary.one('storageOverviewRequest');
  return overviewCache;
}

// =============================================================================
// Render
// =============================================================================

export function renderStorageTab(overview) {
  ensureStyles();
  if (!overview) {
    return `<div class="card"><div class="card-body"><div class="tf-storage-loading">Ładowanie magazynu danych…</div></div></div>`;
  }
  const cats = Array.isArray(overview.categories) ? overview.categories : [];
  const total = Number(overview.diskTotalBytes || 0);
  const avail = Number(overview.diskAvailableBytes || 0);
  const used = Math.max(0, total - avail);
  const usedPct = total > 0 ? Math.min(100, Math.round((used / total) * 100)) : 0;
  const tfTotal = cats.reduce((s, c) => s + Number(c.sizeBytes || 0), 0);

  const rows = cats.map((c) => renderCategoryRow(c)).join('');

  return `
    <div class="card">
      <div class="card-header">
        <h3>Magazyn danych</h3>
        <tf-button variant="ghost" size="sm" icon="refresh" id="storage-refresh">Odśwież</tf-button>
      </div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 16px;">
          Gdzie TentaFlow przechowuje poszczególne rodzaje danych. Zmiana katalogu może obejmować
          migrację istniejących danych — usługi zależne zostaną na chwilę wstrzymane i wznowione
          automatycznie, bez restartu aplikacji. Ustawienia są per-węzeł (nie synchronizują się w meshu).
        </p>

        <div class="tf-disk-summary">
          <div class="tf-disk-ico">${icon('cylinder')}</div>
          <div class="tf-disk-meta">
            <div class="tf-disk-l1">Dysk współdzielonego rootu</div>
            <div class="tf-disk-l2"><code>${escapeHtml(overview.root || '')}</code></div>
          </div>
          <div class="tf-disk-bar"><i style="width:${usedPct}%"></i></div>
          <div class="tf-disk-nums">
            TentaFlow: <strong>${escapeHtml(formatBytes(tfTotal))}</strong> ·
            wolne: <strong>${escapeHtml(formatBytes(avail))}</strong> / ${escapeHtml(formatBytes(total))}
          </div>
        </div>

        <div class="tf-cat-list" id="tf-cat-list">
          ${rows}
        </div>
      </div>
    </div>
  `;
}

function renderCategoryRow(c) {
  const key = c.key;
  const label = CAT_LABEL[key] || key;
  const desc = CAT_DESC[key] || '';
  const ico = CAT_ICON[key] || 'folder';
  const size = formatBytes(Number(c.sizeBytes || 0));
  const pending = c.pendingPath || c.pending_path;
  const overridden = c.overridden;
  const liveTag = c.liveMigratable === false
    ? `<span class="tf-cat-badge warn" title="Zmiana wymaga restartu aplikacji">restart</span>`
    : '';
  const pendingRow = pending
    ? `<div class="tf-cat-pending">${icon('clock')} Zmiana zaplanowana na następny start: <code>${escapeHtml(pending)}</code></div>`
    : '';
  return `
    <div class="tf-cat-row${overridden ? ' overridden' : ''}" data-cat="${escapeAttr(key)}">
      <div class="tf-cat-ico">${icon(ico)}</div>
      <div class="tf-cat-info">
        <div class="tf-cat-name">
          ${escapeHtml(label)}
          <span class="tf-cat-size">${escapeHtml(size)}</span>
          ${liveTag}
        </div>
        <div class="tf-cat-path"><code>${escapeHtml(c.path || '')}</code></div>
        <div class="tf-cat-desc">${escapeHtml(desc)}</div>
        ${pendingRow}
      </div>
      <div class="tf-cat-actions">
        <tf-button variant="secondary" size="sm" icon="folder" data-change="${escapeAttr(key)}">Zmień katalog</tf-button>
      </div>
    </div>
  `;
}

export function bindStorageTab(reload) {
  byId('storage-refresh')?.addEventListener('click', () => reload?.());
  const list = byId('tf-cat-list');
  if (!list) return;
  list.querySelectorAll('[data-change]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const key = btn.getAttribute('data-change');
      const cat = (overviewCache?.categories || []).find((c) => c.key === key);
      if (cat) openChangeFlow(cat, reload);
    });
  });
}

// =============================================================================
// Change flow: picker → confirm → migrate
// =============================================================================

async function openChangeFlow(cat, reload) {
  const newPath = await openDirectoryPicker(cat);
  if (!newPath) return;
  if (newPath === cat.path) {
    toast('To ta sama lokalizacja co obecna.', 'warning');
    return;
  }
  await openMigrateConfirm(cat, newPath, reload);
}

// ---------------------------------------------------------------------------
// Directory picker (tree + create folder)
// ---------------------------------------------------------------------------

function openDirectoryPicker(cat) {
  return new Promise((resolve) => {
    ensureStyles();
    const label = CAT_LABEL[cat.key] || cat.key;
    const win = document.createElement('tf-window');
    win.setAttribute('title', `Wybierz katalog — ${label}`);
    win.setAttribute('draggable', '');
    win.setAttribute('min-width', '520');
    win.setAttribute('width', '560');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');

    const body = document.createElement('div');
    body.slot = 'body';
    body.innerHTML = `
      <div class="tf-picker">
        <div class="tf-tree" id="tf-picker-tree"><div class="tf-tree-loading">Wczytywanie…</div></div>
        <div class="tf-picked-bar">
          <span class="tf-picked-lab">Wybrano:</span>
          <span class="tf-picked-val" id="tf-picked-val">—</span>
          <span class="tf-picked-free" id="tf-picked-free"></span>
        </div>
      </div>
    `;
    win.appendChild(body);

    const footer = document.createElement('div');
    footer.slot = 'footer';
    footer.style.cssText = 'display:flex;gap:8px;align-items:center;width:100%;';
    footer.innerHTML = `
      <tf-button variant="ghost" icon="folder" id="tf-new-dir">Nowy katalog</tf-button>
      <span style="flex:1"></span>
      <tf-button variant="ghost" id="tf-pick-cancel">Anuluj</tf-button>
      <tf-button variant="primary" icon="check" id="tf-pick-ok" disabled>Wybierz katalog</tf-button>
    `;
    win.appendChild(footer);
    document.body.appendChild(win);

    const treeEl = body.querySelector('#tf-picker-tree');
    const pickedValEl = body.querySelector('#tf-picked-val');
    const pickedFreeEl = body.querySelector('#tf-picked-free');
    const okBtn = footer.querySelector('#tf-pick-ok');
    let selected = null;
    let settled = false;

    const finish = (value) => {
      if (settled) return;
      settled = true;
      if (win.parentNode) win.parentNode.removeChild(win);
      resolve(value);
    };

    function selectPath(path, freeBytes) {
      selected = path;
      pickedValEl.textContent = path;
      pickedFreeEl.textContent = Number.isFinite(freeBytes)
        ? `wolne: ${formatBytes(freeBytes)}`
        : '';
      okBtn.removeAttribute('disabled');
    }

    async function loadChildren(path, container, depth) {
      try {
        const resp = await ApiBinary.one('storageBrowseRequest', { path });
        container.innerHTML = '';
        const entries = resp.entries || [];
        if (entries.length === 0) {
          const empty = document.createElement('div');
          empty.className = 'tf-tree-empty';
          empty.textContent = '(brak podkatalogów)';
          container.appendChild(empty);
        }
        for (const e of entries) {
          container.appendChild(buildNode(e, depth));
        }
      } catch (err) {
        container.innerHTML = `<div class="tf-tree-empty err">${escapeHtml(err.message || 'błąd')}</div>`;
      }
    }

    function buildNode(entry, depth) {
      const node = document.createElement('div');
      node.className = 'tf-node';
      const row = document.createElement('div');
      row.className = 'tf-node-row';
      row.style.paddingLeft = `${8 + depth * 16}px`;
      row.innerHTML = `
        <span class="tf-chev${entry.hasChildren ? '' : ' leaf'}">${icon('chevron-right')}</span>
        <span class="tf-fico">${icon('folder')}</span>
        <span class="tf-fname">${escapeHtml(entry.name)}</span>
      `;
      const kids = document.createElement('div');
      kids.className = 'tf-node-kids';
      let loaded = false;
      let open = false;

      row.addEventListener('click', async () => {
        // Select
        treeEl.querySelectorAll('.tf-node-row.selected').forEach((r) => r.classList.remove('selected'));
        row.classList.add('selected');
        selectPath(entry.path, undefined);
        // Expand/collapse
        if (entry.hasChildren) {
          open = !open;
          node.classList.toggle('open', open);
          if (open && !loaded) {
            loaded = true;
            kids.innerHTML = `<div class="tf-tree-loading" style="padding-left:${8 + (depth + 1) * 16}px">…</div>`;
            await loadChildren(entry.path, kids, depth + 1);
          }
        }
      });
      node.appendChild(row);
      node.appendChild(kids);
      return node;
    }

    // Root: browse parent of the current category path (so the user starts near
    // the current location), fallback to "/".
    const startPath = parentOf(cat.path) || '/';
    (async () => {
      treeEl.innerHTML = '';
      await loadChildren(startPath, treeEl, 0);
    })();

    footer.querySelector('#tf-pick-cancel').addEventListener('click', () => finish(null));
    okBtn.addEventListener('click', () => finish(selected));
    win.addEventListener('close', () => finish(null));

    footer.querySelector('#tf-new-dir').addEventListener('click', async () => {
      if (!selected) {
        toast('Najpierw zaznacz katalog nadrzędny w drzewku.', 'warning');
        return;
      }
      const name = await promptFolderName();
      if (!name) return;
      try {
        const resp = await ApiBinary.one('storageCreateDirRequest', { parent: selected, name });
        toast(`Utworzono katalog ${name}`, 'success');
        // Rebuild tree from start to surface the new dir, then select it.
        treeEl.innerHTML = '';
        await loadChildren(startPath, treeEl, 0);
        selectPath(resp.path, undefined);
      } catch (err) {
        toast(err.message, 'error');
      }
    });
  });
}

function promptFolderName() {
  return new Promise((resolve) => {
    const win = document.createElement('tf-window');
    win.setAttribute('title', 'Nowy katalog');
    win.setAttribute('min-width', '360');
    win.setAttribute('width', '400');
    win.setAttribute('modal', '');
    const body = document.createElement('div');
    body.slot = 'body';
    body.innerHTML = `<tf-input id="tf-newdir-name" label="Nazwa katalogu" placeholder="np. tentaflow-models"></tf-input>`;
    win.appendChild(body);
    const footer = document.createElement('div');
    footer.slot = 'footer';
    footer.style.cssText = 'display:flex;gap:8px;justify-content:flex-end;width:100%;';
    footer.innerHTML = `
      <tf-button variant="ghost" id="tf-newdir-cancel">Anuluj</tf-button>
      <tf-button variant="primary" icon="check" id="tf-newdir-ok">Utwórz</tf-button>
    `;
    win.appendChild(footer);
    document.body.appendChild(win);
    let settled = false;
    const finish = (v) => {
      if (settled) return;
      settled = true;
      if (win.parentNode) win.parentNode.removeChild(win);
      resolve(v);
    };
    const input = body.querySelector('#tf-newdir-name');
    setTimeout(() => input?.focus?.(), 50);
    footer.querySelector('#tf-newdir-cancel').addEventListener('click', () => finish(null));
    footer.querySelector('#tf-newdir-ok').addEventListener('click', () => finish((input?.value || '').trim() || null));
    win.addEventListener('close', () => finish(null));
  });
}

// ---------------------------------------------------------------------------
// Migration confirm + progress
// ---------------------------------------------------------------------------

function openMigrateConfirm(cat, newPath, reload) {
  return new Promise((resolve) => {
    ensureStyles();
    const label = CAT_LABEL[cat.key] || cat.key;
    const size = formatBytes(Number(cat.sizeBytes || 0));
    const restartOnly = cat.liveMigratable === false;
    const win = document.createElement('tf-window');
    win.setAttribute('title', 'Przenieść istniejące dane?');
    win.setAttribute('min-width', '480');
    win.setAttribute('width', '520');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');

    const restartNote = restartOnly
      ? `<div class="tf-mig-warn">${icon('info')} <div>Ta kategoria (<strong>${escapeHtml(label)}</strong>) trzyma otwarte pliki przez cały czas działania. Przeniesienie wykona się przy <strong>następnym starcie</strong> aplikacji — aplikacja nie zostanie zrestartowana teraz.</div></div>`
      : `<div class="tf-mig-warn">${icon('info')} <div>Usługi zależne zostaną wstrzymane na czas przenoszenia i wznowione automatycznie — <strong>bez restartu aplikacji</strong>.</div></div>`;

    const body = document.createElement('div');
    body.slot = 'body';
    body.innerHTML = `
      <div class="tf-mig-ask">
        <p class="form-hint" style="margin:0 0 14px;">
          W obecnej lokalizacji znajduje się <strong>${escapeHtml(size)}</strong> danych.
          Możesz je przenieść do nowego katalogu (fizyczne <strong>przeniesienie</strong>, nie kopiowanie),
          albo tylko przełączyć ścieżkę i zostawić dane w starej lokalizacji.
        </p>
        <div class="tf-path-diff">
          <div class="row old"><span class="k">Z</span><code>${escapeHtml(cat.path || '')}</code></div>
          <div class="mid">${icon('arrow')} przeniesienie danych</div>
          <div class="row new"><span class="k">Do</span><code>${escapeHtml(newPath)}</code></div>
        </div>
        ${restartNote}
      </div>
    `;
    win.appendChild(body);

    const footer = document.createElement('div');
    footer.slot = 'footer';
    footer.style.cssText = 'display:flex;gap:8px;align-items:center;width:100%;';
    footer.innerHTML = `
      <tf-button variant="ghost" id="tf-mig-cancel">Anuluj</tf-button>
      <span style="flex:1"></span>
      <tf-button variant="ghost" id="tf-mig-noswitch">Przełącz bez przenoszenia</tf-button>
      <tf-button variant="primary" icon="transform" id="tf-mig-move">Przenieś dane</tf-button>
    `;
    win.appendChild(footer);
    document.body.appendChild(win);

    let settled = false;
    const close = () => {
      if (settled) return;
      settled = true;
      if (win.parentNode) win.parentNode.removeChild(win);
      resolve();
    };

    const submit = async (moveData) => {
      try {
        const resp = await ApiBinary.one('storageMigrateRequest', {
          key: cat.key,
          newPath,
          moveData,
        });
        close();
        if (resp.mode === 'live' && resp.jobId) {
          openProgressModal(cat, newPath, resp.jobId, reload);
        } else if (resp.mode === 'boot') {
          toast('Przeniesienie zaplanowane na następny start aplikacji.', 'success');
          reload?.();
        } else {
          toast('Ścieżka przełączona.', 'success');
          reload?.();
        }
      } catch (err) {
        toast(err.message, 'error');
      }
    };

    footer.querySelector('#tf-mig-cancel').addEventListener('click', close);
    footer.querySelector('#tf-mig-noswitch').addEventListener('click', () => submit(false));
    footer.querySelector('#tf-mig-move').addEventListener('click', () => submit(true));
    win.addEventListener('close', close);
  });
}

const MIG_STEPS = [
  { phase: 'pause', title: 'Wstrzymywanie usług zależnych' },
  { phase: 'move', title: 'Przenoszenie danych' },
  { phase: 'switch', title: 'Przełączanie ścieżki i weryfikacja' },
  { phase: 'resume', title: 'Wznawianie usług' },
];

function openProgressModal(cat, newPath, jobId, reload) {
  ensureStyles();
  const label = CAT_LABEL[cat.key] || cat.key;
  const win = document.createElement('tf-window');
  win.setAttribute('title', `Migracja danych — ${label}`);
  win.setAttribute('min-width', '480');
  win.setAttribute('width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <div class="tf-mig-steps" id="tf-mig-steps">
      ${MIG_STEPS.map((s, i) => `
        <div class="tf-mig-step" data-phase="${s.phase}" data-idx="${i}">
          <div class="tf-mig-dot">${i + 1}</div>
          <div class="tf-mig-binfo">
            <div class="tf-mig-title">${escapeHtml(s.title)}</div>
            <div class="tf-mig-sub" data-sub></div>
            ${s.phase === 'move' ? `<div class="tf-mig-prog" data-prog hidden><i></i></div><div class="tf-mig-progmeta" data-progmeta hidden></div>` : ''}
          </div>
        </div>
      `).join('')}
    </div>
    <div class="tf-mig-note">${icon('info')} Aplikacja działa dalej — migracja odbywa się w tle, bez restartu. Okno można zamknąć; migracja dokończy się w tle.</div>
    <div class="tf-mig-log" id="tf-mig-log" hidden></div>
  `;
  win.appendChild(body);

  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.style.cssText = 'display:flex;gap:8px;justify-content:flex-end;width:100%;';
  footer.innerHTML = `<tf-button variant="ghost" id="tf-mig-hide">Ukryj — kontynuuj w tle</tf-button>`;
  win.appendChild(footer);
  document.body.appendChild(win);

  const stepsEl = body.querySelector('#tf-mig-steps');
  const logEl = body.querySelector('#tf-mig-log');
  let unsubscribe = null;

  function stepEl(phase) {
    return stepsEl.querySelector(`.tf-mig-step[data-phase="${phase}"]`);
  }
  function markActive(phase) {
    const target = stepEl(phase);
    if (!target) return;
    const targetIdx = Number(target.getAttribute('data-idx'));
    stepsEl.querySelectorAll('.tf-mig-step').forEach((el) => {
      const idx = Number(el.getAttribute('data-idx'));
      el.classList.toggle('done', idx < targetIdx);
      el.classList.toggle('active', idx === targetIdx);
      const dot = el.querySelector('.tf-mig-dot');
      if (idx < targetIdx) dot.innerHTML = `<svg class="icon"><use href="#i-check"/></svg>`;
      else dot.textContent = String(idx + 1);
    });
  }
  function setSub(phase, text) {
    const el = stepEl(phase)?.querySelector('[data-sub]');
    if (el) el.textContent = text;
  }
  function setProgress(pct, detail) {
    const step = stepEl('move');
    if (!step) return;
    const bar = step.querySelector('[data-prog]');
    const meta = step.querySelector('[data-progmeta]');
    if (bar) {
      bar.hidden = false;
      bar.querySelector('i').style.width = `${Math.max(0, Math.min(100, pct))}%`;
    }
    if (meta) {
      meta.hidden = false;
      meta.textContent = detail ? `${pct}% · ${detail}` : `${pct}%`;
    }
  }
  function appendLog(line) {
    if (!line) return;
    logEl.hidden = false;
    const div = document.createElement('div');
    div.textContent = line;
    logEl.appendChild(div);
    logEl.scrollTop = logEl.scrollHeight;
  }

  function onChunk(b) {
    if (!b || b.variant !== 'DeploymentStreamChunk') return;
    if (b.kind === 'phase') {
      markActive(b.phase);
      appendLog(`— ${b.line || b.phase}`);
    } else if (b.kind === 'progress') {
      if (b.phase) markActive(b.phase);
      setProgress(Number(b.progressPct || 0), b.line);
    } else if (b.kind === 'log') {
      appendLog(b.line);
    }
  }

  function onEnd(b) {
    stepsEl.querySelectorAll('.tf-mig-step').forEach((el) => {
      el.classList.remove('active');
      el.classList.add('done');
      const dot = el.querySelector('.tf-mig-dot');
      dot.innerHTML = `<svg class="icon"><use href="#i-check"/></svg>`;
    });
    setProgress(100, 'gotowe');
    const ok = !b || b.finalStatus === 'success';
    if (ok) {
      win.setAttribute('title', 'Migracja zakończona');
      toast('Dane przeniesione — usługi wznowione.', 'success');
    } else {
      toast(`Migracja nieudana: ${b.errorMessage || ''}`, 'error');
    }
    reload?.();
  }

  (async () => {
    try {
      unsubscribe = await ApiBinary.subscribe(
        'deploymentLogStreamRequest',
        { deployId: jobId, replayTail: true },
        { onChunk, onEnd, onError: (e) => toast(`Migracja: ${e?.message || 'błąd streamu'}`, 'error') },
      );
    } catch (err) {
      appendLog(`[stream error] ${err?.message || ''}`);
    }
  })();

  const closeModal = () => {
    if (unsubscribe) {
      try { unsubscribe(); } catch (_) {}
      unsubscribe = null;
    }
    if (win.parentNode) win.parentNode.removeChild(win);
  };
  footer.querySelector('#tf-mig-hide').addEventListener('click', closeModal);
  win.addEventListener('close', closeModal);
}

// =============================================================================
// Helpers
// =============================================================================

function parentOf(path) {
  if (!path) return null;
  const trimmed = path.replace(/\/+$/, '');
  const idx = trimmed.lastIndexOf('/');
  if (idx <= 0) return '/';
  return trimmed.slice(0, idx);
}

function ensureStyles() {
  if (byId('tf-storage-styles')) return;
  const style = document.createElement('style');
  style.id = 'tf-storage-styles';
  style.textContent = STORAGE_CSS;
  document.head.appendChild(style);
}

const STORAGE_CSS = `
.tf-disk-summary { display:flex; align-items:center; gap:16px; background:var(--panel,var(--bg-2)); border:1px solid var(--border); border-radius:12px; padding:12px 16px; margin-bottom:16px; flex-wrap:wrap; }
.tf-disk-ico { width:40px; height:40px; border-radius:10px; display:flex; align-items:center; justify-content:center; background:rgba(99,102,241,0.15); color:var(--accent-2,#a78bfa); flex-shrink:0; }
.tf-disk-ico .icon { width:20px; height:20px; }
.tf-disk-meta { min-width:0; flex:1; }
.tf-disk-l1 { font-size:13px; font-weight:700; }
.tf-disk-l2 { font-size:12px; color:var(--text-3,#6a7196); white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.tf-disk-bar { flex:2; min-width:120px; height:8px; border-radius:999px; background:var(--bg-input,#0b0e22); overflow:hidden; }
.tf-disk-bar > i { display:block; height:100%; background:linear-gradient(135deg,#6366f1,#a78bfa); border-radius:999px; transition:width .6s ease; }
.tf-disk-nums { font-size:12px; color:var(--text-2,#a0a8c8); white-space:nowrap; }

.tf-cat-list { display:flex; flex-direction:column; gap:10px; }
.tf-cat-row { display:grid; grid-template-columns:40px 1fr auto; gap:12px; align-items:center; background:var(--panel,var(--bg-2)); border:1px solid var(--border); border-radius:12px; padding:12px 14px; }
.tf-cat-row.overridden { border-color:rgba(34,197,94,0.4); }
.tf-cat-ico { width:40px; height:40px; border-radius:10px; display:flex; align-items:center; justify-content:center; background:var(--bg-input,#0b0e22); border:1px solid var(--border); color:var(--text-2,#a0a8c8); }
.tf-cat-ico .icon { width:19px; height:19px; }
.tf-cat-info { min-width:0; }
.tf-cat-name { font-size:13px; font-weight:700; display:flex; align-items:center; gap:8px; flex-wrap:wrap; }
.tf-cat-size { font-family:ui-monospace,monospace; font-size:10px; font-weight:700; color:var(--text-2,#a0a8c8); background:var(--bg-input,#0b0e22); border:1px solid var(--border); border-radius:999px; padding:1px 8px; }
.tf-cat-badge.warn { font-size:10px; font-weight:700; color:#f59e0b; background:rgba(245,158,11,0.12); border:1px solid rgba(245,158,11,0.3); border-radius:999px; padding:1px 8px; }
.tf-cat-path { font-family:ui-monospace,monospace; font-size:11px; color:var(--text-3,#6a7196); margin-top:3px; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.tf-cat-row.overridden .tf-cat-path { color:#22c55e; }
.tf-cat-desc { font-size:11px; color:var(--text-3,#6a7196); margin-top:1px; }
.tf-cat-pending { font-size:11px; color:#f59e0b; margin-top:5px; display:flex; align-items:center; gap:6px; }
.tf-cat-pending .icon { width:13px; height:13px; }
.tf-cat-actions { flex-shrink:0; }
.tf-storage-loading, .tf-tree-loading, .tf-tree-empty { color:var(--text-3,#6a7196); font-size:13px; padding:12px; }
.tf-tree-empty.err { color:#ef4444; }

.tf-picker { }
.tf-tree { height:280px; overflow-y:auto; background:var(--bg-input,#0b0e22); border:1px solid var(--border); border-radius:10px; padding:6px; }
.tf-node-row { display:flex; align-items:center; gap:7px; padding:6px 8px; border-radius:8px; cursor:pointer; font-size:13px; color:var(--text-2,#a0a8c8); border:1px solid transparent; }
.tf-node-row:hover { background:var(--bg-3,#111535); color:var(--text,#e8ebf5); }
.tf-node-row.selected { background:rgba(99,102,241,0.18); color:var(--accent-2,#a78bfa); border-color:rgba(99,102,241,0.35); }
.tf-chev { width:14px; height:14px; display:flex; align-items:center; justify-content:center; color:var(--text-3,#6a7196); transition:transform .2s; flex-shrink:0; }
.tf-chev .icon { width:11px; height:11px; }
.tf-chev.leaf { opacity:0; }
.tf-node.open > .tf-node-row .tf-chev { transform:rotate(90deg); }
.tf-fico { color:#60a5fa; opacity:.85; display:flex; }
.tf-fico .icon { width:16px; height:16px; }
.tf-fname { flex:1; min-width:0; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.tf-node-kids { display:none; }
.tf-node.open > .tf-node-kids { display:block; }
.tf-picked-bar { display:flex; align-items:center; gap:10px; padding:9px 12px; margin-top:10px; background:var(--bg-input,#0b0e22); border:1px solid var(--border); border-radius:8px; font-size:12px; }
.tf-picked-lab { color:var(--text-3,#6a7196); flex-shrink:0; }
.tf-picked-val { font-family:ui-monospace,monospace; font-size:11px; color:var(--accent-2,#a78bfa); flex:1; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.tf-picked-free { font-family:ui-monospace,monospace; font-size:10px; color:var(--text-3,#6a7196); white-space:nowrap; }

.tf-path-diff { background:var(--bg-input,#0b0e22); border:1px solid var(--border); border-radius:10px; padding:12px 14px; margin-bottom:14px; font-family:ui-monospace,monospace; font-size:11px; }
.tf-path-diff .row { display:flex; align-items:center; gap:10px; padding:3px 0; min-width:0; }
.tf-path-diff .k { width:26px; flex-shrink:0; font-size:9px; font-weight:800; letter-spacing:.08em; text-transform:uppercase; font-family:system-ui,sans-serif; }
.tf-path-diff .row.old .k { color:var(--text-3,#6a7196); }
.tf-path-diff .row.new .k { color:#22c55e; }
.tf-path-diff .row.old code { color:var(--text-3,#6a7196); text-decoration:line-through; opacity:.75; }
.tf-path-diff .row.new code { color:var(--text,#e8ebf5); }
.tf-path-diff code { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.tf-path-diff .mid { display:flex; align-items:center; gap:8px; padding:3px 0 3px 36px; color:var(--accent-2,#a78bfa); font-size:10px; font-family:system-ui,sans-serif; }
.tf-path-diff .mid .icon { width:12px; height:12px; }
.tf-mig-warn { display:flex; align-items:flex-start; gap:9px; padding:10px 12px; background:rgba(96,165,250,0.06); border:1px solid rgba(96,165,250,0.22); border-radius:8px; font-size:12px; color:var(--text-2,#a0a8c8); line-height:1.5; }
.tf-mig-warn .icon { width:15px; height:15px; color:#60a5fa; flex-shrink:0; margin-top:1px; }

.tf-mig-steps { display:flex; flex-direction:column; }
.tf-mig-step { display:flex; gap:14px; position:relative; padding-bottom:18px; opacity:.45; transition:opacity .3s; }
.tf-mig-step:last-child { padding-bottom:4px; }
.tf-mig-step.active, .tf-mig-step.done { opacity:1; }
.tf-mig-step::before { content:''; position:absolute; left:15px; top:34px; bottom:2px; width:2px; background:var(--border); }
.tf-mig-step:last-child::before { display:none; }
.tf-mig-step.done::before { background:linear-gradient(135deg,#22c55e,#10b981); }
.tf-mig-dot { width:32px; height:32px; border-radius:50%; flex-shrink:0; display:flex; align-items:center; justify-content:center; background:var(--bg-input,#0b0e22); border:1px solid var(--border); color:var(--text-3,#6a7196); font-size:12px; font-weight:700; position:relative; z-index:1; }
.tf-mig-step.active .tf-mig-dot { border-color:#6366f1; color:var(--accent-2,#a78bfa); box-shadow:0 0 0 4px rgba(99,102,241,0.18); }
.tf-mig-step.done .tf-mig-dot { background:rgba(34,197,94,0.12); border-color:rgba(34,197,94,0.5); color:#22c55e; }
.tf-mig-dot .icon { width:15px; height:15px; }
.tf-mig-binfo { flex:1; min-width:0; padding-top:5px; }
.tf-mig-title { font-size:13px; font-weight:700; }
.tf-mig-sub { font-size:11px; color:var(--text-3,#6a7196); margin-top:2px; }
.tf-mig-prog { height:6px; border-radius:999px; background:var(--bg-input,#0b0e22); border:1px solid var(--border); overflow:hidden; margin-top:9px; }
.tf-mig-prog > i { display:block; height:100%; width:0; background:linear-gradient(135deg,#6366f1,#a78bfa); border-radius:999px; transition:width .35s ease; }
.tf-mig-progmeta { font-family:ui-monospace,monospace; font-size:10px; color:var(--text-3,#6a7196); margin-top:5px; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.tf-mig-note { display:flex; align-items:center; gap:9px; margin-top:14px; padding:10px 12px; background:rgba(96,165,250,0.06); border:1px solid rgba(96,165,250,0.22); border-radius:8px; font-size:11px; color:var(--text-2,#a0a8c8); line-height:1.5; }
.tf-mig-note .icon { width:15px; height:15px; color:#60a5fa; flex-shrink:0; }
.tf-mig-log { margin-top:12px; max-height:120px; overflow-y:auto; background:var(--bg-input,#0b0e22); border:1px solid var(--border); border-radius:8px; padding:8px 10px; font-family:ui-monospace,monospace; font-size:10px; color:var(--text-3,#6a7196); }
`;
