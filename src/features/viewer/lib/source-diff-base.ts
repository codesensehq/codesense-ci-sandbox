// SPDX-License-Identifier: MIT
import type { SourceDiffBaseMode } from "@/shared/stores/viewer-store";

/**
 * SLW-DV-7 — the working-diff comparison basis, surfaced as a header label + a
 * picker over the base the shipped `source_worktree_changeset` already takes.
 * Pure so both are unit-testable without the overlay; the mode lives in the
 * viewer store (`sourceDiffBaseMode`), the resolved fork-parent baseRef comes
 * from `useSourceDiffBase`.
 */

/**
 * The `baseRef` to feed `useSourceChangeset`/`useSourceFileRedline` for a mode:
 *  - `branchBase` → the resolved fork parent (undefined = WD-1's origin/main
 *    merge-base ladder), so the diff shows all work on the branch.
 *  - `head` → the literal `HEAD` ref. `resolve_source_baseline_tree` merge-bases
 *    HEAD against it and `merge_base(HEAD, HEAD) == HEAD`, so the baseline is
 *    HEAD's tree → the diff shows only uncommitted work. No new git machinery.
 */
export function effectiveBaseRef(
  mode: SourceDiffBaseMode,
  resolvedBaseRef: string | undefined,
): string | undefined {
  return mode === "head" ? "HEAD" : resolvedBaseRef;
}

/**
 * A plain-language statement of what the diff is compared against. Never
 * fabricates a ref: for `branchBase` it names the resolved dependency branch, or
 * the worktree config's default start point when the tree forks from it.
 */
export function comparisonLabel(
  mode: SourceDiffBaseMode,
  resolvedBaseRef: string | undefined,
  defaultStart: string,
): string {
  if (mode === "head") return "vs HEAD (working changes only)";
  // `||` (not `??`) so an empty-string ref also falls back to the default name —
  // the label must never render a blank "vs  (merge-base)".
  return `vs ${resolvedBaseRef || defaultStart} (merge-base)`;
}
