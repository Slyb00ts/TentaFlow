// ===== File: modules/tentaquant/files.js — project files on the CAS wire =====
//
// The upload path, the file-kind vocabulary and the one way this app hands a
// file to the browser, kept apart from the views because the circuit Studio
// saves a `.qasm` through exactly the same request the Pliki tab uses for a
// drag-and-drop, and a run artifact is saved through the same anchor as a
// generated export.

// The five `FileInfo.kind` values the wire defines.
const FILE_KINDS = ['notebook', 'py', 'qasm', 'data', 'md'];

// The chunk size the handler expects; a bigger frame is refused, a smaller one
// only costs round trips.
const CHUNK_BYTES = 4 * 1024 * 1024;

const KIND_BY_EXTENSION = {
  qasm: 'qasm', oq3: 'qasm', py: 'py', md: 'md', ipynb: 'notebook',
};

export function fileKindOf(path) {
  const ext = String(path || '').toLowerCase().split('.').pop();
  return KIND_BY_EXTENSION[ext] || 'data';
}

/// What a row shows: the kind the server stored, falling back to the extension
/// for a row whose kind this build does not know.
export function fileKindLabel(file) {
  const kind = String((file && file.kind) || '');
  return FILE_KINDS.includes(kind) ? kind : fileKindOf(file && file.path);
}

/// Sends one file into the project CAS. Chunks are numbered and must arrive in
/// order, and `seq === 0` restarts the stream, so one upload is one id and the
/// loop never runs two of them at once.
export async function uploadFile(screen, path, bytes) {
  const uploadId = `up-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  const total = Math.max(1, Math.ceil(bytes.length / CHUNK_BYTES));
  let last = null;
  for (let seq = 0; seq < total; seq += 1) {
    // Sequential on purpose: the server rebuilds the file from the stream in
    // arrival order, so two chunks in flight would corrupt it.
    last = await screen.tq('tentaQuantFileUploadChunkRequest', {
      projectId: screen.projectId,
      uploadId,
      path,
      kind: fileKindOf(path),
      seq,
      totalChunks: total,
      bytes: bytes.subarray(seq * CHUNK_BYTES, Math.min((seq + 1) * CHUNK_BYTES, bytes.length)),
    });
  }
  return last;
}

/// Hands one URL to the browser as a download. Used for both kinds this app
/// produces: a blob the screen generated (`downloadText`) and a signed URL the
/// server minted for a run artifact, which must NOT be re-fetched here — the
/// token is one URL, and a fetch-then-blob would spend it twice.
export function downloadUrl(url, filename) {
  const link = document.createElement('a');
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
}

/// Saves generated text (a `.qasm`, a Qiskit program, an SVG) under `filename`.
export function downloadText(filename, text, mime) {
  const url = URL.createObjectURL(new Blob([text], { type: `${mime};charset=utf-8` }));
  downloadUrl(url, filename);
  URL.revokeObjectURL(url);
}
