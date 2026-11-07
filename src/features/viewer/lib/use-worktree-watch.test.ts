// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { useViewerStore } from "@/shared/stores/viewer-store";

const { mockAttach, mockDetach } = vi.hoisted(() => ({
  mockAttach: vi.fn(),
  mockDetach: vi.fn(),
}));

vi.mock("@/tauri/commands", () => ({
  lazyAttachWorktreeWatch: mockAttach,
  detachWorktreeWatch: mockDetach,
}));
vi.mock("@/shared/lib/logger", () => ({ logger: { warn: vi.fn() } }));

import { useWorktreeWatch, __resetWorktreeWatch } from "./use-worktree-watch";

beforeEach(() => {
  __resetWorktreeWatch();
  mockAttach.mockReset().mockResolvedValue(7);
  mockDetach.mockReset().mockResolvedValue(undefined);
});

describe("useWorktreeWatch — ref-counted shared watch (RD-13)", () => {
  it("attaches once for two consumers of the same wsId", async () => {
    const a = renderHook(() => useWorktreeWatch("ws1", "/wt", true));
    const b = renderHook(() => useWorktreeWatch("ws1", "/wt", true));
    await waitFor(() => expect(mockAttach).toHaveBeenCalledTimes(1));
    a.unmount();
    b.unmount();
    await waitFor(() => expect(mockDetach).toHaveBeenCalledTimes(1));
    expect(mockDetach).toHaveBeenCalledWith("ws1", 7);
  });

  it("keeps the watch alive when only ONE of two consumers unmounts (no strand)", async () => {
    const a = renderHook(() => useWorktreeWatch("ws1", "/wt", true));
    const b = renderHook(() => useWorktreeWatch("ws1", "/wt", true));
    await waitFor(() => expect(mockAttach).toHaveBeenCalledTimes(1));

    a.unmount(); // inset closes; overlay (b) still open
    // The watch must NOT be detached while b holds a reference.
    expect(mockDetach).not.toHaveBeenCalled();

    b.unmount();
    await waitFor(() => expect(mockDetach).toHaveBeenCalledTimes(1));
  });

  it("does not attach when disabled", () => {
    renderHook(() => useWorktreeWatch("ws1", "/wt", false));
    expect(mockAttach).not.toHaveBeenCalled();
  });

  it("detaches immediately if released before the in-flight attach resolves", async () => {
    let resolveAttach!: (t: number) => void;
    mockAttach.mockReturnValueOnce(new Promise<number>((r) => (resolveAttach = r)));
    const a = renderHook(() => useWorktreeWatch("ws1", "/wt", true));
    a.unmount(); // release before attach resolves
    resolveAttach(9);
    await waitFor(() => expect(mockDetach).toHaveBeenCalledWith("ws1", 9));
  });
});

describe("useWorktreeWatch — hidden warm teams", () => {
  it("attaches the shown team's worktree when a retained team repeats the workspace id", async () => {
    act(() => warmScopes.activate("org/a"));
    renderHook(() => useWorktreeWatch("ws1", "/team-a/wt", true, "org/a"));
    // A's attach is still in flight when the switch lands.
    act(() => warmScopes.activate("org/b"));
    renderHook(() => useWorktreeWatch("ws1", "/team-b/wt", true, "org/b"));
    await waitFor(() => expect(mockAttach).toHaveBeenCalledWith("ws1", "/team-b/wt"));
    // The retained team's watch never outlives the switch: its token is detached, the shown
    // team's stays.
    await waitFor(() => expect(mockDetach).toHaveBeenCalledTimes(1));
    warmScopes.forget("org/a");
    warmScopes.forget("org/b");
  });
});

describe("useWorktreeWatch — resume signal", () => {
  const nonce = () => useViewerStore.getState().worktreeChangedPath["ws1"]?.nonce ?? 0;

  it("bumps the shared change nonce once when a hidden team is shown again, not on first mount", async () => {
    useViewerStore.setState({ worktreeChangedPath: {} });
    act(() => warmScopes.activate("org/a"));
    renderHook(() => useWorktreeWatch("ws1", "/wt", true, "org/a"));
    await waitFor(() => expect(mockAttach).toHaveBeenCalledTimes(1));
    expect(nonce()).toBe(0);
    act(() => warmScopes.activate("org/b"));
    expect(nonce()).toBe(0);
    act(() => warmScopes.activate("org/a"));
    expect(nonce()).toBe(1);
    expect(useViewerStore.getState().worktreeChangedPath["ws1"]?.resumed).toBe(1);
    warmScopes.forget("org/a");
    warmScopes.forget("org/b");
  });
});
