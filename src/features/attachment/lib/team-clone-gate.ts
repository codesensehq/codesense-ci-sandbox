// SPDX-License-Identifier: MIT
import type { RepoState } from "@/shared/stores/config-store";

/**
 * SW-OB-3 (AC-4 seam): does a SINGLE tracked repo have a usable local clone?
 * This is the one place that decides what "attached" means. Both the per-repo
 * UI state (`NoAttachedCloneState`'s rows) and the team-level gate below are
 * expressed in terms of it, so a future "clone for me" path extends the
 * requirement HERE (e.g. `r.localPath != null || r.managedClone != null`) and
 * every consumer follows automatically. Do NOT inline `localPath != null`.
 *
 * `RepoState.localPath` is `string | null | undefined` (null on an unattached
 * synthetic, undefined on legacy `config-store.repos` entries), so `!= null`
 * deliberately covers both nullish cases.
 */
export function isCloneAttached(repo: RepoState): boolean {
  return repo.localPath != null;
}

/**
 * SW-OB-3 (AC-1, AC-4): the required-clone predicate. A team is satisfied when
 * at least one of its tracked source repos has an attached local clone. Both
 * gate call-sites (the `NoAttachedCloneState` empty-state branch and the
 * `App.tsx` routing boundary) import THIS — never an inline check.
 */
export function teamHasSatisfiedClone(teamRepos: RepoState[]): boolean {
  return teamRepos.some(isCloneAttached);
}
