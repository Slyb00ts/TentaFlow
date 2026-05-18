// =============================================================================
// Plik: legal/index.js — RODO legal documents admin UI
// Opis: Ekran administracyjny F2-P8.d M10 — lista wygenerowanych dokumentow
//       RODO (warianty short/standard/full), generacja nowego PDF i miekkie
//       unieważnienie. Komunikacja przez binary protocol (LegalAdminBody).
//       Permission gating opiera sie o role admin (analogicznie do users/audit).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { TfWindow } from '/js/components/tf-window.js';

// Cache podpisanych URL-i z odpowiedzi GenerateResponse. Lista RPC nie zwraca
// signedUrl, wiec link "Pobierz" dziala tylko dla dokumentow wygenerowanych
// w tej sesji przegladarki. Refresh karty czysci cache — to akceptowalne,
// bo URL jest signed-HMAC i ma krotki TTL po stronie serwera.
const signedUrlCache = new Map();

let documents = [];
let filterText = '';
let includeRevoked = false;
let canWrite = false;

const VARIANT_LABELS = {
  short: 'Skrocony',
  standard: 'Standardowy',
  full: 'Pelny',
};

const VARIANT_DESCRIPTIONS = {
  short: 'Wariant skrocony — krotka informacja RODO dla uzytkownikow.',
  standard: 'Wariant standardowy — pelna klauzula informacyjna RODO.',
  full: 'Wariant pelny — klauzula RODO + zalaczniki techniczne i polityka cookies.',
};

// Mapuje protokolowe kody bledow na komunikaty PL.
function mapErrorMessage(err) {
  const code = err?.code;
  const reason = err?.reason ?? err?.message ?? '';
  if (code === 11 || /quota/i.test(reason)) return 'Przekroczono limit generacji dokumentow.';
  if (code === 7 || /permission/i.test(reason)) return 'Brak uprawnien do tej operacji.';
  if (code === 9 || /conflict/i.test(reason)) {
    if (/already_revoked/i.test(reason)) return 'Dokument byl juz wczesniej uniewazniony.';
    return 'Konflikt stanu dokumentu.';
  }
  if (code === 3 || /bad_request|invalid/i.test(reason)) return 'Nieprawidlowe zadanie.';
  return err?.message || 'Nieznany blad serwera.';
}

const LegalScreen = {
  title: 'Dokumenty RODO',

  render() {
    return `
      <div class="page-header">
        <div>
          <h1>Dokumenty RODO</h1>
          <div class="sub" id="legal-sub">Ladowanie...</div>
        </div>
        <div class="actions" id="legal-actions"></div>
      </div>

      <div class="card" style="padding: 14px; margin-bottom: 14px;">
        <div style="display: grid; grid-template-columns: 1fr auto auto; gap: 10px; align-items: center;">
          <tf-searchbox id="legal-f-search" placeholder="${escapeAttr('Filtruj po wariancie lub dacie...')}" debounce="200"></tf-searchbox>
          <tf-toggle id="legal-f-revoked" ${includeRevoked ? 'checked' : ''}>Pokaz uniewaznione</tf-toggle>
          <tf-button variant="ghost" icon="refresh" id="legal-refresh">Odswiez</tf-button>
        </div>
      </div>

      <div class="card" style="padding: 0; overflow: hidden;">
        <div id="legal-table-host"></div>
      </div>
    `;
  },

  async mount() {
    canWrite = await detectWritePermission();
    renderHeaderActions();
    attachFilterHandlers();
    await loadDocuments();
  },

  unmount() {
    documents = [];
    filterText = '';
    includeRevoked = false;
    canWrite = false;
    signedUrlCache.clear();
  },
};

// Tylko admin moze generowac i uniewazniac. Brak osobnego permission API —
// uzywamy authMeRequest.role analogicznie do modules/audit i modules/users.
async function detectWritePermission() {
  try {
    const me = await ApiBinary.one('authMeRequest');
    const role = String(me?.role || '').toLowerCase();
    return role === 'admin' || me?.isAdmin === true;
  } catch (_) {
    return false;
  }
}

