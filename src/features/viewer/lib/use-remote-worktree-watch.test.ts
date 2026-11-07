// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

const startMock = vi.fn();
const stopMock = vi.fn();
const trackedCountMock = vi.fn();
const listenMock = vi.fn();

vi.mock("@/tauri/commands", () => ({
  sourceRemoteWatchStart: (...args: unknown[]) => startMock(...args),
  sourceRemoteWatchStop: (...args: unknown[]) => stopMock(...args),
  sourceRemoteTrackedCount: (...args: unknown[]) => trackedCountMock(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

import {
  useRemoteWorktreeWatch,
  __resetRemoteWorktreeWatch,
  __remoteWorktreeWatchRefCount,
} from "./use-remote-worktree-watch";

/**
 * DESIGN-remote-source.md §3.3 / AD-2 — the ref-counted watcher lifecycle. Two
 * consumers on the same (machineId, handle) share ONE poll task on the host;
 * out-of-order attach/detach cannot strand the subscriber watcher-dead.
 */
describe("useRemoteWorktreeWatch — ref-counted lifecycle", () => {
  beforeEach(() => {
    __resetRemoteWorktreeWatch();
    vi.clearAllMocks();
    stopMock.mockResolvedValue(undefined);
    // The watcher now listens for per-handle events; the test's mocked listen
    // returns a Promise so attach can proceed without a real Tauri bridge.
    listenMock.mockResolvedValue(() => {});
    // The tracked-count probe fires on attach; a never-resolving Promise keeps
    // the probe inert and prevents setRemoteTrackedCount side effects from
    // leaking into other suites (the cadence module has its own tests).
    trackedCountMock.mockReturnValue(new Promise(() => {}));
  });

  it("first consumer on a (machineId, handle) attaches once with the passed pollMs; count goes to 1", () => {
    startMock.mockReturnValue(new Promise(() => {})); // never resolves
    const { unmount } = renderHook(() => useRemoteWorktreeWatch("m-1", "h-A", true, 5000));
    // DESIGN-remote-phase-2.md B-2 — pollMs is threaded through so the Rust
    // task uses the §0 remote cadence instead of the 2 s default.
    expect(startMock).toHaveBeenCalledWith("m-1", "h-A", 5000);
    expect(__remoteWorktreeWatchRefCount("m-1", "h-A")).toBe(1);
    unmount();
  });

  it("omitted pollMs falls back to the remoteSmall preset (B-2 regression gate)", () => {
    startMock.mockReturnValue(new Promise(() => {}));
    renderHook(() => useRemoteWorktreeWatch("m-9", "h-9", true));
    // Default = CADENCE_PRESETS.remoteSmall.watcherMs = 5000.
    expect(startMock).toHaveBeenCalledWith("m-9", "h-9", 5000);
  });

  it("second consumer on the same key does NOT re-attach; count goes to 2", () => {
    startMock.mockReturnValue(new Promise(() => {}));
    renderHook(() => useRemoteWorktreeWatch("m-1", "h-A", true, 5000));
    renderHook(() => useRemoteWorktreeWatch("m-1", "h-A", true, 5000));
    expect(startMock).toHaveBeenCalledTimes(1);
    expect(__remoteWorktreeWatchRefCount("m-1", "h-A")).toBe(2);
  });

  it("different keys get independent attaches", () => {
    startMock.mockReturnValue(new Promise(() => {}));
    renderHook(() => useRemoteWorktreeWatch("m-1", "h-A", true, 5000));
    renderHook(() => useRemoteWorktreeWatch("m-1", "h-B", true, 5000));
    renderHook(() => useRemoteWorktreeWatch("m-2", "h-A", true, 5000));
    expect(startMock).toHaveBeenCalledTimes(3);
    expect(__remoteWorktreeWatchRefCount("m-1", "h-A")).toBe(1);
    expect(__remoteWorktreeWatchRefCount("m-1", "h-B")).toBe(1);
    expect(__remoteWorktreeWatchRefCount("m-2", "h-A")).toBe(1);
  });

  it("last consumer release detaches via the attach token", async () => {
    startMock.mockResolvedValue(42);
    const { unmount } = renderHook(() => useRemoteWorktreeWatch("m-1", "h-A", true, 5000));
    // Let the attach promise resolve so the token is stored.
    await Promise.resolve();
    await Promise.resolve();
    unmount();
    expect(stopMock).toHaveBeenCalledWith(42);
    expect(__remoteWorktreeWatchRefCount("m-1", "h-A")).toBe(0);
  });

  it("released-before-attach-resolves still cleans up (ordering safe)", async () => {
    let resolveAttach!: (token: number) => void;
    startMock.mockReturnValue(new Promise((r) => (resolveAttach = r)));
    const { unmount } = renderHook(() => useRemoteWorktreeWatch("m-1", "h-A", true, 5000));
    // Release while the attach is in flight.
    unmount();
    // Now resolve — the module must detach immediately.
    resolveAttach(99);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
    expect(stopMock).toHaveBeenCalledWith(99);
    expect(__remoteWorktreeWatchRefCount("m-1", "h-A")).toBe(0);
  });

  it("disabled hook is a no-op (no attach, no refcount)", () => {
    startMock.mockReturnValue(new Promise(() => {}));
    renderHook(() => useRemoteWorktreeWatch("m-1", "h-A", false, 5000));
    expect(startMock).not.toHaveBeenCalled();
    expect(__remoteWorktreeWatchRefCount("m-1", "h-A")).toBe(0);
  });

  it("null machineId / handle is a no-op (used for local tabs)", () => {
    startMock.mockReturnValue(new Promise(() => {}));
    renderHook(() => useRemoteWorktreeWatch(null, null, true, 5000));
    renderHook(() => useRemoteWorktreeWatch("m-1", null, true, 5000));
    renderHook(() => useRemoteWorktreeWatch(null, "h-A", true, 5000));
    expect(startMock).not.toHaveBeenCalled();
  });

  it("an attach failure doesn't block the mount or future acquires", async () => {
    startMock.mockRejectedValueOnce(new Error("transport failure"));
    const { unmount } = renderHook(() => useRemoteWorktreeWatch("m-1", "h-A", true, 5000));
    // Give the rejection a tick to log and settle.
    await Promise.resolve();
    await Promise.resolve();
    // A refcount entry stays (so repeated mounts don't spin on the hard fail).
    expect(__remoteWorktreeWatchRefCount("m-1", "h-A")).toBe(1);
    unmount();
    // After unmount, the count is 0 and stop is NOT called (no token to detach).
    expect(stopMock).not.toHaveBeenCalled();
    expect(__remoteWorktreeWatchRefCount("m-1", "h-A")).toBe(0);
  });
});

// DESIGN-remote-phase-2.md B-4 — the nonce hook attaches a listener via
// `ensureEntry`, so a sibling-component mount OR a hook declared BEFORE
// `useRemoteWorktreeWatch` still receives ticks once the watcher attaches.
describe("useRemoteWatchNonce — B-4 ensure-entry defensive attach", () => {
  beforeEach(() => {
    __resetRemoteWorktreeWatch();
    vi.clearAllMocks();
    stopMock.mockResolvedValue(undefined);
    listenMock.mockResolvedValue(() => {});
    trackedCountMock.mockReturnValue(new Promise(() => {}));
  });

  it("nonce hook called BEFORE the watcher still receives subsequent ticks", async () => {
    const { useRemoteWatchNonce, __fireRemoteWatchTickForTests } =
      await import("./use-remote-worktree-watch");
    // Mount nonce FIRST (the broken order the review flagged for B-4).
    const { result: nonceResult } = renderHook(() => useRemoteWatchNonce("m-x", "h-x"));
    expect(nonceResult.current).toBe(0);
    // Now mount the watch. The nonce listener's entry pre-exists via
    // `ensureEntry`, so acquire upgrades the SAME entry — listeners survive.
    startMock.mockReturnValue(new Promise(() => {}));
    renderHook(() => useRemoteWorktreeWatch("m-x", "h-x", true, 5000));
    // Simulate a watcher tick (bypasses the Tauri event bridge).
    act(() => {
      __fireRemoteWatchTickForTests("m-x", "h-x");
    });
    // The nonce listener MUST have been notified.
    expect(nonceResult.current).toBe(1);
  });

  it("a view mounted over a watch that already ticked starts at its nonce (no step)", async () => {
    const { useRemoteWatchNonce, __fireRemoteWatchTickForTests } =
      await import("./use-remote-worktree-watch");
    startMock.mockReturnValue(new Promise(() => {}));
    renderHook(() => useRemoteWorktreeWatch("m-y", "h-y", true, 5000));
    act(() => {
      __fireRemoteWatchTickForTests("m-y", "h-y");
      __fireRemoteWatchTickForTests("m-y", "h-y");
    });
    const seen: number[] = [];
    renderHook(() => {
      const n = useRemoteWatchNonce("m-y", "h-y");
      seen.push(n);
      return n;
    });
    expect(seen.every((n) => n === 2)).toBe(true);
  });

  it("switching a mounted hook to another watch re-bases on that watch's nonce in the same render", async () => {
    const { useRemoteWatchNonce, __fireRemoteWatchTickForTests } =
      await import("./use-remote-worktree-watch");
    startMock.mockReturnValue(new Promise(() => {}));
    renderHook(() => useRemoteWorktreeWatch("m-z", "h-b", true, 5000));
    act(() => {
      for (let i = 0; i < 3; i++) __fireRemoteWatchTickForTests("m-z", "h-b");
    });
    const seen: number[] = [];
    const { rerender } = renderHook(
      ({ h }) => {
        const n = useRemoteWatchNonce("m-z", h);
        seen.push(n);
        return n;
      },
      { initialProps: { h: "h-a" } },
    );
    seen.length = 0;
    rerender({ h: "h-b" });
    expect(seen.length).toBeGreaterThan(0);
    expect(seen.every((n) => n === 3)).toBe(true);
  });

  it("tracked-count probe fires on first attach and feeds setRemoteTrackedCount", async () => {
    const resolveToken = Promise.resolve(7);
    startMock.mockReturnValue(resolveToken);
    const resolveCount = Promise.resolve(42);
    trackedCountMock.mockReturnValue(resolveCount);
    renderHook(() => useRemoteWorktreeWatch("m-probe", "h-probe", true, 5000));
    // Drain microtasks for both the attach promise AND the probe promise.
    await resolveToken;
    await resolveCount;
    expect(trackedCountMock).toHaveBeenCalledWith("m-probe", "h-probe");
  });

  it("pollMs flows through useRepoWorktreeWatch for a remote repo", async () => {
    const { useRepoWorktreeWatch } = await import("./use-repo-worktree-watch");
    const { setRemoteTrackedCount, LARGE_REPO_THRESHOLD, __resetRemoteCadence } =
      await import("./use-remote-cadence");
    __resetRemoteCadence();
    startMock.mockReturnValue(new Promise(() => {}));
    renderHook(() =>
      useRepoWorktreeWatch("ws-1", { kind: "remote", machineId: "m-r", handle: "h-r" }, true),
    );
    // Default (small-repo) preset's watcherMs = 5000.
    expect(startMock).toHaveBeenCalledWith("m-r", "h-r", 5000);
    // After a large-repo probe, the loosened cadence governs the NEXT attach;
    // the current one keeps the window it was started with (documented in
    // the hook — thrash protection).
    setRemoteTrackedCount("m-r", "h-r", LARGE_REPO_THRESHOLD + 1);
  });
});
