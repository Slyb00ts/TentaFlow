// =============================================================================
// File: tests/e2e/helpers/static-www-server.js
// Description: Tiny zero-dep static HTTP server used by frontend-only e2e
//              specs (no tentaflow binary). Serves files from a configurable
//              root with correct MIME for HTML / JS / CSS so ES module
//              imports resolve in Playwright pages.
// =============================================================================

const http = require('http');
const fs = require('fs');
const path = require('path');

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js':   'application/javascript; charset=utf-8',
  '.mjs':  'application/javascript; charset=utf-8',
  '.css':  'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg':  'image/svg+xml',
  '.png':  'image/png',
  '.ico':  'image/x-icon',
};

function serve({ root, port = 0 }) {
  const server = http.createServer((req, res) => {
    let url = decodeURIComponent(req.url.split('?')[0]);
    if (url === '/' || url === '') url = '/index.html';
    const filePath = path.join(root, url);
    if (!filePath.startsWith(root)) {
      res.writeHead(403); res.end('forbidden'); return;
    }
    fs.readFile(filePath, (err, data) => {
      if (err) { res.writeHead(404); res.end('not found: ' + url); return; }
      const ext = path.extname(filePath).toLowerCase();
      res.writeHead(200, { 'content-type': MIME[ext] || 'application/octet-stream' });
      res.end(data);
    });
  });
  return new Promise((resolve) => {
    server.listen(port, '127.0.0.1', () => {
      const actual = server.address().port;
      resolve({ server, port: actual, baseUrl: `http://127.0.0.1:${actual}` });
    });
  });
}

function stop(handle) {
  return new Promise((resolve) => {
    if (!handle || !handle.server) return resolve();
    handle.server.close(() => resolve());
  });
}

module.exports = { serve, stop };
