// SPDX-License-Identifier: MIT
import { useShallow } from "zustand/react/shallow";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

// Stable empty reference for teams with no workspaces. Frozen so an accidental
// mutation throws in dev; the `as unknown as Workspace[]` cast sheds the
// readonly that `Object.freeze` adds — nothing actually mutates it. Returning a
// fresh `[]` from the selector instead would defeat referential stability.
export const EMPTY_WORKSPACES = Object.freeze([]) as unknown as Workspace[];

/**
 * Shared subscription to a team's workspace list. The single home for the
 * `useViewerStore((s) => s.workspaces[teamKey]) ?? EMPTY_WORKSPACES` pattern
 * that was duplicated across the workspace bar, terminal panels, and team view.
 *
 * `useShallow` returns the previous array reference when the elements are
 * shallow-equal, so consumers re-render only when a workspace is added,
 * removed, reordered, or replaced. Workspace objects are replaced immutably on
 * any field change (no in-place mutation anywhere in the store), so a real
 * field change always produces a new element reference and is never dropped —
 * see use-team-workspaces.test.ts (T-11).
 */
export function useTeamWorkspaces(teamKey: string): Workspace[] {
  return useViewerStore(useShallow((s) => s.workspaces[teamKey] ?? EMPTY_WORKSPACES));
}
