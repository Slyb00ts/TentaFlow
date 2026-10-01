// =============================================================================
// File: modules/org-structure/export.js
// Description: Export of the organization chart: an SVG file, a PNG rendered
//   from that SVG, and a PDF made by printing pages of it (one page per unit,
//   the "as of" date in every footer). Everything runs in the browser from the
//   markup tf-org-tree generates; the server renders nothing. Documents are
//   always light: they are for paper and slides, not for the dark dashboard.
// =============================================================================

import { downloadUrl } from '/js/lib/download.js';
import { esc } from '/js/modules/org-structure/render.js';

// A canvas beyond these bounds fails silently in some browsers (a blank PNG), so a big chart
// is exported at a lower scale rather than at a size the browser cannot hold.
const MAX_SIDE = 8192;
const MAX_PIXELS = 60_000_000;

function saveBlob(blob, fileName) {
  const url = URL.createObjectURL(blob);
  downloadUrl(url, fileName);
  URL.revokeObjectURL(url);
}

/** Saves the whole chart as a standalone SVG. Returns false when there is nothing to export. */
export function exportSvg(tree, fileName, options = {}) {
  const out = tree.toSvg({ theme: 'light', ...options });
  if (!out) return false;
  saveBlob(new Blob([out.svg], { type: 'image/svg+xml;charset=utf-8' }), fileName);
  return true;
}

function loadImage(src) {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error('svg-decode'));
    image.src = src;
  });
}

/** Saves the whole chart as a PNG at up to 2x. Rejects when the browser cannot decode or encode it. */
export async function exportPng(tree, fileName, options = {}) {
  const out = tree.toSvg({ theme: 'light', ...options });
  if (!out) return false;
  const scale = Math.min(2, MAX_SIDE / Math.max(out.width, out.height), Math.sqrt(MAX_PIXELS / (out.width * out.height)));
  const svgUrl = URL.createObjectURL(new Blob([out.svg], { type: 'image/svg+xml;charset=utf-8' }));
  try {
    const image = await loadImage(svgUrl);
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.floor(out.width * scale));
    canvas.height = Math.max(1, Math.floor(out.height * scale));
    canvas.getContext('2d').drawImage(image, 0, 0, canvas.width, canvas.height);
    const blob = await new Promise((resolve) => canvas.toBlob(resolve, 'image/png'));
    if (!blob) throw new Error('png-encode');
    saveBlob(blob, fileName);
    return true;
  } finally {
    URL.revokeObjectURL(svgUrl);
  }
}

/**
 * The document printed to PDF: `pages` are `{ title, svg }`, `footer` is the text under
 * every page, `paper` is 'A4' or 'A3' (landscape). The chart of a page is scaled to the page by CSS, so an A3 sheet works as well.
 */
export function printDocument(pages, { title, footer, paper = 'A4' }) {
  const sections = pages.map((page) => `<section class="page"><h1>${esc(page.title)}</h1>`
    + `<div class="chart">${page.svg}</div><footer>${esc(footer)}</footer></section>`).join('');
  return `<!doctype html><html><head><meta charset="utf-8"><title>${esc(title)}</title><style>`
    + `@page{size:${paper === 'A3' ? 'A3' : 'A4'} landscape;margin:12mm}`
    + 'html,body{margin:0;background:#fff;color:#0f172a;font-family:Manrope,Inter,system-ui,sans-serif}'
    + `.page{height:calc(${paper === 'A3' ? 297 : 210}mm - 24mm);display:flex;flex-direction:column;break-after:page}`
    + '.page:last-child{break-after:auto}'
    + 'h1{margin:0 0 4mm;font-size:14pt;font-weight:800}'
    + '.chart{flex:1;min-height:0}.chart svg{display:block;width:100%;height:100%}'
    + 'footer{margin-top:3mm;padding-top:2mm;border-top:1px solid #cbd5e1;font-size:9pt;color:#64748b}'
    + `</style></head><body>${sections}</body></html>`;
}

/** Opens the browser's print dialog on the pages; the user picks "Save as PDF". */
export function printPages(pages, options) {
  // afterprint does not fire everywhere (some browsers close the dialog silently), so a frame
  // left behind by an earlier print is removed here rather than by a timer.
  document.querySelector('.org-print-frame')?.remove();
  const frame = document.createElement('iframe');
  frame.setAttribute('aria-hidden', 'true');
  frame.className = 'org-print-frame';
  document.body.appendChild(frame);
  const cleanup = () => frame.remove();
  frame.addEventListener('load', () => {
    const win = frame.contentWindow;
    win.addEventListener('afterprint', cleanup);
    win.focus();
    win.print();
  }, { once: true });
  frame.srcdoc = printDocument(pages, options);
}
