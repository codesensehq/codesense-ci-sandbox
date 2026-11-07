// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, act, waitFor } from "@testing-library/react";

import type { WorktreeRow } from "@/tauri/commands";
import { WORKTREE_CHANGED_EVENT } from "@/tauri/events";

let listResult: { rows: WorktreeRow[]; reaped: number } = { rows: [], reaped: 0 };
const listManagedWorktrees = vi.fn(() => Promise.resolve(listResult));
vi.mock("@/tauri/commands", () => ({
  listManagedWorktrees: () => listManagedWorktrees(),
}));

// Capture the event handlers the listener registers so the test can fire them.
const handlers: Record<string, ((e: unknown) => void) | undefined> = {};
const unlisten = vi.fn();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: unknown) => void) => {
    handlers[name] = cb;
    return Promise.resolve(unlisten);
  },
}));

import {
  useWorktreeRosterListener,
  WORKTREE_ROSTER_COALESCE_MS,
} from "./use-worktree-roster-listener";
import { useWorktreeRosterStore } from "../stores/worktree-roster-store";

const row = (path: string): WorktreeRow =>
  ({
    kind: "orphan",
    workspaceKey: `/clone${path}`,
    path,
    branch: null,
    dirty: false,
    repoPath: null,
    liveCliSession: false,
  }) as WorktreeRow;

function Listener() {
  useWorktreeRosterListener();
  return null;
}

beforeEach(() => {
  listResult = { rows: [], reaped: 0 };
  handlers[WORKTREE_CHANGED_EVENT] = undefined;
  listManagedWorktrees.mockClear();
  unlisten.mockClear();
  useWorktreeRosterStore.setState({ rows: null, error: null });
});

describe("useWorktreeRosterListener — single app-root subscription (SW-WU-9)", () => {
  it("warms the store on mount and subscribes to the worktree roster event", async () => {
    listResult = { rows: [row("/a"), row("/b")], reaped: 0 };
    render(<Listener />);
    await waitFor(() => expect(useWorktreeRosterStore.getState().rows).toHaveLength(2));
    expect(listManagedWorktrees).toHaveBeenCalledTimes(1);
    expect(handlers[WORKTREE_CHANGED_EVENT]).toBeTypeOf("function");
  });

  it("refetches on worktree-changed, coalescing a burst into ONE trailing fetch (AC-5)", async () => {
    vi.useFakeTimers();
    try {
      listResult = { rows: [row("/a")], reaped: 0 };
      render(<Listener />);
      await act(async () => {
        await Promise.resolve();
      });
      expect(listManagedWorktrees).toHaveBeenCalledTimes(1); // mount fetch

      act(() => {
        handlers[WORKTREE_CHANGED_EVENT]?.(undefined);
        handlers[WORKTREE_CHANGED_EVENT]?.(undefined);
        handlers[WORKTREE_CHANGED_EVENT]?.(undefined);
      });
      expect(listManagedWorktrees).toHaveBeenCalledTimes(1); // still within window

      await act(async () => {
        vi.advanceTimersByTime(WORKTREE_ROSTER_COALESCE_MS);
        await Promise.resolve();
      });
      expect(listManagedWorktrees).toHaveBeenCalledTimes(2); // exactly one trailing refetch
    } finally {
      vi.useRealTimers();
    }
  });

  it("disposes its subscriptions on unmount", async () => {
    render(<Listener />).unmount();
    await waitFor(() => expect(unlisten).toHaveBeenCalledTimes(1));
  });
});
