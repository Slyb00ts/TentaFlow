// =============================================================================
// Plik: lib/qrcode.js
// Opis: ESM wrapper dla qrcode-generator (Kazuhiko Arase, MIT). Laduje pure-JS
//       encoder do globalu raz, potem eksportuje funkcje helperow.
// =============================================================================

let loadingPromise = null;

function loadScript() {
  if (globalThis.qrcode) return Promise.resolve();
  if (loadingPromise) return loadingPromise;
  loadingPromise = new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = '/js/lib/qrcode-generator.min.js';
    s.async = true;
    s.onload = () => resolve();
    s.onerror = () => reject(new Error('Nie udalo sie zaladowac qrcode-generator'));
    document.head.appendChild(s);
  });
  return loadingPromise;
}

/**
 * Generuje QR code jako SVG string. typeNumber=0 = auto (najmniejszy pasujacy).
 * errorCorrectionLevel: 'L' (7%), 'M' (15%), 'Q' (25%), 'H' (30%).
 */
export async function renderQrSvg(text, {
  size = 220,
  margin = 4,
  errorCorrectionLevel = 'M',
  typeNumber = 0,
  color = '#000000',
  bg = '#ffffff',
} = {}) {
  await loadScript();
  const qr = globalThis.qrcode(typeNumber, errorCorrectionLevel);
  qr.addData(text);
  qr.make();
  const moduleCount = qr.getModuleCount();
  const cellSize = (size - margin * 2) / moduleCount;
  const total = size;
  let rects = '';
  for (let r = 0; r < moduleCount; r += 1) {
    for (let c = 0; c < moduleCount; c += 1) {
      if (qr.isDark(r, c)) {
        const x = (margin + c * cellSize).toFixed(2);
        const y = (margin + r * cellSize).toFixed(2);
        const sz = cellSize.toFixed(2);
        rects += `<rect x="${x}" y="${y}" width="${sz}" height="${sz}" fill="${color}"/>`;
      }
    }
  }
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${total} ${total}" width="${total}" height="${total}"><rect width="${total}" height="${total}" fill="${bg}"/>${rects}</svg>`;
}
