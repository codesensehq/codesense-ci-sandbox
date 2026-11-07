// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { SYNC_WAVE_COMPLETED_EVENT, type SyncWaveCompletedPayload } from "@/tauri/events";
import { bumpFileRefreshForSourceRepo } from "@/shared/lib/refresh-for-source-repo";
import { bumpTeamNavForWorktrees } from "@/shared/lib/reverse-sync-nav-refresh";
import { traceWaveReaction } from "@/shared/lib/sr0-wave-trace";

/**
 * SW-FX-9 Part 2A: re-scan the affected pipelines as soon as a content
 * materialize wave completes.
 *
 * A materialize write is watcher-**suppressed** (loop prevention — the engine
 * must not treat its own write as a user edit and reverse-sync it), so the
 * watcher never emits a `context-file-change` for it. The pipeline scan that
 * would surface a newly-materialized `supporting_docs` ref (or status) then
 * has no trigger and waits for the next *unrelated* scan (navigation, another
 * edit, manual refresh) — the "next scan" latency.
 *
 * The wave is the authoritative writer, so it announces its own writes: on
 * `sync-wave-completed` we bump the file-refresh key for each affected repo,
 * which re-runs `usePipelineTree`'s scan. SW-FX-9's disk-truth gate makes that
 * rescan reflect disk even though the watcher never refreshed the live cache.
 * Bumps route through {@link bumpFileRefreshForSourceRepo} — the same
 * source-path → synthetic-workspace-key mapping the watcher path uses — so
 * workspace consumers (which subscribe on the synthetic key, not the raw disk
 * path the wave emits) actually see the bump.
 *
 * Gated on `materialize_counts.written > 0`: a no-op wave (everything already
 * current) changed nothing on disk, so it must not trigger a rescan. Per-repo
 * bumps coalesce through `usePipelineTree`'s 150ms debounce, so a wave fanning
 * out to many repos cannot cause a rescan storm.
 *
 * SW-XS-4 (K11): no wave runs on its own any more; the only wave this hears is
 * the migration cutover (SW-XS-8 moved the explicit operations onto the
 * preparation engine). It is reason-agnostic.
 *
 * Mounted once at the app root.
 */
export function useSyncWaveRescan(): void {
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    listen<SyncWaveCompletedPayload>(SYNC_WAVE_COMPLETED_EVENT, (event) => {
      const payload = event.payload;
      // SR-0 (Q3): trace EVERY wave's reaction (incl. no-op waves) so the
      // instance-count delta attributes native-backing allocation to the exact
      // wave. No-op unless `VIEWER_PERF.traceWaveReaction` is on — zero cost in
      // normal use, and it must run before the early return below.
      traceWaveReaction(payload?.wave_id);
      // No-op waves (nothing written) leave disk unchanged — skip. Written as
      // `!(written > 0)` rather than `written <= 0` so a runtime-`undefined`
      // `written` crossing the IPC boundary skips (defensive) instead of
      // falling through to a needless bump.
      if (!payload?.materialize_counts || !(payload.materialize_counts.written > 0)) {
        return;
      }
      // SW-XS-4: the SW-SY-4 per-path skip is retired with `changed_paths` — the
      // only waves left (the explicit publish, the migration cutover) carry no
      // per-file list — so broad-refresh each affected repo.
      const affected = payload.affected_repos ?? [];
      for (const repo of affected) {
        bumpFileRefreshForSourceRepo(repo, undefined);
      }
      // SW-NR-1: a publish writes the team's SHARED content, which the team nav (a
      // team-stable synthetic) can't see through the worktree path — resolve each
      // affected worktree to its team nav and counter-only bump it.
      bumpTeamNavForWorktrees(affected);
    })
      .then((fn) => {
        // The effect may have torn down before `listen` resolved — dispose
        // immediately in that case so we never leak a listener.
        if (disposed) safeUnlisten(fn);
        else unlisten = fn;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, []);
}
