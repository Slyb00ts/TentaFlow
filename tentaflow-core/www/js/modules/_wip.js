// =============================================================================
// Plik: modules/_wip.js
// Opis: Fabryka ekranow placeholder renderujaca naglowek strony, opis
//       ekranu, liste plikow zrodlowych i liste features. Uzywana przez
//       router dla tras ktore nie maja jeszcze dedykowanego modulu.
// Przyklad:
//   Router.register('meeting', makeWipScreen({
//     title: 'Meeting Bot',
//     sourcePaths: ['legacy/meeting/MeetingBot.js'],
//     features: ['transcript live', 'speaker diarization'],
//   }));
// =============================================================================

import { escapeHtml } from '/js/utils.js';

export function makeWipScreen({ title, sourcePaths = [], description = '', features = [] }) {
  return {
    title,
    render() {
      return `
        <div class="page-header">
          <div>
            <h1>
              <svg class="icon" style="color: var(--warning);"><use href="#i-alert"/></svg>
              ${escapeHtml(title)}
            </h1>
            <div class="sub">SCREEN W TRAKCIE PORTOWANIA — patrz source paths ponizej</div>
          </div>
          <div class="actions">
            <span class="badge" style="background: rgba(245,158,11,0.15); color: var(--warning);">WIP</span>
          </div>
        </div>

        <div class="card" style="margin-bottom: 16px;">
          <h3 style="font-size: 14px; margin-bottom: 8px; display: flex; align-items: center; gap: 8px;">
            <svg class="icon" style="color: var(--accent-2);"><use href="#i-info"/></svg>
            Status
          </h3>
          <p style="color: var(--text-2); font-size: 13px; line-height: 1.6; margin-bottom: 12px;">
            ${escapeHtml(description || 'Ten ekran nie ma jeszcze nowego modułu. Implementacja — kolejna iteracja.')}
          </p>

          ${sourcePaths.length > 0 ? `
            <h4 style="font-size: 11px; color: var(--text-3); text-transform: uppercase; letter-spacing: 0.06em; margin-top: 16px; margin-bottom: 8px; font-weight: 700;">
              Historyczne source paths:
            </h4>
            <ul style="list-style: none; padding: 0;">
              ${sourcePaths.map((p) => `
                <li style="padding: 6px 12px; background: var(--bg-input); border-radius: var(--radius-sm); margin-bottom: 4px; font-family: 'SF Mono', monospace; font-size: 12px; color: var(--accent-2);">
                  ${escapeHtml(p)}
                </li>
              `).join('')}
            </ul>
          ` : ''}

          ${features.length > 0 ? `
            <h4 style="font-size: 11px; color: var(--text-3); text-transform: uppercase; letter-spacing: 0.06em; margin-top: 16px; margin-bottom: 8px; font-weight: 700;">
              Features ktore MUSI miec (per FEATURES-TO-PRESERVE.md):
            </h4>
            <ul style="padding-left: 20px; color: var(--text-2); font-size: 13px; line-height: 1.7;">
              ${features.map((f) => `<li>${escapeHtml(f)}</li>`).join('')}
            </ul>
          ` : ''}
        </div>
      `;
    },
  };
}