function renderHeaderActions() {
  const host = byId('legal-actions');
  if (!host) return;
  if (canWrite) {
    host.innerHTML = '<tf-button variant="primary" icon="plus" id="legal-generate">Generuj dokument</tf-button>';
    byId('legal-generate')?.addEventListener('click', openGenerateDialog);
  } else {
    host.innerHTML = '';
  }
}

function attachFilterHandlers() {
  byId('legal-f-search')?.addEventListener('search', (e) => {
    filterText = String(e.detail?.value ?? '').toLowerCase();
    renderTable();
  });
  byId('legal-f-revoked')?.addEventListener('change', async (e) => {
    includeRevoked = Boolean(e.detail?.checked ?? e.target?.checked);
    await loadDocuments();
  });
  byId('legal-refresh')?.addEventListener('click', () => { loadDocuments(); });
}

async function loadDocuments() {
  try {
    const resp = await ApiBinary.one('legalDocumentsListRequest', { includeRevoked });
    documents = Array.isArray(resp.documents) ? resp.documents : [];
    updateSubtitle();
    renderTable();
  } catch (err) {
    toast(`Blad: ${mapErrorMessage(err)}`, 'error');
  }
}

function updateSubtitle() {
  const sub = byId('legal-sub');
  if (!sub) return;
  const total = documents.length;
  const active = documents.filter(isActive).length;
  sub.textContent = `${total} dokument(ow) — ${active} aktywnych`;
}

function isActive(doc) {
  const revoked = doc.revokedAtMs ?? doc.revoked_at_ms ?? 0;
  return !revoked || Number(revoked) === 0;
}

function filterDocuments() {
  if (!filterText) return documents;
  return documents.filter((d) => {
    const variant = String(d.variant ?? '').toLowerCase();
    const dateStr = formatDate(d.generatedAtMs ?? d.generated_at_ms).toLowerCase();
    return variant.includes(filterText) || dateStr.includes(filterText);
  });
}

function renderTable() {
  const host = byId('legal-table-host');
  if (!host) return;
  const rows = filterDocuments();
  if (rows.length === 0) {
    host.innerHTML = `<div class="empty-state" style="padding: 36px; text-align: center; color: var(--text-3);">
      Brak dokumentow do wyswietlenia.
    </div>`;
    return;
  }

  const headers = `
    <tr>
      <th>Wariant</th>
      <th>Wygenerowano</th>
      <th>Hash</th>
      <th>Status</th>
      <th style="text-align:right;">Akcje</th>
    </tr>
  `;
  const body = rows.map((d, idx) => {
    const docId = String(d.docId ?? d.doc_id ?? '');
    const variant = String(d.variant ?? '');
    const variantLabel = VARIANT_LABELS[variant] || variant;
    const generatedAt = formatDate(d.generatedAtMs ?? d.generated_at_ms);
    const hashFull = String(d.contentHash ?? d.content_hash ?? '');
    const hashShort = hashFull.slice(0, 12);
    const active = isActive(d);
    const statusChip = active
      ? '<tf-chip variant="success">Aktywny</tf-chip>'
      : '<tf-chip variant="muted">Uniewazniony</tf-chip>';
    const variantChip = `<tf-chip variant="info">${escapeHtml(variantLabel)}</tf-chip>`;
    return `
      <tr data-doc-id="${escapeAttr(docId)}" data-idx="${idx}">
        <td>${variantChip}</td>
        <td>${escapeHtml(generatedAt)}</td>
        <td><code title="${escapeAttr(hashFull)}" style="font-family: 'SF Mono', monospace; font-size: 12px;">${escapeHtml(hashShort)}</code></td>
        <td>${statusChip}</td>
        <td style="text-align:right;">${renderRowActions(docId, active)}</td>
      </tr>
    `;
  }).join('');

  host.innerHTML = `
    <table class="tf-table-plain" style="width:100%; border-collapse: collapse;">
      <thead>${headers}</thead>
      <tbody>${body}</tbody>
    </table>
  `;

  bindRowActions();
}

