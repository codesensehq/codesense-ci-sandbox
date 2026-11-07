// SPDX-License-Identifier: MIT
import type { RepoState } from "@/shared/stores/config-store";
import { isSyntheticRepoPath } from "./build-synthetic-repo";
import { deriveOrgRoot } from "./codesense-home";
import { resolveRepo } from "./resolve-repo";
import { parseTeamKey } from "./team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { logger } from "@/shared/lib/logger";

/** Recover the real on-disk source-code path for a `RepoState`.
 *
 *  - Legacy repos (real-path `repoPath`, no `localPath`): returns `repoPath`.
 *  - Synthetic workspace repos with a matching attachment: returns `localPath`.
 *  - Synthetic workspace repos without an attachment: returns `null` — caller
 *    must surface the empty-state UX (F-6 AC-8) rather than treating the
 *    synthetic key as a filesystem path.
 *
 *  This helper is the single point of truth for "what's the source-code path
 *  for this `RepoState`?" — F-6 routes every downstream consumer through it.
 */
export function resolveSourceRepoPath(repo: RepoState): string | null {
  // R5B-P6: treat empty string the same as null. Empty `localPath` is never
  // a valid filesystem path; passing it downstream would feed `cwd: ""` to
  // terminal spawn (because `?? undefined` keeps `""`, and `cwdOverride ??
  // repoPath` also keeps `""`). Defense-in-depth — `validate_non_empty_no_control`
  // on `repo_path` at save time should prevent this from reaching
  // `attachments.yml`, but pre-existing hand-edited rows could slip through.
  if (repo.localPath != null && repo.localPath !== "") return repo.localPath;
  return isSyntheticRepoPath(repo.repoPath) ? null : repo.repoPath;
}

/** Resolve the on-disk repo root for a repoPath key.
 *  Looks up the RepoState via resolveRepo, then delegates to
 *  resolveSourceRepoPath. Returns null for synthetic keys with no
 *  local clone attached. Legacy real-path keys pass through unchanged. */
export function resolveRealRepoPath(repoPath: string): string | null {
  const repo = resolveRepo(repoPath);
  return repo ? resolveSourceRepoPath(repo) : isSyntheticRepoPath(repoPath) ? null : repoPath;
}

/** SS.8: Resolve the org-root / context-repo path for a `repoPath` key.
 *  Backend worktree readers (`probe_worktree_config`, `get_worktree_config`)
 *  all need `orgRoot` so they can resolve the new
 *  `{orgRoot}/_codesense/worktree/{owner}/{repo}.yaml` path; this helper
 *  is the single source of truth for the lookup.
 *
 *  Iter-2 patch: `RepoState.contextRepoPath` for synthetic workspace repos
 *  is `{orgRoot}/_team/{team}/repos/{owner}/{repo}` per
 *  `build-synthetic-repo.ts` — NOT the bare org root. Without
 *  `deriveOrgRoot()`, every backend call computed a bogus
 *  `{teamPath}/_codesense/worktree/...` path that never existed, silently
 *  degrading to the legacy fallback for every synthetic repo. The strip is
 *  required for SS.8 to do its job.
 *
 *  Returns `""` (empty string) when the repo can't be resolved or its
 *  `contextRepoPath` hasn't been populated yet — callers should treat that
 *  as "skip the new-path lookup; backend falls back to legacy
 *  automatically." */
export function resolveOrgRoot(repoPath: string): string {
  const ctxPath = resolveRepo(repoPath)?.contextRepoPath;
  return ctxPath ? deriveOrgRoot(ctxPath) : "";
}

/** T.5: resolve org-root for a `teamKey` directly, without needing a
 *  repoPath. Used where we have the team bucket but want to skip the
 *  frontmatter-read → repo-lookup chain just for the orgRoot value.
 *
 *  Reads `useOrgStore.orgs[orgSlug].org_root` via `parseTeamKey`. Returns
 *  `""` when the org isn't loaded — `get_worktree_config` and other backend
 *  callers treat empty as "fall back to legacy path lookup" so this is a
 *  safe degradation rather than an error case.
 *
 *  T.5 (iter-1 #F7): emits a `logger.warn` when the org lookup misses so
 *  silent backend-fallback behavior surfaces in dev logs — pre-iter-1
 *  this was a hard no-op with no telemetry. */
export function resolveOrgRootForTeam(teamKey: string): string {
  const { orgSlug } = parseTeamKey(teamKey);
  const org = useOrgStore.getState().orgs.find((o) => o.slug === orgSlug);
  if (!org) {
    logger.warn(
      "viewer",
      `resolveOrgRootForTeam: org "${orgSlug}" not found in org-store (teamKey=${teamKey}) — backend will use legacy yaml-path fallback`,
    );
    return "";
  }
  return org.org_root ?? "";
}
