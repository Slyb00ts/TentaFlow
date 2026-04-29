// =============================================================================
// Plik: protocol/profiling.js
// Opis: Wysokopoziomowe helpery dla 7 par request/response multi-source
//       profilingu (V2). Buduja frame przez codec.encode.profiling*Request,
//       dispatchuja przez ApiBinary i zwracaja zdekodowany body. Field names
//       zwracane przez wasm-glue sa juz w camelCase (sessionId, entries,
//       tarballBytes, envelope, info, ...). `scope` to obiekt zgodny z
//       `ProfileScope`:
//         {
//           sources: u32 (bitmask ProfileSourceFlags),
//           gpuTargets: 'all' | 'none' | { indices: number[] }
//                                        | { byVendor: 'nvidia'|'amd'|... },
//           cpuSamplingHz: u32,
//           target: 'system_wide' | 'own_process' | { pid: u32 },
//           durationSeconds: u32 (0 == manual stop),
//           label: string,
//         }
// =============================================================================

import { ApiBinary } from './api-binary-shim.js';

/**
 * Start nowej sesji multi-source profilingu.
 *
 * @param {object} args
 * @param {string} args.nodeId — endpoint id nodu, na ktorym ma sie uruchomic
 *                               sesja (dla local node mozna podac wlasny id).
 * @param {object} args.scope — patrz naglowek pliku (ProfileScope).
 * @param {string} args.label — przyjazna etykieta sesji (max 128 chars,
 *                              bez control chars; walidowane po stronie backend).
 * @param {string|null=} args.elevationPassword — sudo/admin password gdy
 *                              ktorykolwiek collector wymaga elevation; `null`
 *                              jesli nie podano.
 * @returns {Promise<{
 *   sessionId: string,
 *   startedAtUnixNs: number,
 *   collectorsStarted: string[],
 *   collectorsSkipped: Array<{id: string, reason: string}>,
 * }>}
 */
export async function profilingStart({ nodeId, scope, label, elevationPassword }) {
  return ApiBinary.one('profilingStartRequest', {
    nodeId,
    scope,
    label,
    elevationPassword: elevationPassword == null ? null : elevationPassword,
  });
}

/**
 * Wczesniejsze zatrzymanie aktywnej sesji.
 *
 * @returns {Promise<{ sessionId: string, report: object }>} zwraca pelen
 *          zaparsowany ProfileReportV2 jezeli sesja zostala domkniecia.
 */
export async function profilingStop({ nodeId, sessionId }) {
  return ApiBinary.one('profilingStopRequest', { nodeId, sessionId });
}

/**
 * Lista sesji widocznych na nodzie (FIFO). Kazda pozycja:
 *   { sessionId, label, startedAt: string, durationNs, kind,
 *     collectorsUsed: string[], sizeBytes }.
 *
 * @returns {Promise<{ nodeId: string, entries: Array<object> }>}
 */
export async function profilingSessions({ nodeId }) {
  return ApiBinary.one('profilingSessionsRequest', { nodeId });
}

/**
 * Pobranie zaparsowanego raportu sesji. Envelope decyduje o wersji:
 *   { kind: 'v1_legacy' | 'v2', report: object }.
 *
 * @returns {Promise<{ envelope: { kind: string, report: object } }>}
 */
export async function profilingReport({ nodeId, sessionId }) {
  return ApiBinary.one('profilingReportRequest', { nodeId, sessionId });
}

/**
 * Usuniecie sesji (raw + parsed) na nodzie.
 *
 * @returns {Promise<{ sessionId: string, deleted: boolean }>}
 */
export async function profilingDelete({ nodeId, sessionId }) {
  return ApiBinary.one('profilingDeleteRequest', { nodeId, sessionId });
}

/**
 * Pobranie tarballu (.tar.gz) z manifest.json + summary.bin + raw/.
 *
 * @returns {Promise<{ sessionId: string, filename: string, tarballBytes: Uint8Array }>}
 */
export async function profilingDownload({ nodeId, sessionId }) {
  return ApiBinary.one('profilingDownloadRequest', { nodeId, sessionId });
}

/**
 * Info o aktywnej sesji na nodzie. `info: null` = brak aktywnej sesji.
 * Aktywne info zawiera m.in. plannedDurationNs / elapsedNs / collectorsRunning.
 *
 * @returns {Promise<{ info: object | null }>}
 */
export async function profilingActiveInfo({ nodeId }) {
  return ApiBinary.one('profilingActiveInfoRequest', { nodeId });
}

/**
 * Walidacja sudo password (bez utrwalania) przez binary protocol.
 * Reason tags: ok | bad_password | no_sudo | timeout | empty | in_progress |
 * spawn_error.
 *
 * @returns {Promise<{ ok: boolean, message: string, reason: string }>}
 */
export async function profilingValidateSudo({ nodeId, password }) {
  return ApiBinary.one('profilingValidateSudoRequest', {
    nodeId: nodeId ?? '',
    password,
  });
}

/**
 * Status kolektorow + odkryte sciezki binarnek (cache 5s).
 *
 * @returns {Promise<{ collectors: Array<object>, ageSeconds: number }>}
 */
export async function profilingCollectorsStatus({ nodeId } = {}) {
  return ApiBinary.one('profilingCollectorsStatusRequest', { nodeId: nodeId ?? '' });
}
