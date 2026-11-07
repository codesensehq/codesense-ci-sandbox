// SPDX-License-Identifier: MIT
/**
 * SLW-WD-1 — the source worktree diff data-source seam.
 *
 * The sanctioned swap-point (alongside `file-redline-source.ts`) allowed to
 * import `@/tauri/commands` — see `no-direct-invoke.test.ts`. Wraps the real
 * `source_worktree_changeset` / `source_file_redline` Tauri commands so the
 * WD-2 source-diff review overlay (and any consumer) reaches the diff data over
 * an EXPLICIT worktree path, never touching `invoke` directly.
 *
 * This is the *source*-repo mirror of `file-redline-source.ts` (which serves the
 * *context*-repo publish redline): same `FileRedline` shape and `PublishFile`
 * records, so WD-2 renders both with the existing components. The only
 * difference is the baseline — here the worktree's merge-base (fork point),
 * there the `origin/main` publish tip.
 */

import { sourceCommitChangeset as invokeSourceCommitChangeset } from "@/tauri/commands";
import { sourceCommitFileRedline as invokeSourceCommitFileRedline } from "@/tauri/commands";
import { sourceCommitGraph as invokeSourceCommitGraph } from "@/tauri/commands";
import { sourceFileRedline as invokeSourceFileRedline } from "@/tauri/commands";
import { sourceRemoteChangeset as invokeSourceRemoteChangeset } from "@/tauri/commands";
import { sourceRemoteFileRedline as invokeSourceRemoteFileRedline } from "@/tauri/commands";
import { sourceWorktreeChangeset as invokeSourceWorktreeChangeset } from "@/tauri/commands";

import type { FileRedline, PublishFile, SourceCommitGraphEntry } from "./types";

/**
 * DESIGN-remote-source.md §3.1 — the data-source seam's unified repo identity.
 * Local tabs identify a worktree by its `cwd` on This Mac; remote tabs identify
 * one by `(machineId, handle)` — the machine's id and the managed-workspace
 * handle minted at remote-create. Everything downstream (the dedupe cache, the
 * ref-counted watcher, the resolver) keys on this.
 */
export type SourceRepo =
  | { kind: "local"; cwd: string }
  | { kind: "remote"; machineId: string; handle: string };

/** The canonical cache-key fragment for a {@link SourceRepo}. The local branch
 *  carries just the cwd (unchanged from pre-remote); the remote branch encodes
 *  `machineId<US>handle` where `<US>` is 0x1F so the field separator never
 *  collides with a path, a UUID, or a git ref. */
function repoKey(repo: SourceRepo): string {
  return repo.kind === "local" ? repo.cwd : `${repo.machineId}\x1f${repo.handle}`;
}

/**
 * The per-tab-primitives data-source dedupe cache (DESIGN-per-tab-primitives.md
 * AD-2 / §5.4). Multiple UI subscribers watching the SAME `(cwd, baseRef)` worktree
 * (e.g. two tabs with source open on the same workspace) would otherwise fire
 * independent fetches on every edit burst. This cache gives them one shared
 * in-flight Promise per deterministic request identity:
 *
 *   - A caller with an in-flight key gets the same Promise, not a new fetch.
 *   - Entries evict on settle (resolve or reject), so no stale value lingers.
 *   - Different `(cwd, baseRef)` identities (two different worktrees) do NOT
 *     share — the backend still runs their fetches in parallel, which is
 *     correct (two repos).
 *
 * The seam-level dedupe is the single reason the per-tab primitives design
 * does NOT cap source views to one at a time per CodeSense instance (§5.4).
 * Remote source (deferred follow-up) rides the same cache behind the same
 * identity shape, so a remote fan-out can slot in without a UI cap.
 */

interface InFlight<T> {
  promise: Promise<T>;
}

const changesetInflight = new Map<string, InFlight<PublishFile[]>>();
const redlineInflight = new Map<string, InFlight<FileRedline>>();
const commitChangesetInflight = new Map<string, InFlight<PublishFile[]>>();
const commitRedlineInflight = new Map<string, InFlight<FileRedline>>();

/** Join the fields of a dedupe key with a separator that cannot appear in a
 *  canonical path or git ref. Null/undefined fields collapse to the empty
 *  slot, so `(cwd, undefined)` and `(cwd, "")` are the same key. */
function joinKey(parts: readonly (string | undefined | null)[]): string {
  return parts.map((p) => p ?? "").join("\x1f");
}

