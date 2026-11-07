// SPDX-License-Identifier: MIT
/** `N unpublished context edit(s)`. */
function unpublishedEditsPhrase(count: number): string {
  return `${count} unpublished context ${count === 1 ? "edit" : "edits"}`;
}

/**
 * SW-XS-4 (M4): what removing a worktree would discard, for the confirmation copy.
 * `null` count + `error` → the edits couldn't be checked.
 */
export function removalDiscardMessage(count: number | null, error: string | null): string {
  if (count === null) {
    return `CodeSense couldn't check for unpublished context edits${error ? ` (${error})` : ""}; removing the worktree may discard them.`;
  }
  if (count === 0) return "This worktree holds no unpublished context edits.";
  return `This worktree holds ${unpublishedEditsPhrase(count)}; removing it discards them.`;
}

/**
 * SW-XS-4: the separate confirmation for discarding uncommitted CODE changes when
 * the user already chose to discard unpublished context edits. `null` → the git
 * status couldn't be read.
 */
export function uncommittedCodeDiscardMessage(dirty: boolean | null): string {
  if (dirty === null) {
    return "CodeSense couldn't check this worktree for uncommitted code changes; removing it may discard them.";
  }
  return "This worktree also has uncommitted code changes; removing it discards them too.";
}
