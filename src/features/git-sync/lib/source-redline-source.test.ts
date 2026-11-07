// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invokeSourceWorktreeChangesetMock = vi.fn();
const invokeSourceFileRedlineMock = vi.fn();
const invokeSourceCommitChangesetMock = vi.fn();
const invokeSourceCommitFileRedlineMock = vi.fn();
const invokeSourceCommitGraphMock = vi.fn();
const invokeSourceRemoteChangesetMock = vi.fn();
const invokeSourceRemoteFileRedlineMock = vi.fn();

vi.mock("@/tauri/commands", () => ({
  sourceWorktreeChangeset: (...args: unknown[]) => invokeSourceWorktreeChangesetMock(...args),
  sourceFileRedline: (...args: unknown[]) => invokeSourceFileRedlineMock(...args),
  sourceCommitChangeset: (...args: unknown[]) => invokeSourceCommitChangesetMock(...args),
  sourceCommitFileRedline: (...args: unknown[]) => invokeSourceCommitFileRedlineMock(...args),
  sourceCommitGraph: (...args: unknown[]) => invokeSourceCommitGraphMock(...args),
  sourceRemoteChangeset: (...args: unknown[]) => invokeSourceRemoteChangesetMock(...args),
  sourceRemoteFileRedline: (...args: unknown[]) => invokeSourceRemoteFileRedlineMock(...args),
}));

import {
  __resetSourceRedlineCache,
  sourceChangeset,
  sourceCommitChangeset,
  sourceCommitFileRedline,
  sourceFileRedline,
  sourceRedline,
  sourceWorktreeChangeset,
  type SourceRepo,
} from "./source-redline-source";

// Per DESIGN-per-tab-primitives.md AD-2 / §5.4 — the data-source seam shares one
// in-flight fetch for every identical `(cwd, baseRef, filePath?, oldPath?, sha?)`
// request, so N concurrent UI subscribers on the same worktree cost one call.
describe("source-redline-source — in-flight dedupe", () => {
  beforeEach(() => {
    __resetSourceRedlineCache();
    vi.clearAllMocks();
  });

  it("shares one in-flight changeset fetch across concurrent same-key callers", async () => {
    let resolve!: (v: unknown) => void;
    const inflight = new Promise((r) => (resolve = r));
    invokeSourceWorktreeChangesetMock.mockReturnValue(inflight);

    const a = sourceWorktreeChangeset("/wt", "origin/main");
    const b = sourceWorktreeChangeset("/wt", "origin/main");

    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1);
    resolve([{ path: "a.ts" }]);
    await expect(a).resolves.toEqual([{ path: "a.ts" }]);
    await expect(b).resolves.toEqual([{ path: "a.ts" }]);
  });

  it("different (cwd, baseRef) identities do NOT share — two independent fetches", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([]);
    await Promise.all([
      sourceWorktreeChangeset("/wtA", "origin/main"),
      sourceWorktreeChangeset("/wtB", "origin/main"),
    ]);
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(2);
  });

  it("different baseRef with same cwd does NOT share (correct — different baselines)", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([]);
    await Promise.all([
      sourceWorktreeChangeset("/wt", "origin/main"),
      sourceWorktreeChangeset("/wt", "feat/x"),
    ]);
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(2);
  });

  it("entries evict on settle so a later caller runs fresh (no stale cache)", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([]);
    await sourceWorktreeChangeset("/wt", undefined);
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1);
    await sourceWorktreeChangeset("/wt", undefined);
    // A second call after the first resolved re-invokes the backend — the
    // dedupe is in-flight only; nonce bumps + user retries each get fresh data.
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(2);
  });

  it("shares one in-flight redline fetch across concurrent same-key callers", async () => {
    let resolve!: (v: unknown) => void;
    invokeSourceFileRedlineMock.mockReturnValue(new Promise((r) => (resolve = r)));

    const a = sourceFileRedline("/wt", "src/a.ts", undefined, "origin/main");
    const b = sourceFileRedline("/wt", "src/a.ts", undefined, "origin/main");

    expect(invokeSourceFileRedlineMock).toHaveBeenCalledTimes(1);
    resolve({ status: "modified", binary: false, before: "x", after: "y" });
    await expect(a).resolves.toMatchObject({ status: "modified" });
    await expect(b).resolves.toMatchObject({ status: "modified" });
  });

  it("different file paths on the same cwd do NOT share", async () => {
    invokeSourceFileRedlineMock.mockResolvedValue({ status: "modified" });
    await Promise.all([
      sourceFileRedline("/wt", "src/a.ts", undefined, "origin/main"),
      sourceFileRedline("/wt", "src/b.ts", undefined, "origin/main"),
    ]);
    expect(invokeSourceFileRedlineMock).toHaveBeenCalledTimes(2);
  });

  it("rename `oldPath` distinguishes the key — a non-rename and a rename share nothing", async () => {
    invokeSourceFileRedlineMock.mockResolvedValue({ status: "renamed" });
    await Promise.all([
      sourceFileRedline("/wt", "src/a.ts", undefined, "origin/main"),
      sourceFileRedline("/wt", "src/a.ts", "src/old-a.ts", "origin/main"),
    ]);
    expect(invokeSourceFileRedlineMock).toHaveBeenCalledTimes(2);
  });

  it("evicts commit-redline entry on reject so a later caller runs fresh", async () => {
    invokeSourceCommitFileRedlineMock.mockRejectedValueOnce(new Error("flake"));
    await expect(sourceCommitFileRedline("/wt", "abc", "src/a.ts")).rejects.toThrow("flake");
    invokeSourceCommitFileRedlineMock.mockResolvedValueOnce({ status: "added" });
    await expect(sourceCommitFileRedline("/wt", "abc", "src/a.ts")).resolves.toMatchObject({
      status: "added",
    });
    expect(invokeSourceCommitFileRedlineMock).toHaveBeenCalledTimes(2);
  });

  it("commit changesets dedupe by (cwd, sha)", async () => {
    let resolve!: (v: unknown) => void;
    invokeSourceCommitChangesetMock.mockReturnValue(new Promise((r) => (resolve = r)));

    const a = sourceCommitChangeset("/wt", "abc");
    const b = sourceCommitChangeset("/wt", "abc");
    const c = sourceCommitChangeset("/wt", "def"); // different sha

    expect(invokeSourceCommitChangesetMock).toHaveBeenCalledTimes(2);
    resolve([]);
    invokeSourceCommitChangesetMock.mock.results.at(-1)!.value = Promise.resolve([]);
    await Promise.all([a, b, c]);
  });
});