/** Share one in-flight fetch for `key` across all callers; evict on settle. */
function dedupe<T>(
  cache: Map<string, InFlight<T>>,
  key: string,
  fetch: () => Promise<T>,
): Promise<T> {
  const existing = cache.get(key);
  if (existing) return existing.promise;
  const promise = fetch().finally(() => {
    // Evict only if this is still the current in-flight entry — a later caller
    // that enqueued its own fetch under this key (e.g. after invalidation we
    // don't currently signal) will keep its own promise alive.
    if (cache.get(key)?.promise === promise) cache.delete(key);
  });
  cache.set(key, { promise });
  return promise;
}

/** Test-only: clear every dedupe cache (vitest resets between cases). */
export function __resetSourceRedlineCache(): void {
  changesetInflight.clear();
  redlineInflight.clear();
  commitChangesetInflight.clear();
  commitRedlineInflight.clear();
  __resetSubscribeState();
}

// ---------------------------------------------------------------------------
// DESIGN-remote-phase-2.md §1 — the per-operation debounce + coalesce layer.
//
// Lives at the seam (AD-1) so every subscriber on `(repo, baseRef)` shares ONE
// scheduled fetch per window. Cold-fetch bypasses (AD-2), save never debounces
// (AD-3), per-operation windows (AD-4). A manual-force fetch clears any
// pending scheduled fetch for that key and fires immediately.
// ---------------------------------------------------------------------------

/**
 * One of the three debounced operation classes. `watcher` is the cadence of the
 * tick itself (set at the watcher layer); `source-read` and `diff` are the two
 * seam-level fetch classes. Editor save and user-driven manual refresh never
 * route through here — they bypass.
 */
export type OpClass = "watcher" | "source-read" | "diff";

/** Per-operation cadence preset in milliseconds. The hook `useRemoteCadence`
 *  resolves one of these from `(repo, trackedCount, override)`. */
export interface CadencePreset {
  watcherMs: number;
  sourceReadMs: number;
  diffMs: number;
}

/** Changeset subscription state keyed by `joinKey([repoKey, baseRef])`.
 *  Multiple consumers (overlay + editor + a future split view) attach to the
 *  same entry so they share one scheduled fetch per window. */
interface ChangesetSubscription {
  readonly key: string;
  readonly repo: SourceRepo;
  readonly baseRef: string | undefined;
  /** H-7 fix — operation class picks which `*Ms` of the cadence governs this
   *  subscription's debounce window: `"diff"` → `diffMs` (overlay's changeset),
   *  `"source-read"` → `sourceReadMs` (per-file reads via a future subscription
   *  shape — unconsumed in Phase 2 but validated by the test matrix). */
  opClass: OpClass;
  /** Current preset — the newest consumer's wins (the FE is encouraged to pass
   *  the SAME preset per `(machineId, handle)`, so this is stable in practice). */
  cadence: CadencePreset;
  consumers: Set<ChangesetConsumer>;
  /** The last successful fetch's files, if any. Used to seed a late joiner
   *  immediately so a second overlay opening does not block on debounce. */
  last: readonly PublishFile[] | null;
  /** Monotonic version — incremented on every settled fetch. A late joiner's
   *  seed uses this to know whether to call its own fetch. */
  version: number;
  /** The in-flight fetch's Promise, if any. */
  inflight: Promise<PublishFile[]> | null;
  /** H-12 fix — a tick arrived while a fetch was in flight; the fetch's
   *  `.then` reschedules a second cycle so the next consumer-visible update
   *  isn't lost waiting for a watcher tick that may never come. */
  rearmPending: boolean;
  /** A pending (scheduled) debounced fetch's timer handle. `null` when idle. */
  pendingTimer: ReturnType<typeof setTimeout> | null;
  /** `true` while the FIRST fetch after activation hasn't settled yet. The
   *  first tick is immediate (AD-2); after it settles the window arms. */
  cold: boolean;
  /** A watcher-tick signal arrived while `cold` or during the first fetch — the
   *  next debounce window must fire after the first settles. */
  armedWhileCold: boolean;
  /** The in-flight fetch's generation — bumped on every `fireChangesetFetch`.
   *  Lets a `.then` detect that it was superseded by a later fire and skip
   *  propagating stale results (e.g. after a user-force bypass). */
  generation: number;
}

interface ChangesetConsumer {
  onUpdate: (files: readonly PublishFile[]) => void;
  onError: (err: unknown) => void;
}

const changesetSubs = new Map<string, ChangesetSubscription>();

