// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  SYNC_WAVE_COMPLETED_EVENT,
  CONTEXT_FILE_CHANGE_EVENT,
  type SyncWaveCompletedPayload,
  type WaveReasonView,
} from "@/tauri/events";
import { createKeyedDebounce } from "@/shared/lib/keyed-debounce";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { useCompositeRefreshStore } from "@/shared/stores/composite-refresh-store";

/**
 * SLW-NC-5 AC-1 — decide whether a completed wave changed a layer store the
 * active team's composite overlay draws from, so the overlay must re-resolve.
 *
 * Pure + exported for unit testing. Biases toward freshness (Finding 2): a
 * spurious re-resolve is cheap (debounced, single-repo, stale-while-revalidate,
 * non-blocking), whereas a MISSED one leaves exactly the stale UI this story
 * fixes. So the `default` arm re-resolves on any unrecognized reason that wrote
 * to disk, and only the surfaces known-irrelevant to the team-workspace
 * composite are explicitly skipped.
 */
export function shouldRefreshCompositeOverlay(
  reason: WaveReasonView,
  materializeCounts: SyncWaveCompletedPayload["materialize_counts"],
  cancelled: boolean,
): boolean {
  if (cancelled) return false;
  switch (reason.kind) {
    case "MigrationCutover":
      // Layers reorganized on disk → composite may change.
      return true;
    default:
      // Unknown / future reason: re-resolve only if it actually wrote to disk.
      return materializeCounts.written > 0;
  }
}

/**
 * SLW-NC-5 — live-refresh the source-repo composite overlay.
 *
 * Mounted ONCE (alongside `useSyncWaveRescan`). Listens to the two seams that
 * carry every external change named in the story and, on one relevant to the
 * active team's layer stores, bumps that team's composite-refresh nonce — which
 * re-runs the agent context pivot's want-set resolve (`useAgentContext`, SW-XS-15).
 * Bursts coalesce through a 150ms keyed debounce so a wave touching many files
 * yields a single re-resolve (AC-2); the resolve itself rides the hook's existing
 * stale-while-revalidate lifecycle, so the nav never blocks (AC-2).
 *
 * Two triggers, one mechanism:
 * - `sync-wave-completed` — an explicit publish / migration (the
 *   engine announcing its own writes), filtered by {@link shouldRefreshCompositeOverlay}.
 * - `context-file-change` — a direct agent/raw write into the active team's
 *   context (the exact NC-2 repro) that may not spawn a wave.
 * Both funnel through the SAME debounce + nonce bump keyed on the active teamKey.
 */
export function useCompositeOverlayRefresh(): void {
  useEffect(() => {
    // Keyed + payloaded by teamKey. `createKeyedDebounce` suppresses an
    // `undefined` payload, so pass the teamKey as the (non-undefined) payload.
    // SW-SY-4 (AC-2): leading-edge throttle so a sustained wave/watcher burst
    // keeps the nav re-resolving at a bounded cadence instead of starving until
    // the burst ends (a trailing debounce never fires mid-burst). The re-resolve
    // is cheap (single-repo) and stale-while-revalidate, so the leading fire
    // never blocks; the trailing flush lands the final post-burst state.
    const debounce = createKeyedDebounce<string>(
      150,
      (teamKey) => {
        useCompositeRefreshStore.getState().bumpCompositeRefresh(teamKey);
      },
      { leading: true },
    );

    const group = createListenerGroup();
    const track = group.track;

    track(
      listen<SyncWaveCompletedPayload>(SYNC_WAVE_COMPLETED_EVENT, (event) => {
        const payload = event.payload;
        if (!payload) return;
        const teamKey = getActiveTeamKey();
        // Default absent counts to zero rather than early-returning: reasons that
        // refresh independent of a write (MigrationCutover) must
        // still be evaluated; only the `default`
        // arm reads `written`, where a defaulted 0 correctly skips.
        const counts = payload.materialize_counts ?? { written: 0, skipped: 0, unmaterialized: 0 };
        if (!shouldRefreshCompositeOverlay(payload.reason, counts, payload.cancelled)) {
          return;
        }
        debounce.schedule(teamKey, teamKey);
      }),
    );

    track(
      listen(CONTEXT_FILE_CHANGE_EVENT, () => {
        // A direct write into a watched context repo (the composite's
        // team-workspace layer) — the case a wave may not cover (raw / agent
        // write). The payload carries no team identity the FE can map, so we
        // bump the ACTIVE team unconditionally (bias toward freshness, NC-5
        // Finding 2): a background-team write causes at most one spurious,
        // identical-result re-resolve of the active team's selected repo, and
        // when no repo is selected the hook early-returns — a cheap no-op.
        const teamKey = getActiveTeamKey();
        debounce.schedule(teamKey, teamKey);
      }),
    );

    return () => {
      debounce.cancelAll();
      group.dispose();
    };
  }, []);
}
