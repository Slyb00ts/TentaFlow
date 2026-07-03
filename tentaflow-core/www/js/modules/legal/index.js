// =============================================================================
// Plik: legal/index.js — RODO legal documents admin UI
// Opis: Ekran administracyjny F2-P8.d M10 — lista wygenerowanych dokumentów
//       RODO (warianty short/standard/full), generacja nowego PDF i miekkie
//       unieważnienie. Komunikacja przez binary protocol (LegalAdminBody).
//       Permission gating opiera sie o role admin/dpo (analogicznie do
//       users/audit). Backend i tak gate'uje przez legal.write — to czysto
//       UX matter zeby DPO widzial przyciski generowania.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';

// Cache podpisanych URL-i z odpowiedzi GenerateResponse. Lista RPC nie zwraca
// signedUrl, wiec link "Pobierz" dziala tylko dla dokumentów wygenerowanych
// w tej sesji przegladarki. Refresh karty czysci cache — to akceptowalne,
// bo URL jest signed-HMAC i ma krotki TTL po stronie serwera.
const signedUrlCache = new Map();

let documents = [];
let filterText = '';
let includeRevoked = false;
let canWrite = false;

const VARIANT_LABELS = {
  short: 'Skrócony',
  standard: 'Standardowy',
  full: 'Pełny',
};

const VARIANT_DESCRIPTIONS = {
  short: 'Wariant skrócony — krótka informacja RODO dla użytkowników.',
  standard: 'Wariant standardowy — pełna klauzula informacyjna RODO.',
  full: 'Wariant pełny — klauzula RODO + załączniki techniczne i polityka cookies.',
};

// Mapuje protokolowe kody bledow na komunikaty PL. Nieznane kody przepuszczamy
// verbatim (`${code}: ${reason}`) zeby nie maskowac nowych bledow z serwera
// generycznym komunikatem.
function mapErrorMessage(err) {
  const code = err?.code;
  const reason = String(err?.reason ?? err?.message ?? '').trim();
  // Map only known protocol codes; unknown numeric codes pass through verbatim
  // so the user sees what the server actually sent.
  if (code === 11) return 'Przekroczono limit generacji dokumentów.';
  if (code === 7) return 'Brak uprawnień do tej operacji.';
  if (code === 9) {
    if (/already_revoked/i.test(reason)) return 'Dokument był już wcześniej unieważniony.';
    return reason ? `Konflikt: ${reason}` : 'Konflikt stanu dokumentu.';
  }
  if (code === 3) {
    return reason ? `Nieprawidłowe żądanie: ${reason}` : 'Nieprawidłowe żądanie.';
  }
  if (code != null) return reason ? `${code}: ${reason}` : `Błąd serwera (kod ${code}).`;
  if (reason) return reason;
  return 'Nieznany błąd serwera.';
}

