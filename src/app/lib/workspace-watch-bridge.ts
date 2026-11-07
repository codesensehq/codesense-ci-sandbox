// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { parseTeamKey } from "@/shared/lib/team-key";
import { findWorkspaceRepoBySlug } from "@/shared/lib/build-synthetic-repo";
import { deriveOrgRoot, getCodesenseHome } from "@/shared/lib/codesense-home";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";
import { resolveOrgRootForTeam } from "@/shared/lib/resolve-source-repo-path";
import { registerWorkspaceWatchHooks } from "@/shared/lib/workspace-watch-hooks";
import {
  lazyAttachWorkspace,
  lazyDetachWorkspace,
  toWatchConfig,
  type WatchConfig,
} from "@/tauri/commands";

/**
 * SW-WL-3 T2: resolve an open workspace surface to the data the watcher needs —
 * the per-workspace `WatchConfig`, its source-repo slug, and the resolved team
 * segment — in a single store read. Shared by `buildWorkspaceWatchConfig` (the
 * close path / tests) and `resolveAttachContext` (the open path) so neither
 * re-runs the workspace lookup or the team derivation.
 *
 * The watcher follows the workspace's working dir (`cwd` — the worktree for a
 * story workspace, the source-repo root for a root workspace). The remaining
 * fields (`context_repo_path` / `viewer_root` / staleness / pipeline_root) are
 * shared per team, so they come from the workspace's source-repo `RepoState`.
 *
 * Returns `null` when the workspace or its source repo can't be resolved
 * (e.g. a root still in `creating` with an empty `cwd`, or a workspace with no
 * `sourceRepoSlug`), in which case no attach/detach is issued.
 */
interface ResolvedWorkspaceWatch {
  config: WatchConfig;
  sourceRepoSlug: string;
  team: string;
}

function resolveWorkspaceWatch(
  teamKey: string,
  workspaceId: string,
): ResolvedWorkspaceWatch | null {
  const ws = (useViewerStore.getState().workspaces[teamKey] ?? []).find(
    (w) => w.id === workspaceId,
  );
  if (!ws || !ws.cwd || !ws.sourceRepoSlug) return null;

  const { orgSlug, teamName } = parseTeamKey(teamKey);
  // Synthetic keys use the resolved team segment ("default" fallback), not the
  // `_none` sentinel from `buildTeamKey`.
  const team = teamName ?? "default";
  const syntheticKey = `workspace:${orgSlug}:${team}:${ws.sourceRepoSlug}`;
  // SW-LN-B29: the exact key only matches a GitHub repo. `syntheticRepoKey` host-qualifies
  // every other repo (a no-remote local repo is `local/local/<repo>`), while the workspace
  // names it by the host-less `owner/repo` — so fall back to the team-scoped slug lookup
  // worktree creation already uses. Without it a local org's open worktrees never attach a
  // watch, and no team context edit ever fans out to them.
  const repo =
    useOrgStore.getState().workspaceRepos[syntheticKey] ??
    findWorkspaceRepoBySlug(ws.sourceRepoSlug, { orgSlug, teamName: team });
  if (!repo) return null;

  const config = toWatchConfig({
    repoPath: ws.cwd,
    contextRepoPath: repo.contextRepoPath,
  });
  return { config, sourceRepoSlug: ws.sourceRepoSlug, team };
}

/**
 * SW-WL-3 T2: the per-workspace `WatchConfig` for a surface, or `null` when it
 * can't be resolved. Public seam used by the close path (replays the
 * attach-time config) and the bridge tests.
 */
export function buildWorkspaceWatchConfig(
  teamKey: string,
  workspaceId: string,
): WatchConfig | null {
  return resolveWorkspaceWatch(teamKey, workspaceId)?.config ?? null;
}

/**
 * SW-WL-3 T4: everything `onWorkspaceOpened` needs to attach + reconcile a
 * just-opened surface. Extends the WatchConfig with the source-repo slug
 * (scopes reconcile-on-attach axes a+c) and the capture-rescan args (axis b),
 * which the watcher subsystem can't derive on its own.
 */
