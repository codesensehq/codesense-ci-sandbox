// SPDX-License-Identifier: MIT
import { teamContentIdentityForPath } from "./team-content-context";
import type { RepoState } from "@/shared/stores/config-store";
import type { Attachment, Org, SourceRepoSlug } from "@/shared/types/domain";
import type { WorkspaceRepoOverrides } from "@/shared/lib/workspace-config";
import { useOrgStore } from "@/shared/stores/org-store";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";
import { sourceRepoRemoteKey } from "@/shared/lib/source-repo-key";
import { parseTeamKey } from "@/shared/lib/team-key";

/** Canonical synthetic repoPath key for a workspace-tracked source repo. */
export function syntheticRepoKey(orgSlug: string, team: string, slug: SourceRepoSlug): string {
  const remoteKey = sourceRepoRemoteKey(slug);
  const host = remoteKey.split("/", 1)[0];
  // Preserve the established GitHub shape and owner/repo casing while using
  // the canonical helper's normalized host for every non-GitHub identity.
  const ownerRepo = `${slug.owner}/${slug.repo}`;
  const identity = host === "github.com" ? ownerRepo : `${host}/${ownerRepo}`;
  return `workspace:${orgSlug}:${team}:${identity}`;
}

/** T.4 Phase 10 (Round 2 HIGH #1): inverse of `promptIdentityFromRepoPath`.
 *  Reconstruct a synthetic repoPath from a `{teamKey, sourceRepoSlug}` shape
 *  (e.g. `noClonePromptRepo`). Used by the modal
 *  consumers so the modal displays + attaches the slug the PROMPT was
 *  raised for, not the panel's own slug — critical for M.4 cross-repo
 *  stories where the prompt's slug differs from the panel's repoPath.
 *
 *  Returns null when the inputs can't form a valid synthetic — caller
 *  should treat that as "don't render the modal here."
 *
 *  T.4 Phase 11 (Round 3 MED): `prompt.sourceRepoSlug` is lowercased at
 *  store-write time (by `promptIdentityFromRepoPath`), but `teams.yml`
 *  preserves case (e.g. `AllWidgets/AllWidgets`). Naively reconstructing
 *  with the lowercased slug produces a key that misses the exact-string
 *  lookup `attach-local-clone-dialog.tsx` does against `workspaceRepos`
 *  → auto-retry never fires. Fix: look up the canonical (case-preserving)
 *  repoPath from `workspaceRepos` via `findWorkspaceRepoBySlug` (which is
 *  already case-insensitive) when one matches; fall back to the lowercased
 *  reconstruction when no `workspaceRepos` entry is found (defensive). */
export function buildSyntheticRepoPathFromPrompt(prompt: {
  teamKey: string;
  sourceRepoSlug: string;
}): string | null {
  const idx = prompt.teamKey.indexOf(":");
  if (idx === -1) return null;
  const orgSlug = prompt.teamKey.slice(0, idx);
  const team = prompt.teamKey.slice(idx + 1);
  if (orgSlug === "" || team === "" || team === "_none" || orgSlug === "_no_team") return null;
  // Prefer the canonical case-preserved repoPath from workspaceRepos so
  // downstream exact-string lookups (`attach-local-clone-dialog.tsx`)
  // match the stored key shape. `findWorkspaceRepoBySlug` is
  // case-insensitive on the slug side and now scopes by team so an
  // `owner/repo` collision across two teams cannot return a foreign-team
  // entry (T.4 Round 5: was the root cause of the Chunk-D deferred
  // mixed-case fallback finding).
  const teamName = team === "_none" ? null : team;
  const match = findWorkspaceRepoBySlug(prompt.sourceRepoSlug, { orgSlug, teamName });
  if (match) return match.repoPath;
  // Fallback: no matching workspaceRepos entry for this team (legacy /
  // cold-start race). Reconstruct with the lowercased slug — exact-string
  // lookups will miss for mixed-case `teams.yml` entries, but this preserves
  // the pre-Phase-11 behavior for unknown-team cases.
  return `workspace:${orgSlug}:${team}:${prompt.sourceRepoSlug}`;
}

/** Returns true if the given repoPath is a synthetic workspace key. */
export function isSyntheticRepoPath(repoPath: string): boolean {
  return repoPath.startsWith("workspace:");
}

/** Extract the `"owner/repo"` slug from a synthetic workspace key.
 *  Returns the raw repoPath if it's not a synthetic key. */
export function parseSyntheticSlug(repoPath: string): string {
  if (!repoPath.startsWith("workspace:")) return repoPath;
  const parsed = parseSyntheticRepoKey(repoPath);
  return parsed ? `${parsed.owner}/${parsed.repo}` : repoPath;
}

