// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useViewerStore } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

// SLW-WDE-3: the per-workspace external-change signal the App `worktree-file-change`
// listener drives and the editor inset reacts to.
describe("bumpWorktreeChangedPath (SLW-WDE-3)", () => {
  beforeEach(() => {
    useViewerStore.setState({ worktreeChangedPath: {} });
    vi.clearAllMocks();
  });

  it("records the changed path for a workspace with an initial nonce", () => {
    useViewerStore.getState().bumpWorktreeChangedPath("ws-1", "/ws/story/src/main.rs");
    expect(useViewerStore.getState().worktreeChangedPath["ws-1"]).toEqual({
      path: "/ws/story/src/main.rs",
      nonce: 1,
    });
  });

  it("bumps the nonce on a repeat change to the SAME path (so the effect re-fires)", () => {
    const bump = useViewerStore.getState().bumpWorktreeChangedPath;
    bump("ws-1", "/ws/story/a.txt");
    bump("ws-1", "/ws/story/a.txt");
    const sig = useViewerStore.getState().worktreeChangedPath["ws-1"];
    expect(sig).toEqual({ path: "/ws/story/a.txt", nonce: 2 });
  });

  it("keeps signals independent per workspace", () => {
    const bump = useViewerStore.getState().bumpWorktreeChangedPath;
    bump("ws-1", "/ws/one/a.txt");
    bump("ws-2", "/ws/two/b.txt");
    const map = useViewerStore.getState().worktreeChangedPath;
    expect(map["ws-1"]).toEqual({ path: "/ws/one/a.txt", nonce: 1 });
    expect(map["ws-2"]).toEqual({ path: "/ws/two/b.txt", nonce: 1 });
  });
});