interface AttachContext {
  config: WatchConfig;
  sourceRepoSlug: string;
  orgRoot: string | null;
  codesenseHome: string | null;
  team: string;
}

function resolveAttachContext(teamKey: string, workspaceId: string): AttachContext | null {
  const resolved = resolveWorkspaceWatch(teamKey, workspaceId);
  if (!resolved) return null;
  const { config, sourceRepoSlug, team } = resolved;
  // The org root is derived from the context repo path; the CodeSense home is
  // the one app-wide value. No org-store lookup for the capture-rescan args.
  return {
    config,
    sourceRepoSlug,
    orgRoot: deriveOrgRoot(config.context_repo_path),
    codesenseHome: getCodesenseHome(),
    team,
  };
}

// SW-WL-3 T2: the WatchConfig captured at attach time, replayed at detach.
// The workspace may already be removed from the store by the time close fires
// (close detaches AFTER the store removal), so we can't always rebuild it.
const attachedConfigs = new Map<string, WatchConfig>();
const bridgeKey = (teamKey: string, workspaceId: string) => `${teamKey}::${workspaceId}`;

/**
 * SW-EW-8 (Slice A): the reverse-sync `WatchConfig` for an epic's integration
 * workspace, or `null` when this surface isn't an epic-level integration row.
 *
 * The epic integration workspace carries a synthetic `_epic` identity and has no
 * `workspaceRepos` entry, so `resolveWorkspaceWatch` (repo-keyed) can't serve it
 * — this is the cohesive sibling resolver (D1). It targets the epic dir as
 * `repo_path` and the team's flat content tree as `context_repo_path` (so the
 * reverse watch resolves team the same way every workspace does), enabling the
 * agent-write round-trip CP-4 powers and EW-7 (D4) assigned here.
 *
 * Scoped to the epic-LEVEL row (no `sourceRepoSlug`); a per-repo integration
 * terminal has a real slug and resolves through the normal repo path.
 */
function resolveEpicAttachConfig(teamKey: string, workspaceId: string): WatchConfig | null {
  const ws = (useViewerStore.getState().workspaces[teamKey] ?? []).find(
    (w) => w.id === workspaceId,
  );
  if (!ws || ws.kind !== "epic" || !ws.epicWorkspaceDir || ws.sourceRepoSlug) return null;

  const orgRoot = resolveOrgRootForTeam(teamKey);
  if (!orgRoot) return null;
  const team = parseTeamKey(teamKey).teamName ?? "default";
  // The reverse watch only needs repo_path + a `_team`-bearing
  // context_repo_path for team resolution.
  return toWatchConfig({
    repoPath: ws.epicWorkspaceDir,
    contextRepoPath: pathToTeamContentDirSync(orgRoot, team),
  });
}

/** A resolved attach: the `WatchConfig` and the slug for reconcile-on-attach
 *  scoping (`""` for epics → `lazy_attach` skips reconcile).
 *  SW-CP-18: the axis-b capture args are gone — reconcile-on-attach +
 *  claim/import adopt any inbox files written while the surface was closed. */
interface AttachPlan {
  config: WatchConfig;
  sourceRepoSlug: string;
}

/** Resolve a surface to its attach plan — the repo-keyed path first, then the
 *  SW-EW-8 epic fallback. One resolver so every attach site (open + redrive)
 *  treats epics identically. Returns null when neither resolves. */
