// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";

import type { SourceRepo, CadencePreset } from "@/features/git-sync/lib/source-redline-source";

/**
 * DESIGN-remote-phase-2.md §0 / §1.10 — resolves the debounce + poll cadence
 * for a `(repo, trackedCount)` pair, honoring an optional per-machine override.
 *
 * The three built-in presets come from the §0 cadence table:
 *   - `localNative`  — native fsevents/inotify + tight debounce
 *   - `remoteSmall`  — polling 5 s / source-read 10 s / diff 25 s (default remote)
 *   - `remoteLarge`  — polling 10 s / source-read 15 s / diff 60 s (>5K files)
 *
 * The large-repo threshold (`LARGE_REPO_THRESHOLD`) matches the Linux inotify
 * default (8192 user watches) and the Chromium PSA's ~5K file guidance. The
 * tracked-count probe is a one-shot `WorkspaceFile::TrackedPaths` on first
 * remote-watcher attach; the result lives in `viewer-store` keyed by
 * `(machineId, handle)`.
 */

/** Hard threshold at which the remote defaults loosen to the "monorepo" preset.
 *  Rationale: Linux inotify defaults to 8192 user watches; Chromium's PSA cites
 *  ~5K as the practical point where `git status` walks a tree slowly enough
 *  that fsmonitor pays off. 5000 is the conservative pick. */
export const LARGE_REPO_THRESHOLD = 5000;

/** The §0 cadence presets. Milliseconds throughout. */
export const CADENCE_PRESETS = {
  localNative: {
    watcherMs: 2000,
    sourceReadMs: 10_000,
    diffMs: 15_000,
  },
  remoteSmall: {
    watcherMs: 5000,
    sourceReadMs: 10_000,
    diffMs: 25_000,
  },
  remoteLarge: {
    watcherMs: 10_000,
    sourceReadMs: 15_000,
    diffMs: 60_000,
  },
} as const satisfies Record<string, CadencePreset>;

/** Default export (frozen) — used by callers that only need the "pick a preset"
 *  behavior without the override lookup. */
export function pickCadencePreset(repo: SourceRepo, trackedCount: number | null): CadencePreset {
  if (repo.kind === "local") return CADENCE_PRESETS.localNative;
  if (trackedCount != null && trackedCount > LARGE_REPO_THRESHOLD) {
    return CADENCE_PRESETS.remoteLarge;
  }
  return CADENCE_PRESETS.remoteSmall;
}

// ---------------------------------------------------------------------------
// Session-scoped stores (module-level — not viewer-store persisted) for the
// override + probed-tracked-count. Session scope is correct: a reconnect
// re-probes, a per-machine override is set by the user per session (and the
// Settings UI will persist in a follow-up pass).
// ---------------------------------------------------------------------------

const overrides = new Map<string /* machineId */, CadencePreset>();
const trackedCounts = new Map<string /* machineId\x1fhandle */, number>();
/** DESIGN-remote-phase-2.md §0 / B-3 — one-shot fsmonitor-nudge banner state,
 *  keyed by `machineId\x1fhandle`. `true` once the user has seen (or dismissed)
 *  the banner, so a reconnect's fresh probe doesn't nag again. */
const fsmonitorBannerSeen = new Set<string>();
/** DESIGN-remote-phase-2.md B-3 — the one-shot callback the UI shell wires to
 *  show the "enable git fsmonitor" banner. Set once at app boot; the module
 *  calls it after a tracked-count probe lands over the threshold. */
let fsmonitorNudgeHandler:
  | ((machineId: string, handle: string, trackedCount: number) => void)
  | null = null;
const listeners = new Set<() => void>();

function emit(): void {
  for (const l of listeners) l();
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => void listeners.delete(l);
}

/** Set (or clear) a per-machine override. The Settings UI calls this; a null
 *  value drops the override and falls back to the preset auto-selection. */
export function setRemoteCadenceOverride(machineId: string, preset: CadencePreset | null): void {
  if (preset) overrides.set(machineId, preset);
  else overrides.delete(machineId);
  emit();
}

/** Record a one-shot tracked-paths count for a `(machineId, handle)`. Called
 *  from the remote-watcher hook after its first attach. A later call replaces
 *  the prior value (a reconnect's fresh probe wins).
 *
 *  B-3 (AD-5) — if the count exceeds `LARGE_REPO_THRESHOLD` and the user has
 *  not already seen the fsmonitor banner for this `(machineId, handle)`, the
 *  registered `fsmonitorNudgeHandler` is called ONCE. Dismissing the banner
 *  is the UI shell's concern; the module just suppresses re-nudges. */
export function setRemoteTrackedCount(machineId: string, handle: string, count: number): void {
  const key = `${machineId}\x1f${handle}`;
  trackedCounts.set(key, count);
  if (count > LARGE_REPO_THRESHOLD && !fsmonitorBannerSeen.has(key)) {
    fsmonitorBannerSeen.add(key);
    fsmonitorNudgeHandler?.(machineId, handle, count);
  }
  emit();
}

/** DESIGN-remote-phase-2.md §0 — register the UI shell's handler for the
 *  one-shot "enable git fsmonitor on the remote" banner. The default handler
 *  (shipped in `App.tsx`) emits a `sonner` toast. Tests supply a stub. */
export function registerFsmonitorNudgeHandler(
  handler: ((machineId: string, handle: string, trackedCount: number) => void) | null,
): void {
  fsmonitorNudgeHandler = handler;
}

export function __resetRemoteCadence(): void {
  overrides.clear();
  trackedCounts.clear();
  fsmonitorBannerSeen.clear();
  fsmonitorNudgeHandler = null;
  // Deliberately NOT clearing listeners — tests may hold them live across resets.
}

/**
 * React hook that resolves a `CadencePreset` for a `SourceRepo`, applying a
 * per-machine override if one is set. The tracked-paths count is read from
 * the module-level map, keyed on `(machineId, handle)` — a `null` falls back
 * to the small-remote preset until the first watcher attach probes it.
 */
export function useRemoteCadence(repo: SourceRepo): CadencePreset {
  const [tick, setTick] = useState(0);
  useEffect(() => subscribe(() => setTick((n) => n + 1)), []);
  return useMemo(() => {
    const override = repo.kind === "remote" ? (overrides.get(repo.machineId) ?? null) : null;
    if (override) return override;
    const trackedCount =
      repo.kind === "remote"
        ? (trackedCounts.get(`${repo.machineId}\x1f${repo.handle}`) ?? null)
        : null;
    return pickCadencePreset(repo, trackedCount);
    // `tick` triggers reactivity: a store change → emit → tick++ → re-memo.
  }, [repo, tick]);
}

/** Test-only: inspect the current override + tracked-count for a repo. */
export function __inspectRemoteCadence(repo: SourceRepo): {
  override: CadencePreset | null;
  trackedCount: number | null;
} {
  if (repo.kind === "local") return { override: null, trackedCount: null };
  const key = `${repo.machineId}\x1f${repo.handle}`;
  return {
    override: overrides.get(repo.machineId) ?? null,
    trackedCount: trackedCounts.get(key) ?? null,
  };
}