/** Parse `(orgSlug, team, owner, repo)` from a synthetic workspace key.
 *  Returns null for malformed input.
 *
 *  Strict parsing — rejects keys with extra colon-segments
 *  (e.g. `"workspace:a:b:c:d"`) and keys whose `owner/repo` segment has
 *  more than one slash (e.g. `"owner/sub/repo"`). Synthetic keys are
 *  constructed internally so this is defense-in-depth. */
export function parseSyntheticRepoKey(
  repoPath: string,
): { orgSlug: string; team: string; owner: string; repo: string; host?: string } | null {
  if (!isSyntheticRepoPath(repoPath)) return null;
  // Format: "workspace:{orgSlug}:{team}:{owner}/{repo}"
  const withoutPrefix = repoPath.slice("workspace:".length);
  // Use full split (no limit) so extra colons surface as parse failure.
  const parts = withoutPrefix.split(":");
  if (parts.length !== 3) return null;
  const [orgSlug, team, ownerRepo] = parts;
  if (!orgSlug || !team || !ownerRepo) return null;
  const slashes = ownerRepo.split("/");
  if (slashes.length !== 2 && slashes.length !== 3) return null;
  const [host, owner, repo] = slashes.length === 3 ? slashes : [undefined, slashes[0], slashes[1]];
  if (!owner || !repo) return null;
  // Char-class validation: GitHub owner/repo segments are limited to
  // alphanumerics, dot, dash, underscore. Reject whitespace/control chars
  // even from internally-constructed keys (defense-in-depth against
  // corrupted persisted state).
  const segmentRe = /^[A-Za-z0-9._-]+$/;
  if (!segmentRe.test(owner) || !segmentRe.test(repo) || (host && !segmentRe.test(host)))
    return null;
  return { orgSlug, team, owner, repo, ...(host ? { host } : {}) };
}

/** Find a workspace repo by its `owner/repo` slug, scoped to a specific
 *  `(orgSlug, teamName)` team. Iterates `org-store.workspaceRepos` and
 *  matches on the trailing `{owner}/{repo}` segment, restricted to entries
 *  whose synthetic key begins with `workspace:${orgSlug}:${teamName ?? "_none"}:`.
 *  Returns `undefined` when no entry in the named team carries the slug.
 *
 *  T.4 Round 5 (Edge Case Hunter HIGH): the team filter is required because
 *  teams can independently track the same source repository — slug `acme/web`
 *  may exist in both team A and team B. Pre-Round-5 the function ignored
 *  team scope and returned the first insertion-order match, which stamped
 *  `noClonePromptRepo` / setup-modal identity with the wrong team's
 *  `teamKey` and caused the modal-render teamKey-equality gates to silently
 *  reject (modal never opens, retry callback stranded). Callers must thread
 *  the active team's context — derive via `parseTeamKey(teamKey)` or take
 *  it directly from the prompt identity.
 *
 *  Slug matching stays case-insensitive because `syntheticRepoKey`
 *  preserves the case of `slug.owner`/`slug.repo` from `teams.yml`, so a
 *  stored key like `workspace:9north:smart-web:AllWidgets/AllWidgets` must
 *  still match a lookup for `AllWidgets/AllWidgets` or `allwidgets/allwidgets`.
 *  The team prefix is matched verbatim — orgSlug and teamName segments are
 *  produced by `buildTeamKey` and always agree on case at write time. */
export function findWorkspaceRepoBySlug(
  slug: string,
  team: { orgSlug: string; teamName: string | null },
): RepoState | undefined {
  const normalized = slug.toLowerCase();
  const teamPrefix = `workspace:${team.orgSlug}:${team.teamName ?? "_none"}:`;
  const candidates = Object.values(useOrgStore.getState().workspaceRepos).filter((repo) =>
    repo.repoPath.startsWith(teamPrefix),
  );
  if (normalized.split("/").length === 3) {
    return candidates.find((repo) => repo.repoPath.toLowerCase().endsWith(`:${normalized}`));
  }

  // A legacy owner/repo lookup prefers the legacy GitHub key. It may fall back
  // to one host-qualified row, but never guesses when multiple hosts collide.
  const github = candidates.find(
    (repo) => repo.repoPath.toLowerCase() === `${teamPrefix}${normalized}`.toLowerCase(),
  );
  if (github) return github;
  const hostMatches = candidates.filter((repo) =>
    repo.repoPath.toLowerCase().endsWith(`/${normalized}`),
  );
  return hostMatches.length === 1 ? hostMatches[0] : undefined;
}

