// ===== File: modules/tentabus/hl7-fields.js — the HL7 v2 fields the data-hiding window offers, by address =====
//
// An HL7 v2 topic has no registry pattern that lists its fields (the profile
// holds the required ones only), so the window of a data-hiding rule offers
// the fields most messages of a clinic carry, by the address the server
// filters on ("PID-5": segment, dash, field number). The names are i18n keys
// (`tentabus.hiding.hl7.<segment>_<number>`), the list is only a convenience:
// any other address can be typed in. MSH-1 and MSH-2 are the message's own
// separators and can never be filtered, so they are not here.

import { T } from '/js/modules/tentabus/format.js';

export const HL7_FIELDS = [
  'MSH-3', 'MSH-4', 'MSH-5', 'MSH-6', 'MSH-7', 'MSH-9', 'MSH-10',
  'PID-2', 'PID-3', 'PID-5', 'PID-6', 'PID-7', 'PID-8', 'PID-10', 'PID-11', 'PID-13', 'PID-14', 'PID-15',
  'PID-16', 'PID-17', 'PID-18', 'PID-19', 'PID-22', 'PID-23', 'PID-29', 'PID-30',
  'PV1-2', 'PV1-3', 'PV1-7', 'PV1-8', 'PV1-9', 'PV1-10', 'PV1-17', 'PV1-19', 'PV1-44', 'PV1-45',
  'NK1-2', 'NK1-3', 'NK1-4', 'NK1-5',
  'IN1-2', 'IN1-3', 'IN1-4', 'IN1-16', 'IN1-36',
  'ORC-2', 'ORC-12',
  'OBR-4', 'OBR-7', 'OBR-16',
  'OBX-2', 'OBX-3', 'OBX-5', 'OBX-6', 'OBX-7', 'OBX-8', 'OBX-11', 'OBX-14',
  'DG1-3', 'DG1-4',
  'AL1-3',
];

const KNOWN = new Set(HL7_FIELDS);

/** The i18n key (below `tentabus.`) of a dictionary field's name. */
export function hl7LabelKey(address) {
  return `hiding.hl7.${address.toLowerCase().replace('-', '_')}`;
}

/** The plain name of a dictionary field ("imię i nazwisko pacjenta"), or '' for an address outside the dictionary. */
export function hl7FieldLabel(address) {
  return KNOWN.has(address) ? T(hl7LabelKey(address)) : '';
}
