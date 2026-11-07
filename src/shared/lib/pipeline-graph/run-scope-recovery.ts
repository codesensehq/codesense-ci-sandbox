// SPDX-License-Identifier: MIT
import { toast } from "sonner";
import { findNearestValidScope } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import type { RepoState } from "@/shared/stores/config-store";
import { buildTeamKey, NO_TEAM_SENTINEL, parseTeamKey } from "@/shared/lib/team-key";
import { parseSyntheticRepoKey } from "@/shared/lib/build-synthetic-repo";

/**
 * Single-repo scope recovery: walks back to the nearest still-valid
 * ancestor of `scopePath` under `teamContentPath`.
 *
 * Returns:
 * - `{ from, to }` when the scope needed to change (caller updates state)
 * - `null` when the scope is still valid, or if recovery failed
 *   (errors are logged, not thrown)
 *
 * Used by startup multi-repo recovery (via `runPipelineScopeRecovery` below).
 *
 * Empty `scopePath` is a no-op (already at the content root).
 *
 * PTS-2: anchors on `teamContentPath` (per-team content dir, resolved
 * by Rust). `pipelineRoot` stays at "" — scope-collapse at scanner
 * boundary (R-14).
 */
export async function recoverScopeForRepo(
  repoPath: string,
  teamContentPath: string,
  scopePath: string,
  findScope: typeof findNearestValidScope = findNearestValidScope,
): Promise<{ from: string; to: string } | null> {
  if (!scopePath || !teamContentPath) return null;
  try {
    const next = await findScope(teamContentPath, "", scopePath);
    if (next === scopePath) return null;
    return { from: scopePath, to: next };
  } catch (e) {
    logger.warn(
      "pipeline-scanner",
      `scope recovery failed for ${repoPath}: ${e instanceof Error ? e.message : String(e)}`,
    );
    return null;
  }
}

/**
 * Single-IPC ancestor-walk scope recovery for story 30.7 (AC-34).
 *
 * On app start, after both config-store and viewer-store have hydrated,
 * this walks each `pipelineScope[teamKey]` value and asks the Rust side
 * (`find_nearest_valid_scope`) for the deepest still-valid ancestor.
 * Each team gets at most ONE IPC call.
 *
 * T.4 Phase 5: `pipelineScope` is keyed by `teamKey`. For each
 * scoped team, derive a representative repoPath (any of the team's
 * tracked source repos) to look up `teamContentPath`. Teams that have
 * no tracked source repos or pin to legacy non-synthetic repos
 * (sentinel bucket) are skipped — those scopes will be recovered on
 * next interaction with the team.
 *
 * Results are applied via the injected `setPipelineScope` callback, and a
 * coalesced Sonner toast summarizes the outcome:
 * - zero recoveries → silent
 * - one recovery → specific message naming the team and new scope
 * - N recoveries → "Pipeline scope was reset for <N> teams"
 *
 * Dependencies are injected so this function is trivially unit-testable
 * without spinning up the real stores.
 */
export type ScopeRecoveryDeps = {
  repos: RepoState[];
  pipelineScope: Record<string, string>;
  setPipelineScope: (teamKey: string, scopePath: string) => void;
  findScope?: typeof findNearestValidScope;
  notify?: (msg: string) => void;
};

export type ScopeRecoveryResult = {
  recovered: Array<{ teamKey: string; from: string; to: string }>;
};

export async function runPipelineScopeRecovery(
  deps: ScopeRecoveryDeps,
): Promise<ScopeRecoveryResult> {
  const find = deps.findScope ?? findNearestValidScope;
  const notify = deps.notify ?? ((msg: string) => toast.info(msg));

  // Build a teamKey → representative repo lookup once. Any of a team's
  // tracked source repos resolves to the same `teamContentPath` post-PTS-2,
  // so the first match is fine.
  const teamRepLookup = new Map<string, RepoState>();
  for (const r of deps.repos) {
    const parsed = parseSyntheticRepoKey(r.repoPath);
    if (!parsed) continue;
    const tk = buildTeamKey(parsed.orgSlug, parsed.team);
    if (!teamRepLookup.has(tk)) teamRepLookup.set(tk, r);
  }

  const recovered: ScopeRecoveryResult["recovered"] = [];

  for (const [teamKey, scopePath] of Object.entries(deps.pipelineScope)) {
    if (teamKey === NO_TEAM_SENTINEL) continue;
    const repo = teamRepLookup.get(teamKey);
    if (!repo || !repo.teamContentPath) continue;

    const outcome = await recoverScopeForRepo(repo.repoPath, repo.teamContentPath, scopePath, find);
    if (outcome) {
      deps.setPipelineScope(teamKey, outcome.to);
      recovered.push({ teamKey, from: outcome.from, to: outcome.to });
    }
  }

  if (recovered.length === 1) {
    const { teamKey, to } = recovered[0];
    // T.4 Phase 8 review: use the teamKey directly (or its parsed teamName
    // segment) instead of an arbitrary tracked repo's name. For a team
    // with multiple source repos the prior `teamRepLookup.get(teamKey)?.repoName`
    // returned whichever repo was first into the map — semantically unrelated
    // to the recovered scope path. NO_TEAM_SENTINEL can never reach here
    // because the loop at the top of the function skips it (`continue`),
    // so no explicit sentinel guard is needed.
    const parsed = parseTeamKey(teamKey);
    const teamLabel = parsed.teamName ?? parsed.orgSlug;
    const target = to.length === 0 ? "pipeline root" : to;
    notify(`Pipeline scope reset to ${target} in ${teamLabel}`);
  } else if (recovered.length > 1) {
    notify(`Pipeline scope was reset for ${recovered.length} teams`);
  }

  return { recovered };
}
