// SPDX-License-Identifier: MIT
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttentionBoardDto, WvApprovalRow } from "@/tauri/commands";

let board: AttentionBoardDto = {
  youOwe: [],
  owedToYou: [],
  onAgent: [],
  parked: [],
  doneUnacked: [],
};
let pendingApprovals: unknown[] = [];
const listAttentionBoard = vi.fn(() => Promise.resolve(board));
const wvListPendingApprovals = vi.fn(() => Promise.resolve(pendingApprovals));
vi.mock("@/tauri/commands", () => ({
  listAttentionBoard: () => listAttentionBoard(),
  wvListPendingApprovals: () => wvListPendingApprovals(),
}));

// Capture the wv:attention-changed handler so a test can fire the mock event.
let changedHandler: (() => void) | null = null;
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: () => void) => {
    if (name === "wv:attention-changed") changedHandler = cb;
    return Promise.resolve(() => {});
  },
}));

import { logger } from "@/shared/lib/logger";

import {
  RETRY_BACKOFF_MS,
  useAttentionBoard,
  useWeftYouOwe,
  weftYouOweKey,
} from "./use-attention-board";

describe("SW-WV-F1 · useAttentionBoard", () => {
  beforeEach(() => {
    listAttentionBoard.mockClear();
    wvListPendingApprovals.mockClear();
    changedHandler = null;
    board = { youOwe: [], owedToYou: [], onAgent: [], parked: [], doneUnacked: [] };
    pendingApprovals = [];
  });
  afterEach(() => vi.restoreAllMocks());

  it("AC-1: reads the board once on mount", async () => {
    const { result } = renderHook(() => useAttentionBoard());
    await waitFor(() => expect(result.current.board).not.toBeNull());
    expect(listAttentionBoard).toHaveBeenCalledTimes(1);
  });

  it("every mounted reader shares one read, and a failure is logged once", async () => {
    const warn = vi.spyOn(logger, "warn").mockImplementation(() => {});
    // No active team yet: the first read fails, the self-retry lands.
    listAttentionBoard.mockRejectedValueOnce(new Error("no active team selected"));
    const readers = Array.from({ length: 9 }, () => renderHook(() => useAttentionBoard()));
    await waitFor(() => expect(readers[8].result.current.board).not.toBeNull(), {
      timeout: 3000,
    });
    for (const r of readers) expect(r.result.current.board).not.toBeNull();
    // One failed read + one retry, not one pair per reader.
    expect(listAttentionBoard).toHaveBeenCalledTimes(2);
    expect(warn.mock.calls.filter(([scope]) => scope === "activity")).toHaveLength(1);
  });

  it("a reader mounted after the board loaded reuses it without a new read", async () => {
    const first = renderHook(() => useAttentionBoard());
    await waitFor(() => expect(first.result.current.board).not.toBeNull());
    const second = renderHook(() => useAttentionBoard());
    expect(second.result.current.board).not.toBeNull();
    expect(listAttentionBoard).toHaveBeenCalledTimes(1);
  });

  it("once the last reader goes, a stale reload reads nothing and a new reader starts over", async () => {
    const first = renderHook(() => useAttentionBoard());
    await waitFor(() => expect(first.result.current.board).not.toBeNull());
    const staleReload = first.result.current.reload;
    first.unmount();
    act(() => staleReload());
    expect(listAttentionBoard).toHaveBeenCalledTimes(1);
    const next = renderHook(() => useAttentionBoard());
    expect(next.result.current.board).toBeNull();
    await waitFor(() => expect(next.result.current.board).not.toBeNull());
    expect(listAttentionBoard).toHaveBeenCalledTimes(2);
  });

  it("AC-6: the push event triggers a second fetch (live, no polling)", async () => {
    const { result } = renderHook(() => useAttentionBoard());
    await waitFor(() => expect(listAttentionBoard).toHaveBeenCalledTimes(1));
    // The hook subscribed to wv:attention-changed on mount.
    await waitFor(() => expect(changedHandler).not.toBeNull());
    act(() => changedHandler?.());
    await waitFor(() => expect(listAttentionBoard).toHaveBeenCalledTimes(2));
    expect(result.current.error).toBeNull();
  });

  it("AC-6: registers NO poll interval", async () => {
    const spy = vi.spyOn(window, "setInterval");
    let result: ReturnType<typeof renderHook<ReturnType<typeof useAttentionBoard>, unknown>>;
    // Flush the mount fetch inside act WITHOUT waitFor (waitFor itself uses
    // setInterval internally, which would poison the spy).
    await act(async () => {
      result = renderHook(() => useAttentionBoard());
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(listAttentionBoard).toHaveBeenCalledTimes(1);
    expect(spy).not.toHaveBeenCalled();
    result!.unmount();
  });

  it("AC-6: a fetch failure flips the degraded banner; reload clears it", async () => {
    // The first read and its one automatic retry both fail.
    listAttentionBoard
      .mockRejectedValueOnce(new Error("ipc down"))
      .mockRejectedValueOnce(new Error("ipc down"));
    const { result } = renderHook(() => useAttentionBoard());
    await waitFor(() => expect(result.current.degraded).toEqual({ status: "failed" }), {
      timeout: 3000,
    });
    expect(result.current.error).toBe("ipc down");
    act(() => result.current.reload());
    await waitFor(() => expect(result.current.degraded).toBeNull());
  });

  it("a failure after the board has loaded shows at once, then retries on its own", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const { result } = renderHook(() => useAttentionBoard());
      await waitFor(() => expect(result.current.board).not.toBeNull());
      listAttentionBoard.mockRejectedValueOnce(new Error("ipc down"));
      act(() => result.current.reload());
      await waitFor(() => expect(result.current.degraded).toEqual({ status: "failed" }));
      expect(listAttentionBoard).toHaveBeenCalledTimes(2);
      // No click: the next backoff step re-reads, and the answer clears the error.
      await act(() => vi.advanceTimersByTimeAsync(RETRY_BACKOFF_MS[0] + 50));
      await waitFor(() => expect(result.current.degraded).toBeNull());
      expect(listAttentionBoard).toHaveBeenCalledTimes(3);
      // Healthy again: nothing more is read on its own.
      await act(() => vi.advanceTimersByTimeAsync(60_000));
      expect(listAttentionBoard).toHaveBeenCalledTimes(3);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("SW-WV-F1 · useWeftYouOwe (single merge site)", () => {
  beforeEach(() => {
    changedHandler = null;
    board = {
      youOwe: [{ id: 7, summary: "You owe: review.", source: null, ref: null, initiator: "human" }],
      owedToYou: [],
      onAgent: [],
      parked: [],
      doneUnacked: [],
    };
  });

  it("returns the you_owe attention slice as tagged rows", async () => {
    const { result } = renderHook(() => useWeftYouOwe());
    await waitFor(() => expect(result.current.rows).toHaveLength(1));
    const first = result.current.rows[0];
    expect(first.kind).toBe("attention");
    if (first.kind === "attention") expect(first.row.id).toBe(7);
  });

  it("merges pending approvals into the you_owe list (F5, single merge site)", async () => {
    pendingApprovals = [
      {
        id: 3,
        tier: "T3",
        claim: "drop the prod table",
        postedAt: "2026-09-02T00:00:00Z",
        source: null,
        windowState: "open",
        disposition: "pending",
        ttlS: null,
        acquiredAt: null,
        pauseAccumulatedS: 0,
        surfacedSession: null,
        notSurfacedButExpired: false,
      },
    ];
    const { result } = renderHook(() => useWeftYouOwe());
    // One attention row + one approval row.
    await waitFor(() => expect(result.current.rows).toHaveLength(2));
    const approval = result.current.rows.find((r) => r.kind === "approval");
    expect(approval).toBeDefined();
    if (approval?.kind === "approval") expect(approval.row.id).toBe(3);
  });

  it("keys attention rows task:<id> and approval rows approval:<id>, never colliding (F5)", () => {
    expect(
      weftYouOweKey({
        kind: "attention",
        row: { id: 7, summary: "s", source: null, ref: null, initiator: "human" },
      }),
    ).toBe("task:7");
    expect(weftYouOweKey({ kind: "approval", row: { id: 7 } as unknown as WvApprovalRow })).toBe(
      "approval:7",
    );
  });
});
