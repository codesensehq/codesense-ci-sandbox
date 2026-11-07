// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SW-TS-3 (AC-2): per-surface sync status, keyed by the surface's raw
 * `code_repo_path` (the same key the wave events carry in `affected_repos`). This is **ephemeral**, event-derived state —
 * deliberately NOT in `viewer-store` (which round-trips to the backend via
 * `set_viewer_state`). This store is just the in-window projection of the
 * `sync-wave-started` / `sync-wave-completed` stream so chips can render
 * `Synced` / `Syncing…` / `Failed`.
 *
 * SW-XS-3: nothing syncs a workspace on its own any more, so the auto-retry
 * faces (`retrying`, `failed-terminal`), the manual-mode `out-of-date` face and
 * the chip's Retry are gone.
 */
export type SurfaceSyncState = "syncing" | "synced" | "failed";

export interface SurfaceSyncStatus {
  state: SurfaceSyncState;
  /** The wave that produced this state — used to drop stale/superseded
   *  events (a completed event for an older wave must not clobber a newer
   *  surface state). Monotonic per process. */
  waveId: number;
  /** AC-3: a plain-language reason shown next to the Retry affordance.
   *  Set on `failed` (the first wave error's message). */
  errorMessage?: string;
  /** SW-FX-89 (AC-1c): the wave's human label ("Preparing context…",
   *  "Switching team…", …), derived once from the wave reason at `markStarted`
   *  and shown on the `syncing` hover so a spinner is never unexplained. Absent
   *  only for the pre-SW-FX-89 no-reason path; the chip falls back to a generic
   *  "Syncing…" so the hover is never blank. */
  reasonLabel?: string;
}

interface SurfaceSyncStore {
  /** code_repo_path → status. */
  byPath: Record<string, SurfaceSyncStatus>;
  /**
   * SLW-CO-6 (AC-6): non-wave assembly failures, keyed by surface path. The
   * epic's per-repo worktree assembly (`assemble_epic_worktrees_core`) is a
   * DISTINCT source from the `EpicInheritance` wave that `byPath` reflects — and
   * the two race on the same surface key. Keeping assembly failures in their own
   * slot (OR'd into the chip's `failed` state — see {@link useSurfaceSyncStatus})
   * means a racing inheritance-`synced` can't clobber the assembly failure.
   * Assembly OWNS this slot: only a fresh successful assembly clears it
   * (`clearAssemblyFailure`) — NOT `markStarted`, since inheritance waves fire
   * independently of assembly and would otherwise green a still-broken epic.
   */
  assemblyFailure: Record<string, string>;
  /** A surface's wave started — it is now syncing. `reasonLabel` (SW-FX-89) is
   *  the wave's human label for the `syncing` hover. */
  markStarted: (codeRepoPath: string, waveId: number, reasonLabel?: string) => void;
  /** SW-FX-89 (AC-1/AC-3): a single fan-out wave started on N surfaces at once
   *  (content/source/startup/team-switch → `affected_repos`). Applies the same
   *  stale/supersede guards as `markStarted` per path but in ONE `set`, so a
   *  50-repo fan-out is one store notification, not 50. */
  markStartedMany: (codeRepoPaths: string[], waveId: number, reasonLabel?: string) => void;
  /** A surface's wave completed — `synced` unless it errored/cancelled. */
  markCompleted: (
    codeRepoPath: string,
    waveId: number,
    outcome: { ok: true } | { ok: false; errorMessage?: string },
  ) => void;
  /** SLW-CO-6 (AC-6): record a non-wave assembly failure for a surface — shown
   *  as `failed` on its chip, independent of (and un-clobberable by) the wave. */
  markAssemblyFailure: (codeRepoPath: string, errorMessage: string) => void;
  /** SLW-CO-6 (AC-6): clear a surface's assembly failure — called when a fresh
   *  assembly succeeds (assembly owns this slot; nothing else clears it). */
  clearAssemblyFailure: (codeRepoPath: string) => void;
}

/** A later wave id always wins; an equal id lets a completed event finalize the
 *  started state it matches. An older id is a superseded/stale event — ignore. */
function isStale(prev: SurfaceSyncStatus | undefined, waveId: number): boolean {
  return prev !== undefined && waveId < prev.waveId;
}

