// =============================================================================
// Plik: modules/qr-scanner.js
// Opis: QR scanner dla telefonow/tabletow — kamera + native BarcodeDetector
//       (Chrome/Edge/Android Chrome/Safari iOS 17+). Brak fallback dla
//       przegladrek bez BarcodeDetector — graceful degradation: przycisk
//       "Zeskanuj kamerą" ukryty jak API niedostepne.
// =============================================================================

import { I18n } from '/js/i18n.js';

/**
 * Czy przeglądarka wspiera skanowanie QR przez BarcodeDetector.
 * Sprawdza tez czy urzadzenie ma jakakolwiek kamere.
 */
export async function isScannerSupported() {
  if (typeof BarcodeDetector === 'undefined') return false;
  if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) return false;
  try {
    const formats = await BarcodeDetector.getSupportedFormats();
    if (!formats.includes('qr_code')) return false;
  } catch {
    return false;
  }
  return true;
}

/**
 * Otwiera fullscreen overlay z podgladem kamery + automatycznym skanowaniem QR.
 * Zwraca Promise<string|null> — odczytany string z QR albo null gdy user anuluje.
 */
export async function scanQr() {
  if (!(await isScannerSupported())) {
    throw new Error('QR scanner not supported on this device');
  }

  return new Promise(async (resolve) => {
    const overlay = document.createElement('div');
    overlay.className = 'qr-scanner-overlay';
    overlay.innerHTML = `
      <div class="qr-scanner-head">
        <div class="qr-scanner-title">${escapeHtml(I18n.t('mesh.qr_scanner_title'))}</div>
        <button type="button" class="qr-scanner-close" aria-label="Close">
          <svg viewBox="0 0 24 24"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
        </button>
      </div>
      <div class="qr-scanner-viewport">
        <video autoplay playsinline muted></video>
        <div class="qr-scanner-frame">
          <span class="corner tl"></span>
          <span class="corner tr"></span>
          <span class="corner bl"></span>
          <span class="corner br"></span>
          <span class="scan-line"></span>
        </div>
      </div>
      <div class="qr-scanner-hint">${escapeHtml(I18n.t('mesh.qr_scanner_hint'))}</div>
    `;
    document.body.appendChild(overlay);

    const video = overlay.querySelector('video');
    const closeBtn = overlay.querySelector('.qr-scanner-close');
    let stream = null;
    let rafId = null;
    let closed = false;

    const cleanup = (result) => {
      if (closed) return;
      closed = true;
      if (rafId) cancelAnimationFrame(rafId);
      if (stream) {
        stream.getTracks().forEach((t) => t.stop());
      }
      overlay.classList.add('closing');
      setTimeout(() => {
        if (overlay.parentNode) overlay.parentNode.removeChild(overlay);
      }, 200);
      resolve(result);
    };

    closeBtn.addEventListener('click', () => cleanup(null));
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) cleanup(null);
    });
    // ESC closes
    const onKey = (e) => {
      if (e.key === 'Escape') {
        document.removeEventListener('keydown', onKey);
        cleanup(null);
      }
    };
    document.addEventListener('keydown', onKey);

    try {
      stream = await navigator.mediaDevices.getUserMedia({
        video: { facingMode: { ideal: 'environment' }, width: { ideal: 1280 }, height: { ideal: 720 } },
        audio: false,
      });
      video.srcObject = stream;
      await video.play();
    } catch (err) {
      console.warn('[qr-scanner] getUserMedia:', err);
      cleanup(null);
      throw err;
    }

    // eslint-disable-next-line no-undef
    const detector = new BarcodeDetector({ formats: ['qr_code'] });

    const tick = async () => {
      if (closed) return;
      if (video.readyState === video.HAVE_ENOUGH_DATA) {
        try {
          const results = await detector.detect(video);
          if (results && results.length > 0) {
            const raw = results[0].rawValue;
            if (raw) {
              cleanup(raw);
              return;
            }
          }
        } catch (e) {
          // continue scanning
        }
      }
      rafId = requestAnimationFrame(tick);
    };
    rafId = requestAnimationFrame(tick);
  });
}

/**
 * Parsuje `tentaflow-pair://<hex>?pin=<pin>&host=<name>&ver=1` URI albo
 * plain hex. Zwraca { hex, pin, host } albo null gdy nie pasuje.
 */
export function parsePairUri(raw) {
  if (!raw) return null;
  const str = String(raw).trim();
  // Plain hex (64 chars)?
  if (/^[0-9a-f]{64}$/i.test(str)) {
    return { hex: str.toLowerCase(), pin: '', host: '' };
  }
  // tentaflow-pair://HEX?pin=XXX&host=YYY&ver=1
  const m = str.match(/^tentaflow-pair:\/\/([0-9a-fA-F]{64})(?:\?(.*))?$/);
  if (!m) return null;
  const hex = m[1].toLowerCase();
  const qs = new URLSearchParams(m[2] || '');
  return {
    hex,
    pin: qs.get('pin') || '',
    host: qs.get('host') || '',
  };
}

function escapeHtml(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
  ));
}