/** SW-FX-16: resolve the synthetic workspace key for a `context_repo_path`.
 *
 *  The `context-file-change` payload carries `context_repo_path` =
 *  `{org_root}/_team/{team}/repos/{owner}/{repo}` — the synthetic repo's
 *  `contextRepoPath`, which is derived from (org, team, slug) and is therefore
 *  **independent of which surface emitted the event**. The emitted `repo_path`
 *  is the surface's `ws.cwd` instead, so for a worktree surface it matches no
 *  `workspaceRepos[].localPath` (= the root-clone attachment path) and the
 *  disk-path fan-out in `bumpFileRefreshForSourceRepo` lands on a dead key.
 *
 *  Matching the payload's `context_repo_path` back to the owning workspace repo
 *  recovers the synthetic key the pipeline tree + viewers subscribe on,
 *  surface-independently. `contextRepoPath` is unique per (org, team, owner,
 *  repo), so the exact-string match cannot bleed across teams. The exact match
 *  is robust (not normalization-fragile): the payload value round-trips
 *  verbatim — the FE sends `repo.contextRepoPath` via `toWatchConfig`, and Rust
 *  echoes `config.context_repo_path` unchanged in `base_event_payload` — so it
 *  is byte-for-byte the same string `buildSyntheticRepo` stamped here.
 *
 *  Returns `null` when no workspace repo owns the dir (e.g. the `_codesense`
 *  internal handler, or a cold-start race before `workspaceRepos` is hydrated)
 *  — callers fall back to the raw emitted `repo_path`. */
export function syntheticKeyForContextRepoPath(contextRepoPath?: string): string | null {
  if (!contextRepoPath) return null;
  const match = Object.values(useOrgStore.getState().workspaceRepos).find(
    (r) => r.contextRepoPath === contextRepoPath,
  );
  return match?.repoPath ?? null;
}

/** SW-NR-1: resolve the owning team's synthetic workspace key for an absolute
 *  content-layer path — e.g. a watcher-reported team-content edit.
 *
 *  A reverse-sync write lands in a team's SHARED content dir
 *  (`{org_root}/{team}/content` = `RepoState.teamContentPath`). The team/epic
 *  pipeline nav subscribes on a team-stable synthetic whose `localPath` is the
 *  root-clone attachment — never the editing worktree — so the worktree-path
 *  `localPath` fan-out in `bumpFileRefreshForSourceRepo` can never reach it.
 *  Prefix-match the written path against each synthetic's `teamContentPath` and
 *  return ANY one owning synthetic key: every synthetic of a team shares that
 *  dir, so feeding the returned key back through `bumpFileRefreshForSourceRepo`
 *  fans out to the whole `(org, team)` for free. This is the reverse-direction
 *  twin of {@link syntheticKeyForContextRepoPath} (SW-FX-16).
 *
 *  Path-boundary safe: an exact match or a `/`-delimited descendant only, so a
 *  `…/content` root never matches a `…/content-archive` sibling. Entries with an
 *  empty `teamContentPath` (non-synthetic own-repo rows carry `""`) are skipped
 *  — an empty prefix would otherwise swallow every path.
 *
 *  Returns `null` when no known team owns the path (a write outside any attached
 *  team, or a cold-start race before `workspaceRepos` is hydrated) — callers
 *  simply skip the team-nav bump. */
export function syntheticKeyForContentPath(contentPath?: string): string | null {
  if (!contentPath) return null;
  const match = Object.values(useOrgStore.getState().workspaceRepos).find((r) => {
    const root = r.teamContentPath;
    return !!root && (contentPath === root || contentPath.startsWith(`${root}/`));
  });
  return match?.repoPath ?? null;
}

/** G-8: the file-refresh key a `context-file-change` event bumps. A workspace
 *  subscriber's change routes by its `context_repo_path` (SW-FX-16); a team-level
 *  change — the team content watch firing with no workspace subscriber, so it
 *  names no repo — routes by the content path it touched, to the owning team
 *  (whose synthetic key fans out to the whole team); otherwise the emitted
 *  `repo_path`. SW-XS-20: a change in a parent team's content routes to the
 *  team that inherits it (`inherited_by`) first. `null` when nothing names an owner (nothing to bump). */
export function bumpKeyForContextFileChange(payload: {
  repo_path: string;
  context_repo_path?: string;
  file_path?: string;
  inherited_by?: string;
}): string | null {
  return (
    // SW-XS-20: a parent team's file changed — refresh the inheriting team,
    // whose nav shows it (the parent itself is not the open team).
    teamContentIdentityForPath(payload.inherited_by, useOrgStore.getState().orgs) ??
    syntheticKeyForContentPath(payload.inherited_by) ??
    teamContentIdentityForPath(payload.file_path, useOrgStore.getState().orgs) ??
    syntheticKeyForContextRepoPath(payload.context_repo_path) ??
    (payload.repo_path ? null : syntheticKeyForContentPath(payload.file_path)) ??
    (payload.repo_path || null)
  );
}

