// =============================================================================
// Plik: modules/flows-builder/node-i18n.js
// Opis: Pomocnicze funkcje tlumaczen nazw i opisow blokow Flow Buildera oraz
//       rozpoznawanie domyslnych etykiet zapisanych w danych flow.
// =============================================================================

import { I18n } from '/js/i18n.js';

export function getNodeName(nodeType, fallbackLabel = '') {
  const key = `flows.node_names.${nodeType}`;
  const translated = I18n.t(key);
  return translated !== key ? translated : (fallbackLabel || nodeType);
}

export function getNodeDescription(nodeType, fallbackDescription = '') {
  const key = `flows.node_descriptions.${nodeType}`;
  const translated = I18n.t(key);
  return translated !== key ? translated : (fallbackDescription || '');
}

export function isAutoNodeLabel(label, nodeType, fallbackLabel = '') {
  const value = String(label || '').trim();
  if (!value) return true;
  if (value === nodeType) return true;
  if (fallbackLabel && value === String(fallbackLabel).trim()) return true;

  const key = `flows.node_names.${nodeType}`;
  const translated = I18n.t(key);
  return translated !== key && value === translated;
}

// Blok, ktorego konfiguracja NAZYWA to, co uruchamia, przedstawia sie lepiej niz
// jego typ: trzy bloki `spawn` to trzy rozni agenci, a trzykrotne "Uruchom
// subagenta" nie mowi o nich nic. Typ zostaje na nodzie (ikona, kolor, znacznik)
// i w podtytule inspektora, wiec nic nie ginie.
const IDENTITY_CONFIG_KEY = { spawn: 'agent_name', await_subagents: 'run_ids_var' };

/** Nazwa z konfiguracji bloku albo pusty string, gdy blok jej nie niesie. */
export function getNodeIdentity(node) {
  const key = IDENTITY_CONFIG_KEY[node?.type];
  if (!key) return '';
  const value = node?.config?.[key];
  return typeof value === 'string' ? value.trim() : '';
}

export function getNodeDisplayTitle(node, template = null) {
  if (!node) return '';
  if (!isAutoNodeLabel(node.label, node.type, template?.label)) {
    return node.label;
  }
  return getNodeIdentity(node) || getNodeName(node.type, template?.label);
}
