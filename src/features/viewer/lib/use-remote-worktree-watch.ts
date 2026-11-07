// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  sourceRemoteTrackedCount,
  sourceRemoteWatchStart,
  sourceRemoteWatchStop,
} from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import { tickChangesetsForRepo } from "@/features/git-sync/lib/source-redline-source";
import { CADENCE_PRESETS, LARGE_REPO_THRESHOLD, setRemoteTrackedCount } from "./use-remote-cadence";

/**
 * DESIGN-remote-source.md §3.3 / AD-2 — a REF-COUNTED remote-worktree watcher
 * shared by every consumer of a (machineId, handle)'s change signal.
 *
 * Mirrors `use-worktree-watch.ts` exactly, keyed on `(machineId, handle)`
 * instead of a local `wsId` and backed by the polling-based remote watcher
 * (v1) or the pushed `WatchWorkspaceFiles` wire (v2). Multiple consumers on
 * the same (machineId, handle) — e.g. two tabs with source open on the same
 * remote workspace, or the editor inset + the diff overlay — share ONE poll
 * task on the host, which emits ONE event stream.
 *
 * The naive implementation (one watcher per view) would swamp a remote machine:
 * 2 s polling times N views times M handles = M * N background calls. The ref
 * count keeps that at `M` — one per unique (machineId, handle).
 *
 * DESIGN-remote-phase-2.md §0 / B-2 / B-3 — the poll period is passed from
 * the cadence preset (5 s small / 10 s large) so §0 isn't bypassed on the
 * wire, and a one-shot `source_remote_tracked_count` probe seeds
 * `useRemoteCadence`'s large-repo threshold the first time a (machineId,
 * handle) is attached.
 */

interface WatchEntry {
  count: number;
  /** The attach token once resolved; `null` while the attach is in flight. */
  token: number | null;
  /** Released to 0 before the in-flight attach resolved → its `.then` detaches. */
  detached: boolean;
  /** The Tauri event unlisten for this `(machineId, handle)`'s event stream. */
  unlisten: UnlistenFn | null;
  /** Monotonic tick count for `useRemoteWatchNonce`. */
  nonce: number;
  /** Change listeners for `useRemoteWatchNonce` (bump React state on tick). */
  listeners: Set<() => void>;
  /** `true` once the one-shot tracked-paths probe has run (success OR failure);
   *  prevents a reconnect loop from probing on every acquire. A full reset
   *  (clear + reacquire) re-arms it via a fresh entry. */
  trackedProbed: boolean;
  /** The poll period requested at attach time, so a re-attach after a
   *  cadence change can replace it. */
  pollMs: number;
}

/** The map key is a joined `(machineId, handle)` with 0x1F — the same separator
 *  the seam dedupe cache uses, so an off-by-one key cannot cause a cache hit
 *  without a watcher attach, or vice-versa. */
function watchKey(machineId: string, handle: string): string {
  return `${machineId}\x1f${handle}`;
}

const watches = new Map<string, WatchEntry>();

/** DESIGN-remote-phase-2.md B-4 fix — a shared entry both `acquire` and
 *  `useRemoteWatchNonce` route through, so a nonce listener attached BEFORE
 *  `acquire` runs (sibling-component case, or declaration order in one
 *  component) still fires on tick. The entry's `count` stays 0 until an
 *  `acquire` bumps it; the Tauri attach only runs on the 0→1 transition. */
function ensureEntry(key: string): WatchEntry {
  let entry = watches.get(key);
  if (!entry) {
    entry = {
      count: 0,
      token: null,
      detached: false,
      unlisten: null,
      nonce: 0,
      listeners: new Set(),
      trackedProbed: false,
      pollMs: CADENCE_PRESETS.remoteSmall.watcherMs,
    };
    watches.set(key, entry);
  }
  return entry;
}

/** Spawn the Tauri event-listen for this `(machineId, handle)`'s event stream.
 *  Captures the entry in the closure (not just the key) so a release+reacquire
 *  cycle while `listen()` is pending — which replaces the keyed entry under us
 *  — cannot double-write `unlisten` on the successor. See H-1 in the review. */
function startListen(machineId: string, handle: string, bound: WatchEntry): void {
  const key = watchKey(machineId, handle);
  const eventName = `remote-worktree-file-change-${machineId}-${handle}`;
  listen<{ changedPath: string | null }>(eventName, () => {
    // Read the CURRENT entry under the key — a replacement entry (after a
    // release+reacquire cycle) owns the new listeners. If it changed under
    // us the `bound` entry has been retired and this closure is a leaked
    // listener; detach-on-first-emit as a defensive sweep.
    const live = watches.get(key);
    if (!live) return;
    if (live !== bound) return; // leaked listener on a retired entry — ignore
    live.nonce++;
    tickChangesetsForRepo({ kind: "remote", machineId, handle });
    for (const l of live.listeners) l();
  })
    .then((fn) => {
      // The entry under the key may have been replaced (release-reacquire
      // raced the listen promise). Only install the unlisten on the SAME
      // entry we started the listen for; otherwise detach immediately so
      // the new entry can start its own listener cleanly.
      const live = watches.get(key);
      if (live !== bound || (bound.detached && bound.count === 0)) {
        fn();
        return;
      }
      bound.unlisten = fn;
    })
    .catch((err) => {
      logger.warn(
        "useRemoteWorktreeWatch",
        `remote worktree watch listen failed for ${key} — ${err}`,
      );
    });
}