// DESIGN-remote-source.md §3.2 — the remote branch of the seam. The `SourceRepo`
// shape (`{kind:"local", cwd}` vs `{kind:"remote", machineId, handle}`) routes
// through the SAME dedupe cache so two tabs on the same remote worktree share
// one fetch, exactly as two local tabs do.
describe("source-redline-source — remote branch (SourceRepo keying)", () => {
  beforeEach(() => {
    __resetSourceRedlineCache();
    vi.clearAllMocks();
  });

  const local: SourceRepo = { kind: "local", cwd: "/wt" };
  const remoteA: SourceRepo = { kind: "remote", machineId: "m-01", handle: "remote-workspace-abc" };
  const remoteB: SourceRepo = { kind: "remote", machineId: "m-01", handle: "remote-workspace-def" };

  it("remote changeset routes to invokeSourceRemoteChangeset", async () => {
    invokeSourceRemoteChangesetMock.mockResolvedValue([{ path: "r.ts" }]);
    const result = await sourceChangeset(remoteA, "origin/main");
    expect(invokeSourceRemoteChangesetMock).toHaveBeenCalledWith(
      "m-01",
      "remote-workspace-abc",
      "origin/main",
    );
    expect(result).toEqual([{ path: "r.ts" }]);
  });

  it("local SourceRepo still routes to the LOCAL invoke (no remote hit)", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([{ path: "l.ts" }]);
    const result = await sourceChangeset(local, "origin/main");
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledWith("/wt", "origin/main");
    expect(invokeSourceRemoteChangesetMock).not.toHaveBeenCalled();
    expect(result).toEqual([{ path: "l.ts" }]);
  });

  it("two concurrent remote callers on the same (machineId, handle) share ONE fetch", async () => {
    let resolve!: (v: unknown) => void;
    invokeSourceRemoteChangesetMock.mockReturnValue(new Promise((r) => (resolve = r)));

    const a = sourceChangeset(remoteA, "origin/main");
    const b = sourceChangeset(remoteA, "origin/main");

    expect(invokeSourceRemoteChangesetMock).toHaveBeenCalledTimes(1);
    resolve([{ path: "x.ts" }]);
    await expect(a).resolves.toEqual([{ path: "x.ts" }]);
    await expect(b).resolves.toEqual([{ path: "x.ts" }]);
  });

  it("realistic local cwd + remote key don't collide (production invariant)", async () => {
    // In production a local `cwd` is an absolute filesystem path (starts with
    // "/"), and a remote key is `machineId<0x1F>handle` where machineId is a
    // UUID-shaped `m-...` prefix. The two strings cannot collide. This test
    // runs a representative pair concurrently and proves each hits ITS own
    // backend path with ITS own cache entry.
    invokeSourceWorktreeChangesetMock.mockResolvedValue([{ path: "l" }]);
    invokeSourceRemoteChangesetMock.mockResolvedValue([{ path: "r" }]);
    const [l, r] = await Promise.all([
      sourceChangeset({ kind: "local", cwd: "/Users/dev/wt" }, undefined),
      sourceChangeset(remoteA, undefined),
    ]);
    expect(l).toEqual([{ path: "l" }]);
    expect(r).toEqual([{ path: "r" }]);
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1);
    expect(invokeSourceRemoteChangesetMock).toHaveBeenCalledTimes(1);
  });

  it("different remote handles do NOT share", async () => {
    invokeSourceRemoteChangesetMock.mockResolvedValue([]);
    await Promise.all([
      sourceChangeset(remoteA, "origin/main"),
      sourceChangeset(remoteB, "origin/main"),
    ]);
    expect(invokeSourceRemoteChangesetMock).toHaveBeenCalledTimes(2);
  });

  it("remote redline shares one in-flight fetch across same-key callers", async () => {
    let resolve!: (v: unknown) => void;
    invokeSourceRemoteFileRedlineMock.mockReturnValue(new Promise((r) => (resolve = r)));

    const a = sourceRedline(remoteA, "src/a.ts", undefined, "origin/main");
    const b = sourceRedline(remoteA, "src/a.ts", undefined, "origin/main");

    expect(invokeSourceRemoteFileRedlineMock).toHaveBeenCalledTimes(1);
    resolve({ status: "modified" });
    await expect(a).resolves.toMatchObject({ status: "modified" });
    await expect(b).resolves.toMatchObject({ status: "modified" });
  });

  it("remote-redline rename `oldPath` distinguishes the key", async () => {
    invokeSourceRemoteFileRedlineMock.mockResolvedValue({ status: "renamed" });
    await Promise.all([
      sourceRedline(remoteA, "src/a.ts", undefined, "origin/main"),
      sourceRedline(remoteA, "src/a.ts", "src/old-a.ts", "origin/main"),
    ]);
    expect(invokeSourceRemoteFileRedlineMock).toHaveBeenCalledTimes(2);
  });
});

