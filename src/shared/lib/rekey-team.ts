// SPDX-License-Identifier: MIT
/**
 * SW-HB-8 — pure teamKey/team-name rewrites for a team rename.
 *
 * Renaming a team changes a value that is simultaneously a registry field, a
 * path component (handled by the Rust orchestrator), AND an in-memory state
 * key. This module owns the pure org-store half of that re-key so it is
 * unit-testable in isolation and applied ONCE, atomically, after the backend
 * confirms — mirroring how `createTeam` reflects disk-truth in-memory only
 * after the commit resolves. The viewer-store half lives in `viewer-store.ts`
 * (`rekeyKeyedSlices` over `TEAM_KEYED_SLICES`), which owns the canonical list
 * of team-keyed slices.
 *
 * `workspaceRepos` is intentionally NOT rewritten here: it is runtime-only and
 * re-derived on the team-activation the rename triggers (changing
 * `activeTeamName` re-fires `setActiveTeamForWave`), so rewriting it would only
 * risk drift against that re-derivation.
 *
 * `suspendedWorkspaceRepos` (a NON-active team's frozen `workspaceRepos`
 * snapshot) is DROPPED, not moved: its inner `RepoState.repoPath` values embed
 * the OLD team name (`workspace:{org}:{oldTeam}:{owner}/{repo}`), and the
 * team-activation resume guard compares only owner/repo slugs — which a rename
 * doesn't change — so it would NOT evict a stale-inner-key snapshot and would
 * resume with synthetics keyed to the old name (repos then fail to resolve).
 * Dropping the snapshot makes the next activation fall through to a clean
 * re-mint under the new name.
 */
import type { Org } from "@/shared/types/domain";
import type { RepoState } from "@/shared/stores/config-store";
import { buildTeamKey } from "@/shared/lib/team-key";

/** The minimal org-store shape this transform reads + rewrites. */
export interface RekeyableOrgState {
  orgs: Org[];
  activeOrgSlug: string | null;
  activeTeamName: string | null;
  lastTeamByOrgSlug: Record<string, string>;
  shownTeamsByOrgSlug: Record<string, string[]>;
  suspendedWorkspaceRepos: Record<string, Record<string, RepoState>>;
  recentlyRemovedRepos: Record<string, Set<string>>;
}

/** Move a `Record`'s `oldKey` entry to `newKey`. Returns the SAME reference
 *  when `oldKey` is absent (nothing to move) so a Zustand set stays
 *  `Object.is`-stable for untouched slices. */
function renameRecordKey<T>(
  record: Record<string, T>,
  oldKey: string,
  newKey: string,
): Record<string, T> {
  if (!(oldKey in record)) return record;
  const { [oldKey]: moved, ...rest } = record;
  return { ...rest, [newKey]: moved };
}

/** Drop a `Record`'s `oldKey` entry entirely. Returns the SAME reference when
 *  `oldKey` is absent so untouched slices stay `Object.is`-stable. */
function dropRecordKey<T>(record: Record<string, T>, oldKey: string): Record<string, T> {
  if (!(oldKey in record)) return record;
  const { [oldKey]: _dropped, ...rest } = record;
  return rest;
}

/** Rewrite the team name inside the org's shown-set (an array of team names),
 *  preserving order. Returns the same map reference when there's nothing to
 *  rewrite (no curated set, or the old name absent from it). */
function renameInShownSet(
  shown: Record<string, string[]>,
  orgSlug: string,
  oldName: string,
  newName: string,
): Record<string, string[]> {
  const set = shown[orgSlug];
  if (set === undefined || !set.includes(oldName)) return shown;
  return { ...shown, [orgSlug]: set.map((n) => (n === oldName ? newName : n)) };
}

/**
 * Pure transform: after team `oldName` is renamed to `newName` within
 * `orgSlug`, rewrite every org-store field keyed by the team name or its
 * teamKey. Returns a `Partial` to merge via `set`. No-op (`{}`) when the names
 * are equal.
 */
export function rekeyOrgStateForRename(
  state: RekeyableOrgState,
  orgSlug: string,
  oldName: string,
  newName: string,
): Partial<RekeyableOrgState> {
  if (oldName === newName) return {};
  const oldKey = buildTeamKey(orgSlug, oldName);
  const newKey = buildTeamKey(orgSlug, newName);

  return {
    // Reflect disk-truth in-memory: rename the team in the matching org only.
    orgs: state.orgs.map((o) =>
      o.slug === orgSlug
        ? { ...o, teams: o.teams.map((t) => (t.name === oldName ? { ...t, name: newName } : t)) }
        : o,
    ),
    // The active team never falls to `_none`: if it WAS the renamed team, it
    // follows the rename. Team names are only unique within an org, so a
    // same-named team renamed in another org leaves the active team alone.
    activeTeamName:
      state.activeOrgSlug === orgSlug && state.activeTeamName === oldName
        ? newName
        : state.activeTeamName,
    // `lastTeamByOrgSlug` value is a team name.
    lastTeamByOrgSlug:
      state.lastTeamByOrgSlug[orgSlug] === oldName
        ? { ...state.lastTeamByOrgSlug, [orgSlug]: newName }
        : state.lastTeamByOrgSlug,
    // `shownTeamsByOrgSlug` value is an array of team names.
    shownTeamsByOrgSlug: renameInShownSet(state.shownTeamsByOrgSlug, orgSlug, oldName, newName),
    // `recentlyRemovedRepos` is keyed by teamKey; its values are team-agnostic
    // owner/repo slugs, so a plain key move is safe.
    recentlyRemovedRepos: renameRecordKey(state.recentlyRemovedRepos, oldKey, newKey),
    // Dropped, not moved — see the module doc (stale-inner-key snapshot).
    suspendedWorkspaceRepos: dropRecordKey(state.suspendedWorkspaceRepos, oldKey),
  };
}
