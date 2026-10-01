// =============================================================================
// File: modules/org-structure/import-i18n.test.js
// Description: Every code the import and the structure writes can answer with
//   has a sentence in all five locales — a file error, an issue kind, a
//   warning kind, a rule of the structure — so the "this version cannot
//   describe it" fallback never shows for a code the server really sends.
//   The lists are the codes of services/org_structure/import/report.rs and
//   error.rs; a code added there must be added here and translated.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const WWW_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const bundles = Object.fromEntries(LOCALES.map((l) => [l, JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${l}.json`), 'utf8')).org_structure]));

const FILE_ERRORS = [
  'file_too_large', 'file_expands_too_large', 'too_many_rows', 'unreadable_file', 'sheet_too_large',
  'invalid_encoding', 'empty_file', 'missing_column', 'duplicate_column',
];
const ISSUES = [
  'invalid_boolean', 'invalid_number', 'invalid_share', 'invalid_date', 'value_too_long', 'missing_unit_code',
  'missing_position_code', 'missing_unit_name', 'missing_position_name', 'conflicting_values', 'duplicate_assignment',
  'ambiguous_code', 'code_reserved', 'unknown_unit_type', 'unknown_person', 'ambiguous_person', 'missing_parent_unit',
  'unit_cycle', 'missing_manager', 'reporting_cycle', 'staff_manager', 'two_heads', 'head_is_deputy',
  'duplicate_deputy_order', 'two_primary_positions', 'position_unit_mismatch', 'backdated_confirmation_required',
  'resolution_not_applicable', 'replace_matches_nothing', 'ended_confirmation_required', 'rejected',
  'share_overbooked', 'unit_without_head', 'person_without_primary', 'position_has_other_holder', 'unknown_column',
  'other_sheets_ignored',
];
const RULES = [
  'not_found', 'invalid_date', 'invalid_interval', 'empty_field', 'invalid_value', 'invalid_timezone',
  'backdated_confirmation_required', 'not_valid_at', 'outside_validity', 'primary_line_overlap',
  'duplicate_functional_line', 'reporting_cycle', 'unit_cycle', 'staff_position_cannot_manage', 'assignment_overlap',
  'primary_assignment_overlap', 'unit_not_empty', 'position_has_subordinates', 'position_is_head',
  'position_heads_another_unit', 'head_is_deputy', 'position_not_in_unit', 'unit_type_in_use', 'duplicate', 'internal',
];
const COLUMNS = [
  'unit_code', 'unit_name', 'parent_code', 'unit_type', 'position_code', 'position', 'staff', 'head', 'primary',
  'person', 'person_name', 'email', 'share', 'manager', 'deputy_order', 'from',
];

const text = (locale, group, key) => bundles[locale][group]?.[key];

for (const locale of LOCALES) {
  test(`${locale}: every file error, issue, rule and column has a sentence`, () => {
    const missing = [
      ...FILE_ERRORS.map((c) => ['import', `file_error_${c}`]),
      ...ISSUES.map((c) => ['import', `issue_${c}`]),
      ...COLUMNS.map((c) => ['import', `col_${c}`]),
      ...RULES.map((c) => ['list', null, c]),
    ].filter(([group, key, rule]) => {
      const value = rule ? bundles[locale].list.errors?.[rule] : text(locale, group, key);
      return typeof value !== 'string' || !value.trim();
    });
    assert.deepEqual(missing, []);
  });
}
