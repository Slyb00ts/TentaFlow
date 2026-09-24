// ===== File: modules/tentanas/disk-wipe.js — the guarded "clear this disk" dialog of the Disks tab (n03) =====
//
// WHY it exists where it does. A disk whose role is the catch-all `used` can
// go into neither a pool nor an array, and until this dialog there was no way
// to free one: the only code in the product that erased a disk was the
// pool-create wizard, so "free a disk" meant "build an array on it". A
// dissolved Elastic Array leaves every one of its disks in exactly that
// state, because a dissolve keeps the filesystems on purpose.
//
// WHAT THE DIALOG IS FOR, and it is not the button. The node decides; this
// dialog exists to show the admin WHY before they decide, which means it
// renders the plan the node produced rather than any judgement of its own:
// what would be removed, and every refusal as the node's own sentence. The
// two confirmations are the only state it owns — the retyped device name, and
// a SEPARATE acknowledgement when a dissolved array's journal still claims
// the disk, because that second loss is the array's recoverability and the
// device name says nothing about it.
//
// Nothing here is a safety check. The plan's refusals can all be wrong at
// once (they were, on a live node, for 17 disks holding 2.1 TiB): an Elastic
// Array's branches are mounted inside the union process's own mount
// namespace, so lsblk and /proc/mounts on the host show a live member as an
// idle disk with a plain xfs signature. The guarantee is the exclusive open
// of the block device on the privileged side, which the kernel refuses for a
// filesystem mounted in ANY namespace — and `wipe_disk.explain` says so in
// the dialog, so the refusal is not read as a transient fault.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { T, sprite, fmtBytes, ADMIN_TIMEOUT_MS } from '/js/modules/tentanas/format.js';
import { openRetypeDialog } from '/js/lib/retype-dialog.js';
import { followResponse, warningHtml, NAS_DIALOG } from '/js/modules/tentanas/dialogs.js';
import { journalOwnerPhrase, journalOwnerIds, isOtherOrgOnNode } from '/js/modules/tentanas/journal-owner.js';
import '/js/components/tf-checkbox.js';

// `NasDiskWipeJournalClaim.arrayRole` → the locale key the Disks tab already
// uses for the same three parts, so a disk's part reads the same in the row
// chip and in this dialog.
const ARRAY_PART_KEYS = {
  data: 'role.array_data',
  cache: 'role.array_cache',
  parity: 'role.array_parity',
};

function lossListHtml(plan) {
  const items = [];
  if (plan.fsType) {
    // The label is what an admin named the filesystem, so it is text. The
    // UUID identifies it and names nothing — no mockup prints one — so it is
    // the tooltip of the line, there for the admin who wants to check it
    // against `blkid` before erasing.
    const detail = plan.fsLabel ? T('wipe_disk.fs_label', { label: plan.fsLabel }) : '';
    const title = plan.fsUuid ? ` title="${escapeAttr(T('wipe_disk.fs_uuid', { uuid: plan.fsUuid }))}"` : '';
    items.push(`<b${title}>${escapeHtml(T('wipe_disk.fs', { type: plan.fsType }))}</b>${detail ? ` — ${escapeHtml(detail)}` : ''}`);
  }
  if (plan.mountpoints?.length) {
    items.push(escapeHtml(T('wipe_disk.mounted_at', { paths: plan.mountpoints.join(', ') })));
  }
  if (!items.length) return `<div class="explain-box">${escapeHtml(T('wipe_disk.nothing'))}</div>`;
  return `
    <h2 class="wizard-section-title">${escapeHtml(T('wipe_disk.removes'))}</h2>
    <ul class="loss-list">${items.map((i) => `<li class="ll bad">${sprite('trash')}<span>${i}</span></li>`).join('')}</ul>`;
}

// The refusals the dialog words itself, by code, in the admin's language.
// Every other refusal is the node's own sentence (`detail`).
// `journal_other_org` is one the admin must be able to read in full: the
// disk is another organisation's on this node, and the node's detail names
// no array on purpose — so the wording here must not either.
const REFUSAL_KEYS = {
  journal_other_org: 'wipe_disk.refusal_journal_other_org',
};

function refusalText(refusal, plan) {
  const key = REFUSAL_KEYS[refusal?.code];
  return key ? T(key, { name: plan.name }) : (refusal?.detail || '');
}