/** DESIGN-remote-phase-2.md B-3 — one-shot probe of the remote tracked-paths
 *  count on the first attach of a (machineId, handle). Seeds
 *  `useRemoteCadence` so large repos auto-loosen to the `remoteLarge` preset.
 *  Failures are logged but non-fatal (the preset falls back to `remoteSmall`,
 *  which is correct for an unknown-size repo). */
function probeTrackedCount(machineId: string, handle: string, bound: WatchEntry): void {
  if (bound.trackedProbed) return;
  bound.trackedProbed = true;
  sourceRemoteTrackedCount(machineId, handle)
    .then((count) => {
      // The entry may have been retired between dispatch and resolution —
      // only publish if this is still the entry under the key.
      if (watches.get(watchKey(machineId, handle)) !== bound) return;
      setRemoteTrackedCount(machineId, handle, count);
    })
    .catch((err) => {
      logger.warn(
        "useRemoteWorktreeWatch",
        `remote tracked-paths probe failed for ${machineId}/${handle} — ${err}; preset falls back to remoteSmall`,
      );
    });
}

function acquire(machineId: string, handle: string, pollMs: number): void {
  const key = watchKey(machineId, handle);
  const existing = watches.get(key);
  if (existing && existing.count > 0) {
    existing.count++;
    existing.detached = false; // re-acquired before an in-flight attach resolved
    return;
  }
  // A pre-existing entry with count=0 was created by `useRemoteWatchNonce`
  // before any acquire — upgrade it in place so the nonce listeners it holds
  // survive the first acquire and start receiving ticks.
  const entry = existing ?? ensureEntry(key);
  entry.count = 1;
  entry.detached = false;
  entry.pollMs = pollMs;
  // Fire the per-handle listen only on the 0→1 transition — a pre-existing
  // entry with its own listen may already be live from a prior detach-race.
  if (!entry.unlisten) {
    startListen(machineId, handle, entry);
  }
  // Fire-and-forget — a watch failure must not block the mount. A `catch`
  // logs and leaves `token: null`; the next release is a no-op (nothing to
  // detach); the next acquire re-tries.
  sourceRemoteWatchStart(machineId, handle, pollMs)
    .then((token) => {
      // Only publish the token onto the SAME entry we started the attach for.
      // A release-reacquire cycle may have replaced it; the new entry has its
      // own in-flight attach, so detach this one.
      const live = watches.get(key);
      if (live !== entry || (entry.detached && entry.count === 0)) {
        sourceRemoteWatchStop(token).catch(() => {});
        return;
      }
      entry.token = token;
      // The attach is up — one-shot probe tracked-paths to seed the preset.
      probeTrackedCount(machineId, handle, entry);
    })
    .catch((err) => {
      logger.warn(
        "useRemoteWorktreeWatch",
        `remote worktree watch attach failed for ${key} — ${err}`,
      );
      // Keep the entry so repeated mounts don't spin on a hard-fail; the next
      // release decrements and clears it when count hits zero.
    });
}

function release(machineId: string, handle: string): void {
  const key = watchKey(machineId, handle);
  const entry = watches.get(key);
  if (!entry) return;
  entry.count--;
  if (entry.count > 0) return;
  if (entry.token !== null) {
    sourceRemoteWatchStop(entry.token).catch(() => {});
    if (entry.unlisten) entry.unlisten();
    // Keep listeners alive if any `useRemoteWatchNonce` still holds a
    // listener — a late consumer (e.g. a sibling re-mount) will reacquire
    // and the entry's nonce + listeners persist. Only drop the entry when
    // nothing is listening either.
    if (entry.listeners.size === 0) {
      watches.delete(key);
    } else {
      entry.token = null;
      entry.unlisten = null;
      entry.trackedProbed = false; // allow a reacquire to re-probe
    }
  } else {
    // Attach still in flight — its `.then` sees count 0 and detaches + removes.
    entry.detached = true;
  }
}

/**
 * Attach the remote worktree watcher for `(machineId, handle)` while `enabled`
 * (and detach on cleanup), ref-counted so multiple consumers share one watch.
 * Idempotent per consumer — each mount acquires exactly one reference.
 *
 * Pass `enabled: false` on local tabs; the hook is a no-op there (so a
 * component that computes a `SourceRepo` at its root can call both local and
 * remote hooks unconditionally).
 *
 * DESIGN-remote-phase-2.md B-2 — `pollMs` is threaded from the caller's
 * resolved cadence (`useRemoteCadence(repo).watcherMs`) so the Rust task polls
 * at the §0 remote cadence (5 s small / 10 s large) rather than the local-
 * native 2 s default.
 */