function resolveAttachPlan(teamKey: string, workspaceId: string): AttachPlan | null {
  const ctx = resolveAttachContext(teamKey, workspaceId);
  if (ctx) {
    return {
      config: ctx.config,
      sourceRepoSlug: ctx.sourceRepoSlug,
    };
  }
  // SW-EW-8 (Slice A): epic integration workspaces have no source-repo slug /
  // `workspaceRepos` entry, so `resolveAttachContext` returns null. Attach the
  // reverse watch directly with an empty slug — `lazy_attach_workspace` then
  // registers the watch but SKIPS reconcile-on-attach (its documented
  // unparseable-slug branch); the epic's forward materialization is owned by
  // EW-7's EpicInheritance wave, so reconciling the synthetic `_epic/{id}` slug
  // would be redundant + run an unverified path. We want the reverse watch only.
  const epicCfg = resolveEpicAttachConfig(teamKey, workspaceId);
  return epicCfg ? { config: epicCfg, sourceRepoSlug: "" } : null;
}

/**
 * SW-WL-3 T2/T4 (AC-2/AC-5): wire the `WorkspaceOpenSet` transition hooks to
 * the surface-bound watcher commands. On open: attach + reconcile-on-attach
 * (axes a+c via `lazy_attach_workspace`, axis b via `capture_pending_files`).
 * Call once at app startup. Idempotent — re-registration replaces the hooks.
 */
export function registerWorkspaceWatchBridge(): void {
  registerWorkspaceWatchHooks({
    onWorkspaceOpened: (teamKey, workspaceId) => {
      const plan = resolveAttachPlan(teamKey, workspaceId);
      if (!plan) return;
      attachedConfigs.set(bridgeKey(teamKey, workspaceId), plan.config);
      // Attach + reconcile-on-attach (Rust enqueues the wave; epics pass an
      // empty slug so reconcile is skipped). SW-CP-18: the former axis-b
      // capture-inbox rescan is gone — reconcile-on-attach + claim/import adopt
      // any files written to the repo while the surface was closed.
      lazyAttachWorkspace(workspaceId, plan.sourceRepoSlug, plan.config).catch(() => {});
    },
    onWorkspaceClosed: (teamKey, workspaceId) => {
      const k = bridgeKey(teamKey, workspaceId);
      const cfg = attachedConfigs.get(k) ?? buildWorkspaceWatchConfig(teamKey, workspaceId);
      attachedConfigs.delete(k);
      if (cfg) {
        lazyDetachWorkspace(workspaceId, cfg).catch(() => {});
      }
    },
  });
}

/**
 * SW-LN-B51: re-attach every open workspace whose watch was attached with a different
 * context-repo path than its repo now resolves to — typically an empty path, because
 * the worktree opened on a repo added mid-session before Rust resolved its path. The
 * old subscription is detached first (its identity includes the context path), then the
 * workspace attaches with the resolved one, which subscribes it to the team content
 * watch so context edits fan out to it. Workspaces whose path is unchanged are left
 * alone.
 */
export function reattachWorkspaceWatchesWithNewContext(): void {
  for (const [key, old] of [...attachedConfigs.entries()]) {
    const sep = key.indexOf("::");
    if (sep < 0) continue;
    const teamKey = key.slice(0, sep);
    const workspaceId = key.slice(sep + 2);
    const plan = resolveAttachPlan(teamKey, workspaceId);
    if (!plan || plan.config.context_repo_path === old.context_repo_path) continue;
    attachedConfigs.set(key, plan.config);
    void lazyDetachWorkspace(workspaceId, old)
      .catch((e) => logger.debug("workspace-sync", `re-attach: detach failed: ${e}`))
      .then(() => {
        // Closed (or re-planned) while the detach was in flight: don't attach.
        if (attachedConfigs.get(key) !== plan.config) return;
        return lazyAttachWorkspace(workspaceId, plan.sourceRepoSlug, plan.config);
      })
      .catch((e) => {
        // Rust holds no watch now; forget the new config so the next pass retries.
        if (attachedConfigs.get(key) === plan.config) attachedConfigs.set(key, old);
        logger.warn("workspace-sync", `re-attach of ${workspaceId} failed: ${e}`);
      });
  }
}

/** Test-only: clear the captured-config cache between cases. */
export function __resetWorkspaceWatchBridge(): void {
  attachedConfigs.clear();
}