/** Test-only: cancel every pending timer and clear subscription state. */
function __resetSubscribeState(): void {
  for (const sub of changesetSubs.values()) {
    if (sub.pendingTimer != null) clearTimeout(sub.pendingTimer);
  }
  changesetSubs.clear();
  pendingTicks.clear();
}

export function __resetSourceRedlineSubscriptions(): void {
  __resetSubscribeState();
}

/** H-7 — the window for this subscription is picked by its op class. The
 *  watcher's cadence is enforced at the Rust task level (via `pollMs`), not
 *  at the seam, so it has no entry here. */
function windowFor(sub: ChangesetSubscription): number {
  switch (sub.opClass) {
    case "source-read":
      return sub.cadence.sourceReadMs;
    case "diff":
      return sub.cadence.diffMs;
    case "watcher":
      // Shouldn't be used for a subscription (watcher cadence is Rust-side),
      // but fall back to the watcher window if it ever is.
      return sub.cadence.watcherMs;
  }
}

/** Perform ONE changeset fetch; shared across every consumer attached to `sub`. */
function fireChangesetFetch(sub: ChangesetSubscription): void {
  // If a fetch is already in flight, record a re-arm request so the fetch's
  // `.then` scheduler can arm the next cycle (H-12). The seam's `sourceChangeset`
  // call below dedupes through `changesetInflight` so we never issue two
  // parallel fetches.
  if (sub.inflight) {
    sub.rearmPending = true;
    return;
  }
  sub.generation++;
  const myGen = sub.generation;
  const promise = sourceChangeset(sub.repo, sub.baseRef);
  sub.inflight = promise;
  promise
    .then((files) => {
      if (changesetSubs.get(sub.key) !== sub || sub.generation !== myGen) return;
      sub.last = files;
      sub.version++;
      sub.cold = false;
      sub.inflight = null;
      for (const c of sub.consumers) c.onUpdate(files);
      // H-12 — a tick arrived while this fetch was in flight. Arm the next cycle.
      if (sub.rearmPending) {
        sub.rearmPending = false;
        scheduleChangesetDebounce(sub);
      }
      // A watcher tick landed while cold → arm the next cycle now.
      if (sub.armedWhileCold) {
        sub.armedWhileCold = false;
        scheduleChangesetDebounce(sub);
      }
    })
    .catch((err) => {
      if (changesetSubs.get(sub.key) !== sub || sub.generation !== myGen) return;
      sub.inflight = null;
      // H-6 fix — do NOT set `cold = false` on failure. A late joiner that
      // arrives after a cold-fetch failure must still see cold-fetch semantics
      // (immediate retry on the next subscribe or user force), not get stuck
      // because `last` is null and `cold` lies about readiness. The caller
      // (overlay) reacts to `onError` and can offer a retry button; the next
      // watcher tick enters through `scheduleChangesetDebounce` which checks
      // `cold` and arms `armedWhileCold` for the next cold success.
      sub.rearmPending = false;
      sub.armedWhileCold = false;
      for (const c of sub.consumers) c.onError(err);
    });
}

/** Arm (or re-arm) the scheduled debounce for `sub`. Multiple ticks within one
 *  window collapse — the timer is not reset. */
function scheduleChangesetDebounce(sub: ChangesetSubscription): void {
  if (sub.cold) {
    // A tick during cold-fetch arms the next cycle, not a second fetch now.
    sub.armedWhileCold = true;
    return;
  }
  if (sub.inflight) {
    // A fetch is in flight — mark for re-arm on completion (H-12).
    sub.rearmPending = true;
    return;
  }
  if (sub.pendingTimer != null) return; // leading-edge: tick collapses
  const windowMs = windowFor(sub);
  sub.pendingTimer = setTimeout(() => {
    if (changesetSubs.get(sub.key) !== sub) return;
    sub.pendingTimer = null;
    fireChangesetFetch(sub);
  }, windowMs);
}

/** Options for {@link subscribeChangeset}. */
export interface SubscribeChangesetOptions {
  /** H-7 — which `cadence.*Ms` field governs this subscription's debounce
   *  window. Overlay (full-list refresh) is `"diff"` (default); a future
   *  per-file remote read subscription uses `"source-read"`. */
  opClass?: Extract<OpClass, "source-read" | "diff">;
}

