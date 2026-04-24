// =============================================================================
// Plik: lib/confirm-dialog.js
// Opis: Modal potwierdzenia na tf-window. Zastepuje window.confirm() ktorego
//       zachowanie w iOS WKWebView jest niestabilne. Zwraca Promise<boolean>.
//       Obsluguje warianty z peer summary card i lista konsekwencji (np. dla
//       "usun sparowanie") oraz prosty lead-only body dla zwyklych potwierdzen.
// Przyklad:
//   const ok = await confirmDialog({
//     title: 'Usunac parowanie?',
//     lead: 'Klucz zostanie usuniety.',
//     peer: { name: 'spark-001', id: 'b09b...' },
//     consequences: ['Sesja rozlaczona', 'Klucz usuniety'],
//     confirmLabel: 'Usun parowanie',
//     confirmIcon: 'trash',
//     variant: 'danger',
//   });
//   if (!ok) return;
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';

const PEER_ICON_SVG = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="4" width="16" height="16" rx="2" ry="2"/><rect x="9" y="9" width="6" height="6"/><line x1="9" y1="2" x2="9" y2="4"/><line x1="15" y1="2" x2="15" y2="4"/><line x1="9" y1="20" x2="9" y2="22"/><line x1="15" y1="20" x2="15" y2="22"/><line x1="20" y1="9" x2="22" y2="9"/><line x1="20" y1="14" x2="22" y2="14"/><line x1="2" y1="9" x2="4" y2="9"/><line x1="2" y1="14" x2="4" y2="14"/></svg>';

const CHECK_BULLET_SVG = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>';

/**
 * Pokazuje modal potwierdzenia i zwraca Promise<boolean>.
 * true = user potwierdzil, false = cancel/close/Esc.
 *
 * @param {Object}   opts
 * @param {string}   opts.title                — naglowek okna (obowiazkowy)
 * @param {string}   opts.lead                 — zdanie prowadzace w body
 * @param {Object}  [opts.peer]                — karta peera {name, id, icon?}
 * @param {string[]}[opts.consequences]        — lista konsekwencji (buletki)
 * @param {string}   opts.confirmLabel         — label przycisku potwierdzenia
 * @param {string}  [opts.cancelLabel]         — label przycisku cancel (domyslnie "Anuluj")
 * @param {string}  [opts.confirmIcon]         — 'trash' | 'check' lub undefined
 * @param {string}  [opts.variant='danger']    — 'danger' | 'primary'
 */
export function confirmDialog({
  title,
  lead,
  peer,
  consequences,
  confirmLabel,
  cancelLabel,
  confirmIcon,
  variant = 'danger',
} = {}) {
  return new Promise((resolve) => {
    const cancel = cancelLabel || 'Anuluj';

    const peerHtml = peer ? `
      <div class="confirm-dlg__peer">
        <div class="confirm-dlg__peer-ico">${PEER_ICON_SVG}</div>
        <div class="confirm-dlg__peer-meta">
          <div class="confirm-dlg__peer-name">${escapeHtml(peer.name || '')}</div>
          ${peer.id ? `<div class="confirm-dlg__peer-id">${escapeHtml(peer.id)}</div>` : ''}
        </div>
      </div>` : '';

    const consequencesHtml = Array.isArray(consequences) && consequences.length ? `
      <div class="confirm-dlg__consequences confirm-dlg__consequences--${variant}">
        ${consequences.map((c) => `
          <div class="confirm-dlg__item">
            ${CHECK_BULLET_SVG}
            <span>${escapeHtml(c)}</span>
          </div>`).join('')}
      </div>` : '';

    const win = document.createElement('tf-window');
    win.setAttribute('title', title || '');
    // Ikona w headerze tf-window — sprite id z #i-*. Dla danger uzywamy alert,
    // dla primary info. Unikamy drugiej ikony w body (wczesniej byla
    // dublowana — tf-window renderowal czerwona kropke obok tytulu, a my
    // wrzucalismy trojkat ostrzegawczy w srodek).
    win.setAttribute('icon', variant === 'primary' ? 'info' : 'alert');
    win.setAttribute('buttons', 'close');
    win.setAttribute('width', '460');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    win.classList.add('tf-window--confirm', `tf-window--confirm-${variant}`);

    const body = document.createElement('div');
    body.slot = 'body';
    body.className = 'confirm-dlg__body';
    body.innerHTML = `
      ${lead ? `<p class="confirm-dlg__lead">${escapeHtml(lead)}</p>` : ''}
      ${peerHtml}
      ${consequencesHtml}
    `;
    win.appendChild(body);

    const btnVariant = variant === 'primary' ? 'primary' : 'danger-solid';
    const foot = document.createElement('div');
    foot.slot = 'footer';
    foot.className = 'confirm-dlg__foot';
    foot.innerHTML = `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(cancel)}</tf-button>
      <tf-button variant="${btnVariant}"${confirmIcon ? ` icon="${escapeAttr(confirmIcon)}"` : ''} data-action="confirm">${escapeHtml(confirmLabel || 'OK')}</tf-button>
    `;
    win.appendChild(foot);

    const backdrop = document.createElement('div');
    backdrop.className = 'tf-window-backdrop';
    document.body.append(backdrop, win);

    let settled = false;
    const cleanup = (result) => {
      if (settled) return;
      settled = true;
      win.remove();
      backdrop.remove();
      resolve(result);
    };

    win.addEventListener('action', (e) => {
      const a = e.detail?.action;
      if (a === 'cancel' || a === 'close') cleanup(false);
      else if (a === 'confirm') cleanup(true);
    });

    backdrop.addEventListener('click', () => cleanup(false));
  });
}

export default confirmDialog;
