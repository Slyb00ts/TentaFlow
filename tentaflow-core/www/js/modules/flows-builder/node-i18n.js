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

export function getNodeDisplayTitle(node, template = null) {
  if (!node) return '';
  if (!isAutoNodeLabel(node.label, node.type, template?.label)) {
    return node.label;
  }
  return getNodeName(node.type, template?.label);
}