/**
 * Attach a debounced subscription to the `(repo, baseRef)` changeset. Returns
 * an unsubscribe function. The first consumer on a key fires an IMMEDIATE
 * cold fetch; subsequent consumers get the last fetched value synchronously
 * via `onUpdate` and then share the debounce window.
 *
 * Call `tickChangeset(repo, baseRef)` when the watcher reports a change — it
 * schedules a debounced fetch (coalesced across concurrent ticks).
 * Call `forceChangeset(repo, baseRef)` to bypass the debounce (user override).
 */
export function subscribeChangeset(
  repo: SourceRepo,
  baseRef: string | undefined,
  cadence: CadencePreset,
  onUpdate: (files: readonly PublishFile[]) => void,
  onError: (err: unknown) => void,
  options?: SubscribeChangesetOptions,
): () => void {
  const key = joinKey([repoKey(repo), baseRef]);
  const consumer: ChangesetConsumer = { onUpdate, onError };
  const opClass: OpClass = options?.opClass ?? "diff";
  let sub = changesetSubs.get(key);
  if (!sub) {
    sub = {
      key,
      repo,
      baseRef,
      opClass,
      cadence,
      consumers: new Set(),
      last: null,
      version: 0,
      inflight: null,
      rearmPending: false,
      pendingTimer: null,
      cold: true,
      armedWhileCold: false,
      generation: 0,
    };
    changesetSubs.set(key, sub);
    sub.consumers.add(consumer);
    // M-11 — a watcher tick that arrived before this subscribe is remembered
    // for a short window. Honor it by arming the next cycle after cold-fetch
    // (via armedWhileCold), so a reconnect race (tick first, overlay opens
    // after) never leaves the overlay stale on its cold-fetch result.
    const bareTickKey = `${repoKey(repo)}\x1f`;
    if (consumePendingTick(key) || consumePendingTick(bareTickKey)) {
      sub.armedWhileCold = true;
    }
    // First consumer → cold fetch is IMMEDIATE (AD-2).
    fireChangesetFetch(sub);
  } else {
    // H-10 fix — if the newest consumer's cadence is TIGHTER than what's
    // armed on the current timer, cancel + reschedule so the user override
    // takes effect this cycle instead of the next. (A looser cadence waits
    // for the next cycle — no benefit to lengthening a window already short.)
    const prevWindow = windowFor(sub);
    sub.cadence = cadence;
    sub.opClass = opClass;
    const newWindow = windowFor(sub);
    if (sub.pendingTimer != null && newWindow < prevWindow) {
      clearTimeout(sub.pendingTimer);
      sub.pendingTimer = null;
      scheduleChangesetDebounce(sub);
    }
    sub.consumers.add(consumer);
    // Late joiner on a settled sub: seed from `last` so the overlay paints now.
    if (sub.last && !sub.cold) {
      const seed = sub.last;
      queueMicrotask(() => {
        if (sub?.consumers.has(consumer)) consumer.onUpdate(seed);
      });
    }
  }
  return () => {
    if (!sub) return;
    sub.consumers.delete(consumer);
    if (sub.consumers.size === 0) {
      if (sub.pendingTimer != null) clearTimeout(sub.pendingTimer);
      changesetSubs.delete(sub.key);
    }
  };
}

/**
 * H-5 — release every consumer on `(repo, baseRef)` and delete the entry.
 * Called on an `Unauthorized` error so a revoked workspace authority doesn't
 * continue to poll/debounce under a dead key. Each consumer sees `onError`
 * with the error that triggered the release so the toast chain still runs.
 */
export function releaseChangesetSubscriptions(
  repo: SourceRepo,
  baseRef: string | undefined,
  err: unknown,
): void {
  const key = joinKey([repoKey(repo), baseRef]);
  const sub = changesetSubs.get(key);
  if (!sub) return;
  if (sub.pendingTimer != null) clearTimeout(sub.pendingTimer);
  const consumers = Array.from(sub.consumers);
  changesetSubs.delete(key);
  for (const c of consumers) c.onError(err);
}

/** M-11 fix — tick arrivals before any subscribe are remembered for a short
 *  window so a subscribe-right-after-reconnect reads them as "already dirty"
 *  and promotes `cold` into an immediate cold-fetch (which it would do anyway)
 *  OR, if a cold-fetch already settled, immediately re-schedules a debounce. */
const pendingTicks = new Map<string, number>(); // key → tick timestamp (ms)
const PENDING_TICK_WINDOW_MS = 15_000;

function consumePendingTick(key: string): boolean {
  const ts = pendingTicks.get(key);
  if (ts == null) return false;
  pendingTicks.delete(key);
  return Date.now() - ts <= PENDING_TICK_WINDOW_MS;
}