function journalHtml(claim) {
  const role = T(ARRAY_PART_KEYS[claim.arrayRole] || ('role.' + claim.arrayRole));
  // "Foreign" is the NODE's verdict (`owner_foreign`), taken against the
  // instance asking — never inferred from the owner ids: those used to be
  // always filled in, which called this instance's own dissolved arrays
  // another instance's, and for another installation the node now blanks
  // them altogether. The owner phrase is composed from the node's code
  // (`journalOwnerPhrase`); the ids, when this org may see them, are only the
  // tooltip.
  const foreign = claim.ownerForeign
    ? ` <span title="${escapeAttr(journalOwnerIds(claim))}">${escapeHtml(T('wipe_disk.journal_foreign', { owner: journalOwnerPhrase(claim) }))}</span>`
    : '';
  return `
    <div class="wizard-warning danger">${sprite('alert')}<div>
      <b>${escapeHtml(T('wipe_disk.journal_title'))}</b><br>
      ${escapeHtml(T('wipe_disk.journal_body', { name: claim.name, role, n: claim.memberCount || 0 }))}
      ${foreign}<br>
      ${escapeHtml(T('wipe_disk.journal_cost', { name: claim.name }))}
    </div></div>
    <tf-checkbox id="nas-wipe-ack" label="${escapeAttr(T('wipe_disk.journal_ack', { name: claim.name }))}"></tf-checkbox>`;
}

/**
 * Reads the plan for `disk` and opens the confirmation over it. Returns the
 * window, or `null` when the sudo prompt was cancelled (nothing was read).
 * A failed read throws, so the caller reports it where the admin clicked.
 */
export async function openDiskWipeDialog(screen, disk, onDone) {
  const answer = await screen.withSudo(
    (sudoPassword) => screen.nas('tentaNasDiskWipePlanRequest', { diskId: disk.diskId, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }),
    T('wipe_disk.title', { name: disk.name }),
  );
  if (answer === null) return null;
  const plan = answer?.plan;
  // The plan is what the button is armed from, so a shape this build cannot
  // read must not become an enabled danger button over an unknown device.
  if (!plan || typeof plan.name !== 'string' || !plan.name || !Array.isArray(plan.refusals)) {
    throw new Error(T('wipe_disk.bad_plan'));
  }
  // Another organisation's claim is never shown, whatever carries it: the
  // node refuses such a disk (`journal_other_org`) and sends no claim, and a
  // claim of that kind that did arrive must not print its array's name.
  const claim = plan.allowed && !isOtherOrgOnNode(plan.journalClaim) ? plan.journalClaim : null;
  const bodyHtml = `
    ${warningHtml('danger', T('wipe_disk.warning', { name: plan.name }))}
    ${plan.refusals.map((r) => `<div class="wizard-warning danger">${sprite('alert')}<div><b>${escapeHtml(T('wipe_disk.refused'))}</b><br>${escapeHtml(refusalText(r, plan))}</div></div>`).join('')}
    ${lossListHtml(plan)}
    ${claim ? journalHtml(claim) : ''}
    <div class="explain-box">${escapeHtml(T('wipe_disk.explain'))}</div>`;
  let acknowledged = false;
  return openRetypeDialog({
    ...NAS_DIALOG,
    title: T('wipe_disk.title', { name: plan.name }),
    // The device NAME can move between reboots; the path and the serial are
    // how an admin checks they are erasing the disk they walked to the rack
    // for. Both are on the header, beside the name they have to retype.
    subtitle: T('wipe_disk.subtitle', {
      model: plan.model || '—',
      size: fmtBytes(plan.sizeBytes),
      path: plan.path || '—',
      serial: plan.serial || '—',
    }),
    icon: 'alert',
    name: plan.name,
    bodyHtml,
    retypeLabel: `${escapeHtml(T('wipe_disk.retype'))} <span class="mono num-err">${escapeHtml(plan.name)}</span>`,
    confirmLabel: T('wipe_disk.confirm', { name: plan.name }),
    // A refused plan can never arm the button, and a claimed disk needs the
    // acknowledgement as well as the retyped name.
    alsoArmed: () => plan.refusals.length === 0 && (!claim || acknowledged),
    wire: (win, syncButton) => {
      win.querySelector('#nas-wipe-ack')?.addEventListener('change', (e) => {
        acknowledged = Boolean(e.detail?.checked ?? e.target.checked);
        syncButton();
      });
    },
    onConfirm: async () => {
      const res = await screen.withSudo(
        (sudoPassword) => screen.nas('tentaNasDiskWipeRequest', {
          diskId: plan.diskId,
          confirmDevice: plan.name,
          // The array NAME, never a bare flag: the node checks it against the
          // journal that claims this very disk, so an acknowledgement for one
          // array cannot release another's.
          releaseJournalArray: claim && acknowledged ? claim.name : '',
          sudoPassword,
        }, { timeoutMs: ADMIN_TIMEOUT_MS }),
        T('wipe_disk.title', { name: plan.name }),
      );
      if (res === null) return false;
      followResponse(screen, res, onDone, T('wipe_disk.done', { name: plan.name }));
      return true;
    },
  });
}
