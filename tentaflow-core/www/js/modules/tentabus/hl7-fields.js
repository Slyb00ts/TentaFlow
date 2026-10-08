// ===== File: modules/tentabus/hl7-fields.js — the HL7 v2 fields the data-hiding window offers, by address =====
//
// An HL7 v2 topic has no registry pattern that lists its fields (the profile
// holds the required ones only), so the window of a data-hiding rule offers
// the fields most messages of a clinic carry, by the address the server
// filters on ("PID-5": segment, dash, field number). The names are i18n keys
// (`tentabus.hiding.hl7.<segment>_<number>`), the list is only a convenience:
// any other address can be typed in. MSH-1 and MSH-2 are the message's own
// separators and can never be filtered, so they are not here.
//
// The server lists EVERY position a message carries, empty or not
// (`Hl7V2Format::list_fields`), and a writing rule refuses any position that is
// not allowed. So a writing rule built from the named fields alone would refuse
// the positions nobody named — PID-1, PID-4, PV1-1, EVN-1 and so on — and with
// them every real message. The positions the standard defines for the segments
// below are therefore "implicit" for WRITING rules only: a new rule allows them
// without listing them as rows, a stored rule that leaves one out keeps leaving
// it out. A READING rule never adds them: what it does not list is hidden, as
// its window says, so a position nobody named stays hidden.

import { T } from '/js/modules/tentabus/format.js';

export const HL7_FIELDS = [
  'MSH-3', 'MSH-4', 'MSH-5', 'MSH-6', 'MSH-7', 'MSH-8', 'MSH-9', 'MSH-10', 'MSH-11', 'MSH-12',
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

/**
 * The highest position HL7 v2.8/v2.9 define for each segment; earlier versions
 * define fewer. Where the exact count was not certain the larger known count
 * is used (OBX, whose later positions differ between v2.8 and v2.9, is given
 * a safe upper bound): the bound only widens the allow-list of a writing rule,
 * so a position a message does not have costs nothing, while one too few
 * refuses an ordinary message.
 */
const SEGMENT_WIDTH = {
  MSH: 25, EVN: 7, PID: 39, PD1: 22, NK1: 41, PV1: 54, PV2: 50, ROL: 13, GT1: 57, IN1: 55, IN2: 85,
  ORC: 35, OBR: 53, OBX: 33, DG1: 26, AL1: 6, NTE: 9, SFT: 6, TQ1: 14, SPM: 30, MSA: 6, ERR: 12, MRG: 7,
};

const KNOWN = new Set(HL7_FIELDS);

/** Every position of the segments above that has no row of its own, in segment and number order. */
export const HL7_IMPLICIT_FIELDS = Object.entries(SEGMENT_WIDTH).flatMap(([segment, width]) => {
  const out = [];
  for (let n = segment === 'MSH' ? 3 : 1; n <= width; n += 1) {
    const address = `${segment}-${n}`;
    if (!KNOWN.has(address)) out.push(address);
  }
  return out;
});

/** The i18n key (below `tentabus.`) of a dictionary field's name. */
export function hl7LabelKey(address) {
  return `hiding.hl7.${address.toLowerCase().replace('-', '_')}`;
}

/** The plain name of a dictionary field ("imię i nazwisko pacjenta"), or '' for an address outside the dictionary. */
export function hl7FieldLabel(address) {
  return KNOWN.has(address) ? T(hl7LabelKey(address)) : '';
}