export function useRemoteWorktreeWatch(
  machineId: string | null,
  handle: string | null,
  enabled: boolean,
  pollMs?: number,
): void {
  useEffect(() => {
    if (!enabled || !machineId || !handle) return;
    const resolvedPollMs = pollMs ?? CADENCE_PRESETS.remoteSmall.watcherMs;
    acquire(machineId, handle, resolvedPollMs);
    return () => release(machineId, handle);
    // Omit `pollMs` from deps intentionally — a cadence tweak mid-session
    // takes effect on the next attach cycle (overlay close + reopen), to avoid
    // thrashing the Rust task on every render if the preset flipped due to a
    // tracked-count probe landing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, machineId, handle]);
}

/**
 * DESIGN-remote-phase-2.md §1.6 — a nonce that bumps on every watcher-reported
 * change for `(machineId, handle)`. Components subscribe to re-render on a
 * change (file-level reconcile the open editor, etc). The nonce is shared
 * across the ref-counted watch, so N views see the same bumps.
 *
 * B-4 fix: the listener attaches via `ensureEntry` even BEFORE `acquire` has
 * run, so a hook-order inversion (nonce declared before the watch hook, or
 * in a sibling component mounted first) still receives subsequent ticks. The
 * acquire's 0→1 transition then starts the Tauri attach and the listeners
 * fire on tick.
 */
export function useRemoteWatchNonce(machineId: string | null, handle: string | null): number {
  // Start from the entry's current nonce, and re-base on a switch to another
  // watch in the same render, so a watch that already ticked reads as where
  // it is, not as a fresh tick (a false "editing…" and refetch on opening or
  // switching a view over it).
  const key = machineId && handle ? watchKey(machineId, handle) : null;
  const current = () => (key ? (watches.get(key)?.nonce ?? 0) : 0);
  const [state, setState] = useState(() => ({ key, nonce: current() }));
  let nonce = state.nonce;
  if (state.key !== key) {
    nonce = current();
    setState({ key, nonce });
  }
  useEffect(() => {
    if (!machineId || !handle) return;
    const key = watchKey(machineId, handle);
    const setNonce = (n: number) => setState((s) => (s.key === key ? { key, nonce: n } : s));
    // Guarantee the entry exists so a sibling-component or declaration-order
    // mount of the nonce hook before `useRemoteWorktreeWatch` doesn't miss
    // ticks forever (B-4). The entry stays with count=0 until a real
    // `acquire` bumps it — ensure alone starts no watcher.
    const entry = ensureEntry(key);
    const listener = () => setNonce(entry.nonce);
    entry.listeners.add(listener);
    // Seed with the current nonce — if a tick landed between the first attach
    // and this effect running, we would otherwise miss it.
    setNonce(entry.nonce);
    return () => {
      // H-2 fix — resolve the entry via the captured reference, not via
      // `watches.get(key)`, so a release+reacquire that replaced the entry
      // under this key doesn't orphan our listener.
      entry.listeners.delete(listener);
      // If nothing else holds the entry (no acquire, no listeners), GC it.
      if (entry.count === 0 && entry.listeners.size === 0 && !entry.token && !entry.unlisten) {
        if (watches.get(key) === entry) watches.delete(key);
      }
    };
  }, [machineId, handle]);
  return nonce;
}

/** Test-only: reset the shared refcount map between cases. */
export function __resetRemoteWorktreeWatch(): void {
  for (const entry of watches.values()) {
    if (entry.unlisten) entry.unlisten();
  }
  watches.clear();
}

/** Test-only: inspect the current refcount for a (machineId, handle). */
export function __remoteWorktreeWatchRefCount(machineId: string, handle: string): number {
  return watches.get(watchKey(machineId, handle))?.count ?? 0;
}

/** Test-only: synthesize a watcher tick for the given handle (bump nonce +
 *  tick seam subscriptions). Lets tests exercise the debounce+coalesce path
 *  without wiring up the real Tauri event. */
export function __fireRemoteWatchTickForTests(machineId: string, handle: string): void {
  const entry = watches.get(watchKey(machineId, handle));
  if (!entry) return;
  entry.nonce++;
  tickChangesetsForRepo({ kind: "remote", machineId, handle });
  for (const l of entry.listeners) l();
}

/** Test-only: get the pollMs the active entry was attached with (null if no
 *  active entry). Lets a vitest assert that the §0 cadence table is actually
 *  threaded on the wire (B-2 regression gate). */
export function __remoteWorktreeWatchPollMs(machineId: string, handle: string): number | null {
  const entry = watches.get(watchKey(machineId, handle));
  return entry && entry.count > 0 ? entry.pollMs : null;
}

/** Test-only: the §0 scale-fallback banner's threshold, re-exported so tests
 *  that mock a tracked-count can assert the preset flip. */
export { LARGE_REPO_THRESHOLD };
