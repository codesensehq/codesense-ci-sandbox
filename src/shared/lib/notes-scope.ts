// SPDX-License-Identifier: MIT
import { currentGithubUserId } from "@/shared/lib/get-current-user";
import { useOrgStore } from "@/shared/stores/org-store";

/**
 * SLW-PT-19: notes live per ORG in the user's scratch space. Every notes command
 * names the org explicitly — captured when the edit or read was made, never
 * resolved on the backend at call time — so a save that lands after an org switch
 * still goes to the org it was typed in.
 */
export interface NotesScope {
  githubUserId: number;
  /** `null` → the machine-local fallback (no org selected). */
  orgSlug: string | null;
}

/** The notes scope right now: the selected profile + the active org. */
export function currentNotesScope(): NotesScope {
  return {
    githubUserId: currentGithubUserId(),
    orgSlug: useOrgStore.getState().activeOrgSlug ?? null,
  };
}

/** Whether `scope`'s notes have a live records projection: an org's notes do; the
 *  machine-local store (no org, or signed out) is written only by the person. */
export function notesHaveProjection(scope: NotesScope): boolean {
  return !!scope.orgSlug && scope.githubUserId !== 0;
}

/** A stable string identity for a scope (debounce keys, reload guards). */
export function notesScopeKey(scope: NotesScope): string {
  return `${scope.githubUserId}@${scope.orgSlug ?? ""}`;
}
