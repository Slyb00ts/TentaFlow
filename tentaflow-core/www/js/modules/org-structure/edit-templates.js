// =============================================================================
// File: modules/org-structure/edit-templates.js
// Description: The starter structures of the edit mode ("Mała firma", "Firma z
//   działami", "Firma z pionami i zastępcami"): units and positions, never people.
//   A template is data here, and loading one adds its operations to the draft like
//   any other change — with temporary ids, so units, positions, heads and deputy
//   heads depend on one another inside ONE batch — which makes it visible on the
//   canvas before it is saved, undoable, and saved atomically with the rest. The
//   codes carry a template prefix, so a template loaded next to a structure that
//   has its own never meets a code of the administrator's.
// =============================================================================

// Positions are `[code, nameKey, managerCode, flags]`: `head` names the unit's head, `staff` marks a
// staff position, `deputy` is the deputy's place in the unit's order. A unit is `[code, nameKey, parentCode, positions]`.
export const TEMPLATES = [
  {
    id: 'small',
    units: [
      ['MF', 'tpl_unit_company', null, [
        ['MF-OWN', 'tpl_pos_owner', null, { head: true }],
        ['MF-EMP1', 'tpl_pos_employee', 'MF-OWN', {}],
        ['MF-EMP2', 'tpl_pos_employee', 'MF-OWN', {}],
        ['MF-EMP3', 'tpl_pos_employee', 'MF-OWN', {}],
      ]],
    ],
  },
  {
    id: 'departments',
    units: [
      ['FD', 'tpl_unit_company', null, [
        ['FD-CEO', 'tpl_pos_ceo', null, { head: true }],
        ['FD-ASST', 'tpl_pos_assistant', 'FD-CEO', { staff: true }],
      ]],
      ['FD-SAL', 'tpl_unit_sales', 'FD', [
        ['FD-SAL-H', 'tpl_pos_unit_head', 'FD-CEO', { head: true }],
        ['FD-SAL-1', 'tpl_pos_specialist', 'FD-SAL-H', {}],
        ['FD-SAL-2', 'tpl_pos_specialist', 'FD-SAL-H', {}],
      ]],
      ['FD-OPS', 'tpl_unit_operations', 'FD', [
        ['FD-OPS-H', 'tpl_pos_unit_head', 'FD-CEO', { head: true }],
        ['FD-OPS-1', 'tpl_pos_specialist', 'FD-OPS-H', {}],
        ['FD-OPS-2', 'tpl_pos_specialist', 'FD-OPS-H', {}],
      ]],
      ['FD-FIN', 'tpl_unit_finance', 'FD', [
        ['FD-FIN-H', 'tpl_pos_unit_head', 'FD-CEO', { head: true }],
        ['FD-FIN-1', 'tpl_pos_specialist', 'FD-FIN-H', {}],
      ]],
    ],
  },
  {
    id: 'divisions',
    units: [
      ['FP', 'tpl_unit_company', null, [
        ['FP-CEO', 'tpl_pos_ceo', null, { head: true }],
        ['FP-ASST', 'tpl_pos_assistant', 'FP-CEO', { staff: true }],
      ]],
      ['FP-TEC', 'tpl_unit_division_technology', 'FP', [
        ['FP-TEC-D', 'tpl_pos_division_director', 'FP-CEO', { head: true }],
        ['FP-TEC-V', 'tpl_pos_deputy_director', 'FP-TEC-D', { deputy: 1 }],
      ]],
      ['FP-TEC-DEV', 'tpl_unit_development', 'FP-TEC', [
        ['FP-TEC-DEV-H', 'tpl_pos_unit_head', 'FP-TEC-D', { head: true }],
        ['FP-TEC-DEV-1', 'tpl_pos_specialist', 'FP-TEC-DEV-H', {}],
        ['FP-TEC-DEV-2', 'tpl_pos_specialist', 'FP-TEC-DEV-H', {}],
      ]],
      ['FP-COM', 'tpl_unit_division_commercial', 'FP', [
        ['FP-COM-D', 'tpl_pos_division_director', 'FP-CEO', { head: true }],
        ['FP-COM-V', 'tpl_pos_deputy_director', 'FP-COM-D', { deputy: 1 }],
      ]],
      ['FP-COM-SAL', 'tpl_unit_sales', 'FP-COM', [
        ['FP-COM-SAL-H', 'tpl_pos_unit_head', 'FP-COM-D', { head: true }],
        ['FP-COM-SAL-1', 'tpl_pos_specialist', 'FP-COM-SAL-H', {}],
      ]],
      ['FP-FIN', 'tpl_unit_division_finance', 'FP', [
        ['FP-FIN-D', 'tpl_pos_division_director', 'FP-CEO', { head: true }],
        ['FP-FIN-V', 'tpl_pos_deputy_director', 'FP-FIN-D', { deputy: 1 }],
        ['FP-FIN-1', 'tpl_pos_specialist', 'FP-FIN-D', {}],
      ]],
    ],
  },
];

/**
 * The operations of a template dated `from`; `t` turns a name key into the interface language. Temporary ids carry a
 * short nonce, so the same template loaded twice into one draft does not define one id twice (its codes then clash,
 * which the structure says on the second copy).
 */
export function templateOps(template, t, from) {
  const nonce = Math.random().toString(36).slice(2, 6);
  const unitTemp = (code) => `tmp:tpl${nonce}-u-${code}`;
  const positionTemp = (code) => `tmp:tpl${nonce}-p-${code}`;
  const ops = [];
  for (const [code, nameKey, parent] of template.units) {
    ops.push({
      kind: 'unitCreate', tempId: unitTemp(code), name: t(nameKey), code, typeId: null, color: null,
      parentUnitId: parent ? unitTemp(parent) : null, validFrom: from,
    });
  }
  for (const [unitCode, , , positions] of template.units) {
    for (const [code, nameKey, manager, flags] of positions) {
      ops.push({
        kind: 'positionCreate', tempId: positionTemp(code), unitId: unitTemp(unitCode), name: t(nameKey), code, roleId: null,
        isStaff: Boolean(flags.staff), parentPositionId: manager ? positionTemp(manager) : null, validFrom: from,
      });
    }
  }
  for (const [unitCode, , , positions] of template.units) {
    const head = positions.find((p) => p[3].head);
    if (head) ops.push({ kind: 'headSet', unitId: unitTemp(unitCode), headPositionId: positionTemp(head[0]), from });
    const deputies = positions.filter((p) => p[3].deputy).sort((a, b) => a[3].deputy - b[3].deputy);
    if (deputies.length) ops.push({ kind: 'deputyHeadsSet', unitId: unitTemp(unitCode), positionIds: deputies.map((p) => positionTemp(p[0])), from });
  }
  return ops;
}

/** The template as an indented outline for the preview: `{ depth, unit, positions: [name] }` per unit. */
export function templateOutline(template, t) {
  const depthOf = new Map();
  return template.units.map(([code, nameKey, parent, positions]) => {
    const depth = parent ? depthOf.get(parent) + 1 : 0;
    depthOf.set(code, depth);
    return { depth, unit: t(nameKey), positions: positions.map(([, key]) => t(key)) };
  });
}

/** Counts a template adds: what the preview promises and what the dry run has to confirm. */
export function templateCounts(template) {
  return {
    units: template.units.length,
    positions: template.units.reduce((sum, unit) => sum + unit[3].length, 0),
  };
}
