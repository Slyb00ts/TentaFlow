// =============================================================================
// File: modules/org-structure/handover-nav.js
// Description: The one way into the handover screen "Do przekazania" from any
//   place — the person menus of the structure, the header counter, the
//   profile, and the member removal of a project. The screen is a mode of the
//   structure's list tab, addressed by the route parameters below, so every
//   entry is a plain navigation: the address is shareable, the back button
//   works, and a draft of the edit mode is asked about before it is left.
// =============================================================================

import { Router } from '/js/router.js';

/**
 * Opens the screen for `userId`.
 * @param {{ userId: string, reason?: 'departure' | 'absence' | 'project_removal', projectId?: string }} target
 *   `projectId` is required for (and only used by) a project removal.
 */
export function openHandover({ userId, reason = 'departure', projectId = null }) {
  const params = { tab: 'list', handover: userId, reason };
  if (projectId) params.project = projectId;
  return Router.navigate('org-structure', params);
}

/** The screen's target from route parameters, or null when they do not ask for it. */
export function handoverTarget(params) {
  const userId = String(params?.handover ?? '');
  if (!userId) return null;
  return {
    userId,
    reason: String(params?.reason ?? 'departure'),
    projectId: params?.project ? String(params.project) : null,
  };
}
