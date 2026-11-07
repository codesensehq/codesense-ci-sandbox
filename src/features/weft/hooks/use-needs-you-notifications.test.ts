// SPDX-License-Identifier: MIT
import { createElement } from "react";
import { act, cleanup, render, renderHook, screen } from "@testing-library/react";
import { Toaster } from "sonner";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttentionBoardDto, AttentionRowDto } from "@/tauri/commands";

// ── The board seam (AC-4: reuse the board's YOU-OWE read, no separate source) ──
let board: AttentionBoardDto | null = null;
const useAttentionBoardMock = vi.fn(() => ({
  board,
  error: null,
  degraded: null,
  reload: () => {},
  leaseStatus: null,
}));
vi.mock("./use-attention-board", () => ({
  useAttentionBoard: () => useAttentionBoardMock(),
}));

// ── The OS boundary: the Tauri notification plugin and window ──
const os = vi.hoisted(() => ({
  sendNotification: vi.fn(),
  setFocus: vi.fn(),
  revealSlot: vi.fn(),
  focusCb: null as null | ((e: { payload: boolean }) => void),
}));
vi.mock("@/features/shell/slots/reveal-slot", () => ({ revealSlot: os.revealSlot }));
const expectInboxReveal = () =>
  expect(os.revealSlot).toHaveBeenCalledWith("right-pane", {
    surface: "inbox",
    intent: { focusComposer: false },
  });
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: () => Promise.resolve(true),
  requestPermission: () => Promise.resolve("granted"),
  sendNotification: os.sendNotification,
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isFocused: () => Promise.resolve(true),
    onFocusChanged: (cb: (e: { payload: boolean }) => void) => {
      os.focusCb = cb;
      return Promise.resolve(() => {});
    },
    setBadgeCount: () => Promise.resolve(),
    unminimize: () => Promise.resolve(),
    show: () => Promise.resolve(),
    setFocus: () => {
      os.setFocus();
      return Promise.resolve();
    },
  }),
}));

import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  BURST_MS,
  COOLDOWN_MS,
  LAND_WINDOW_MS,
  startAttentionAlerts,
} from "@/features/viewer/lib/attention-alerts-controller";
import { selectNewNeedsYou, useNeedsYouNotifications } from "./use-needs-you-notifications";

function row(id: number, summary = `item ${id}`): AttentionRowDto {
  return { id, summary, source: null, ref: null, initiator: "agent" };
}

function boardOf(youOwe: AttentionRowDto[]): AttentionBoardDto {
  return { youOwe, owedToYou: [], onAgent: [], parked: [], doneUnacked: [] };
}

describe("SW-WA-E11 · selectNewNeedsYou (de-dup core, AC-5)", () => {
  it("returns only ids not already seen and rebuilds the seen-set from current rows", () => {
    const first = selectNewNeedsYou(new Set(), [row(1), row(2)]);
    expect(first.newRows.map((r) => r.id)).toEqual([1, 2]);
    expect([...first.nextSeen].sort()).toEqual([1, 2]);

    // Re-deliver the same rows → nothing new (idempotent on id).
    const again = selectNewNeedsYou(first.nextSeen, [row(1), row(2)]);
    expect(again.newRows).toEqual([]);

    // A new id arrives → only it is new.
    const third = selectNewNeedsYou(again.nextSeen, [row(1), row(2), row(3)]);
    expect(third.newRows.map((r) => r.id)).toEqual([3]);
  });

  it("forgets a resolved id so its later return re-notifies", () => {
    const seen = new Set([1]);
    // Row 1 leaves YOU-OWE → nextSeen drops it.
    const gone = selectNewNeedsYou(seen, []);
    expect(gone.nextSeen.has(1)).toBe(false);
    // Row 1 returns → treated as a genuinely new arrival.
    const back = selectNewNeedsYou(gone.nextSeen, [row(1)]);
    expect(back.newRows.map((r) => r.id)).toEqual([1]);
  });
});

/**
 * Seam: board arrivals → the app's one "needs you" stream (real controller) → the
 * notification plugin, and the notification's return → the Activity board.
 */