/** Filter a `workspaceRepos` map to entries owned by a single team.
 *
 *  Used by T.2's `ensureRootWorkspaces(teamKey)`, auto-sync orphan
 *  detection, and persistence v1 → v2 migration. Returns array order
 *  matching `workspaceRepos` insertion order, which `use-workspace-viewer-sync`
 *  builds in `teams.yml.source_repos` order — i.e. canonical bar order.
 *
 *  Returns empty for the `NO_TEAM_SENTINEL` teamKey (parsed teamName === null);
 *  synthetic keys always carry a non-null team segment, so no match is
 *  possible. Returns empty when no entries match. */
export function workspaceReposForTeam(
  workspaceRepos: Record<string, RepoState>,
  teamKey: string,
): RepoState[] {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  if (teamName === null) return [];
  return Object.values(workspaceRepos).filter((r) => {
    const parsed = parseSyntheticRepoKey(r.repoPath);
    return parsed?.orgSlug === orgSlug && parsed?.team === teamName;
  });
}

/** The first (canonical-order) source `repoPath` for a team, or `undefined` when
 *  the team has no source repo. This is the value every `openFile` /
 *  `revealOpenedFile` / `openFile` caller needs as its 2nd arg: it is
 *  consumed only to resolve the team's `teamContentPath`, and every team-member
 *  synthetic resolves to the same content path — so the first entry is
 *  sufficient. Folds the duplicated `workspaceReposForTeam(...)[0]?.repoPath`
 *  one-liner into one source of truth. */
export function firstRepoPathForTeam(
  workspaceRepos: Record<string, RepoState>,
  teamKey: string,
): string | undefined {
  return workspaceReposForTeam(workspaceRepos, teamKey)[0]?.repoPath;
}

/** SF-8: filter `workspaceRepos` by the literal `(orgSlug, team)` pair used
 *  in synthetic-key construction. Unlike `workspaceReposForTeam`, this does
 *  not roundtrip through `parseTeamKey`'s `_none` convention — it takes the
 *  team string directly, mirroring the value `buildSyntheticRepo` stamps
 *  into the synthetic `repoPath`. Required for hard-switch Phase 1 + the
 *  bail-cleanup helper, where the resolved team (with `?? "default"`
 *  fallback) is the only consistent way to find a run's own synthetics. */
export function workspaceReposForOrgTeam(
  workspaceRepos: Record<string, RepoState>,
  orgSlug: string,
  team: string,
): RepoState[] {
  return Object.values(workspaceRepos).filter((r) => {
    const parsed = parseSyntheticRepoKey(r.repoPath);
    return parsed?.orgSlug === orgSlug && parsed?.team === team;
  });
}

/** Build a runtime-only RepoState for a workspace-tracked source repo.
 *  The synthetic `repoPath` key is NOT a filesystem path — it encodes
 *  the (workspace, team, source repo) tuple for use as a store key.
 *
 *  F-6: `attachmentsByRemote` is the lookup produced by
 *  `listAttachmentsByRemote(github_user_id)` — keys are lowercase
 *  `"owner/repo"`. The caller passes an empty Map when `currentUser` is
 *  null; downstream code uses `resolveSourceRepoPath(repo)` to recover
 *  the real on-disk path.
 *
 *  SF-8: `teamContentPath` was populated via an async Tauri call into Rust's
 *  `path_to_team_content_dir` (PTS-3). That function performs pure path math
 *  with no IO, so the async hop was gratuitous — replaced by a synchronous
 *  TS mirror (`pathToTeamContentDirSync`, see `team-content-path.ts`). The
 *  sync form unblocks SF-8 Piece 1's same-frame workspace-bar mount in the
 *  hard-switch IIFE. */
export function buildSyntheticRepo(
  org: Org,
  team: string,
  slug: SourceRepoSlug,
  attachmentsByRemote: Map<string, Attachment>,
  overrides?: WorkspaceRepoOverrides,
): RepoState {
  const key = sourceRepoRemoteKey(slug);
  const localPath = attachmentsByRemote.get(key)?.repo_path ?? null;
  const teamContentPath = pathToTeamContentDirSync(org.org_root, team);

  return {
    repoPath: syntheticRepoKey(org.slug, team, slug),
    repoName: `${slug.repo} (${org.display_name})`,
    // Rust hydrates the physical path. The renderer owns logical identity and
    // display only; it never formats the host-aware filesystem key.
    contextRepoPath: org.resolved_repository_paths?.[`${team}\n${key}`] ?? "",
    teamContentPath,
    hasRemote: !!org.remote_url,
    remoteUrl: org.remote_url,
    mainDoc: overrides?.mainDoc ?? "",
    localPath,
  };
}
