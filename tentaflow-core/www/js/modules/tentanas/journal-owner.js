// ===== File: modules/tentanas/journal-owner.js — an Elastic journal's owner, in words =====
//
// WHY it is composed here. The node judges whose journal it is and sends a
// CODE (`ownerKind`) plus, only for another instance of the asking
// organisation, that instance's display name (`ownerInstanceName`). The
// sentence belongs to the front end, because a Polish phrase built on the
// server reads as Polish in every locale. Another organisation's names never
// arrive at all — a node shared by several tenants does not tell one tenant
// what another is called, nor its ids.
//
// Two kinds of "another organisation" (owner's rule, 2026-09-22):
// - one this node has NO record of — disks moved in from another machine —
//   is 'other_installation': shown and adoptable, and "another installation"
//   is all that can be said about it;
// - one that EXISTS ON THIS NODE is 'other_org_on_node': another tenant's
//   array, invisible to this one. The node leaves such a journal out of the
//   import scan and refuses a wipe of its disks with the code
//   `journal_other_org` and no array name, so this code should never reach a
//   list — `isOtherOrgOnNode` is the front's own guard in case it does.
//
// Used by the disk-wipe dialog (a dissolved array's journal still claiming a
// disk) and the Elastic import dialog (a journal offered for adoption).

import { T } from '/js/modules/tentanas/format.js';

/** Another tenant of this node owns the journal: nothing of it is shown. */
export const OTHER_ORG_ON_NODE = 'other_org_on_node';

/**
 * Whether a journal claim / import candidate belongs to another organisation
 * of this node. Such an entry is dropped, never rendered: printing its array
 * name would tell one tenant about another's storage.
 */
export function isOtherOrgOnNode(c) {
  return c?.ownerKind === OTHER_ORG_ON_NODE;
}

/**
 * The owner of a journal claim / import candidate as a phrase.
 *
 * `ownerKind` is 'this_instance' | 'this_org' | 'other_installation'. A node
 * that predates the code sends none; its `ownerForeign` flag still says
 * whether the owner is this instance, and a foreign owner is then "another
 * installation" — the phrase that claims nothing it does not know.
 */
// The owner kind, with the fallback an older node (no code, only the flag)
// needs: a foreign owner it cannot place is "another installation".
const ownerKindOf = (c) => c?.ownerKind || (c?.ownerForeign === false ? 'this_instance' : 'other_installation');

export function journalOwnerPhrase(c) {
  const kind = ownerKindOf(c);
  if (kind === 'this_instance') return T('journal_owner.this_instance');
  if (kind === 'this_org') {
    const name = String(c?.ownerInstanceName || '').trim();
    return name ? T('journal_owner.this_org_named', { name }) : T('journal_owner.this_org');
  }
  return T('journal_owner.other_installation');
}

/**
 * The owner ids, for a tooltip only: they identify, nobody reads them. Only
 * an owner of the asking organisation has any — the node blanks another
 * installation's (`owner_ids_for` in elastic.rs), and this repeats the rule
 * so an older node that still sends them does not put another tenant's ids
 * in the tooltip either.
 */
export function journalOwnerIds(c) {
  const kind = ownerKindOf(c);
  if (kind !== 'this_instance' && kind !== 'this_org') return '';
  return [c?.ownerOrgId, c?.ownerAddonId].filter(Boolean).join(' / ');
}
