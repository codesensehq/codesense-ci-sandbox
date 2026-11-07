// SPDX-License-Identifier: MIT
import {
  useViewerStore,
  type SoftClosedWorkspace,
  type Workspace,
} from "@/shared/stores/viewer-store";

/**
 * SW-EW-44 (AC-3): the live cwd set the reap guard checks — every open workspace
 * cwd unioned with every soft-closed (minimized) shadow's cwd. A minimized epic
 * terminal still holds a live PTY at its cwd, so it must count. Empty cwds
 * (born-"creating" placeholders) are skipped, and the set is deduped.
 *
 * Deliberately does NOT filter to the epic root: the "is this cwd at or below the
 * epic root" decision lives in ONE place — the backend `first_in_use_path` guard,
 * which owns the canonicalization + macOS `/tmp`↔`/private/tmp` handling. The FE
 * only enumerates; the backend matches. Pure over the store slices so it is
 * unit-testable without the store.
 */
export function collectInUsePaths(
  workspaces: Record<string, Workspace[]>,
  softClosedWorkspaces: Record<string, Record<string, SoftClosedWorkspace>>,
): string[] {
  const out = new Set<string>();
  for (const list of Object.values(workspaces ?? {})) {
    for (const w of list) {
      if (w.cwd) out.add(w.cwd);
    }
  }
  for (const byWorkspace of Object.values(softClosedWorkspaces ?? {})) {
    for (const shadow of Object.values(byWorkspace)) {
      if (shadow.workspace.cwd) out.add(shadow.workspace.cwd);
    }
  }
  return [...out];
}

/** Effectful wrapper: read the live cwd set from the viewer store. */
export function collectInUsePathsFromStore(): string[] {
  const s = useViewerStore.getState();
  return collectInUsePaths(s.workspaces, s.softClosedWorkspaces);
}
