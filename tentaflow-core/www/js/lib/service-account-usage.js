// ===== File: lib/service-account-usage.js — the warning shown before an API key that flows read under goes away =====
//
// A general API key can be the service account of `bus_consume` flows: they
// read the bus as that key. Revoking, disabling or deleting it silently stops
// them, so the confirmation names them first. The server answers with the
// active flows of the caller's organisation (`ServiceAccountUsageRequest`).
//
// Pure: the request and the translator are injected, so the wording and the
// failure handling are testable without a browser.

/** Flow names listed before the rest collapses into "+N". */
export const NAMES_SHOWN = 5;

function namesOf(flows) {
  const names = flows.map((f) => String(f?.name || f?.flowId || '')).filter(Boolean);
  if (names.length <= NAMES_SHOWN) return names.join(', ');
  return `${names.slice(0, NAMES_SHOWN).join(', ')}, +${names.length - NAMES_SHOWN}`;
}

/** The warning line for `flows`, or `null` when no flow reads under the key. */
export function usageWarning(flows, t) {
  if (!Array.isArray(flows) || flows.length === 0) return null;
  return t('access_keys.revoke_service_account_warning', {
    count: flows.length,
    names: namesOf(flows),
  });
}

/**
 * The text of the revoke confirmation. `loadUsage(keyUid)` resolves to the
 * flows reading under the key. When it fails the question is still asked, with
 * a line saying the check did not happen — silence would read as "no flow uses
 * this key".
 */
export async function revokeConfirmMessage(keyUid, { baseMessage, loadUsage, t }) {
  const lines = [baseMessage];
  try {
    const warning = usageWarning(await loadUsage(keyUid), t);
    if (warning) lines.push(warning);
  } catch (_) {
    lines.push(t('access_keys.revoke_usage_unknown', null));
  }
  return lines.join('\n\n');
}