/**
 * Report that a change was detected for `(repo, baseRef)` — schedule a
 * debounced changeset fetch (coalesced). Called by the watcher hook on each
 * emitted file-change event.
 */
export function tickChangeset(repo: SourceRepo, baseRef: string | undefined): void {
  const key = joinKey([repoKey(repo), baseRef]);
  const sub = changesetSubs.get(key);
  if (!sub) {
    pendingTicks.set(key, Date.now());
    return;
  }
  scheduleChangesetDebounce(sub);
}

/**
 * Tick every subscription whose `repo` matches `(machineId, handle)` or whose
 * local repo matches `cwd` — the watcher only knows the repo identity, not
 * every baseRef in use (two overlays may differ), so it fan-outs across them.
 * Each matching subscription schedules its own debounced fetch.
 *
 * L-1 note — `joinKey([repoKey, baseRef])` with `baseRef === undefined`
 * produces `prefix + "\x1f"` (one trailing separator), never the bare
 * `prefix`. The former shape covers every live subscription.
 */
export function tickChangesetsForRepo(repo: SourceRepo): void {
  const prefix = repoKey(repo) + "\x1f";
  let anyMatched = false;
  for (const sub of changesetSubs.values()) {
    if (sub.key.startsWith(prefix)) {
      scheduleChangesetDebounce(sub);
      anyMatched = true;
    }
  }
  // No subs → remember a reconnect-race tick under the bare prefix so a
  // late-subscribe for this repo can detect "there was a tick while I was
  // absent" and arm the next cycle after cold-fetch.
  if (!anyMatched) {
    pendingTicks.set(prefix, Date.now());
  }
}

/**
 * Force an immediate changeset fetch (user override — manual refresh). Clears
 * any pending scheduled fetch so the result reflects right-now. Returns the
 * Promise so the caller can show a saving spinner / error toast.
 */
export function forceChangeset(
  repo: SourceRepo,
  baseRef: string | undefined,
): Promise<readonly PublishFile[]> {
  const key = joinKey([repoKey(repo), baseRef]);
  const sub = changesetSubs.get(key);
  if (!sub) {
    // No subscription (unusual but defensible — a caller forcing without a
    // subscribe). Fall back to a plain seam fetch.
    return sourceChangeset(repo, baseRef);
  }
  if (sub.pendingTimer != null) {
    clearTimeout(sub.pendingTimer);
    sub.pendingTimer = null;
  }
  if (sub.inflight) return sub.inflight;
  fireChangesetFetch(sub);
  return sub.inflight ?? sourceChangeset(repo, baseRef);
}

/** Introspection for tests. */
export function __inspectChangesetSubscription(
  repo: SourceRepo,
  baseRef: string | undefined,
): {
  consumerCount: number;
  cold: boolean;
  hasPendingTimer: boolean;
  version: number;
} | null {
  const key = joinKey([repoKey(repo), baseRef]);
  const sub = changesetSubs.get(key);
  if (!sub) return null;
  return {
    consumerCount: sub.consumers.size,
    cold: sub.cold,
    hasPendingTimer: sub.pendingTimer != null,
    version: sub.version,
  };
}

/**
 * Fetch the flat change-set of a source worktree: the files that differ between
 * the worktree's merge-base baseline and its working tree (committed +
 * uncommitted + untracked). The caller supplies the worktree path (WD-2 passes
 * `ws.cwd`).
 *
 * `baseRef` (RD-7) overrides the fork-parent branch for a STACKED worktree — a
 * story branched off a dependency's branch passes that branch so the diff shows
 * only its own work, not the dependency's. Omit it to merge-base against
 * `origin/main` (the common, fork-from-main case).
 */
export function sourceWorktreeChangeset(
  worktreePath: string,
  baseRef?: string,
): Promise<PublishFile[]> {
  const key = joinKey([worktreePath, baseRef]);
  return dedupe(changesetInflight, key, () => invokeSourceWorktreeChangeset(worktreePath, baseRef));
}

/**
 * Fetch the before/after redline content for one changed file in a source
 * worktree, computed against the same merge-base baseline as
 * {@link sourceWorktreeChangeset}. `oldPath` (worktree-relative) is set only for
 * a rename, so "before" reads from the pre-rename baseline path (a moved file's
 * content lives at the old path) instead of reading as all-added. `baseRef`
 * mirrors the change-set's fork-parent override so the list and the redline
 * always agree on the baseline.
 */