function renderRowActions(docId, active) {
  const hasUrl = signedUrlCache.has(docId);
  const downloadAttr = hasUrl ? '' : 'disabled';
  const downloadTitle = hasUrl
    ? 'Pobierz PDF (sygnowany URL z tej sesji)'
    : 'Wygeneruj ponownie, aby uzyskac aktywny URL pobrania';
  const revokeBtn = (canWrite && active)
    ? `<tf-button variant="ghost" size="sm" data-act="revoke" data-doc-id="${escapeAttr(docId)}">Uniewaznij</tf-button>`
    : '';
  return `
    <tf-button variant="ghost" size="sm" data-act="download" data-doc-id="${escapeAttr(docId)}" ${downloadAttr} title="${escapeAttr(downloadTitle)}">Pobierz</tf-button>
    ${revokeBtn}
  `;
}

function bindRowActions() {
  document.querySelectorAll('#legal-table-host [data-act="download"]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const id = btn.getAttribute('data-doc-id');
      const url = signedUrlCache.get(id);
      if (!url) {
        toast('Brak aktywnego URL — wygeneruj ponownie.', 'warn');
        return;
      }
      window.location.href = url;
    });
  });
  document.querySelectorAll('#legal-table-host [data-act="revoke"]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const id = btn.getAttribute('data-doc-id');
      if (id) openRevokeConfirm(id);
    });
  });
}

// Format ISO ms -> 'YYYY-MM-DD HH:mm' w lokalu pl-PL. Wartosci null/0 -> "—".
function formatDate(ms) {
  const n = Number(ms);
  if (!n) return '—';
  const d = new Date(n);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleString('pl-PL', {
    year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit',
  });
}

// =============================================================================
// Dialog generacji
// =============================================================================

function openGenerateDialog() {
  if (!canWrite) return;
  let submitInFlight = false;

  const variantOptions = ['short', 'standard', 'full'].map((v) => `
    <option value="${v}">${escapeHtml(VARIANT_LABELS[v])} — ${escapeHtml(VARIANT_DESCRIPTIONS[v])}</option>
  `).join('');

  const bodyHtml = `
    <div style="display: grid; gap: 12px;">
      <div>
        <label style="display:block; font-size:12px; color:var(--text-3); margin-bottom:6px;">Wariant dokumentu</label>
        <tf-select id="legal-gen-variant" value="standard" style="width:100%;">
          ${variantOptions}
        </tf-select>
      </div>
      <div id="legal-gen-error" style="display:none;"></div>
    </div>
  `;
  const footerHtml = `
    <tf-button variant="ghost" data-action="cancel">Anuluj</tf-button>
    <tf-button variant="primary" data-action="generate" id="legal-gen-submit">Generuj</tf-button>
  `;

  TfWindow.open({
    title: 'Generuj dokument RODO',
    body: bodyHtml,
    footer: footerHtml,
    buttons: 'close',
    modal: true,
    width: 520,
    minHeight: 220,
  });

  // Po otwarciu okno jest w DOM — bind handlerow po tick'u zeby slotted nodes
  // i tf-select byly zainicjowane.
  setTimeout(() => {
    const win = document.querySelector('tf-window:last-of-type');
    if (!win) return;

    win.addEventListener('close-request', (e) => {
      if (submitInFlight) e.preventDefault();
    });

    win.addEventListener('action', async (e) => {
      const act = e.detail?.action;
      if (act === 'generate') {
        e.preventDefault();
        if (submitInFlight) return;
        const variantEl = document.getElementById('legal-gen-variant');
        const variant = String(variantEl?.value ?? 'standard');
        await executeGenerate(win, variant, (val) => { submitInFlight = val; });
      }
    });
  }, 0);
}