const LegalScreen = {
  title: 'Dokumenty RODO',

  render() {
    return `
      <div class="page-header">
        <div>
          <h1>Dokumenty RODO</h1>
          <div class="sub" id="legal-sub">Ładowanie...</div>
        </div>
        <div class="actions" id="legal-actions"></div>
      </div>

      <div class="card" style="padding: 14px; margin-bottom: 14px;">
        <div style="display: flex; flex-wrap: wrap; gap: 10px; align-items: center;">
          <tf-searchbox id="legal-f-search" placeholder="${escapeAttr('Filtruj po wariancie lub dacie...')}" debounce="200" style="flex: 1 1 200px;"></tf-searchbox>
          <tf-toggle id="legal-f-revoked" ${includeRevoked ? 'checked' : ''}>Pokaż unieważnione</tf-toggle>
          <tf-button variant="ghost" icon="refresh" id="legal-refresh">Odśwież</tf-button>
        </div>
      </div>

      <div class="card" style="padding: 0; overflow: hidden;">
        <tf-table id="legal-table">
          <tf-column key="variant" label="${escapeAttr('Wariant')}" renderer="chip"></tf-column>
          <tf-column key="generatedAt" label="${escapeAttr('Wygenerowano')}"></tf-column>
          <tf-column key="hash" label="${escapeAttr('Hash')}" renderer="html"></tf-column>
          <tf-column key="status" label="${escapeAttr('Status')}" renderer="chip"></tf-column>
          <tf-column key="actions" label="${escapeAttr('Akcje')}" renderer="html"></tf-column>
        </tf-table>
        <div id="legal-empty" hidden style="padding: 36px; text-align: center; color: var(--text-3);">
          Brak dokumentów do wyświetlenia.
        </div>
      </div>
    `;
  },

  async mount() {
    canWrite = await detectWritePermission();
    renderHeaderActions();
    attachFilterHandlers();
    bindTableActions();
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

// Tylko admin / DPO moze generowac i uniewazniac. AuthMeResponse nie zwraca
// listy permissions, wiec stosujemy role check. Backend dodatkowo wymusza
// permission `legal.write` — tu chodzi wylacznie o UI gating.
async function detectWritePermission() {
  try {
    const me = await ApiBinary.one('authMeRequest');
    const role = String(me?.role || '').toLowerCase();
    return role === 'admin' || role === 'dpo' || me?.isAdmin === true;
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

// Delegujemy klik na przyciski wewnatrz <tf-table> przez composedPath (komorki
// dla renderer="html" siedza w shadow DOM tabeli).
function bindTableActions() {
  const table = byId('legal-table');
  if (!table) return;
  table.addEventListener('click', (ev) => {
    const path = ev.composedPath();
    const btn = path.find((el) => el && el.tagName === 'TF-BUTTON' && el.dataset && el.dataset.act);
    if (!btn) return;
    const id = btn.dataset.docId;
    if (!id) return;
    if (btn.dataset.act === 'download') {
      const url = signedUrlCache.get(id);
      if (!url) {
        toast('Brak aktywnego URL — wygeneruj ponownie.', 'warn');
        return;
      }
      window.location.href = url;
    } else if (btn.dataset.act === 'revoke') {
      openRevokeConfirm(id);
    }
  });
}

async function loadDocuments() {
  try {
    const resp = await ApiBinary.one('legalDocumentsListRequest', { includeRevoked });
    documents = Array.isArray(resp.documents) ? resp.documents : [];
    updateSubtitle();
    renderTable();
  } catch (err) {
    toast(`Błąd: ${mapErrorMessage(err)}`, 'error');
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

function buildRows() {
  return filterDocuments().map((d) => {
    const docId = String(d.docId ?? d.doc_id ?? '');
    const variant = String(d.variant ?? '');
    const variantLabel = VARIANT_LABELS[variant] || variant;
    const generatedAt = formatDate(d.generatedAtMs ?? d.generated_at_ms);
    const hashFull = String(d.contentHash ?? d.content_hash ?? '');
    const hashShort = hashFull.slice(0, 12);
    const active = isActive(d);
    return {
      variant: { status: 'info', label: variantLabel },
      generatedAt,
      hash: `<code title="${escapeAttr(hashFull)}" style="font-family: 'SF Mono', monospace; font-size: 12px;">${escapeHtml(hashShort)}</code>`,
      status: active
        ? { status: 'ok', label: 'Aktywny' }
        : { status: 'muted', label: 'Uniewazniony' },
      actions: renderRowActions(docId, active),
    };
  });
}

function renderTable() {
  const table = byId('legal-table');
  const empty = byId('legal-empty');
  if (!table) return;
  const rows = buildRows();
  if (rows.length === 0) {
    table.hidden = true;
    if (empty) empty.hidden = false;
    table.rows = [];
    return;
  }
  if (empty) empty.hidden = true;
  table.hidden = false;
  table.rows = rows;
}

function renderRowActions(docId, active) {
  const hasUrl = signedUrlCache.has(docId);
  const downloadAttr = hasUrl ? '' : 'disabled';
  const downloadTitle = hasUrl
    ? 'Pobierz PDF (sygnowany URL z tej sesji)'
    : 'Wygeneruj ponownie, aby uzyskac aktywny URL pobrania';
  const revokeBtn = (canWrite && active)
    ? `<tf-button variant="ghost" size="sm" data-act="revoke" data-doc-id="${escapeAttr(docId)}">Unieważnij</tf-button>`
    : '';
  return `
    <tf-button variant="ghost" size="sm" data-act="download" data-doc-id="${escapeAttr(docId)}" ${downloadAttr} title="${escapeAttr(downloadTitle)}">Pobierz</tf-button>
    ${revokeBtn}
  `;
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
//
// Budujemy <tf-window> recznie (nie przez TfWindow.open) zeby:
//  - rejestrowac listener close-request PRZED appendChild (capture race-free),
//  - footer buttons uzywaja data-role + bezposrednich handlerow zamiast
//    polegania na auto-close po evencie 'action' (ktore moze zamknac okno
//    zanim ustawimy submitInFlight).
// =============================================================================

function openGenerateDialog() {
  if (!canWrite) return;
  let submitInFlight = false;

  const dlg = document.createElement('tf-window');
  dlg.setAttribute('title', 'Generuj dokument RODO');
  dlg.setAttribute('buttons', 'close');
  dlg.setAttribute('width', '520');
  dlg.setAttribute('min-height', '220');
  dlg.setAttribute('initial-x', 'center');
  dlg.setAttribute('initial-y', 'center');
  dlg.setAttribute('role', 'dialog');
  dlg.setAttribute('aria-modal', 'true');

  const variantOptions = ['short', 'standard', 'full'].map((v) => `
    <option value="${v}">${escapeHtml(VARIANT_LABELS[v])} — ${escapeHtml(VARIANT_DESCRIPTIONS[v])}</option>
  `).join('');

  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <div style="display: grid; gap: 12px;">
      <div>
        <label style="display:block; font-size:12px; color:var(--text-3); margin-bottom:6px;">Wariant dokumentu</label>
        <tf-select data-role="variant" value="standard" style="width:100%;">
          ${variantOptions}
        </tf-select>
      </div>
      <div data-role="error" style="display:none;"></div>
    </div>
  `;
  dlg.appendChild(body);

  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.style.cssText = 'display:flex;gap:8px;width:100%;';
  footer.innerHTML = `
    <div style="flex:1"></div>
    <tf-button variant="ghost" data-role="cancel">Anuluj</tf-button>
    <tf-button variant="primary" data-role="submit">Generuj</tf-button>
  `;
  dlg.appendChild(footer);

  // Modalne tlo + blokada zamkniecia podczas in-flight RPC.
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.appendChild(backdrop);

  dlg.addEventListener('close-request', (e) => {
    if (submitInFlight) { e.preventDefault(); return; }
  });

  document.body.appendChild(dlg);

  const cleanup = () => {
    if (backdrop.isConnected) backdrop.remove();
  };
  // Sprzata backdrop gdy okno zostanie usuniete z DOM.
  const mo = new MutationObserver(() => {
    if (!dlg.isConnected) {
      mo.disconnect();
      cleanup();
    }
  });
  mo.observe(document.body, { childList: true, subtree: true });

  const cancelBtn = footer.querySelector('[data-role="cancel"]');
  const submitBtn = footer.querySelector('[data-role="submit"]');
  const errorBox = body.querySelector('[data-role="error"]');

  cancelBtn?.addEventListener('click', () => {
    if (submitInFlight) return;
    dlg.close(true);
  });

  submitBtn?.addEventListener('click', async () => {
    if (submitInFlight) return;
    submitInFlight = true;
    submitBtn.setAttribute('disabled', '');
    cancelBtn?.setAttribute('disabled', '');
    if (errorBox) { errorBox.style.display = 'none'; errorBox.innerHTML = ''; }
    const variantEl = body.querySelector('tf-select[data-role="variant"]');
    const variant = String(variantEl?.value ?? 'standard');
    try {
      const resp = await ApiBinary.one('legalDocumentGenerateRequest', { variant });
      const docId = String(resp.docId ?? resp.doc_id ?? '');
      const signedUrl = String(resp.signedUrl ?? resp.signed_url ?? '');
      if (docId && signedUrl) {
        signedUrlCache.set(docId, signedUrl);
      }
      submitInFlight = false;
      dlg.close(true);
      toast('Dokument wygenerowany — rozpoczynam pobieranie.', 'success');
      if (signedUrl) {
        window.location.href = signedUrl;
      }
      await loadDocuments();
    } catch (err) {
      submitInFlight = false;
      submitBtn.removeAttribute('disabled');
      cancelBtn?.removeAttribute('disabled');
      const msg = mapErrorMessage(err);
      if (errorBox) {
        errorBox.style.display = 'block';
        errorBox.innerHTML = `<tf-chip variant="danger">${escapeHtml(msg)}</tf-chip>`;
      } else {
        toast(`Błąd: ${msg}`, 'error');
      }
    }
  });
}

// =============================================================================
// Dialog uniewaznienia
// =============================================================================

function openRevokeConfirm(docId) {
  if (!canWrite) return;
  let submitInFlight = false;

  const dlg = document.createElement('tf-window');
  dlg.setAttribute('title', 'Unieważnij dokument');
  dlg.setAttribute('buttons', 'close');
  dlg.setAttribute('width', '440');
  dlg.setAttribute('min-height', '200');
  dlg.setAttribute('initial-x', 'center');
  dlg.setAttribute('initial-y', 'center');
  dlg.setAttribute('role', 'dialog');
  dlg.setAttribute('aria-modal', 'true');

  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <p style="color: var(--text-2); font-size: 13px;">
      Czy na pewno chcesz unieważnić dokument <code style="font-family:'SF Mono',monospace;">${escapeHtml(docId.slice(0, 12))}</code>?
    </p>
    <p style="color: var(--text-3); font-size: 12px;">
      Operacja oznacza dokument jako unieważniony. Plik PDF pozostaje na dysku
      do celow audytu, ale nie bedzie zwracany w domyslnej liscie.
    </p>
    <div data-role="error" style="display:none; margin-top:10px;"></div>
  `;
  dlg.appendChild(body);

  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.style.cssText = 'display:flex;gap:8px;width:100%;';
  footer.innerHTML = `
    <div style="flex:1"></div>
    <tf-button variant="ghost" data-role="cancel">Anuluj</tf-button>
    <tf-button variant="danger-solid" icon="trash" data-role="submit">Unieważnij</tf-button>
  `;
  dlg.appendChild(footer);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.appendChild(backdrop);

  dlg.addEventListener('close-request', (e) => {
    if (submitInFlight) { e.preventDefault(); return; }
  });

  document.body.appendChild(dlg);

  const cleanup = () => {
    if (backdrop.isConnected) backdrop.remove();
  };
  const mo = new MutationObserver(() => {
    if (!dlg.isConnected) {
      mo.disconnect();
      cleanup();
    }
  });
  mo.observe(document.body, { childList: true, subtree: true });

  const cancelBtn = footer.querySelector('[data-role="cancel"]');
  const submitBtn = footer.querySelector('[data-role="submit"]');
  const errorBox = body.querySelector('[data-role="error"]');

  cancelBtn?.addEventListener('click', () => {
    if (submitInFlight) return;
    dlg.close(true);
  });

  submitBtn?.addEventListener('click', async () => {
    if (submitInFlight) return;
    submitInFlight = true;
    submitBtn.setAttribute('disabled', '');
    cancelBtn?.setAttribute('disabled', '');
    if (errorBox) { errorBox.style.display = 'none'; errorBox.innerHTML = ''; }
    try {
      await ApiBinary.one('legalDocumentRevokeRequest', { docId });
      signedUrlCache.delete(docId);
      submitInFlight = false;
      dlg.close(true);
      toast('Dokument unieważniony.', 'success');
      await loadDocuments();
    } catch (err) {
      const code = err?.code;
      const reason = String(err?.reason ?? err?.message ?? '');
      // already_revoked = idempotentny sukces. signedUrl jest juz unieważniony
      // po stronie serwera, wiec wyrzucamy z cache zeby przycisk Pobierz nie
      // wprowadzal w blad.
      if (code === 9 && /already_revoked/i.test(reason)) {
        signedUrlCache.delete(docId);
        submitInFlight = false;
        dlg.close(true);
        toast('Dokument był już wcześniej unieważniony.', 'info');
        await loadDocuments();
        return;
      }
      submitInFlight = false;
      submitBtn.removeAttribute('disabled');
      cancelBtn?.removeAttribute('disabled');
      const msg = mapErrorMessage(err);
      if (errorBox) {
        errorBox.style.display = 'block';
        errorBox.innerHTML = `<tf-chip variant="danger">${escapeHtml(msg)}</tf-chip>`;
      } else {
        toast(`Błąd: ${msg}`, 'error');
      }
    }
  });
}

export default LegalScreen;