export function sourceFileRedline(
  worktreePath: string,
  filePath: string,
  oldPath?: string,
  baseRef?: string,
): Promise<FileRedline> {
  const key = joinKey([worktreePath, filePath, oldPath, baseRef]);
  return dedupe(redlineInflight, key, () =>
    invokeSourceFileRedline(worktreePath, filePath, oldPath, baseRef),
  );
}

/**
 * DESIGN-remote-source.md §3.2 — repo-agnostic change-set entry point. Local
 * callers continue to use {@link sourceWorktreeChangeset}; remote callers (and
 * callers that already hold a {@link SourceRepo}) route through here. Both
 * share the single `changesetInflight` cache, so two tabs with source open on
 * the same (local cwd / remote (machineId, handle)) worktree share ONE fetch.
 */
export function sourceChangeset(repo: SourceRepo, baseRef?: string): Promise<PublishFile[]> {
  if (repo.kind === "local") return sourceWorktreeChangeset(repo.cwd, baseRef);
  // The remote key fragment and the local `cwd` fragment live in the same cache
  // Map without collision: a cwd is an absolute path (starts with `/`), a
  // remote key starts with the machineId (a UUID-shaped `m-...` prefix). The
  // 0x1F separator in `repoKey` keeps `machineId|handle` and `cwd` disjoint.
  const key = joinKey([repoKey(repo), baseRef]);
  return dedupe(changesetInflight, key, () =>
    invokeSourceRemoteChangeset(repo.machineId, repo.handle, baseRef),
  );
}

/**
 * DESIGN-remote-source.md §3.2 — repo-agnostic redline entry point.
 */
export function sourceRedline(
  repo: SourceRepo,
  filePath: string,
  oldPath?: string,
  baseRef?: string,
): Promise<FileRedline> {
  if (repo.kind === "local") return sourceFileRedline(repo.cwd, filePath, oldPath, baseRef);
  const key = joinKey([repoKey(repo), filePath, oldPath, baseRef]);
  return dedupe(redlineInflight, key, () =>
    invokeSourceRemoteFileRedline(repo.machineId, repo.handle, filePath, oldPath, baseRef),
  );
}

/**
 * SLW-WD-8 — fetch a page of the source worktree's all-refs commit graph: a
 * newest-first window of commits (from every local + remote branch ref) with each
 * commit's `parents` (edges) + ref decorations (branch tips + the synthetic
 * `HEAD`). The graph carries NO per-commit diff — WD-9 fetches that lazily via
 * {@link sourceCommitChangeset} when a node opens. `cursor` (a sha) resumes AFTER
 * it for load-older paging; the ref set must be treated as FIXED within one page
 * sequence, so WD-9 restarts from the tip when the change nonce bumps.
 */
export function sourceCommitGraph(
  worktreePath: string,
  limit: number,
  cursor?: string,
): Promise<SourceCommitGraphEntry[]> {
  return invokeSourceCommitGraph(worktreePath, limit, cursor);
}

/**
 * SLW-WD-8 — fetch the FLAT change-set of one commit in a source worktree (its
 * tree vs its first-parent tree; a root commit vs the empty tree). Same
 * `PublishFile` shape as {@link sourceWorktreeChangeset}, so WD-9 renders the file
 * list with the existing components.
 */
export function sourceCommitChangeset(worktreePath: string, sha: string): Promise<PublishFile[]> {
  const key = joinKey([worktreePath, sha]);
  return dedupe(commitChangesetInflight, key, () => invokeSourceCommitChangeset(worktreePath, sha));
}

/**
 * SLW-WD-8 — fetch the before/after redline for one file in a specific commit
 * (first-parent blob vs commit blob). Byte-identical `FileRedline` shape to
 * {@link sourceFileRedline}, so the same `SourceDiffPane` renders it. `oldPath`
 * (worktree-relative) is set only for a rename, so "before" reads the pre-rename
 * path in the first-parent tree instead of reading as all-added.
 */
export function sourceCommitFileRedline(
  worktreePath: string,
  sha: string,
  filePath: string,
  oldPath?: string,
): Promise<FileRedline> {
  const key = joinKey([worktreePath, sha, filePath, oldPath]);
  return dedupe(commitRedlineInflight, key, () =>
    invokeSourceCommitFileRedline(worktreePath, sha, filePath, oldPath),
  );
}

/** Revalidates configured attachment and WORKTREE identity on every call. */
export { openDiffRead as readOpenDiff } from "@/tauri/commands";
