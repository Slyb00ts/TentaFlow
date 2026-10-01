// ===== File: lib/download.js — handing a file to the browser as a download, shared by every screen =====
//
// Two kinds of download exist: a blob the screen generated itself
// (`downloadText`) and a URL the server minted (`downloadUrl`) — a signed
// URL must NOT be re-fetched here, its token is spent by the one request.

/** Hands one URL to the browser as a download under `filename`. */
export function downloadUrl(url, filename) {
  const link = document.createElement('a');
  link.href = url;
  link.download = filename;
  link.hidden = true;
  document.body.appendChild(link);
  link.click();
  link.remove();
}

/** Saves generated text under `filename`; the object URL is released once the click was dispatched. */
export function downloadText(filename, text, mime = 'text/plain') {
  const url = URL.createObjectURL(new Blob([text], { type: `${mime};charset=utf-8` }));
  downloadUrl(url, filename);
  URL.revokeObjectURL(url);
}

/** Saves bytes a server produced (a CSV or XLSX export) under `filename`; the object URL is released after the click. */
export function downloadBytes(filename, bytes, mime = 'application/octet-stream') {
  const url = URL.createObjectURL(new Blob([bytes], { type: mime }));
  downloadUrl(url, filename);
  URL.revokeObjectURL(url);
}
