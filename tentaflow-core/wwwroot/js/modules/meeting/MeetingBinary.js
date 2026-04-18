// =============================================================================
// Plik: modules/meeting/MeetingBinary.js
// Opis: Meeting bot (Teams/Outlook) ekran zmigrowany na binary protocol.
//       Bootstrap: voice profiles list dla speaker identification. Pelne
//       meeting state + bot lifecycle wymaga MeetingBot variants w phase 2.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const MeetingBinary = (() => {
  'use strict';
  let voiceProfiles = [];

  async function loadProfiles() {
    try {
      voiceProfiles = await ApiBinary.list('voiceProfileListRequest');
      renderProfiles();
    } catch (err) {
      console.error('[meeting-binary] profiles load failed:', err);
      voiceProfiles = [];
      renderProfiles();
    }
  }

  function renderProfiles() {
    const tbody = document.getElementById('voice-profiles-tbody');
    if (!tbody) return;
    tbody.innerHTML = voiceProfiles.length === 0
      ? `<tr><td colspan="3"><div class="empty-state"><div class="empty-state-text">${I18n.t('meeting.no_profiles')}</div></div></td></tr>`
      : voiceProfiles.map(p => `
          <tr>
            <td>${Utils.escapeHtml(p.displayName)}</td>
            <td>${p.embeddingCount}</td>
            <td>${Utils.formatDate(p.createdAtEpoch * 1000)}</td>
          </tr>
        `).join('');
  }

  return {
    mount: () => loadProfiles(),
    unmount: () => { voiceProfiles = []; },
  };
})();

export default MeetingBinary;
