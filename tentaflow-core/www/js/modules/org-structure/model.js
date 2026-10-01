// =============================================================================
// File: modules/org-structure/model.js
// Description: Pure read-model of an `OrgStructureBody` structure answer: the
//   numbers the org-structure screen draws. No DOM and no
//   transport, so the rules (who counts as a person) are tested without a page. Wire fields are read in their snake_case
//   spelling — the wasm decoder emits both spellings, and this module picks one.
// =============================================================================

/** Stable identity of a person on the wire: a platform account or an external one. */
export function subjectKey(subject) {
  return `${subject?.kind ?? ''}:${subject?.id ?? ''}`;
}

/** The structure's headline numbers. A person holding two positions counts once. */
export function summarize(view) {
  const people = new Set((view.assignments ?? []).map((a) => subjectKey(a.subject)));
  return {
    units: (view.units ?? []).length,
    positions: (view.positions ?? []).length,
    people: people.size,
    vacancies: (view.vacancies ?? []).length,
  };
}

/** A warning as one sentence, named by the unit or person it is about. */
export function warningText(warning, view, t) {
  const names = new Map((view.assignments ?? []).map((a) => [subjectKey(a.subject), a.display_name]));
  const person = () => names.get(subjectKey(warning.subject)) || t('unknown_person');
  switch (warning.kind) {
    case 'unit_without_head': {
      const unit = (view.units ?? []).find((u) => u.unit_id === warning.unit_id);
      return t('warning_unit_without_head', { unit: unit?.name ?? t('unknown_unit') });
    }
    case 'share_overbooked':
      return t('warning_share_overbooked', { person: person(), total: Number(warning.total.toFixed(2)) });
    case 'person_without_primary':
      return t('warning_person_without_primary', { person: person() });
    default:
      // A kind a newer server sends: say there is something, never the raw code.
      return t('warning_unknown');
  }
}
