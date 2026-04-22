// =============================================================================
// Plik: modules/qr-scanner.js
// Opis: QR scanner dla telefonow/tabletow — kamera + native BarcodeDetector
//       (Chrome/Edge/Android Chrome/Safari iOS 17+). Brak fallback dla
//       przegladrek bez BarcodeDetector — graceful degradation: przycisk
//       "Zeskanuj kamerą" ukryty jak API niedostepne.
// =============================================================================

import { I18n } from '/js/i18n.js';

let _jsqrPromise = null;
function loadJsQR() {
  if (globalThis.jsQR) return Promise.resolve(globalThis.jsQR);
  if (_jsqrPromise) return _jsqrPromise;
  _jsqrPromise = new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = '/js/lib/jsqr.min.js';
    s.async = true;
    s.onload = () => resolve(globalThis.jsQR);
    s.onerror = () => reject(new Error('jsQR fetch failed'));
    document.head.appendChild(s);
  });
  return _jsqrPromise;
}

/**
 * Sprawdza czy można użyć skanera QR — BarcodeDetector (natywne) albo jsQR
 * (canvas pixel scan, fallback dla Firefox/starszych Safari). Wymaga kamery.
 */
export async function isScannerSupported() {
  if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) return false;
  // Preferuj native
  if (typeof BarcodeDetector !== 'undefined') {
    try {
      const formats = await BarcodeDetector.getSupportedFormats();
      if (formats.includes('qr_code')) return true;
    } catch { /* fall through */ }
  }
  // Fallback — jsQR dziala wszedzie, potrzebuje tylko canvas + video.
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

    // Preferuj BarcodeDetector, fallback na jsQR przez canvas pixel read.
    const useNative = typeof BarcodeDetector !== 'undefined';
    let detector = null;
    let jsqrFn = null;
    let canvas = null;
    let ctx = null;

    if (useNative) {
      // eslint-disable-next-line no-undef
      try { detector = new BarcodeDetector({ formats: ['qr_code'] }); } catch { /* fall */ }
    }
    if (!detector) {
      try { jsqrFn = await loadJsQR(); } catch (e) {
        console.warn('[qr-scanner] jsQR load failed:', e);
        cleanup(null);
        return;
      }
      canvas = document.createElement('canvas');
      ctx = canvas.getContext('2d', { willReadFrequently: true });
    }

    const tick = async () => {
      if (closed) return;
      if (video.readyState === video.HAVE_ENOUGH_DATA) {
        try {
          if (detector) {
            const results = await detector.detect(video);
            if (results && results.length > 0) {
              const raw = results[0].rawValue;
              if (raw) { cleanup(raw); return; }
            }
          } else if (jsqrFn && ctx) {
            // jsQR fallback — grab klatke do canvas + scan pixel.
            const w = video.videoWidth;
            const h = video.videoHeight;
            if (w > 0 && h > 0) {
              // Downsample do max 640px dla szybkosci na telefonach.
              const scale = Math.min(1, 640 / Math.max(w, h));
              const cw = Math.floor(w * scale);
              const ch = Math.floor(h * scale);
              if (canvas.width !== cw) canvas.width = cw;
              if (canvas.height !== ch) canvas.height = ch;
              ctx.drawImage(video, 0, 0, cw, ch);
              const imageData = ctx.getImageData(0, 0, cw, ch);
              const code = jsqrFn(imageData.data, cw, ch, { inversionAttempts: 'dontInvert' });
              if (code && code.data) { cleanup(code.data); return; }
            }
          }
        } catch (_e) {
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
