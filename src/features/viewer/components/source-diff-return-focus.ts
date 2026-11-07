// SPDX-License-Identifier: MIT
/**
 * Where a tab's source diff hands focus back when it closes, when the element
 * focused at the moment it opened can't serve: the caption menu's "Open diff"
 * (focus sits on a menu item that unmounts right after) and the code editor's
 * handoff back to the diff (the editor's focus is gone with it). The opener
 * records the target here just before opening; the overlay takes it once, on
 * open. Keyed by the tab's session id.
 */
const pending = new Map<string, HTMLElement>();

export function setSourceDiffReturnFocus(sessionId: string, el: HTMLElement): void {
  pending.set(sessionId, el);
}

/** The recorded target for `sessionId`, consumed: null when none was recorded
 *  or it has left the document since. */
export function takeSourceDiffReturnFocus(sessionId: string): HTMLElement | null {
  const el = pending.get(sessionId) ?? null;
  pending.delete(sessionId);
  return el?.isConnected ? el : null;
}
