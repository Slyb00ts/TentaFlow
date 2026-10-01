// ============ File: project-attachment-worker.js — Service Worker byte ranges backed by authorized binary reads ============

(() => {
  const CHUNK_BYTES = 512 * 1024;
  const sessions = new Map();
  let nextRequest = 1;

  function byteRange(header, totalSize) {
    if (!header) return { start: 0, end: totalSize - 1, partial: false };
    const match = /^bytes=(\d*)-(\d*)$/.exec(header);
    if (!match || !match[1] && !match[2] || totalSize === 0) return null;
    let start;
    let end;
    if (!match[1]) {
      const suffix = Number(match[2]);
      if (!Number.isSafeInteger(suffix) || suffix < 1) return null;
      start = Math.max(0, totalSize - suffix); end = totalSize - 1;
    } else {
      start = Number(match[1]); end = match[2] ? Math.min(Number(match[2]), totalSize - 1) : totalSize - 1;
    }
    return Number.isSafeInteger(start) && Number.isSafeInteger(end) && start >= 0 && start < totalSize && end >= start ? { start, end, partial: true } : null;
  }

  async function connectClient(client, token) {
    const channel = new MessageChannel();
    const port = channel.port1;
    try {
      const metadata = await new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error('Attachment page is unavailable')), 10000);
        port.onmessage = ({ data }) => { clearTimeout(timeout); if (data.error) reject(new Error(data.error)); else resolve(data); };
        port.onmessageerror = () => { clearTimeout(timeout); reject(new Error('Attachment channel failed')); };
        port.start();
        client.postMessage({ type: 'project-attachment-connect', token }, [channel.port2]);
      });
      if (!Number.isSafeInteger(metadata.totalSize) || metadata.totalSize < 0) throw new Error('Invalid attachment metadata');
      const pending = new Map();
      const session = { ...metadata, port, pending, clientId: client.id };
      port.onmessage = ({ data }) => {
        if (data.type === 'revoked') {
          sessions.delete(token);
          for (const request of pending.values()) request.reject(new Error('Attachment access was closed'));
          pending.clear(); port.close(); return;
        }
        const request = pending.get(data.requestId);
        if (!request) return;
        pending.delete(data.requestId);
        if (data.error) request.reject(new Error(data.error)); else request.resolve(data);
      };
      sessions.set(token, session);
      return session;
    } catch (error) { port.close(); throw error; }
  }

  async function findSession(event, token) {
    const existing = sessions.get(token);
    if (existing && await self.clients.get(existing.clientId)) return existing;
    sessions.delete(token);
    const candidates = await self.clients.matchAll({ type: 'window', includeUncontrolled: false });
    candidates.sort((a, b) => Number(b.id === event.clientId) - Number(a.id === event.clientId));
    for (const client of candidates) {
      try { return await connectClient(client, token); }
      catch { /* A token belongs to one live page; other pages reject it. */ }
    }
    throw new Error('Attachment page is unavailable');
  }

  function read(session, offset, maxBytes) {
    const requestId = nextRequest++;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => { session.pending.delete(requestId); reject(new Error('Attachment read timed out')); }, 35000);
      session.pending.set(requestId, { resolve: (value) => { clearTimeout(timeout); resolve(value); }, reject: (error) => { clearTimeout(timeout); reject(error); } });
      session.port.postMessage({ type: 'read', requestId, offset, maxBytes });
    });
  }

  function finishDownload(token, session, type, error) {
    session.port.postMessage({ type, ...(error ? { error: error.message } : {}) });
    for (const request of session.pending.values()) request.reject(new Error(error?.message || 'Download ended'));
    session.pending.clear();
    sessions.delete(token);
    session.port.close();
  }

  async function respond(event) {
    const url = new URL(event.request.url);
    const token = url.pathname.split('/')[2];
    if (!/^[a-f0-9-]{36}$/.test(token || '') || !['GET', 'HEAD'].includes(event.request.method)) return new Response('Invalid attachment request', { status: 400 });
    let session;
    try { session = await findSession(event, token); }
    catch { return new Response('Attachment page is unavailable', { status: 410, headers: { 'Cache-Control': 'no-store' } }); }
    const range = byteRange(event.request.headers.get('Range'), session.totalSize);
    const headers = new Headers({
      'Content-Type': session.mime || 'application/octet-stream',
      'Accept-Ranges': 'bytes', 'Cache-Control': 'no-store',
      'X-Content-Type-Options': 'nosniff', "Content-Security-Policy": "default-src 'none'; sandbox",
    });
    if (!range) { headers.set('Content-Range', `bytes */${session.totalSize}`); return new Response(null, { status: 416, headers }); }
    headers.set('Content-Length', String(Math.max(0, range.end - range.start + 1)));
    if (range.partial) headers.set('Content-Range', `bytes ${range.start}-${range.end}/${session.totalSize}`);
    const download = url.searchParams.get('download') === '1';
    if (download) headers.set('Content-Disposition', `attachment; filename*=UTF-8''${encodeURIComponent(session.filename || 'attachment').replace(/[!'()*]/g, (char) => '%' + char.charCodeAt(0).toString(16))}`);
    if (event.request.method === 'HEAD') return new Response(null, { status: range.partial ? 206 : 200, headers });
    let offset = range.start;
    let canceled = false;
    const body = new ReadableStream({
      async pull(controller) {
        if (canceled) return;
        if (offset > range.end) { controller.close(); if (download) finishDownload(token, session, 'download-complete'); return; }
        try {
          const length = Math.min(CHUNK_BYTES, range.end - offset + 1);
          const chunk = await read(session, offset, length);
          if (canceled) return;
          const bytes = chunk.bytes instanceof Uint8Array ? chunk.bytes : new Uint8Array(chunk.bytes);
          if (bytes.length !== length || chunk.eof && offset + bytes.length < session.totalSize) throw new Error('Incomplete attachment range');
          offset += bytes.length;
          controller.enqueue(bytes);
          if (offset > range.end) {
            controller.close();
            if (download) finishDownload(token, session, 'download-complete');
          }
        } catch (error) {
          if (!canceled) { controller.error(error); if (download) finishDownload(token, session, 'download-error', error); }
        }
      },
      cancel() { canceled = true; if (download) finishDownload(token, session, 'download-canceled'); },
    }, { highWaterMark: 0 });
    return new Response(body, { status: range.partial ? 206 : 200, headers });
  }

  self.ProjectAttachmentWorker = { respond, byteRange };
})();