describe("SW-WA-E11 · useNeedsYouNotifications through the one needs-you stream", () => {
  let stop: () => void = () => {};
  const setFocused = (f: boolean) => os.focusCb?.({ payload: f });
  const bodies = () =>
    os.sendNotification.mock.calls.map((c) => c[0] as { title: string; body: string });

  beforeEach(async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    board = null;
    os.sendNotification.mockReset();
    os.setFocus.mockReset();
    os.revealSlot.mockReset();
    // SLW-TL-4: Inbox OS notifications are opt-in — turn them ON so these tests
    // exercise the notify/reveal path. (The default-off behavior is its own test.)
    useViewerStore.setState((s) => ({
      settings: { ...(s.settings ?? {}), inboxNotifications: true },
    }));
    stop = startAttentionAlerts();
    await vi.advanceTimersByTimeAsync(0);
  });
  afterEach(() => {
    stop();
    cleanup();
    useViewerStore.setState((s) => ({
      settings: { ...(s.settings ?? {}), inboxNotifications: undefined },
    }));
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("SLW-TL-4: ambient Inbox arrivals are OPT-IN — no OS notification by default (badge is the floor)", async () => {
    useViewerStore.setState((s) => ({
      settings: { ...(s.settings ?? {}), inboxNotifications: false },
    }));
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    // No declared `need` → ambient, respects the opt-in.
    board = boardOf([row(7, "Agent blocked — needs your call")]);
    rerender();
    rerender();
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  it("a BLOCKING `needs-approval` arrival ALWAYS notifies, even with the opt-in Inbox toggle OFF (barb regression)", async () => {
    // The user filed: barb entered 'Needs approval' / 'Waiting for your approval'
    // but produced NO OS notification. Root cause was `inboxNotifications` default
    // off eating this row. Blocking needs MUST notify at the agent floor.
    useViewerStore.setState((s) => ({
      settings: { ...(s.settings ?? {}), inboxNotifications: false },
    }));
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    board = {
      ...boardOf([]),
      youOwe: [
        {
          id: 101,
          summary: "barb: Waiting for your approval",
          source: "post",
          ref: "run-barb-1",
          initiator: "agent",
          need: "needs-approval",
        },
      ],
    };
    rerender();
    rerender();
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS * 2);
    expect(bodies()).toEqual([
      { title: "Something needs you", body: "barb: Waiting for your approval" },
    ]);
  });

  it.each<["awaiting-answer" | "needs-approval" | "awaiting-validation"]>([
    ["awaiting-answer"],
    ["needs-approval"],
    ["awaiting-validation"],
  ])("a BLOCKING `%s` arrival notifies with Inbox toggle OFF", async (need) => {
    useViewerStore.setState((s) => ({
      settings: { ...(s.settings ?? {}), inboxNotifications: false },
    }));
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    board = {
      ...boardOf([]),
      youOwe: [
        {
          id: 11,
          summary: `agent: ${need}`,
          source: "post",
          ref: "r1",
          initiator: "agent",
          need,
        },
      ],
    };
    rerender();
    rerender();
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS * 2);
    expect(os.sendNotification).toHaveBeenCalledTimes(1);
  });

  it("an AMBIENT `needs-attention` arrival still respects the opt-in (does NOT notify by default)", async () => {
    useViewerStore.setState((s) => ({
      settings: { ...(s.settings ?? {}), inboxNotifications: false },
    }));
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    board = {
      ...boardOf([]),
      youOwe: [
        {
          id: 12,
          summary: "fyi: a thing happened",
          source: "post",
          ref: "r2",
          initiator: "agent",
          need: "needs-attention",
        },
      ],
    };
    rerender();
    rerender();
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  it("SLW-TL-4: a posted item (universal board) flows through the same reveal/notify path", async () => {
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    // A CLI/agent post lands as a YOU-OWE row (source "post") — not weft-native.
    board = {
      ...boardOf([]),
      youOwe: [
        {
          id: 42,
          summary: "I need you: approve the deploy",
          source: "post",
          ref: "run-1",
          initiator: "agent",
          need: "needs-approval",
        },
      ],
    };
    rerender();
    rerender();
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS * 2);
    expect(bodies()).toEqual([
      { title: "Something needs you", body: "I need you: approve the deploy" },
    ]);
  });

  it("does NOT notify for pre-existing items on the first board settle (baseline seed)", async () => {
    setFocused(false);
    board = boardOf([row(1), row(2)]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    rerender();
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  it("AC-1: one notification for a new arrival while unfocused, never twice", async () => {
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    board = boardOf([row(7, "Agent blocked — needs your call")]);
    rerender();
    rerender();
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS * 2);
    expect(bodies()).toEqual([
      { title: "Something needs you", body: "Agent blocked — needs your call" },
    ]);
  });

  it("an item that leaves YOU-OWE before the burst ends never notifies", async () => {
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    board = boardOf([row(5)]);
    rerender();
    await vi.advanceTimersByTimeAsync(BURST_MS / 2);
    board = boardOf([]);
    rerender();
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  it("AC-2: no notification for a new arrival while focused, even if the user then leaves", async () => {
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    board = boardOf([row(9)]);
    rerender();
    await vi.advanceTimersByTimeAsync(100);
    setFocused(false);
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  it("shares the throttle: arrivals within the burst collapse into one summary", async () => {
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    board = boardOf([row(1)]);
    rerender();
    board = boardOf([row(1), row(2)]);
    rerender();
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(bodies()).toEqual([
      {
        title: "2 items need you",
        body: "Inbox: item 1\nInbox: item 2",
      },
    ]);
  });

  it("the notification's quick return lands on the Inbox", async () => {
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    board = boardOf([row(3)]);
    rerender();
    await vi.advanceTimersByTimeAsync(BURST_MS);
    act(() => setFocused(true));
    await vi.advanceTimersByTimeAsync(200);
    expectInboxReveal();
  });

  it("SLW-TL-4: the in-app reveal is ALWAYS-ON — it fires on return even with the OS push opted out", async () => {
    useViewerStore.setState((s) => ({
      settings: { ...(s.settings ?? {}), inboxNotifications: false },
    }));
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    board = boardOf([row(3)]);
    rerender();
    await vi.advanceTimersByTimeAsync(BURST_MS);
    // No OS/push notification (opt-out), but the reveal still lands on return.
    expect(os.sendNotification).not.toHaveBeenCalled();
    act(() => setFocused(true));
    await vi.advanceTimersByTimeAsync(200);
    expectInboxReveal();
  });

  it("a late return offers a pill, and it goes away once the item leaves YOU-OWE", async () => {
    render(createElement(Toaster));
    board = boardOf([]);
    const { rerender } = renderHook(() => useNeedsYouNotifications());
    setFocused(false);
    board = boardOf([row(4, "Approve the rollout")]);
    rerender();
    await vi.advanceTimersByTimeAsync(BURST_MS + LAND_WINDOW_MS + 1);
    act(() => setFocused(true));
    await vi.advanceTimersByTimeAsync(200);
    const text = "Inbox: Approve the rollout";
    await vi.waitFor(() => expect(screen.getByText(text)).toBeInTheDocument());
    board = boardOf([]);
    rerender();
    await act(() => vi.advanceTimersByTimeAsync(1000));
    await vi.waitFor(() => expect(screen.queryByText(text)).not.toBeInTheDocument());
  });
});
