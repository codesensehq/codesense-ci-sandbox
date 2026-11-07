// SPDX-License-Identifier: MIT
/**
 * The editor buffer's ownership and identity model: who owns unsaved text and
 * how open files are keyed. The buffer's own states (when disk may touch it,
 * when it may be saved) are the state machine in `buffer-machine.ts`.
 *
 * ## Ownership
 *
 * A file's unsaved text (its draft: `buffer` plus the `lastSaved` baseline it
 * diverged from) has exactly one owner at any moment:
 *
 * - the mounted editor (`useFileEditor`) while a pane for the file is mounted;
 * - the draft store (the worktree editor's kept drafts, mirrored to the viewer
 *   store so an unfocus survives) while no pane is mounted.
 *
 * Ownership moves only at mount and unmount. An unmounting pane hands its draft
 * to the store (a dirty buffer, or a hand-merge), or clears it when it holds
 * no work or the tab was closed on purpose. A mounting pane takes the stored
 * draft as `restore`. The one other write is a clear: a mounted pane whose
 * buffer goes clean clears its stored copy, which then holds nothing worth
 * restoring.
 *
 * ## Identity
 *
 * Tabs, panes, drafts, sizes and line jumps are keyed by the repo-relative
 * path. The worktree root is transport, not identity:
 *
 * - panes do not mount until the root is known;
 * - a root that moves re-targets each mounted pane in place.
 *
 * A re-target is not a remount and never clears a draft. The editor carries a
 * dirty buffer across as an unverified draft for the new location (see
 * {@link retargetDraft}). An open merge is not carried: its pre-merge edits
 * stay in the buffer and the hand-merged side is dropped. (Panes wait for the
 * root, so only a root moving from one known directory to another re-targets.)
 *
 * A restored or carried draft enters the machine with an unverified base, so
 * disk is checked before it can be saved.
 *
 * ## When disk may replace the buffer, and saves
 *
 * The buffer itself is one pure state machine, `buffer-machine.ts`
 * (EDITOR-BUFFER-STATE-SPEC.md): one reducer, one IO runner (`useFileEditor`)
 * with at most one tagged read and one tagged write in flight, a derived
 * conflict, a lock on every write, a synchronous Reload, and a merge that
 * freezes its disk snapshot. Its invariants are model-checked in
 * `buffer-machine.test.ts`.
 */

import type { FileDraft } from "./use-file-editor";

export const BINARY_MESSAGE = "Binary file — no preview";

/** The draft a re-targeted editor carries to its new path: its unsaved edits,
 *  unverified there, or nothing when the buffer is clean. */
export function retargetDraft(
  editable: boolean,
  buffer: string,
  lastSaved: string,
): FileDraft | null {
  return editable && buffer !== lastSaved ? { buffer, lastSaved } : null;
}
