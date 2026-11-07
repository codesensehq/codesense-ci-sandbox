// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useSurfaceSyncStore } from "@/features/shell/stores/surface-sync-store";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { trackWorktreeEstablish } from "@/shared/lib/worktree/pending-establish";
import { awaitContextForLaunch, awaitWorktreeContext } from "./await-worktree-context";
const CWD = "/wt/explicit-story";
const TEAM = "local:general";
function seed(overrides: Partial<Workspace> = {}) {
  useViewerStore.setState({
    workspaces: {
      [TEAM]: [{ id: "story", kind: "story", cwd: CWD, status: "ready", ...overrides }],
    },
  });
}
describe("story launch readiness", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    seed();
  });
  afterEach(() => vi.useRealTimers());
  it("ignores legacy sync failure and overlay state on registered setup-complete roots", async () => {
    seed({ needsSyncOverlay: true });
    useSurfaceSyncStore.getState().markStarted(CWD, 1);
    useSurfaceSyncStore.getState().markCompleted(CWD, 1, { ok: false });
    await expect(awaitWorktreeContext(CWD)).resolves.toBe("landed");
  });
  it("waits for registration before launching", async () => {
    let finish!: () => void;
    trackWorktreeEstablish(
      CWD,
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
    );
    const spawn = vi.fn();
    const launch = awaitContextForLaunch(CWD).then(spawn);
    await vi.advanceTimersByTimeAsync(0);
    expect(spawn).not.toHaveBeenCalled();
    finish();
    await launch;
    expect(spawn).toHaveBeenCalledOnce();
  });
  it("waits for repository setup", async () => {
    seed({ backgroundSetup: { status: "queued", label: "Setup" } });
    const spawn = vi.fn();
    const launch = awaitContextForLaunch(CWD).then(spawn);
    await vi.advanceTimersByTimeAsync(0);
    expect(spawn).not.toHaveBeenCalled();
    seed();
    await launch;
    expect(spawn).toHaveBeenCalledOnce();
  });
  it("blocks failed setup", async () => {
    seed({ backgroundSetup: { status: "failed", error: "failed setup" } });
    await expect(awaitContextForLaunch(CWD)).rejects.toThrow("launch blocked");
  });
  it("cancels on close while waiting", async () => {
    seed({ status: "creating" });
    const waiting = awaitWorktreeContext(CWD);
    await vi.advanceTimersByTimeAsync(0);
    useViewerStore.setState({ workspaces: {} });
    await expect(waiting).resolves.toBe("closed");
  });
  it("bounds setup waiting without permitting spawn", async () => {
    seed({ status: "creating" });
    const waiting = awaitWorktreeContext(CWD, 100);
    await vi.advanceTimersByTimeAsync(100);
    await expect(waiting).resolves.toBe("timeout");
  });
  it.each(["failed", "closed", "timeout", "stopped"] as const)(
    "blocks spawn after %s",
    async (outcome) => {
      const spawn = vi.fn();
      await expect(awaitContextForLaunch(CWD, async () => outcome).then(spawn)).rejects.toThrow(
        "launch blocked",
      );
      expect(spawn).not.toHaveBeenCalled();
    },
  );
});
