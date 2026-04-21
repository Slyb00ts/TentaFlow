// =============================================================================
// Plik: modules/addons/oauth-popup.js
// Opis: Wspolny helper uruchamiajacy okno OAuth dla addona. Otwiera popup z
//       URL-em autoryzacji zwroconym przez backend i nasluchuje postMessage
//       z callback page (window.opener.postMessage({type:'tf-oauth-result', ...})).
// Przyklad:
//   await runOAuthPopup({ addon_id: 'gmail', provider_id: 'google', mode: 'individual' });
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// Uruchamia flow OAuth. Gdy podano accountIdForReauth, wysyla reauthorize;
// w przeciwnym razie authorize-start. Zwraca promise rozwiazywany po
// otrzymaniu postMessage z callback.
export async function runOAuthPopup({ addon_id, provider_id, mode, accountIdForReauth } = {}) {
  const resp = accountIdForReauth
    ? await ApiBinary.action('addonOAuthReauthorizeRequest', { accountId: accountIdForReauth })
    : await ApiBinary.action('addonOAuthAuthorizeStartRequest', {
        addonId: addon_id,
        providerId: provider_id,
        mode: mode || 'individual',
      });

  const url = resp.authorizeUrl || resp.authorize_url;
  if (!url) throw new Error('missing authorize_url');

  const popup = window.open(url, 'tf-oauth', 'width=560,height=680,scrollbars=yes,resizable=yes');
  if (!popup) throw new Error('popup_blocked');

  return new Promise((resolve, reject) => {
    let timer = null;

    const cleanup = () => {
      window.removeEventListener('message', onMsg);
      if (timer) clearInterval(timer);
    };

    const onMsg = (ev) => {
      if (ev.origin !== window.location.origin) return;
      const data = ev.data;
      if (!data || data.type !== 'tf-oauth-result') return;
      cleanup();
      if (data.ok) {
        resolve(data);
      } else {
        reject(new Error(data.error || 'oauth_failed'));
      }
    };

    window.addEventListener('message', onMsg);

    // Monitorowanie zamkniecia okna bez wyniku = anulowanie.
    timer = setInterval(() => {
      if (popup.closed) {
        cleanup();
        reject(new Error('popup_closed'));
      }
    }, 500);
  });
}
