// =============================================================================
// File: modules/access-keys-scopes.js — scope ids for API-key grants that carry
//       an action (the schema registry REST, "Wzory wiadomości")
// =============================================================================
// A `bus_schema_registry` grant is stored under the same composite id the REST
// gate rebuilds from the request path and `?org_id=` (`sync::resource_id::
// composite_resource_id([instance_id, org_id])` in Rust): every part written as
// `<UTF-8 byte length><U+001F><part>`. Read and write are separate grants, so a
// scope's identity on screen is (type, id, action), not (type, id).
// No DOM or wire dependencies here, so the encoding can be tested on its own.

export const BUS_SCHEMA_REGISTRY = 'bus_schema_registry';
export const BUS_SCHEMA_ACTIONS = ['read', 'write'];

const SEP = '\u001f';
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });

/** Composite scope id for one TentaBus instance and one organisation. */
export function busSchemaScopeId(instanceId, orgId) {
  return [instanceId, orgId]
    .map((part) => `${encoder.encode(part).length}${SEP}${part}`)
    .join('');
}

/**
 * Inverse of `busSchemaScopeId`: `{ instanceId, orgId }`, or `null` for
 * anything that is not exactly two well-formed parts (a label must never be
 * built from a guess).
 */
export function parseBusSchemaScopeId(id) {
  const bytes = encoder.encode(String(id ?? ''));
  const parts = [];
  let pos = 0;
  while (pos < bytes.length) {
    const sepAt = bytes.indexOf(0x1f, pos);
    if (sepAt < 0) return null;
    const lenText = decoder.decode(bytes.subarray(pos, sepAt));
    if (!/^\d+$/.test(lenText)) return null;
    const start = sepAt + 1;
    const end = start + Number(lenText);
    if (end > bytes.length) return null;
    try {
      parts.push(decoder.decode(bytes.subarray(start, end)));
    } catch {
      return null;
    }
    pos = end;
  }
  if (parts.length !== 2 || !parts[0] || !parts[1]) return null;
  return { instanceId: parts[0], orgId: parts[1] };
}

/**
 * Key a grant is filed under on screen: action-blind grants by (type, id),
 * action-bearing ones by (type, id, action) so read and write never overwrite
 * each other in a lookup map.
 */
export function scopeKey(resourceType, resourceId, action) {
  const base = `${resourceType}:${resourceId}`;
  return action && action !== '*' ? `${base}#${action}` : base;
}

/**
 * Human names for schema-registry scope ids, as a Map from id to
 * `{ instance, org }`. Falls back to the given "removed" texts — never to the
 * raw ids — when an instance or organisation is no longer known. Several
 * different removed instances (or organisations) get a running number, so two
 * matrix headers never read the same while meaning different grants.
 */
export function busSchemaScopeNames(scopeIds, instances, orgs, unknown) {
  const parsed = new Map(scopeIds.map((id) => [id, parseBusSchemaScopeId(id)]));
  const numbering = (pick, known) => {
    const missing = [...new Set([...parsed.values()]
      .map((p) => (p ? pick(p) : null))
      .filter((v) => v !== null && !known(v)))].sort();
    return (value) => (missing.length > 1 ? ` (${missing.indexOf(value) + 1})` : '');
  };
  const instanceSuffix = numbering((p) => p.instanceId, (v) => instances.some((i) => i.addonId === v));
  const orgSuffix = numbering((p) => p.orgId, (v) => orgs.some((o) => o.orgId === v));
  const names = new Map();
  for (const [id, p] of parsed) {
    const instance = p && instances.find((i) => i.addonId === p.instanceId);
    const org = p && orgs.find((o) => o.orgId === p.orgId);
    names.set(id, {
      instance: instance?.title || `${unknown.instance}${p ? instanceSuffix(p.instanceId) : ''}`,
      org: org?.name || `${unknown.org}${p ? orgSuffix(p.orgId) : ''}`,
    });
  }
  return names;
}
