// SPDX-License-Identifier: MIT
/** Stable identity and sizing shared by native and pane-hosted message frames. */
export function messageDocKey(message: { org: string; team: string; messageId: number }): string {
  return `inbox-message://${message.org}/${message.team}/${message.messageId}`;
}

export const INITIAL_MESSAGE_HEIGHT = 160;
export function messageFrameHeight(height: number): number {
  return Math.min(720, Math.max(32, Math.ceil(height)));
}
