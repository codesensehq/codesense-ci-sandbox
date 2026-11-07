// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef } from "react";
import { toast } from "sonner";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { useOrgStore } from "@/shared/stores/org-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  initWatchedCache,
  listAttachmentsByRemote,
  reconcileWorktreesOnLaunch,
  removeWatch,
  toWatchConfig,
} from "@/tauri/commands";
import { commitActiveTeamForWave } from "@/shared/lib/backend-active-team";
import {
  buildSyntheticRepo,
  parseSyntheticRepoKey,
  syntheticRepoKey,
  workspaceReposForOrgTeam,
} from "@/shared/lib/build-synthetic-repo";
import { sourceRepoRemoteKey } from "@/shared/lib/source-repo-key";
import { buildTeamKey, NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { getCodesenseHome } from "@/shared/lib/codesense-home";
import { workspaceRepoOverrides } from "@/shared/stores/config-store";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { logger } from "@/shared/lib/logger";
import type { Attachment, SourceRepoSlug } from "@/shared/types/domain";

interface PrevWorkspace {
  orgSlug: string | null;
  team: string;
  /** Raw activeTeamName (null when no team selected). */
  activeTeamName: string | null;
  sourceReposKey: string;
  /** F-6: tracks whether attachments data has refreshed. A change here
   *  alone (without sourceReposKey change) triggers a SOFT refresh —
   *  in-place rebuild of synthetic repos so `localPath` updates without
   *  losing selection/watchers. */
  githubUserId: number | null;
  attachmentsVersion: number;
}

/** Tear down all synthetic workspace repos: remove watchers, suspend or
 *  close repos, and clear/suspend the workspace repo store.
 *
 *  F-19: when `suspend` is true and `workspaceKey` is provided, repos are
 *  suspended (team-keyed slices preserved on the viewer-store; the team's
 *  PTYs survive the F-16 guard) instead of fully closed. The non-suspend
 *  path is used by unmount cleanup, null-org teardown, and org-removal
 *  teardown — it calls `closeTeam(outgoingTeamKey)` to free team-keyed
 *  slices + kill PTYs. `outgoingTeamKey` is passed in by the caller
 *  (`null` for cold-start where no outgoing team exists). */
function teardownWorkspaceRepos(opts?: {
  suspend?: boolean;
  workspaceKey?: string;
  outgoingTeamKey?: string | null;
}): void {
  const suspend = opts?.suspend ?? false;
  const outgoing = Object.values(useOrgStore.getState().workspaceRepos);
  if (outgoing.length === 0) return;
  // T.6: `selectRepo`, `closeRepo`, `goToPicker` retire — the per-repo
  // teardown collapses to a single `closeTeam` call that frees team-keyed
  // slices + kills PTYs. `removeWatch` still loops per source repo since
  // each one has its own filesystem watch.
  for (const outgoingRepo of outgoing) {
    removeWatch(toWatchConfig(outgoingRepo)).catch(() => {});
  }
  if (suspend && opts?.workspaceKey) {
    useOrgStore.getState().suspendWorkspaceRepos(opts.workspaceKey);
  } else {
    useOrgStore.getState().clearWorkspaceRepos();
  }
  // Close only after the synthetics are gone: a team whose synthetics are still
  // mounted is re-opened (see `reopenMountedActiveTeam`).
  if (!suspend && opts?.outgoingTeamKey !== undefined) {
    useViewerStore.getState().closeTeam(opts.outgoingTeamKey);
  }
}

/** SF-8: surgical suspend of a single team's synthetics. Unlike
 *  `teardownWorkspaceRepos({suspend, workspaceKey})` which snapshots ALL
 *  of `workspaceRepos`, this only touches entries whose synthetic key
 *  matches `(orgSlug, team)`. Required during rapid-switch (A→B→A) when
 *  more than one team's synthetics briefly coexist in `workspaceRepos`:
 *  a wholesale suspend would mis-attribute the other team's entries
 *  under the outgoing key. Pass the resolved `team` string (with
 *  `?? "default"` fallback) — synthetic keys use that segment, not the
 *  `_none` convention from `buildTeamKey`.
 *
 *  No-op if no matching entries exist. */
function suspendTeamSynthetics(workspaceKey: string, orgSlug: string, team: string): void {
  const targetRepos = workspaceReposForOrgTeam(
    useOrgStore.getState().workspaceRepos,
    orgSlug,
    team,
  );
  if (targetRepos.length === 0) return;
  // No removeWatch: SW-WL-3 moved watch attach out of this hook, so there is no watch of its own
  // to remove (and the IPC would queue behind the activation lock on every switch).
  useOrgStore.getState().suspendTeamRepos(workspaceKey, targetRepos);
}

/** SF-8 review iter-2 (BH-2): surgical CLOSE of a single team's
 *  synthetics, used when bailing BEFORE Phase 2b's attachment refinement.
 *  Phase 2a mints placeholder synthetics with empty attachments (i.e.
 *  `localPath: null`); if we suspend those, the F-19 resume path on a
 *  later re-click restores the stale placeholders verbatim — the user
 *  ends up with a "permanently broken" team (null localPath, no
 *  re-attempt of `loadAttachmentsMap`). Instead, clear them: a fresh
 *  re-click takes the hard-switch path and re-mints with current
 *  attachments. The team-keyed `closeTeam` call frees any per-team
 *  slices (PTYs etc.) that may have been touched during the brief
 *  window. */
function closeTeamSynthetics(workspaceKey: string, orgSlug: string, team: string): void {
  const targetRepos = workspaceReposForOrgTeam(
    useOrgStore.getState().workspaceRepos,
    orgSlug,
    team,
  );
  // SF-8 review iter-2 (BH-2, retest fix): only close the team-keyed
  // slices when this run still holds live synthetics in `workspaceRepos`.
  // If a concurrent run's Phase 1 already moved them to
  // `suspendedWorkspaceRepos` (the common A→B→A interleave), the team
  // is still alive — just not under this run's ownership — and calling
  // `closeTeam(workspaceKey)` would wrongly evict it from `openedTeams`
  // and kill PTYs the OTHER run intends to keep. The early-return
  // preserves F-19's "suspended-not-closed" invariant under bail.
  if (targetRepos.length === 0) return;
  for (const r of targetRepos) {
    removeWatch(toWatchConfig(r)).catch(() => {});
    useOrgStore.getState().removeWorkspaceRepo(r.repoPath);
  }
  useViewerStore.getState().closeTeam(workspaceKey);
}

/** T.1 D-4 step 6: sweep the migration sentinel bucket
 *  (`workspaces[NO_TEAM_SENTINEL]`) once the first hard-switch lands and
 *  `workspaceRepos` is populated. Retries each workspace's
 *  `sourceRepoSlug` → teamKey resolution. Successes re-bucket into their
 *  team; persistent failures are dropped with a single warn. After the
 *  sweep, the sentinel bucket is deleted entirely.
 *
 *  Runs at most once per hook lifetime (post-migration). The `swept` ref
 *  in the hook gates re-invocation. */
function sweepSentinelBucket(): void {
  const state = useViewerStore.getState();
  const sentinelWorkspaces = state.workspaces[NO_TEAM_SENTINEL] ?? [];

  const workspaceRepos = useOrgStore.getState().workspaceRepos;
  // Build slug → teamKey lookup. Use parseSyntheticRepoKey so the recovered
  // teamKey preserves the original casing of activeOrgSlug / activeTeamName
  // (the lowercased regex variant produced a key the UI never read from).
  const slugToTeamKey = new Map<string, string>();
  for (const repo of Object.values(workspaceRepos)) {
    const parsed = parseSyntheticRepoKey(repo.repoPath);
    if (!parsed) continue;
    const slug = `${parsed.owner}/${parsed.repo}`.toLowerCase();
    slugToTeamKey.set(slug, buildTeamKey(parsed.orgSlug, parsed.team));
  }

  const reBucketed: Record<string, typeof sentinelWorkspaces> = {};
  const dropped: string[] = [];
  for (const ws of sentinelWorkspaces) {
    const slug = ws.sourceRepoSlug?.toLowerCase();
    const teamKey = slug ? slugToTeamKey.get(slug) : undefined;
    if (teamKey !== undefined) {
      reBucketed[teamKey] = [...(reBucketed[teamKey] ?? []), ws];
    } else {
      dropped.push(ws.id);
    }
  }

  // Re-bucket recoverable workspaces into their owning teams. closeTeam
  // below handles the sentinel cleanup across all six team-keyed slices
  // (workspaces, activeWorkspace, 4 terminal slices) and kills any orphan
  // PTYs left in the sentinel bucket from the migration.
  if (Object.keys(reBucketed).length > 0) {
    useViewerStore.setState((s) => {
      const { [NO_TEAM_SENTINEL]: _sw, ...restWorkspaces } = s.workspaces;
      const merged = { ...restWorkspaces };
      for (const [teamKey, list] of Object.entries(reBucketed)) {
        merged[teamKey] = [...(merged[teamKey] ?? []), ...list];
      }
      return { workspaces: merged };
    });
  }
  // Always run closeTeam(NO_TEAM_SENTINEL) — it's the single path that
  // cleans every team-keyed slice consistently and kills any sentinel
  // PTYs. No-op when the bucket is already empty.
  useViewerStore.getState().closeTeam(NO_TEAM_SENTINEL);

  const reBucketedCount = Object.values(reBucketed).reduce((acc, list) => acc + list.length, 0);
  if (reBucketedCount > 0 || dropped.length > 0) {
    logger.warn(
      "workspace-sync",
      `[migration sweep] sentinel bucket cleaned: ${reBucketedCount} re-bucketed, ${dropped.length} dropped${
        dropped.length > 0 ? ` (${dropped.join(", ")})` : ""
      }`,
    );
  }
}

/** SF-8: rollback for the synchronously-mounted incoming team after a bail
 *  fired post-`openTeam`. The hard-switch IIFE mounts the incoming team
 *  SYNCHRONOUSLY before any await — that buys same-frame chrome but means
 *  a stale-runId bail mid-await leaves the new team mounted unless we
 *  explicitly clean it up.
 *
 *  Behavior: surgically SUSPEND `newTeamKey` (move ONLY this run's
 *  synthetics from `workspaceRepos` into `suspendedWorkspaceRepos[newTeamKey]`)
 *  rather than close it. The user clicked this team intentionally before
 *  superseding it; preserving the mount in suspended state lets a future
 *  switch back resume PTYs / state via the existing resume path.
 *  `openedTeams` stays unchanged so the team remains in the per-team
 *  stack at `display:none`.
 *
 *  Takes `orgSlug` + `team` as the SYNTHETIC-KEY scoping pair (resolved
 *  with `?? "default"` fallback) — not derived from `newTeamKey` because
 *  `buildTeamKey`'s `_none` convention doesn't roundtrip to the synthetic
 *  team segment.
 *
 *  SF-8 review iter-1 (EC-1): per-run ownership gate. `mountRunIdRef`
 *  records which run minted the current mount for each `newTeamKey`.
 *  If a later run has re-mounted the same team (mountRunIdRef value
 *  no longer matches this run's `myRunId`), the bail skips its suspend
 *  — otherwise a stale Run N's bail would clobber Run M's live mount.
 *  The clean-mount case (no owner registered) lets the suspend proceed
 *  for defensive reasons (e.g. pre-mount bail paths). On a successful
 *  bail, the ownership entry is cleared so a subsequent re-claim by a
 *  later run starts fresh. */
/** SW-LN-B68: back-off between retries of a failed repo-less active-team commit. */
const NO_REPO_COMMIT_RETRY_MS = [1000, 2000, 4000] as const;

function bailHardSwitchEarlyMount(
  newTeamKey: string,
  orgSlug: string,
  team: string,
  reason: string,
  myRunId: number,
  mountRunIdMap: Map<string, number>,
  opts?: { prePhase2b?: boolean },
): void {
  const owner = mountRunIdMap.get(newTeamKey);
  if (owner !== undefined && owner !== myRunId) {
    logger.debug(
      "workspace-sync",
      `bail skipped for ${newTeamKey}: mount re-claimed by run ${owner} (this is run ${myRunId}) — ${reason}`,
    );
    return;
  }
  // SF-8 review iter-2 (BH-2): bails BEFORE Phase 2b's attachment
  // refinement clear placeholder synthetics rather than suspending them.
  // Suspending placeholders persists the empty-attachments shape into
  // `suspendedWorkspaceRepos[newTeamKey]`, which the F-19 resume path
  // restores verbatim on a future re-click — leaving the user with
  // `localPath: null` and no path back to a fresh mint.
  if (opts?.prePhase2b) {
    closeTeamSynthetics(newTeamKey, orgSlug, team);
    mountRunIdMap.delete(newTeamKey);
    logger.debug(
      "workspace-sync",
      `hard-switch superseded mid-await — closed placeholder mount for ${newTeamKey} (${reason})`,
    );
    return;
  }
  suspendTeamSynthetics(newTeamKey, orgSlug, team);
  mountRunIdMap.delete(newTeamKey);
  logger.debug(
    "workspace-sync",
    `hard-switch superseded mid-await — suspended early mount for ${newTeamKey} (${reason})`,
  );
}

/** A scope the warm-scope owner dropped (suspended past the switch threshold): free the team's
 *  renderer state and team-scoped data while keeping its terminal sessions, which are global and
 *  restore through the serialize/snapshot path on the next visit. A team with unsaved editor
 *  buffers is pinned by the render gate, so the owner never drops it. */
function releaseGoneScope(key: string): void {
  const repoPaths = useOrgStore.getState().evictSuspendedWorkspace(key);
  // Free team-keyed slices once for the dropped workspaceKey (a teamKey in shape:
  // `${orgSlug}:${teamName ?? "_none"}`).
  useViewerStore.getState().closeTeam(key, { preserveTerminalSessions: true });
  // SW-FX-64 (AC-8): drop the dropped repos' file-refresh-key slots so they don't accumulate
  // add-only across a long multi-worktree session. These are team-scoped synthetic keys, safe to
  // delete (no mounted viewers).
  useFileRefreshStore.getState().pruneRepos(repoPaths);
  logger.info("workspace-sync", `Released gone workspace ${key} (${repoPaths.length} repos)`);
}

/** F-6: Load attachments for the given user.
 *
 *  Returns:
 *  - `new Map()` when `githubUserId` is null (pre-login / hydration). All
 *    synthetics graceful-degrade to `localPath: null` per spec Approach.
 *  - The populated Map on IPC success.
 *  - `null` on IPC failure. R5B-P5: callers distinguish this from "empty
 *    map" so a transient failure during a soft refresh doesn't wipe
 *    `localPath` for every synthetic (including one the user just
 *    successfully attached). Hard-switch callers can treat null as empty
 *    Map (a fresh switch with no attachments data is a legitimate state).
 */
async function loadAttachmentsMap(
  githubUserId: number | null,
): Promise<Map<string, Attachment> | null> {
  if (githubUserId === null) return new Map();
  try {
    return await listAttachmentsByRemote(githubUserId);
  } catch (e) {
    logger.warn("workspace-sync", `listAttachmentsByRemote failed: ${e}`);
    return null;
  }
}

/** Synchronizes the viewer with the active workspace.
 *
 *  Watches `[activeOrgSlug, activeTeamName, orgs, githubUserId, attachmentsVersion]`
 *  and runs one of two paths on change:
 *
 *  - **Hard switch** (workspace identity or repo set changed): tear down
 *    outgoing synthetic repos, build new ones, open team, set up watcher.
 *
 *  - **Soft refresh** (F-6: only `githubUserId` or `attachmentsVersion`
 *    changed): rebuild synthetic repos in place with the new attachments
 *    map. No teardown, no selection reset, no watcher churn — used to
 *    propagate `localPath` updates after silent auto-attach or the
 *    AC-8 empty-state CTA.
 *
 *  The `orgs` dep handles the cold-start race: `activeOrgSlug` is restored
 *  from persistence before `setOrgs()` runs, so the hook must re-fire when
 *  the org list becomes available. */
export function useWorkspaceViewerSync(): void {
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const orgs = useOrgStore((s) => s.orgs);
  const githubUserId = useCurrentUser()?.github_user_id ?? null;
  const attachmentsVersion = useOrgStore((s) => s.attachmentsVersion);
  // SW-FX-66 (D2.1): a bump signals a worktree link was just (re)established.
  // Latest identity for the verify-beat timer, read at fire time. Kept in a ref
  // (updated every render) so the one-shot timer re-arms ONLY on a new establish
  // — never on unrelated `orgs`/`activeOrgSlug`/`githubUserId` reference churn,
  // which would clear-and-restart the countdown and could starve it forever.

  const prevRef = useRef<PrevWorkspace>({
    orgSlug: null,
    team: "default",
    activeTeamName: null,
    sourceReposKey: "",
    githubUserId: null,
    attachmentsVersion: 0,
  });

  /** Tracks the active workspace composite key for stale-flight detection.
   *  Distinct from prevRef.sourceReposKey which tracks repo-set identity. */
  const workspaceKeyRef = useRef<string | null>(null);

  /** P2 (code-review patch): stale-flight token. Bumped at the top of every
   *  effect run; each async IIFE captures its own id and bails on any await
   *  resumption where the live id has moved on. Prevents a previous
   *  invocation's `setWorkspaceRepo` / `prevRef.current` writes from
   *  clobbering a more recent run's state when two effect runs overlap
   *  (e.g., workspace switch + login completing close together). */
  const runIdRef = useRef(0);

  /** T.1 D-4 step 6: tracks whether the post-migration sentinel sweep has
   *  run for this hook lifetime. Runs at most once. */
  const sentinelSweptRef = useRef(false);

  /** SF-8 review iter-1 (EC-1): per-run ownership map for synchronously-
   *  mounted teams. Keyed by `nextKey`; value is the `myRunId` of the
   *  run that minted the current mount. Read in `bailHardSwitchEarlyMount`
   *  to prevent a stale run from suspending a mount that a later run has
   *  re-claimed. Stable across renders (useRef); cleared on a successful
   *  bail (so a subsequent re-claim by a later run starts fresh). */
  const mountRunIdRef = useRef<Map<string, number>>(new Map());

  /** The run id of the hard switch still in flight, or null. A soft refresh
   *  must not supersede one: the soft path never re-claims the mount, so the
   *  superseded hard switch would bail as the mount's owner and close (or
   *  suspend) the team that is still active, and its remaining work (watchers,
   *  cache init, team wave) would never run. */
  const hardSwitchInFlightRef = useRef<number | null>(null);

  /** SS-4c AC-16: tracks the first-mount-with-team team activation state
   *  for this hook lifetime. Tri-state per iter-3 (Blind H1 / Edge
   *  HIGH-3): `'idle'` allows the next effect to enter; `'in_flight'`
   *  blocks duplicate concurrent enters (StrictMode double-mount or
   *  dep churn before the awaits resolve); `'done'` blocks all further
   *  attempts (success). On failure, we reset to `'idle'` for retry. */
  const startupStateRef = useRef<"idle" | "in_flight" | "done">("idle");
  // SW-LN-B51: the `{org}\n{team}\n{sourceReposKey}` Rust's wave engine last accepted via
  // `setActiveTeamForWave`. The source-repos refresh below compares against THIS, not
  // `prev.sourceReposKey`: Phase 2a advances `prevRef` synchronously, so when a newer
  // effect run supersedes the one that saw the repo set change (adding a repo also bumps
  // attachments), the newer run saw no change and Rust kept the old repo set — worktrees
  // of the added repo were never in the team's forward fan-out.
  const waveTargetKeyRef = useRef<string | null>(null);

  // SW-TS-5 §4.4: the github user id the worktree launch reconcile last fired
  // for (null = not yet, or logged out). Keying on the user id — rather than a
  // bare "fired" boolean — makes the once-per-session reconcile re-fire when a
  // DIFFERENT user signs in (a different `users/{uid}/` worktree tree), even on
  // a direct account switch with no logout hop, while still de-duping
  // StrictMode double-mounts, team/org navigation, and dep churn for the same
  // user. (Logout only flips auth status — it does not null `activeOrgSlug` —
  // so the reset can't live on the no-active-org branch.)
  const worktreeReconciledUidRef = useRef<number | null>(null);

  // Unmount-only cleanup: tear down watchers when the component hosting
  // this hook unmounts. Dep-change teardown is handled by Phase 1 inside
  // the main effect. Using a separate effect avoids the cleanup running
  // on every dep change (which would wipe workspaceRepos before the no-op
  // guard has a chance to skip the rebuild).
  useEffect(() => {
    return () => {
      // R3-P1: invalidate any in-flight IIFE so it doesn't write to the
      // shared workspaceRepos store after the hook has torn down. The hook
      // is normally mounted at the app shell level, so this matters most
      // for HMR / test re-mount scenarios. The ref-changed-by-cleanup lint
      // warning is a false positive — reading the *latest* current value
      // at cleanup time is the desired behavior here.
      // eslint-disable-next-line react-hooks/exhaustive-deps
      ++runIdRef.current;
      // T.1 AC-11: pass outgoing teamKey (or null if cold-start) so the
      // non-suspend teardown frees team-keyed slices via closeTeam.
      const unmountOutgoingTeamKey =
        prevRef.current.orgSlug !== null
          ? buildTeamKey(prevRef.current.orgSlug, prevRef.current.activeTeamName)
          : null;
      teardownWorkspaceRepos({ outgoingTeamKey: unmountOutgoingTeamKey });
    };
  }, []);

  // The active team's pane mounts only while it is in `openedTeams`. A mounted
  // active team missing from the list is never a legitimate state, yet it
  // happens when a persisted list loads over a fresh mount — and it leaves the
  // content area blank with no way back. Re-open the team whenever that happens.
  //
  // "Mounted" is either of: its synthetics are present (every close path removes
  // them before closing, so a real close never trips this), or — for a team
  // that tracks no source repos, so has no synthetics (SW-FU-B2: the typical
  // v0.0.11 `general` team) — this hook finished switching to it and it still
  // exists in the org. Org removal, a null org and unmount all reset that.
  const reopenMountedActiveTeam = useCallback(() => {
    const {
      activeOrgSlug: orgSlug,
      activeTeamName: teamName,
      workspaceRepos,
      orgs: currentOrgs,
    } = useOrgStore.getState();
    if (orgSlug === null) return;
    const viewer = useViewerStore.getState();
    if (!viewer.stateLoaded) return;
    const teamKey = buildTeamKey(orgSlug, teamName);
    if (viewer.openedTeams.includes(teamKey)) return;
    const hasSynthetics =
      workspaceReposForOrgTeam(workspaceRepos, orgSlug, teamName ?? "default").length > 0;
    if (!hasSynthetics) {
      const team = currentOrgs
        .find((o) => o.slug === orgSlug)
        ?.teams.find((t) => t.name === teamName);
      const settledHere =
        prevRef.current.orgSlug === orgSlug && prevRef.current.activeTeamName === teamName;
      if (!team || team.source_repos.length > 0 || !settledHere) return;
    }
    logger.warn("workspace-sync", `re-opening active team ${teamKey} (mounted but not opened)`);
    viewer.openTeam(teamKey);
  }, []);

  // The owner drops a scope once it has been suspended past the switch threshold.
  useEffect(() => {
    warmScopes.setGoneHandler(releaseGoneScope);
    return () => warmScopes.setGoneHandler(null);
  }, []);

  useEffect(() => {
    reopenMountedActiveTeam();
    const unsubscribeViewer = useViewerStore.subscribe(reopenMountedActiveTeam);
    const unsubscribeOrg = useOrgStore.subscribe(reopenMountedActiveTeam);
    return () => {
      unsubscribeViewer();
      unsubscribeOrg();
    };
  }, [reopenMountedActiveTeam]);

  useEffect(() => {
    if (activeOrgSlug === null) {
      // No workspace active — tear down any leftover synthetic repos.
      // R3-P1: bump runIdRef BEFORE the synchronous writes so any in-flight
      // IIFE from a prior run sees a stale myRunId on resumption and bails,
      // rather than re-populating workspaceRepos that we just tore down.
      ++runIdRef.current;
      // T.1 AC-11: free team-keyed slices for the outgoing team.
      const nullOrgOutgoingTeamKey =
        prevRef.current.orgSlug !== null
          ? buildTeamKey(prevRef.current.orgSlug, prevRef.current.activeTeamName)
          : null;
      teardownWorkspaceRepos({ outgoingTeamKey: nullOrgOutgoingTeamKey });
      prevRef.current = {
        orgSlug: null,
        team: "default",
        activeTeamName: null,
        sourceReposKey: "",
        githubUserId,
        attachmentsVersion,
      };
      workspaceKeyRef.current = null;
      // Iter-5 (Edge HIGH-3): logout / no-active-org clears the Startup
      // "fire once per session" gate so the next login fires its own
      // team activation. Without this reset, a session that logs out and
      // back in stays at `'done'` and never converges drift on relogin.
      startupStateRef.current = "idle";
      waveTargetKeyRef.current = null;
      return;
    }

    const org = orgs.find((o) => o.slug === activeOrgSlug);
    if (!org) {
      // R3-P1: bump runIdRef so prior in-flight IIFEs are invalidated —
      // otherwise a late-arriving IIFE could write to workspaceRepos after
      // this branch returned.
      ++runIdRef.current;
      // R5B-P3: distinguish cold-start race (`orgs` not yet loaded) from
      // org-removal (`orgs` non-empty but `activeOrgSlug` no longer in the
      // list — e.g., membership deleted, registry refresh removed the org).
      // Cold-start: just return; next effect run rebuilds once orgs arrive.
      // Removal: tear down stale synthetic repos + reset prev so future
      // effect runs don't short-circuit against the now-defunct workspace.
      if (orgs.length === 0) {
        return;
      }
      // T.1 AC-11: org-removal teardown frees the outgoing team's slices.
      const removedOrgOutgoingTeamKey =
        prevRef.current.orgSlug !== null
          ? buildTeamKey(prevRef.current.orgSlug, prevRef.current.activeTeamName)
          : null;
      teardownWorkspaceRepos({ outgoingTeamKey: removedOrgOutgoingTeamKey });
      prevRef.current = {
        orgSlug: null,
        team: "default",
        activeTeamName: null,
        sourceReposKey: "",
        githubUserId,
        attachmentsVersion,
      };
      workspaceKeyRef.current = null;
      // Iter-5 (Edge HIGH-3): same as the activeOrgSlug=null branch —
      // org removal clears the Startup gate so re-selecting / re-adding
      // an org later in the same session fires a fresh first-mount activation.
      startupStateRef.current = "idle";
      waveTargetKeyRef.current = null;
      return;
    }

    const team = activeTeamName ?? "default";
    const trackedRepos: SourceRepoSlug[] =
      org.teams.find((t) => t.name === team)?.source_repos ?? [];

    // Derive a stable identity for this workspace's repo set.
    const sourceReposKey = trackedRepos
      .map((s) => `${s.owner}/${s.repo}`)
      .sort()
      .join(",");
    const waveTargetKey = `${org.slug}\n${team}\n${sourceReposKey}`;

    const prev = prevRef.current;
    const workspaceChanged =
      prev.orgSlug !== activeOrgSlug ||
      prev.team !== team ||
      prev.sourceReposKey !== sourceReposKey;
    const attachmentsChanged =
      prev.githubUserId !== githubUserId || prev.attachmentsVersion !== attachmentsVersion;

    if (!workspaceChanged && !attachmentsChanged) {
      return; // No-op: nothing meaningful changed.
    }

    // SW-TS-5 §4.4: launch reconcile across worktree surfaces. Fired here —
    // ABOVE the hard-switch/soft-refresh branch split — so it runs on whichever
    // path the first run for a given user takes. (If login resolves AFTER the
    // workspace is first established, that later run routes through the
    // soft-refresh branch, which returns before the hard-switch body; a
    // hard-switch-only call would then never fire.) Keyed on the github user
    // id: fires once per user per session and re-fires on an account switch or
    // re-login, but not on org/team navigation. Team-agnostic, un-awaited; the
    // backend offloads the loop to a blocking task so it never delays the
    // viewer landing (AC-7/AC-12). The uid is recorded only once a
    // codesenseHome is known, so a null home doesn't burn it.
    if (githubUserId === null) {
      // Logged out — clear the gate so the next sign-in (any user) reconciles.
      worktreeReconciledUidRef.current = null;
    } else if (worktreeReconciledUidRef.current !== githubUserId) {
      const reconcileHome = getCodesenseHome();
      if (reconcileHome !== null) {
        worktreeReconciledUidRef.current = githubUserId;
        void reconcileWorktreesOnLaunch(reconcileHome, githubUserId).catch((e) => {
          logger.warn(
            "workspace-sync",
            `SW-TS-5 launch reconcile failed (non-fatal): ${e instanceof Error ? e.message : String(e)}`,
          );
        });
      }
    }

    // P2 (code-review patch): bump the run id BEFORE either async IIFE
    // kicks off. Both paths capture this `myRunId` and bail on any await
    // resumption where `runIdRef.current` has moved on (a newer effect run
    // started). This prevents stale writes from a previous invocation
    // overwriting a more recent invocation's state.
    const myRunId = ++runIdRef.current;

    // Iter-7 (Blind HIGH-2 follow-up): snapshot the Startup gate's
    // tri-state ref BEFORE the Startup IIFE runs and synchronously
    // promotes it. The source_repos-only branch later in this IIFE
    // reads this snapshot (not the post-IIFE ref) to decide whether
    // to fire its own setActiveTeamForWave call — avoids double-fire
    // on the first effect run where both Startup and source_repos-only
    // would otherwise match.
    const startupStateRunStartValue = startupStateRef.current;

    // SW-LN-B68: a selected team with no repo yet must still be the backend's active
    // team — Activity and every other active-team read fail until it is. The Startup
    // and team-switch commits wait for a repo (they converge repo content), so a
    // repo-less team is committed here instead. Not awaited: the mount stays
    // synchronous. The target is claimed up front so the same-team refresh doesn't
    // commit it twice, released on failure, and retried a few times while this run is
    // still current — a failed IPC changes none of the effect's deps, so nothing else
    // would retry it.
    const commitTeamWithoutRepos = (codesenseHome: string, attempt = 0): void => {
      if (
        activeTeamName === null ||
        trackedRepos.length !== 0 ||
        githubUserId === null ||
        waveTargetKeyRef.current === waveTargetKey
      ) {
        return;
      }
      waveTargetKeyRef.current = waveTargetKey;
      commitActiveTeamForWave({
        codesenseHome,
        githubUserId,
        orgSlug: org.slug,
        team,
        sourceRepos: [],
      }).catch((e: unknown) => {
        if (waveTargetKeyRef.current === waveTargetKey) waveTargetKeyRef.current = null;
        logger.warn(
          "workspace-sync",
          `setActiveTeamForWave (team with no repos) failed (attempt ${attempt + 1}): ${e instanceof Error ? e.message : String(e)}`,
        );
        if (attempt >= NO_REPO_COMMIT_RETRY_MS.length) return;
        setTimeout(() => {
          if (runIdRef.current === myRunId) commitTeamWithoutRepos(codesenseHome, attempt + 1);
        }, NO_REPO_COMMIT_RETRY_MS[attempt]);
      });
    };

    // R2-P6 patch: update workspaceKeyRef SYNCHRONOUSLY for hard switches
    // (before any await), so a soft-refresh that interleaves with an
    // aborted hard-switch sees the new key. Previously, when a hard-switch
    // bailed before reaching `workspaceKeyRef.current = nextKey`, a
    // subsequent soft-refresh would update `prevRef` but leave the ref
    // pointing at the OLD workspace — causing the next hard-switch to
    // misidentify the outgoing workspace.
    if (workspaceChanged) {
      const nextKey = buildTeamKey(activeOrgSlug, activeTeamName);
      workspaceKeyRef.current = nextKey;
    }

    // F-6 soft refresh: only attachments changed (same workspace + repo set).
    // Re-build synthetic repos in place so `localPath` updates without
    // losing selection/watchers/file-refresh state.
    //
    // R4-1 (revert of R3-P1's harmful change): `prevRef.current` is advanced
    // at the END of the IIFE, not before. The end-of-IIFE pattern is
    // *self-healing*: if the IIFE bails (stale runId from a flicker), prev
    // stays at the prior state and the next effect run recomputes
    // workspaceChanged/attachmentsChanged correctly and retries. Advancing
    // prev synchronously before the IIFE (R3-P1's original approach) breaks
    // this — a bail would leave prev claiming "done" while workspaceRepos
    // was never written, causing a permanent no-op short-circuit. The
    // *correct* R3-P1 fix is only the `runIdRef` bumps in cleanup branches
    // (above) — those prevent prior in-flight asyncs from writing
    // post-teardown, without disturbing the self-healing prev pattern.
    if (!workspaceChanged && attachmentsChanged && hardSwitchInFlightRef.current === null) {
      // SW-LN-B68: sign-in can resolve after the workspace mounted, which lands here
      // rather than in the hard switch — a repo-less team is committed then.
      const softCommitHome = getCodesenseHome();
      if (softCommitHome !== null) commitTeamWithoutRepos(softCommitHome);
      void (async () => {
        const attachmentsByRemote = await loadAttachmentsMap(githubUserId);
        if (runIdRef.current !== myRunId) return; // stale invocation
        // R5B-P5: if the IPC failed (null), do NOT rebuild — that would
        // wipe `localPath` for every synthetic, including one the user
        // may have just successfully attached. Advance prevRef so we
        // don't loop on every render, and wait for the next
        // attachmentsVersion bump (or hard switch) to retry.
        if (attachmentsByRemote === null) {
          logger.warn(
            "workspace-sync",
            "soft-refresh skipped — listAttachmentsByRemote failed; preserving existing localPath values",
          );
          prevRef.current = {
            orgSlug: activeOrgSlug,
            team,
            activeTeamName,
            sourceReposKey,
            githubUserId,
            attachmentsVersion,
          };
          return;
        }
        if (runIdRef.current !== myRunId) return;
        // SF-8: `buildSyntheticRepo` is now synchronous (no Tauri IPC). A
        // straight map → setWorkspaceRepo replaces the prior Promise.allSettled
        // pattern; per-repo failure isolation is no longer needed because
        // pathToTeamContentDirSync cannot fail.
        for (const slug of trackedRepos) {
          const key = syntheticRepoKey(org.slug, team, slug);
          const overrides = workspaceRepoOverrides(key);
          useOrgStore
            .getState()
            .setWorkspaceRepo(buildSyntheticRepo(org, team, slug, attachmentsByRemote, overrides));
        }
        // T.2 D-12: after the soft-refresh rebuilds workspaceRepos with new
        // `localPath` values, the attachment-change flow is complete. (The
        // former root-workspace re-sync was retired with root workspaces —
        // SW-WV-A6; the team-folder workspace is ensured on last-close instead.)
        prevRef.current = {
          orgSlug: activeOrgSlug,
          team,
          activeTeamName,
          sourceReposKey,
          githubUserId,
          attachmentsVersion,
        };
        logger.info(
          "workspace-sync",
          `Soft-refreshed ${trackedRepos.length} synthetic repos (attachments changed)`,
        );
      })();
      return;
    }

    // Hard switch: workspace identity or repo set changed, or an attachments
    // change landed while a hard switch was still in flight (it re-runs the
    // switch and takes over the mount, so the superseded run's bail is skipped).
    hardSwitchInFlightRef.current = myRunId;
    void (async () => {
      const nextKey = buildTeamKey(activeOrgSlug, activeTeamName);
      // R-2: Shared across resume and hard-switch paths.
      // The home is resolved before first render; null only if the OS home could
      // not be read. Bail before any teardown/state mutation rather than build a
      // "null/cache" path.
      const codesenseHome = getCodesenseHome();
      if (codesenseHome === null) {
        logger.warn(
          "workspace-sync",
          "hard-switch: the CodeSense home is unavailable; aborting sync",
        );
        return;
      }
      const cachePath = `${codesenseHome}/cache/frontmatter-cache.json`;

      // SW-LN-B68: commit a repo-less team before the mount (see the helper).
      commitTeamWithoutRepos(codesenseHome);

      // === F-19: Resume path — check if incoming workspace was suspended ===
      // SF-8 review iter-4 (EC-iter4-1): before taking the resume branch,
      // verify the suspended snapshot's slug set matches current teams.yml.
      // A suspended snapshot is frozen at the moment of suspend; if
      // teams.yml mutated source_repos for this team while it was
      // suspended (or in the brief window between BH-1's surgical-remove
      // and a post-Phase-2b bail-suspend), restoring the snapshot
      // verbatim would resurrect dropped slugs and miss added ones — the
      // resume code path doesn't reconcile against current trackedRepos.
      // Evict the stale snapshot and fall through to the standard path
      // for a clean re-mint; PTYs survive across the eviction because
      // viewer-store team-keyed slices aren't touched by evictSuspendedWorkspace.
      const suspendedSnapshot = useOrgStore.getState().suspendedWorkspaceRepos[nextKey];
      if (suspendedSnapshot) {
        const snapshotSlugs = new Set<string>();
        for (const r of Object.values(suspendedSnapshot)) {
          const parsed = parseSyntheticRepoKey(r.repoPath);
          if (parsed) {
            snapshotSlugs.add(sourceRepoRemoteKey(parsed));
          }
        }
        const trackedSlugs = new Set(trackedRepos.map((s) => sourceRepoRemoteKey(s)));
        const sameSize = snapshotSlugs.size === trackedSlugs.size;
        const allMatch = sameSize && [...snapshotSlugs].every((s) => trackedSlugs.has(s));
        if (!allMatch) {
          logger.info(
            "workspace-sync",
            `discarding stale suspended snapshot for ${nextKey} (slug set diverges from teams.yml); falling through to standard path`,
          );
          useOrgStore.getState().evictSuspendedWorkspace(nextKey);
        }
      }
      const incomingSuspended = useOrgStore.getState().suspendedWorkspaceRepos[nextKey];
      if (incomingSuspended) {
        // Outgoing workspace: suspend (not close).
        // Guard: only suspend if there's an actual outgoing workspace
        // (prev.orgSlug !== null). When null (cold-start edge), do a
        // non-suspend teardown to avoid creating a bogus "null:_none" key.
        if (prev.orgSlug !== null) {
          // SF-8 review iter-1 (BH#2): use surgical `suspendTeamSynthetics`
          // here too — wholesale `teardownWorkspaceRepos({suspend:true})`
          // would snapshot EVERY entry of `workspaceRepos` under the
          // outgoing key. In A→B→A rapid-switch sequences where B's
          // still-in-flight synthetics are co-resident with A's, the
          // wholesale snapshot mis-attributes B into A's suspend bucket.
          // The hard-switch path was already migrated; the resume path
          // missed the same fix.
          const outgoingKey = buildTeamKey(prev.orgSlug, prev.activeTeamName);
          suspendTeamSynthetics(outgoingKey, prev.orgSlug, prev.team);
        } else {
          // T.1 AC-11: cold-start edge — no outgoing team to clean up.
          teardownWorkspaceRepos({ outgoingTeamKey: null });
        }

        // Incoming workspace: resume from suspended state.
        useOrgStore.getState().resumeWorkspaceRepos(nextKey);
        for (const repo of workspaceReposForOrgTeam(
          useOrgStore.getState().workspaceRepos,
          org.slug,
          team,
        )) {
          const mainDoc = workspaceRepoOverrides(repo.repoPath).mainDoc ?? "";
          if (repo.mainDoc !== mainDoc)
            useOrgStore.getState().updateWorkspaceRepo(repo.repoPath, { mainDoc });
        }
        // SW-WV-A6: the former safety-net root re-sync (retired with root
        // workspaces) ran here on resume-from-suspend; the resume flow itself
        // (`resumeWorkspaceRepos` above) is unchanged.
        // The team is already open: the warm-scope owner opened it in the same tick
        // the store made it active. Bails below must call `bailHardSwitchEarlyMount`
        // to roll back the synthetics minted for it.
        // SF-8 review iter-1 (EC-1): claim ownership of this team's
        // mount for this run so a later run's resume / hard-switch can
        // race-flag stale-bail attempts against this run.
        mountRunIdRef.current.set(nextKey, myRunId);

        // SF-8: advance `prevRef.current` synchronously now that the resume
        // is committed. See the hard-switch counterpart for rationale.
        prevRef.current = {
          orgSlug: activeOrgSlug,
          team,
          activeTeamName,
          sourceReposKey,
          githubUserId,
          attachmentsVersion,
        };

        // SF-8 review iter-1 (EC-4): scope to the incoming team's
        // synthetics only. Pre-fix the resume path iterated all of
        // `workspaceRepos` — during concurrent runs that left
        // co-resident synthetics from other teams, the resume path
        // would fire `capturePendingFiles` + `bumpFileRefresh` against
        // foreign-team repos (mis-attribution + needless tree rebuilds).
        const resumedRepos = workspaceReposForOrgTeam(
          useOrgStore.getState().workspaceRepos,
          org.slug,
          team,
        );

        // R-3: Atomic clear + watcher-start + cache build + merge.
        // Replaces the previous buildFrontmatterCache + watchRepos two-step
        // and handles the workspace-transition reset so prior workspaces'
        // entries don't leak. CR-4 (rebuild on resume): may be stale if
        // files changed during suspension — init_watched_cache picks them
        // up via load_or_build + mtime_diff.
        try {
          await initWatchedCache(org.org_root, cachePath);
          if (runIdRef.current !== myRunId) {
            bailHardSwitchEarlyMount(
              nextKey,
              org.slug,
              team,
              "stale after initWatchedCache (resume)",
              myRunId,
              mountRunIdRef.current,
            );
            return;
          }
        } catch (e) {
          if (runIdRef.current !== myRunId) {
            bailHardSwitchEarlyMount(
              nextKey,
              org.slug,
              team,
              "stale after initWatchedCache (resume, error)",
              myRunId,
              mountRunIdRef.current,
            );
            return;
          }
          logger.warn("workspace-sync", `initWatchedCache (resume) failed: ${e}`);
        }

        try {
          // SW-CP-18: per-repo capture on resume is retired. Files written
          // while the workspace was suspended are adopted by claim/import on
          // the next materialize and round-tripped by the reverse watch; the
          // `initWatchedCache` rebuild above + the refresh bump below surface
          // them in the viewer.

          // PTS-5 bug 2: workspace-resume staleness. During suspension the
          // watcher was torn down, so FSEvents-driven bumps never fired for
          // this team's content. `initWatchedCache` above rebuilt the Rust
          // cache via `mtime_diff`, which sees any post-suspend writes; bump
          // each resumed repo's fileRefreshKey so `usePipelineTree` re-derives
          // against the now-current cache. `changedFilePath = undefined`
          // signals "multi-file or unknown" → full rescan (F-26 semantics).
          for (const resumedRepo of resumedRepos) {
            useFileRefreshStore.getState().bumpFileRefresh(resumedRepo.repoPath, undefined);
          }
        } catch (e) {
          logger.warn("workspace-sync", `resume file-refresh failed: ${e}`);
          if (runIdRef.current !== myRunId) {
            bailHardSwitchEarlyMount(
              nextKey,
              org.slug,
              team,
              "stale at resume file-refresh catch",
              myRunId,
              mountRunIdRef.current,
            );
            return;
          }
          // Advance prevRef even on failure so the next effect run doesn't
          // re-enter the resume path with stale prev — that would miskey
          // the outgoing suspend. The workspace is already resumed
          // (workspaceRepos restored), only the file-refresh bump failed.
        }

        prevRef.current = {
          orgSlug: activeOrgSlug,
          team,
          activeTeamName,
          sourceReposKey,
          githubUserId,
          attachmentsVersion,
        };
        // T.1 D-4 step 6: one-shot post-migration sentinel sweep.
        if (!sentinelSweptRef.current) {
          sentinelSweptRef.current = true;
          sweepSentinelBucket();
        }
        // SW-LN-B68: the resume path skips the team-switch commit, so the backend would
        // keep the team just left (e.g. a repo-less one) and Activity would show its
        // board. Commit the resumed team unless the backend already holds it.
        if (githubUserId !== null && waveTargetKeyRef.current !== waveTargetKey) {
          waveTargetKeyRef.current = waveTargetKey;
          commitActiveTeamForWave({
            codesenseHome,
            githubUserId,
            orgSlug: org.slug,
            team,
            sourceRepos: trackedRepos,
          }).catch((e: unknown) => {
            if (waveTargetKeyRef.current === waveTargetKey) waveTargetKeyRef.current = null;
            logger.warn(
              "workspace-sync",
              `setActiveTeamForWave (resume) failed: ${e instanceof Error ? e.message : String(e)}`,
            );
          });
        }
        logger.info("workspace-sync", `Resumed suspended workspace ${nextKey}`);
        return;
      }

      // === Standard path — no suspended entry for incoming workspace ===

      // === PHASE 1: Pre-switch — suspend outgoing workspace (F-19) ===
      // SF-8: moved BEFORE the loadAttachmentsMap await so the synchronous
      // early-mount below has a clean workspaceRepos slate. The teardown
      // itself is synchronous and doesn't depend on attachments.
      //
      // Surgical: only touches the OUTGOING team's synthetic entries via
      // `suspendTeamSynthetics`, not other teams that may have been mounted
      // concurrently by a still-in-flight earlier run. When `prev.orgSlug`
      // is null (cold-start OR rapid switch where prev hasn't advanced
      // yet), there's no outgoing team to suspend — SKIP entirely; the
      // earlier run's bail cleans up its own synthetics when it discovers
      // it's stale.
      //
      // SF-8 review iter-3 (BH-1): same-team source_repos mutation
      // (`outgoingKey === nextKey`) doesn't suspend (the team isn't
      // leaving), but it MUST surgically remove synthetics for slugs
      // dropped from the new `trackedRepos` list. Phase 2a's
      // `setWorkspaceRepo` loop only writes entries for the NEW slugs
      // (keyed by repoPath, so unchanged slugs get overwritten in
      // place), but `setWorkspaceRepo` never DELETES — pre-iter-3 the
      // dropped-slug synthetics leaked into `initWatchedCache`, the
      // catch-up bump loop, and eventually into `suspendedWorkspaceRepos`
      // on the next team-leave, only to resurface on resume.
      if (prev.orgSlug !== null) {
        const outgoingKey = buildTeamKey(prev.orgSlug, prev.activeTeamName);
        if (outgoingKey !== nextKey) {
          suspendTeamSynthetics(outgoingKey, prev.orgSlug, prev.team);
        } else if (prev.sourceReposKey !== sourceReposKey) {
          // Same-team source_repos mutation: drop synthetics whose slug
          // is no longer in `trackedRepos`. `setWorkspaceRepo`'s
          // overwrite semantics handle the keep-and-add cases; we only
          // need to handle the drop case here.
          const newSlugSet = new Set(trackedRepos.map((s) => sourceRepoRemoteKey(s)));
          const liveRepos = workspaceReposForOrgTeam(
            useOrgStore.getState().workspaceRepos,
            org.slug,
            team,
          );
          for (const r of liveRepos) {
            const parsed = parseSyntheticRepoKey(r.repoPath);
            if (!parsed) continue;
            const liveSlug = sourceRepoRemoteKey(parsed);
            if (!newSlugSet.has(liveSlug)) {
              removeWatch(toWatchConfig(r)).catch(() => {});
              useOrgStore.getState().removeWorkspaceRepo(r.repoPath);
            }
          }
        }
      }

      // === PHASE 2a (SF-8): Synchronous early-mount with placeholder defaults ===
      // `buildSyntheticRepo` is sync (T1); attachments + workspace-config
      // overrides resolve asynchronously below and refine the synthetics
      // in place. Minting with empty maps now gives the App.tsx slot a
      // mounted `<TeamWorkspaceView>` in the same animation frame as the
      // user's click — `<TeamWorkspaceSkeleton>` (T3) fills the content
      // area until `usePipelineTree` resolves against the rebuilt cache.
      for (const slug of trackedRepos) {
        useOrgStore
          .getState()
          .setWorkspaceRepo(
            buildSyntheticRepo(org, team, slug, new Map<string, Attachment>(), undefined),
          );
      }
      // The incoming team is already open (the warm-scope owner opened it with the
      // store write), so its `<TeamWorkspaceView>` mounts in the click's own frame.
      // Every subsequent runIdRef bail in this IIFE must call
      // `bailHardSwitchEarlyMount(nextKey, …)` to roll back its synthetics.
      // SF-8 review iter-1 (EC-1): claim ownership of this team's mount
      // for this run. Stale Run N bailing later will compare against
      // this map and skip its suspend if Run M has re-claimed the same
      // team in between.
      mountRunIdRef.current.set(nextKey, myRunId);

      // SF-8: advance `prevRef.current` SYNCHRONOUSLY immediately after the
      // mount commits. Local `prev` is a value-capture from the top of the
      // effect (a reassignment here doesn't change what `prev` reads —
      // `prev` still points to the prior object). Advancing the ref now is
      // load-bearing for concurrent runs: a Run N+1 that fires before this
      // IIFE's awaits resolve sees the up-to-date state via its own
      // `prev = prevRef.current` read, so Phase 1's outgoing teardown +
      // the team-switch gate work correctly. The end-of-IIFE advance
      // (the R4-1 self-healing point) becomes redundant and is retired —
      // a bail mid-await suspends the team via `bailHardSwitchEarlyMount`,
      // which is recoverable by the resume path on a subsequent re-click.
      prevRef.current = {
        orgSlug: activeOrgSlug,
        team,
        activeTeamName,
        sourceReposKey,
        githubUserId,
        attachmentsVersion,
      };

      // === PHASE 2b: Async refinement — load attachments + workspace overrides ===
      const attachmentsRaw = await loadAttachmentsMap(githubUserId);
      if (runIdRef.current !== myRunId) {
        bailHardSwitchEarlyMount(
          nextKey,
          org.slug,
          team,
          "stale after loadAttachmentsMap",
          myRunId,
          mountRunIdRef.current,
          { prePhase2b: true },
        );
        return;
      }
      // R5B-P5: hard switch treats IPC failure as empty map (graceful
      // degradation per spec Approach). Unlike the soft-refresh case, a
      // hard switch means the user just changed workspaces — they expect
      // to see the new repos, even if attachments couldn't load (every
      // synthetic surfaces AC-8 modal on first action).
      const attachmentsByRemote = attachmentsRaw ?? new Map<string, Attachment>();

      // SF-8 Phase 2b refinement: re-set each synthetic with the now-resolved
      // attachments + overrides. `setWorkspaceRepo` is keyed by `repoPath`,
      // so this overwrites the placeholder entries minted in Phase 2a. The
      // workspace bar pills remain visible across the refresh because their
      // identity (slug) is unchanged.
      for (const slug of trackedRepos) {
        const key = syntheticRepoKey(org.slug, team, slug);
        const overrides = workspaceRepoOverrides(key);
        useOrgStore
          .getState()
          .setWorkspaceRepo(buildSyntheticRepo(org, team, slug, attachmentsByRemote, overrides));
      }

      // SW-WV-A6: root-workspace re-sync (retired with root workspaces) ran
      // here after Phase 2b's `setWorkspaceRepo` loop. Orphan detection (below)
      // reads `viewerStore.workspaces[teamKey]` for story workspaces only, so
      // its ordering is unaffected by the removal.

      // T.2 D-12: detect orphan story workspaces from removed source_repos.
      // Fires only when this is a team-internal mutation (same org + team,
      // different source_repos), not an org/team switch. One toast per
      // REMOVED SLUG (so removing N slugs in one teams.yml edit emits N
      // toasts). Multiple orphan stories under the same removed slug still
      // emit a single toast.
      //
      // Review iter-2, finding C: orphan detection must run BEFORE any future
      // cascade-close-stories patch — closing the orphans before we look would
      // silently break detection.
      // Review iter-2, finding D: `prevRef.current.sourceReposKey` is
      // advanced immediately after the toast block so a downstream IIFE
      // bail (PTS-2 migration, team switch) doesn't cause the
      // toast to re-fire on the next effect run.
      if (
        prev.orgSlug === activeOrgSlug &&
        prev.team === team &&
        prev.sourceReposKey !== sourceReposKey
      ) {
        const prevSlugs = new Set(prev.sourceReposKey ? prev.sourceReposKey.split(",") : []);
        const nextSlugs = new Set(sourceReposKey ? sourceReposKey.split(",") : []);
        const removedSlugs: string[] = [];
        for (const s of prevSlugs) if (!nextSlugs.has(s)) removedSlugs.push(s);
        if (removedSlugs.length > 0) {
          const teamKey = buildTeamKey(activeOrgSlug, activeTeamName);
          const teamWorkspaces = useViewerStore.getState().workspaces[teamKey] ?? [];
          for (const slug of removedSlugs) {
            const hasOrphans = teamWorkspaces.some(
              (w) => w.kind === "story" && w.sourceRepoSlug === slug,
            );
            if (hasOrphans) {
              toast.warning(
                `Removed source repo ${slug}; its open story workspaces will remain until you close them.`,
              );
            }
          }
        }
        // SF-8 review iter-3 (BH-3): the iter-2 BH-4 self-spread of
        // `prevRef.current` is dead. Phase 2a (line ~864) already wrote
        // `prevRef.current.sourceReposKey = sourceReposKey` synchronously
        // along with every other field. There's nothing to advance
        // here — the orphan toast above is the only side-effect this
        // block needs to run.
      }

      // === PHASE 3: R-3 atomic cache clear + build + merge ===
      // SW-WL-3: watcher attach is no longer done here — it is surface-bound
      // (WorkspaceOpenSet drives lazy_attach_workspace per open workspace).
      // This step only resets + rebuilds the frontmatter cache, preserving any
      // entries a surface-bound watcher updates during the build via merge_from.
      try {
        const count = await initWatchedCache(org.org_root, cachePath);
        if (runIdRef.current !== myRunId) {
          bailHardSwitchEarlyMount(
            nextKey,
            org.slug,
            team,
            "stale after initWatchedCache",
            myRunId,
            mountRunIdRef.current,
          );
          return;
        }
        logger.info("workspace-sync", `Frontmatter cache: ${count} entries indexed`);
      } catch (e) {
        if (runIdRef.current !== myRunId) {
          bailHardSwitchEarlyMount(
            nextKey,
            org.slug,
            team,
            "stale after initWatchedCache (error path)",
            myRunId,
            mountRunIdRef.current,
          );
          return;
        }
        logger.warn("workspace-sync", `initWatchedCache failed: ${e}`);
      }

      // === SS-4c AC-16: first-mount-with-team activation ===
      // Fires once per session: when the frontend restores a persisted
      // active team and this is the first effect-run where that team
      // appears, register its content-layer watches and commit it as the
      // backend's active team. SW-XS-3: no Startup wave — launch never
      // writes content into a workspace.
      //
      // Iter-5 (Blind H3 / Edge HIGH-2 / Edge HIGH-3): the gate used to
      // include `prev.activeTeamName == null`, but `prevRef.current` is
      // written at the END of the IIFE — so once Run-1 succeeds (or any
      // run completes), `prev.activeTeamName` is non-null forever and
      // the gate is unreachable on retry. Retry after a transient
      // setActiveTeamForWave failure was effectively
      // dead. Same problem for logout/re-login: prevRef gets reset to
      // `null` by the activeOrgSlug=null branch, but `startupStateRef`
      // stayed `'done'` — second login never fired Startup.
      //
      // Fix: drop `prev.activeTeamName == null` from the gate. The
      // tri-state ref (`idle | in_flight | done`) is the sole "fire
      // once per session" enforcement. The logout/no-org branches reset
      // the ref to `'idle'` so re-login can fire a fresh Startup.
      // First-time-team-mount semantics are preserved: until a team
      // resolves, `team` is null and the gate is closed; once a team
      // arrives + the ref is idle, Startup fires.
      //
      // Iter-6 (Edge MED-1): gate also requires `trackedRepos.length > 0`.
      // Without it, a session that opens with `activeTeamName == null`
      // fires Startup with `team = "default"` and empty trackedRepos
      // — Rust no-ops, gate transitions to `done`, then the user's
      // actual team selection finds `done` and no team-switch fires
      // (TeamSwitch gate also fails because prev.activeTeamName is
      // null from the cold start). The next dep change converges; the
      // user perceives a no-op selection. Holding the gate closed
      // until trackedRepos populates keeps Startup matched to a real
      // team with real source-repos.
      if (
        team != null &&
        trackedRepos.length > 0 &&
        startupStateRef.current === "idle" &&
        githubUserId !== null
      ) {
        // Iter-3 (Blind H1 / Edge HIGH-3): tri-state ref closes the
        // double-fire window. Set to `'in_flight'` SYNCHRONOUSLY before
        // any await so a concurrent effect run (StrictMode double-mount
        // or dep churn) sees the slot is claimed. On success → `'done'`.
        // On failure → reset to `'idle'` for retry.
        startupStateRef.current = "in_flight";
        try {
          await commitActiveTeamForWave({
            codesenseHome,
            githubUserId,
            orgSlug: org.slug,
            team,
            sourceRepos: trackedRepos,
          });
          if (runIdRef.current === myRunId) waveTargetKeyRef.current = waveTargetKey;
          // A fresh effect run that bumped the runId owns the activation now;
          // reset to 'idle' so it can commit its own.
          if (runIdRef.current !== myRunId) {
            startupStateRef.current = "idle";
            bailHardSwitchEarlyMount(
              nextKey,
              org.slug,
              team,
              "stale after first-mount setActiveTeamForWave",
              myRunId,
              mountRunIdRef.current,
            );
            return;
          }
          startupStateRef.current = "done";
        } catch (e) {
          startupStateRef.current = "idle";
          logger.warn(
            "workspace-sync",
            `SS-4c first-mount team activation failed (non-fatal; resets to idle for next dep-driven effect run): ${e instanceof Error ? e.message : String(e)}`,
          );
        }
      }

      // === Iter-6 (Auditor MED): refresh handlers on source_repos-only change ===
      // The iter-5 fix made `register_content_layer_watches` idempotent
      // w.r.t. handler refresh (always unregister + re-register), but the
      // TS hook only invoked `setActiveTeamForWave` from the first-mount
      // Startup branch and the team-switch IIFE. A source_repos-only
      // mutation in the same team (add/remove a tracked repo via
      // attachments) bumps `sourceReposKey` and re-enters the effect,
      // but neither downstream gate fires → the existing
      // TeamWorkspace handler keeps the OLD `SlugScope::AllSlugs(...)`
      // capture and fan-out misses the new repos.
      //
      // Fix: when sourceReposKey changed but team didn't, re-register
      // handlers so the new slug list propagates. We DON'T fire a
      // team switch in this branch — the source-repo addition is
      // not a team change; converging the new repo happens on its
      // first source-side event (or a subsequent Startup/team-switch).
      //
      // Iter-7 (Blind HIGH-2 follow-up): skip this branch if the Startup
      // IIFE above already fired `setActiveTeamForWave` in THIS effect
      // run. Checking the post-IIFE ref value isn't enough because the
      // Startup branch promotes ref to `"done"` synchronously before
      // this branch evaluates — so a plain `=== "done"` check would
      // still match and we'd double-fire. Capture the ref's value at
      // the start of the IIFE (before the Startup gate flips it) and
      // skip when Startup just fired in this run.
      //
      // Iter-7 (Edge HIGH-2): track refresh failures so prevRef's
      // `sourceReposKey` doesn't advance on failure. Without that, a
      // transient setActiveTeamForWave error advanced the cursor and
      // permanently stranded handlers until the user mutated
      // source_repos AGAIN. With the failure-tracking flag, the next
      // effect run sees sourceReposKey != prev.sourceReposKey and
      // retries.
      let sourceReposRefreshFailed = false;
      const startupAlreadyDoneAtRunStart = startupStateRunStartValue === "done";
      if (
        prev.activeTeamName === team &&
        waveTargetKeyRef.current !== waveTargetKey &&
        startupAlreadyDoneAtRunStart &&
        githubUserId !== null
      ) {
        try {
          await commitActiveTeamForWave({
            codesenseHome,
            githubUserId,
            orgSlug: org.slug,
            team,
            sourceRepos: trackedRepos,
          });
          if (runIdRef.current === myRunId) waveTargetKeyRef.current = waveTargetKey;
        } catch (e) {
          sourceReposRefreshFailed = true;
          logger.warn(
            "workspace-sync",
            `setActiveTeamForWave (sourceRepos refresh) failed; sourceReposKey not advanced so the next dep change retries: ${e instanceof Error ? e.message : String(e)}`,
          );
        }
      }

      // === Team switch: commit the new active team ===
      // SW-XS-3: a team switch only re-registers the team's watches and commits the
      // active team the MCP server reads. It never writes content into a workspace
      // (the TeamSwitch materialization wave is gone).
      if (prev.activeTeamName != null && prev.activeTeamName !== team) {
        const hasAttachedRepo = trackedRepos.some((s) =>
          attachmentsByRemote.has(sourceRepoRemoteKey(s)),
        );
        // SW-LN-B68: a team whose repos aren't attached here still becomes the
        // backend's active team (its Activity is read from it). A repo-less team was
        // already committed by `commitTeamWithoutRepos` (its target matches).
        if (
          githubUserId !== null &&
          (hasAttachedRepo ||
            (activeTeamName !== null && waveTargetKeyRef.current !== waveTargetKey))
        ) {
          try {
            await commitActiveTeamForWave({
              codesenseHome,
              githubUserId,
              orgSlug: org.slug,
              team,
              sourceRepos: trackedRepos,
            });
            if (runIdRef.current === myRunId) waveTargetKeyRef.current = waveTargetKey;
          } catch (e) {
            logger.warn(
              "workspace-sync",
              `setActiveTeamForWave failed (continuing): ${e instanceof Error ? e.message : String(e)}`,
            );
          }
        }
      }

      // SF-8: `openTeam(nextKey)` already fired synchronously in Phase 2a
      // above. The previous post-await call here is retired — it was
      // redundant (openTeam is idempotent) and obscured the new flow.
      // Zero tracked repos is handled by T.7's empty state when
      // `openedTeams.length === 0` (App.tsx:790-797).

      // SW-CP-18: the capture pass (inline + wave-gated) is retired. Keep the
      // stale-flight guard for the IIFE tail (catch-up bumps below) — bail if
      // the workspace changed underneath this run.
      if (workspaceKeyRef.current !== nextKey) {
        bailHardSwitchEarlyMount(
          nextKey,
          org.slug,
          team,
          "stale workspaceKey at post-switch tail",
          myRunId,
          mountRunIdRef.current,
        );
        return;
      }

      // SF-8 review iter-2 (EC-iter2-A): bail-helper guard between the
      // capture-pass try/catch and the catch-up bump loop. Every other
      // post-mount mutation site in this IIFE bails on stale runId via
      // `bailHardSwitchEarlyMount`; the successful-capture exit path
      // fell through to the catch-up loop without the same check, so a
      // Run N that completed capture while Run N+1 had already re-mounted
      // the same team would bump Run N+1's freshly-minted synthetics
      // (bounded by SF-6's `refreshDebounce` — no user-visible bug, but
      // an attribution gap relative to every other site).
      if (runIdRef.current !== myRunId) {
        bailHardSwitchEarlyMount(
          nextKey,
          org.slug,
          team,
          "stale before catch-up bump",
          myRunId,
          mountRunIdRef.current,
        );
        return;
      }

      // === SF-8 (AC-5): atomic catch-up — single deferred rescan per repo ===
      // After the awaits resolve (initWatchedCache rebuilt the cache,
      // the team switch committed), bump fileRefresh per
      // incoming repo so `usePipelineTree` re-derives against the
      // now-current cache. SF-6's shared `refreshDebounce` (App.tsx:
      // 600-685) coalesces these with any concurrent watcher-driven
      // bumps in one 300ms window, and `usePipelineTree`'s 150ms +
      // lastFingerprintRef collapses multiple repo bumps into a single
      // tree-commit. C.1 / F.34 / P.14 scroll / selection / viewport
      // preservation patterns flow through their existing viewKey-keyed
      // ref guards — undefined `changedFilePath` triggers a full
      // rescan (F-26 semantics, mirrors the resume-path pattern at
      // :667-669).
      //
      // SF-8 review iter-1 (BH#1): scope to the incoming team's
      // synthetics only. The pre-fix `Object.values(workspaceRepos)`
      // looped over EVERY repo, including any concurrent run's
      // foreign-team synthetics that briefly co-existed in the store
      // (A→B→A scenarios). That mis-attributed catch-up bumps to the
      // wrong team's repos.
      const incomingReposForRefresh = workspaceReposForOrgTeam(
        useOrgStore.getState().workspaceRepos,
        org.slug,
        team,
      );
      for (const incomingRepo of incomingReposForRefresh) {
        useFileRefreshStore.getState().bumpFileRefresh(incomingRepo.repoPath, undefined);
      }

      // R4-1: advance prevRef at the END of the IIFE so a bail (stale runId
      // from a flicker) leaves prev stale and triggers self-healing retry on
      // the next effect run. See the soft-refresh block's comment for the
      // full rationale.
      //
      // Iter-7 (Edge HIGH-2): if the source_repos-only refresh branch
      // failed, KEEP `prev.sourceReposKey` at its previous value so the
      // next effect run's workspaceChanged-check fires and the branch
      // retries the registration call. Every other field still advances
      // (auto-select, restore, etc. proceeded normally).
      prevRef.current = {
        orgSlug: activeOrgSlug,
        team,
        activeTeamName,
        sourceReposKey: sourceReposRefreshFailed ? prev.sourceReposKey : sourceReposKey,
        githubUserId,
        attachmentsVersion,
      };
      // A repo-less team counts as mounted only from here (see
      // `reopenMountedActiveTeam`): re-check in case a load dropped it mid-switch.
      reopenMountedActiveTeam();
      // T.1 D-4 step 6: one-shot post-migration sentinel sweep.
      if (!sentinelSweptRef.current) {
        sentinelSweptRef.current = true;
        sweepSentinelBucket();
      }
      logger.info(
        "workspace-sync",
        `Switched to workspace ${activeOrgSlug}/${team} (${trackedRepos.length} repos)`,
      );
    })().finally(() => {
      if (hardSwitchInFlightRef.current === myRunId) hardSwitchInFlightRef.current = null;
    });
  }, [
    activeOrgSlug,
    activeTeamName,
    orgs,
    githubUserId,
    attachmentsVersion,
    reopenMountedActiveTeam,
  ]);
}