/** SW-FX-89: fold one surface's `started` into `byPath`, applying the
 *  stale/supersede guards. Returns the SAME map when the event is stale or a
 *  same-id re-`started` after settle (a no-op). */
function applyStarted(
  byPath: Record<string, SurfaceSyncStatus>,
  codeRepoPath: string,
  waveId: number,
  reasonLabel: string | undefined,
): Record<string, SurfaceSyncStatus> {
  const prev = byPath[codeRepoPath];
  if (isStale(prev, waveId)) return byPath;
  // A late `started` must not downgrade synced/failed back to syncing.
  if (prev && prev.waveId === waveId && prev.state !== "syncing") return byPath;
  return { ...byPath, [codeRepoPath]: { state: "syncing", waveId, reasonLabel } };
}

export const useSurfaceSyncStore = create<SurfaceSyncStore>((set) => ({
  byPath: {},
  assemblyFailure: {},
  markStarted: (codeRepoPath, waveId, reasonLabel) =>
    set((s) => {
      const byPath = applyStarted(s.byPath, codeRepoPath, waveId, reasonLabel);
      return byPath === s.byPath ? s : { byPath };
    }),
  markStartedMany: (codeRepoPaths, waveId, reasonLabel) =>
    set((s) => {
      // One `set` for the whole fan-out: one store notification.
      let byPath = s.byPath;
      for (const path of codeRepoPaths) byPath = applyStarted(byPath, path, waveId, reasonLabel);
      return byPath === s.byPath ? s : { byPath };
    }),
  markAssemblyFailure: (codeRepoPath, errorMessage) =>
    set((s) => ({
      assemblyFailure: { ...s.assemblyFailure, [codeRepoPath]: errorMessage },
    })),
  clearAssemblyFailure: (codeRepoPath) =>
    set((s) => {
      if (s.assemblyFailure[codeRepoPath] === undefined) return s;
      const { [codeRepoPath]: _cleared, ...rest } = s.assemblyFailure;
      return { assemblyFailure: rest };
    }),
  markCompleted: (codeRepoPath, waveId, outcome) =>
    set((s) => {
      if (isStale(s.byPath[codeRepoPath], waveId)) return s;
      const next: SurfaceSyncStatus = outcome.ok
        ? { state: "synced", waveId }
        : { state: "failed", waveId, errorMessage: outcome.errorMessage };
      return { byPath: { ...s.byPath, [codeRepoPath]: next } };
    }),
}));

/** Selector hook: a single surface's wave status (or `undefined` before its
 *  first wave). Components subscribe to just their surface, so an unrelated
 *  surface's wave never re-renders them. Prefer {@link useSurfaceSyncStatus},
 *  which also folds in any non-wave assembly failure. */
export const useSurfaceWaveStatus = (
  codeRepoPath: string | undefined,
): SurfaceSyncStatus | undefined =>
  useSurfaceSyncStore((s) => (codeRepoPath ? s.byPath[codeRepoPath] : undefined));

/** Selector hook: a single surface's non-wave assembly failure message, if any. */
export const useSurfaceAssemblyFailure = (codeRepoPath: string | undefined): string | undefined =>
  useSurfaceSyncStore((s) => (codeRepoPath ? s.assemblyFailure[codeRepoPath] : undefined));

/**
 * Selector hook: a surface's EFFECTIVE status — the wave status with any
 * non-wave assembly failure (SLW-CO-6 AC-6) OR'd in as a `failed`. Subscribes to
 * both slots via stable selectors and combines in render, so the derived object
 * doesn't churn the store's equality check. SW-XS-4: the automatic reverse-sync
 * conflict state is gone with the automatic conflict path.
 */
export const useSurfaceSyncStatus = (
  codeRepoPath: string | undefined,
): SurfaceSyncStatus | undefined => {
  const wave = useSurfaceWaveStatus(codeRepoPath);
  const assemblyErr = useSurfaceAssemblyFailure(codeRepoPath);
  if (assemblyErr !== undefined) {
    // Non-wave failure: takes precedence over the wave status.
    return { state: "failed", waveId: wave?.waveId ?? 0, errorMessage: assemblyErr };
  }
  return wave;
};