async function executeGenerate(win, variant, setInFlight) {
  const submitBtn = document.getElementById('legal-gen-submit');
  const errorBox = document.getElementById('legal-gen-error');
  if (errorBox) { errorBox.style.display = 'none'; errorBox.innerHTML = ''; }
  if (submitBtn) submitBtn.setAttribute('disabled', '');
  setInFlight(true);
  try {
    const resp = await ApiBinary.one('legalDocumentGenerateRequest', { variant });
    const docId = String(resp.docId ?? resp.doc_id ?? '');
    const signedUrl = String(resp.signedUrl ?? resp.signed_url ?? '');
    if (docId && signedUrl) {
      signedUrlCache.set(docId, signedUrl);
    }
    setInFlight(false);
    win.close(true);
    toast('Dokument wygenerowany — rozpoczynam pobieranie.', 'success');
    if (signedUrl) {
      window.location.href = signedUrl;
    }
    await loadDocuments();
  } catch (err) {
    setInFlight(false);
    if (submitBtn) submitBtn.removeAttribute('disabled');
    const msg = mapErrorMessage(err);
    if (errorBox) {
      errorBox.style.display = 'block';
      errorBox.innerHTML = `<tf-chip variant="danger">${escapeHtml(msg)}</tf-chip>`;
    } else {
      toast(`Blad: ${msg}`, 'error');
    }
  }
}

// =============================================================================
// Dialog uniewaznienia
// =============================================================================

function openRevokeConfirm(docId) {
  if (!canWrite) return;
  let submitInFlight = false;

  const bodyHtml = `
    <p style="color: var(--text-2); font-size: 13px;">
      Czy na pewno chcesz uniewaznic dokument <code style="font-family:'SF Mono',monospace;">${escapeHtml(docId.slice(0, 12))}</code>?
    </p>
    <p style="color: var(--text-3); font-size: 12px;">
      Operacja oznacza dokument jako uniewazniony. Plik PDF pozostaje na dysku
      do celow audytu, ale nie bedzie zwracany w domyslnej liscie.
    </p>
    <div id="legal-rev-error" style="display:none; margin-top:10px;"></div>
  `;
  const footerHtml = `
    <tf-button variant="ghost" data-action="cancel">Anuluj</tf-button>
    <tf-button variant="danger-solid" icon="trash" data-action="revoke" id="legal-rev-submit">Uniewaznij</tf-button>
  `;

  TfWindow.open({
    title: 'Uniewaznij dokument',
    body: bodyHtml,
    footer: footerHtml,
    buttons: 'close',
    modal: true,
    width: 440,
    minHeight: 200,
  });

  setTimeout(() => {
    const win = document.querySelector('tf-window:last-of-type');
    if (!win) return;

    win.addEventListener('close-request', (e) => {
      if (submitInFlight) e.preventDefault();
    });

    win.addEventListener('action', async (e) => {
      const act = e.detail?.action;
      if (act === 'revoke') {
        e.preventDefault();
        if (submitInFlight) return;
        await executeRevoke(win, docId, (val) => { submitInFlight = val; });
      }
    });
  }, 0);
}

async function executeRevoke(win, docId, setInFlight) {
  const submitBtn = document.getElementById('legal-rev-submit');
  const errorBox = document.getElementById('legal-rev-error');
  if (errorBox) { errorBox.style.display = 'none'; errorBox.innerHTML = ''; }
  if (submitBtn) submitBtn.setAttribute('disabled', '');
  setInFlight(true);
  try {
    await ApiBinary.one('legalDocumentRevokeRequest', { docId });
    setInFlight(false);
    win.close(true);
    toast('Dokument uniewazniony.', 'success');
    signedUrlCache.delete(docId);
    await loadDocuments();
  } catch (err) {
    setInFlight(false);
    if (submitBtn) submitBtn.removeAttribute('disabled');
    const code = err?.code;
    const reason = err?.reason ?? err?.message ?? '';
    // Specjalny przypadek: already_revoked = idempotentny sukces dla UX.
    if (code === 9 && /already_revoked/i.test(reason)) {
      win.close(true);
      toast('Dokument byl juz wczesniej uniewazniony.', 'info');
      await loadDocuments();
      return;
    }
    const msg = mapErrorMessage(err);
    if (errorBox) {
      errorBox.style.display = 'block';
      errorBox.innerHTML = `<tf-chip variant="danger">${escapeHtml(msg)}</tf-chip>`;
    } else {
      toast(`Blad: ${msg}`, 'error');
    }
  }
}

export default LegalScreen;
