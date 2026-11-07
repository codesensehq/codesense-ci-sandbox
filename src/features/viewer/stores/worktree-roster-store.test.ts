// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { renderHook } from "@testing-library/react";

import type { WorktreeRow } from "@/tauri/commands";

let listResult: { rows: WorktreeRow[]; reaped: number } = { rows: [], reaped: 0 };
let listReject: unknown = null;
const listManagedWorktrees = vi.fn(() =>
  listReject ? Promise.reject(listReject) : Promise.resolve(listResult),
);
vi.mock("@/tauri/commands", () => ({
  listManagedWorktrees: () => listManagedWorktrees(),
}));

import { useLiveCliSession, useWorktreeRosterStore } from "./worktree-roster-store";

const mkRows = (n: number): WorktreeRow[] =>
  Array.from(
    { length: n },
    (_, i) =>
      ({
        kind: "orphan",
        workspaceKey: `/clone-${i}`,
        path: `/wt-${i}`,
        branch: null,
        dirty: false,
        repoPath: null,
        liveCliSession: false,
      }) as WorktreeRow,
  );

beforeEach(() => {
  listResult = { rows: [], reaped: 0 };
  listReject = null;
  listManagedWorktrees.mockClear();
  useWorktreeRosterStore.setState({ rows: null, error: null });
});

describe("worktree-roster-store", () => {
  it("starts not-loaded (null, distinct from empty)", () => {
    expect(useWorktreeRosterStore.getState().rows).toBeNull();
    expect(useWorktreeRosterStore.getState().error).toBeNull();
  });

  it("reload() enumerates rows into the store and clears error", async () => {
    listResult = { rows: mkRows(3), reaped: 0 };
    await useWorktreeRosterStore.getState().reload();
    expect(useWorktreeRosterStore.getState().rows).toHaveLength(3);
    expect(useWorktreeRosterStore.getState().error).toBeNull();
    expect(listManagedWorktrees).toHaveBeenCalledTimes(1);
  });

  it("reload() ignores the `reaped` count (the toast owns it, not the list)", async () => {
    listResult = { rows: mkRows(2), reaped: 5 };
    await useWorktreeRosterStore.getState().reload();
    expect(useWorktreeRosterStore.getState().rows).toHaveLength(2);
  });

  it("reload() distinguishes empty ([]) from not-loaded (null)", async () => {
    listResult = { rows: [], reaped: 0 };
    await useWorktreeRosterStore.getState().reload();
    expect(useWorktreeRosterStore.getState().rows).toEqual([]);
    expect(useWorktreeRosterStore.getState().rows).not.toBeNull();
  });

  it("reload() records the error and retains the last-known rows on a failed refetch", async () => {
    listResult = { rows: mkRows(2), reaped: 0 };
    await useWorktreeRosterStore.getState().reload();
    listReject = new Error("boom");
    await useWorktreeRosterStore.getState().reload();
    expect(useWorktreeRosterStore.getState().error).toBe("boom");
    expect(useWorktreeRosterStore.getState().rows).toHaveLength(2);
  });

  it("gen-guards concurrent reloads: last caller wins, stale resolution dropped", async () => {
    let resolveSlow!: (v: { rows: WorktreeRow[]; reaped: number }) => void;
    listManagedWorktrees
      .mockImplementationOnce(() => new Promise((res) => (resolveSlow = res)))
      .mockImplementationOnce(() => Promise.resolve({ rows: mkRows(2), reaped: 0 }));

    const slow = useWorktreeRosterStore.getState().reload();
    const fast = useWorktreeRosterStore.getState().reload();
    await fast;
    expect(useWorktreeRosterStore.getState().rows).toHaveLength(2);

    resolveSlow({ rows: mkRows(5), reaped: 0 });
    await slow;
    expect(useWorktreeRosterStore.getState().rows).toHaveLength(2);
  });
});

describe("useLiveCliSession (git-sync live-diff suspend signal)", () => {
  const liveRow = (path: string, liveCliSession: boolean): WorktreeRow =>
    ({ ...mkRows(1)[0], path, liveCliSession }) as WorktreeRow;

  beforeEach(() => {
    useWorktreeRosterStore.setState({ rows: null, error: null });
  });

  it("(a) exact worktree path match → that row's own liveCliSession", () => {
    useWorktreeRosterStore.setState({
      rows: [liveRow("/wt-a", true), liveRow("/wt-b", false)],
    });
    expect(renderHook(() => useLiveCliSession("/wt-a")).result.current).toBe(true);
    expect(renderHook(() => useLiveCliSession("/wt-b")).result.current).toBe(false);
  });

  it("(b) org-root path (not a worktree) → true iff SOME row is live", () => {
    // No roster row has this path (it's the org-root/content sync view).
    useWorktreeRosterStore.setState({
      rows: [liveRow("/wt-a", false), liveRow("/wt-b", false)],
    });
    expect(renderHook(() => useLiveCliSession("/ctx/org-root")).result.current).toBe(false);

    useWorktreeRosterStore.setState({
      rows: [liveRow("/wt-a", false), liveRow("/wt-b", true)],
    });
    expect(renderHook(() => useLiveCliSession("/ctx/org-root")).result.current).toBe(true);
  });

  it("(c) empty or not-loaded roster → false", () => {
    useWorktreeRosterStore.setState({ rows: null });
    expect(renderHook(() => useLiveCliSession("/anything")).result.current).toBe(false);
    useWorktreeRosterStore.setState({ rows: [] });
    expect(renderHook(() => useLiveCliSession("/anything")).result.current).toBe(false);
  });
});
