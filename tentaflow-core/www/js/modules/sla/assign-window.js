// ===== File: modules/sla/assign-window.js — "Przypisz politykę SLA": one policy (or none) for one flow =====

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { buildField } from '/js/lib/actions/fields.js';
import { SlaRefusal, T, describeSlaError, failureText, policyOptionLabel, unlessRefused } from './model.js';

/**
 * Opens the window for `flow` (`{ id, name, slaPolicyId }`) over `policies`
 * (`SlaPolicyWire[]`). An empty choice takes the policy off the flow.
 * `onChanged()` runs after the server accepted the change.
 */
export function openAssignPolicyWindow({ flow, policies, onChanged = () => {}, anchor = null }) {
  const current = policies.some((policy) => policy.id === flow.slaPolicyId) ? flow.slaPolicyId : '';
  const field = buildField({
    key: 'policy',
    label: T('assign.field'),
    kind: 'select',
    value: current,
    options: policies.map((policy) => ({ value: policy.id, label: policyOptionLabel(policy) })),
    hint: T(policies.length ? 'assign.hint' : 'assign.hint_no_policies'),
  });
  const section = document.createElement('div');
  section.append(field.el);
  const nameOf = (id) => policies.find((policy) => policy.id === id)?.spec.name ?? '';

  return openFormWindow({
    title: T('assign.title'),
    icon: 'shield',
    subject: T('assign.subject', { name: flow.name }),
    note: { tone: 'info', text: T('assign.note') },
    sections: [section],
    fields: [field],
    collect: () => field.read(),
    submitLabel: T('assign.submit'),
    errorMessage: (err) => (err instanceof SlaRefusal ? describeSlaError(err.slaError).message : failureText(err)),
    onSubmit: async (policyId) => {
      if (policyId === null) {
        unlessRefused(await ApiBinary.action('slaUnassignRequest', { flowId: flow.id }));
        onChanged();
        return { message: T('assign.unassigned_ok', { flow: flow.name }) };
      }
      unlessRefused(await ApiBinary.action('slaAssignRequest', { flowId: flow.id, policyId }));
      onChanged();
      return { message: T('assign.assigned_ok', { policy: nameOf(policyId), flow: flow.name }) };
    },
    anchor,
  });
}
