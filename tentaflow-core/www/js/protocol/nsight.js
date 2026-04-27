// =============================================================================
// Plik: protocol/nsight.js
// Opis: Wysokopoziomowe helpery dla 5 par request/response Nsight Systems.
//       Buduja frame przez codec.encode.nsight*Request, dispatchuja przez
//       ApiBinary i zwracaja zdekodowany body z `decodeMessageBody`.
// Przyklad:
//   import { nsightStart } from '/js/protocol/nsight.js';
//   const { sessionId, startedAtMs } = await nsightStart({
//     nodeId, scope: 'gpu_all', durationSecs: 30, label: 'vllm-cold-start',
//   });
// =============================================================================

import { ApiBinary } from './api-binary-shim.js';

/**
 * Start nowej sesji profilowania `nsys`.
 *
 * @param {object} args
 * @param {string} args.nodeId — endpoint id nodu (hex/base32) na ktorym ma sie
 *                               uruchomic profiler.
 * @param {string|object} args.scope — zakres zbierania danych:
 *                       'cpu' | 'gpu_all' | 'both_all' albo
 *                       { kind: 'gpu_index'|'both_index', idx: <u8> }.
 * @param {number} args.durationSecs — auto-stop timeout (s).
 * @param {string} args.label — przyjazna etykieta sesji.
 * @returns {Promise<{ sessionId: string, startedAtMs: number }>}
 */
export async function nsightStart({ nodeId, scope, durationSecs, label }) {
  const body = await ApiBinary.one('nsightStartRequest', {
    nodeId,
    scope,
    durationSecs,
    label,
  });
  return body;
}

/**
 * Wczesniejsze zatrzymanie biezacej sesji.
 *
 * @returns {Promise<{ sessionId: string, status: string }>}
 *          status: 'Running' | 'Stopping' | 'Done' | 'Failed'.
 */
export async function nsightStop({ nodeId, sessionId }) {
  return ApiBinary.one('nsightStopRequest', { nodeId, sessionId });
}

/**
 * Lista wszystkich sesji widocznych na nodzie.
 *
 * @returns {Promise<{ nodeId: string, sessions: Array<object> }>}
 *          Kazda pozycja ma: sessionId, label, scope, status, startedAtMs,
 *          durationMs, sizeBytes, error?.
 */
export async function nsightSessions({ nodeId }) {
  return ApiBinary.one('nsightSessionsRequest', { nodeId });
}

/**
 * Pobranie sparsowanego raportu (.nsys-rep -> ProfileReport).
 *
 * @returns {Promise<{ report: object }>}
 *          report: { meta, kpi, gpuKernelsTop, cudaApiTop, gpuMemOps,
 *                    cpuSamplesTop, nvtxRangesTop, gpuUtilTimeline }.
 */
export async function nsightReport({ nodeId, sessionId }) {
  return ApiBinary.one('nsightReportRequest', { nodeId, sessionId });
}

/**
 * Usuniecie zapisanego raportu i metadanych sesji.
 *
 * @returns {Promise<{ sessionId: string, ok: boolean }>}
 */
export async function nsightDelete({ nodeId, sessionId }) {
  return ApiBinary.one('nsightDeleteRequest', { nodeId, sessionId });
}

/**
 * Pobranie surowego pliku `.nsys-rep` (binary blob) — do otwarcia w nsys-ui.
 *
 * @returns {Promise<{ sessionId: string, filename: string, bytes: Uint8Array }>}
 */
export async function nsightDownload({ nodeId, sessionId }) {
  return ApiBinary.one('nsightDownloadRequest', { nodeId, sessionId });
}