// DESIGN-remote-phase-2.md §1 — the debounce + coalesce layer at the seam.
// Cold-fetch is immediate; subsequent watcher ticks collapse into one scheduled
// fetch per window; cancellation on last unsubscribe; user-force bypasses.
describe("source-redline-source — debounce + coalesce", () => {
  const localRepo: SourceRepo = { kind: "local", cwd: "/wt" };
  const cadence = { watcherMs: 100, sourceReadMs: 100, diffMs: 100 };

  beforeEach(async () => {
    __resetSourceRedlineCache();
    vi.clearAllMocks();
    vi.useFakeTimers();
    const mod = await import("./source-redline-source");
    mod.__resetSourceRedlineSubscriptions();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("cold-fetch is IMMEDIATE on first subscribe (no debounce)", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([{ path: "a.ts" }]);
    const updates: unknown[] = [];
    const mod = await import("./source-redline-source");
    const unsubscribe = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      (files) => updates.push(files),
      () => {},
    );
    // No timer advance — the first fetch should fire now.
    await vi.waitFor(() => expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(updates).toHaveLength(1));
    unsubscribe();
  });

  it("multiple watcher ticks within a window coalesce to ONE scheduled fetch", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([]);
    const mod = await import("./source-redline-source");
    const unsubscribe = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      () => {},
      () => {},
    );
    // Let cold-fetch settle.
    await vi.waitFor(() => expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1));

    // Burst of ticks — should collapse to ONE scheduled fetch after window.
    mod.tickChangeset(localRepo, "origin/main");
    mod.tickChangeset(localRepo, "origin/main");
    mod.tickChangeset(localRepo, "origin/main");
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1); // not fired yet
    await vi.advanceTimersByTimeAsync(cadence.diffMs + 10);
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(2);

    unsubscribe();
  });

  it("force bypasses the debounce window and cancels any pending timer", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([]);
    const mod = await import("./source-redline-source");
    const updates: unknown[] = [];
    const unsubscribe = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      (files) => updates.push(files),
      () => {},
    );
    // Wait for the cold-fetch to actually settle (not just be called), so
    // sub.inflight is cleared before force can take.
    await vi.waitFor(() => expect(updates).toHaveLength(1));

    mod.tickChangeset(localRepo, "origin/main");
    // Still armed; now force before the window elapses.
    await mod.forceChangeset(localRepo, "origin/main");
    await vi.waitFor(() => expect(updates).toHaveLength(2));
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(2);

    // No further fire when the (now-canceled) timer would have expired.
    await vi.advanceTimersByTimeAsync(cadence.diffMs + 10);
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(2);
    unsubscribe();
  });

  it("last unsubscribe cancels a pending scheduled fetch (no stray fire)", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([]);
    const mod = await import("./source-redline-source");
    const unsubscribe = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      () => {},
      () => {},
    );
    await vi.waitFor(() => expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1));
    mod.tickChangeset(localRepo, "origin/main"); // schedules
    unsubscribe();
    await vi.advanceTimersByTimeAsync(cadence.diffMs + 10);
    // No second fetch — the cancellation cleared the timer.
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1);
  });

  it("late joiner on a settled sub gets the last value synchronously (micro)", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([{ path: "a.ts" }]);
    const mod = await import("./source-redline-source");
    const firstUpdates: unknown[] = [];
    const unsubA = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      (files) => firstUpdates.push(files),
      () => {},
    );
    await vi.waitFor(() => expect(firstUpdates).toHaveLength(1));

    const secondUpdates: unknown[] = [];
    const unsubB = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      (files) => secondUpdates.push(files),
      () => {},
    );
    // Still only one fetch — late joiner gets a seed, not a new fetch.
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(secondUpdates).toHaveLength(1));
    expect(secondUpdates[0]).toEqual([{ path: "a.ts" }]);

    unsubA();
    unsubB();
  });

  it("tickChangesetsForRepo fans out across baseRefs for one (machineId, handle)", async () => {
    invokeSourceRemoteChangesetMock.mockResolvedValue([]);
    const remote: SourceRepo = { kind: "remote", machineId: "m-1", handle: "h-1" };
    const mod = await import("./source-redline-source");
    const unsubA = mod.subscribeChangeset(
      remote,
      "origin/main",
      cadence,
      () => {},
      () => {},
    );
    const unsubB = mod.subscribeChangeset(
      remote,
      "parent",
      cadence,
      () => {},
      () => {},
    );

    // Two cold fetches (one per baseRef key).
    await vi.waitFor(() => expect(invokeSourceRemoteChangesetMock).toHaveBeenCalledTimes(2));

    mod.tickChangesetsForRepo(remote); // one wire tick → schedule both
    await vi.advanceTimersByTimeAsync(cadence.diffMs + 10);
    expect(invokeSourceRemoteChangesetMock).toHaveBeenCalledTimes(4);

    unsubA();
    unsubB();
  });

  // DESIGN-remote-phase-2.md H-7 / AD-4 — opClass picks which cadence window
  // governs the subscription's debounce schedule.
  it("opClass:source-read honors sourceReadMs instead of diffMs", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([]);
    const customCadence = { watcherMs: 100, sourceReadMs: 50, diffMs: 500 };
    const mod = await import("./source-redline-source");
    const unsubscribe = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      customCadence,
      () => {},
      () => {},
      { opClass: "source-read" },
    );
    await vi.waitFor(() => expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1));
    mod.tickChangeset(localRepo, "origin/main");
    // After `sourceReadMs` (50ms), fire — NOT `diffMs` (500ms).
    await vi.advanceTimersByTimeAsync(60);
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(2);
    unsubscribe();
  });

  // DESIGN-remote-phase-2.md H-6 — a cold-fetch failure must NOT set `cold =
  // false`; a late joiner (or the next tick) must still get cold-fetch
  // semantics (immediate retry on next subscribe).
  it("cold-fetch failure keeps cold=true so a next subscribe refires immediately", async () => {
    let callCount = 0;
    invokeSourceWorktreeChangesetMock.mockImplementation(() => {
      callCount++;
      return callCount === 1 ? Promise.reject(new Error("transient")) : Promise.resolve([]);
    });
    const mod = await import("./source-redline-source");
    const errors: unknown[] = [];
    const firstSub = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      () => {},
      (e) => errors.push(e),
    );
    await vi.waitFor(() => expect(errors).toHaveLength(1));
    firstSub();
    // Next subscribe → cold-fetch IMMEDIATE again (not stuck "warm" with null
    // `last` and no re-fetch path).
    const updates: unknown[] = [];
    const secondSub = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      (files) => updates.push(files),
      () => {},
    );
    await vi.waitFor(() => expect(updates).toHaveLength(1));
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(2);
    secondSub();
  });

  // DESIGN-remote-phase-2.md H-12 — a timer that fires while another fetch is
  // in flight marks a rearmPending; the completing fetch's .then re-schedules.
  it("tick during an in-flight fetch rearms the next cycle", async () => {
    const resolvers: Array<(v: unknown) => void> = [];
    invokeSourceWorktreeChangesetMock.mockImplementation(
      () => new Promise((r) => resolvers.push(r)),
    );
    const mod = await import("./source-redline-source");
    const updates: unknown[] = [];
    const unsubscribe = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      (files) => updates.push(files),
      () => {},
    );
    // Resolve cold-fetch so `cold = false`.
    expect(resolvers).toHaveLength(1);
    const resolveFirst = resolvers[0];
    resolveFirst([]);
    await vi.waitFor(() => expect(updates).toHaveLength(1));

    // Tick → arms the timer, which fires after cadence.diffMs. Advance to fire.
    mod.tickChangeset(localRepo, "origin/main");
    await vi.advanceTimersByTimeAsync(cadence.diffMs + 10);
    // Second fetch is now IN FLIGHT (its resolver isn't drained).
    expect(resolvers).toHaveLength(2);

    // A tick arrives during this in-flight → rearmPending set.
    mod.tickChangeset(localRepo, "origin/main");

    // Resolve the second fetch — the .then should see rearmPending and
    // schedule a third cycle.
    resolvers[1](["x"]);
    await vi.waitFor(() => expect(updates).toHaveLength(2));
    await vi.advanceTimersByTimeAsync(cadence.diffMs + 10);
    expect(resolvers).toHaveLength(3);
    resolvers[2]([]);
    unsubscribe();
  });

  // DESIGN-remote-phase-2.md H-5 — `releaseChangesetSubscriptions` tears down
  // every consumer with the final error on the key.
  it("releaseChangesetSubscriptions fires onError on every consumer and clears the key", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([]);
    const mod = await import("./source-redline-source");
    const errorsA: unknown[] = [];
    const errorsB: unknown[] = [];
    const unsubA = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      () => {},
      (e) => errorsA.push(e),
    );
    const unsubB = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      () => {},
      (e) => errorsB.push(e),
    );
    await vi.waitFor(() => expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1));
    const fatal = { kind: "unauthorized" };
    mod.releaseChangesetSubscriptions(localRepo, "origin/main", fatal);
    expect(errorsA).toEqual([fatal]);
    expect(errorsB).toEqual([fatal]);
    // Entry gone — a next tick finds no sub.
    mod.tickChangeset(localRepo, "origin/main");
    await vi.advanceTimersByTimeAsync(cadence.diffMs + 10);
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(1);
    // Cleanup (unsub becomes a no-op post-release, but still safe to call).
    unsubA();
    unsubB();
  });

  // DESIGN-remote-phase-2.md M-11 — a tick arriving BEFORE subscribe is
  // remembered for a short window; the next subscribe arms the debounce.
  it("tick before subscribe arms the next cycle after cold-fetch (reconnect race)", async () => {
    invokeSourceWorktreeChangesetMock.mockResolvedValue([]);
    const mod = await import("./source-redline-source");
    // Tick ARRIVES first (watcher ran before overlay opened).
    mod.tickChangeset(localRepo, "origin/main");
    const updates: unknown[] = [];
    const unsubscribe = mod.subscribeChangeset(
      localRepo,
      "origin/main",
      cadence,
      (files) => updates.push(files),
      () => {},
    );
    // Cold-fetch fires immediately.
    await vi.waitFor(() => expect(updates).toHaveLength(1));
    // Because a tick was pending, after cold-fetch the next cycle arms —
    // advance past the window and expect a second fire.
    await vi.advanceTimersByTimeAsync(cadence.diffMs + 10);
    expect(invokeSourceWorktreeChangesetMock).toHaveBeenCalledTimes(2);
    unsubscribe();
  });
});
