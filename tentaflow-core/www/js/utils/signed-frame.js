// =============================================================================
// Plik: utils/signed-frame.js
// Opis: Resolver źródeł obrazów SDK (`ImageSource`). Pobiera signed URL klatki
//       kamery przez binarny dispatch `cameraFrameUrlRequest` i cache'uje go
//       z poszanowaniem TTL (margines bezpieczeństwa 60 s przed wygaśnięciem).
//       Wspólne wejście dla Avatar/Image/RadioCard/Canvas image drawcmd.
// Przykład:
//   import { resolveImageSource } from '/js/utils/signed-frame.js';
//   const r = await resolveImageSource({ kind: 'signed_frame', camera_id });
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// Klucz cache: "cameraId:frameRef". `frameRef` puste => "latest".
const URL_CACHE = new Map();

// Margines przed wygaśnięciem — odświeżamy zanim URL umrze w locie.
const SAFETY_MARGIN_MS = 60_000;

// Domyślny TTL żądania (sekundy). Backend ogranicza zakres do 5..300.
const DEFAULT_TTL_SECS = 300;

// Background GC cyklicznie czyści wpisy które wygasły. Trzymamy luźny interwał
// (30 s) — wpisy są niewielkie i tak żyją max 5 min.
const GC_INTERVAL_MS = 30_000;
let _gcTimer = null;

function ensureGcTimer() {
  if (_gcTimer != null) return;
  _gcTimer = setInterval(() => {
    const now = Date.now();
    for (const [key, entry] of URL_CACHE) {
      if (entry.expiresAt <= now) URL_CACHE.delete(key);
    }
  }, GC_INTERVAL_MS);
  // Nie blokuj zamknięcia procesu hostingowego (np. testy node) — w przeglądarce
  // unref nie istnieje na timerze, więc strażujemy `typeof`.
  if (typeof _gcTimer?.unref === 'function') _gcTimer.unref();
}

/**
 * Pobiera signed URL klatki dla wskazanej kamery. Zwraca string albo `null`
 * gdy kamera offline / brak uprawnień / pusta odpowiedź. Inne błędy
 * (sieć, protokół) propagujemy w górę — wywołujący decyduje czy retry.
 */
export async function resolveSignedFrame(signedFrame) {
  if (!signedFrame || !signedFrame.camera_id) return null;
  ensureGcTimer();

  const cacheKey = `${signedFrame.camera_id}:${signedFrame.frame_ref || 'latest'}`;
  const cached = URL_CACHE.get(cacheKey);
  if (cached && cached.expiresAt > Date.now() + SAFETY_MARGIN_MS) {
    return cached.url;
  }

  try {
    const result = await ApiBinary.one('cameraFrameUrlRequest', {
      cameraId: signedFrame.camera_id,
      ttlSecs: DEFAULT_TTL_SECS,
    });
    const url = String(result?.signedUrl ?? result?.signed_url ?? '').trim();
    if (!url) return null;
    const expiresMs = Number(result?.expiresAtMs ?? result?.expires_at_ms);
    const expiresAt = Number.isFinite(expiresMs) && expiresMs > 0
      ? expiresMs
      : Date.now() + DEFAULT_TTL_SECS * 1000;
    URL_CACHE.set(cacheKey, { url, expiresAt });
    return url;
  } catch (e) {
    const code = e?.code;
    // Brak uprawnień albo kamera offline = brak preview, ale nie błąd protokołu.
    if (code === 'permission_denied' || code === 'camera_offline') return null;
    // ProtocolErrorCode::NotFound (9) = brak klatki w LRU. Traktujemy jak
    // tymczasową niedostępność — bez throw, wywołujący pokaże placeholder.
    if (code === 9 || code === 'not_found') return null;
    throw e;
  }
}

/**
 * Normalizuje `ImageSource` SDK do bezpiecznej formy do renderu. Async ze względu
 * na `signed_frame` (wymaga round-tripa po URL). Pozostałe warianty rozwiązują
 * się natychmiast.
 *
 * Zwracane kształty:
 *   { kind: 'url', url }
 *   { kind: 'initials', text, background }
 *   { kind: 'placeholder' }
 */
export async function resolveImageSource(source) {
  if (!source) return { kind: 'placeholder' };
  if (source.kind === 'url' || (source.url && !source.kind)) {
    return { kind: 'url', url: source.url };
  }
  if (source.kind === 'signed_frame') {
    const url = await resolveSignedFrame({
      camera_id: source.camera_id,
      frame_ref: source.frame_ref,
    });
    return url ? { kind: 'url', url } : { kind: 'placeholder' };
  }
  if (source.kind === 'initials') {
    return {
      kind: 'initials',
      text: source.text || '?',
      background: source.background,
    };
  }
  return { kind: 'placeholder' };
}

/** Tylko do testów — opróżnia cache. Nie używaj w runtime. */
export function _clearSignedFrameCache() {
  URL_CACHE.clear();
}
