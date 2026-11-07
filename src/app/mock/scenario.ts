// SPDX-License-Identifier: MIT
/**
 * A named first-run world the mock boots into. The `?scenario=` URL param
 * selects one so every state a brand-new user passes through — the five
 * empty-state ladder rungs plus a set-up team — is reachable and demoable
 * without a rebuild (sub-second reload keeps the design loop
 * tight, and the link is shareable: `localhost:5174/?scenario=noSourceRepos`).
 *
 * Each preset is a coherent snapshot of how far a user has progressed: the
 * empty-state ladder (does an org/team/repo/clone exist yet) and whether the
 * team has a worktree config on disk.
 */
export type ScenarioKey =
  | "noOrg"
  | "noTeamInOrg"
  | "noSourceRepos"
  | "noAttachedClone"
  | "fresh"
  | "onboarded";

/** The structured world one preset describes. Handlers read these facts to
 *  shape their fixtures so a single switch moves every first-run surface in
 *  lockstep. */
export interface MockScenario {
  key: ScenarioKey;
  /** An org is discoverable (false → the NoOrg rung). */
  hasOrg: boolean;
  /** The org has a team (false → the NoTeamInOrg rung). */
  hasTeam: boolean;
  /** How many source repos the team declares (0 → the NoSourceRepos rung). */
  sourceRepoCount: number;
  /** A declared repo has a local clone attached (false → the NoAttachedClone
   *  rung: repo listed, nothing on disk). */
  cloneAttached: boolean;
  /** The team has a worktree.yaml on disk (drives the worktree-config probes). */
  worktreePresent: boolean;
}

/** Every preset, keyed by `ScenarioKey`. Ordered as the ladder climbs so the
 *  progression reads top-to-bottom. */
const SCENARIOS: Record<ScenarioKey, MockScenario> = {
  noOrg: {
    key: "noOrg",
    hasOrg: false,
    hasTeam: false,
    sourceRepoCount: 0,
    cloneAttached: false,
    worktreePresent: false,
  },
  noTeamInOrg: {
    key: "noTeamInOrg",
    hasOrg: true,
    hasTeam: false,
    sourceRepoCount: 0,
    cloneAttached: false,
    worktreePresent: false,
  },
  noSourceRepos: {
    key: "noSourceRepos",
    hasOrg: true,
    hasTeam: true,
    sourceRepoCount: 0,
    cloneAttached: false,
    worktreePresent: false,
  },
  noAttachedClone: {
    key: "noAttachedClone",
    hasOrg: true,
    hasTeam: true,
    sourceRepoCount: 1,
    cloneAttached: false,
    worktreePresent: false,
  },
  // A team with an attached clone and no worktree config yet.
  fresh: {
    key: "fresh",
    hasOrg: true,
    hasTeam: true,
    sourceRepoCount: 1,
    cloneAttached: true,
    worktreePresent: false,
  },
  // A set-up team: attached clone plus a worktree config on disk.
  onboarded: {
    key: "onboarded",
    hasOrg: true,
    hasTeam: true,
    sourceRepoCount: 1,
    cloneAttached: true,
    worktreePresent: true,
  },
};

/** The first-run start when no `?scenario=` is given: an org + team with an
 *  attached clone and nothing set up, so the app opens on the true beginning of
 *  the first-run journey rather than a populated steady state. */
export const DEFAULT_SCENARIO: ScenarioKey = "fresh";

function isScenarioKey(value: string): value is ScenarioKey {
  return value in SCENARIOS;
}

/** Resolve a `ScenarioKey` from a raw query string (e.g. `location.search`).
 *  Unknown or absent keys fall back to {@link DEFAULT_SCENARIO} — a bad link
 *  degrades to the canonical first-run start, never a blank screen. */
export function resolveScenario(search: string): MockScenario {
  const raw = new URLSearchParams(search).get("scenario");
  const key = raw && isScenarioKey(raw) ? raw : DEFAULT_SCENARIO;
  return SCENARIOS[key];
}
