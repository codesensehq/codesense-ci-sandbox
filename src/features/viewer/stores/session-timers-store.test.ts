// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionTimerRow } from "@/tauri/commands";

type Handler = (event: { payload: unknown }) => void;
const { handlers, sessionTimers, cancelSessionTimer, remindSession, wakeHistory } = vi.hoisted(
  () => ({
    handlers: new Map<string, Handler>(),
    sessionTimers: vi.fn(),
    cancelSessionTimer: vi.fn(),
    remindSession: vi.fn(),
    wakeHistory: vi.fn(),
  }),
);

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, handler: Handler) => {
    handlers.set(name, handler);
    return Promise.resolve(() => handlers.delete(name));
  }),
}));

const { terminalFor } = vi.hoisted(() => ({ terminalFor: vi.fn() }));
vi.mock("@/shared/terminal/terminal-manager", () => ({ terminalManager: { terminalFor } }));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  sessionTimers,
  cancelSessionTimer,
  remindSession,
  wakeHistory,
}));

import {
  SESSION_TIMERS_CHANGED_EVENT,
  WAKE_DELIVERED_EVENT,
  type WakeDeliveredEvent,
} from "@/shared/lib/wake-events";
import {
  WOKEN_CAPTION_MS,
  resetSessionTimersForTests,
  useSessionTimersStore,
  visibleTimers,
  watchSessionTimers,
} from "./session-timers-store";
import { resetWakeMarkersForTests } from "../lib/wake-marker";

const REMOTE = { sessionId: "s-barb", machineId: "codesense-test-1" };
const LOCAL = { sessionId: "s-claude" };

function row(id: string, patch: Partial<SessionTimerRow> = {}): SessionTimerRow {
  return {
    timerId: id,
    name: `timer ${id}`,
    prompt: "check the build",
    nextAt: "2026-10-05T16:18:00Z",
    every: null,
    until: null,
    count: null,
    remaining: null,
    fires: 0,
    createdBy: "agent:Barb",
    machineId: "codesense-test-1",
    sessionId: "s-barb",
    ...patch,
  };
}

function delivered(patch: Partial<WakeDeliveredEvent> = {}): WakeDeliveredEvent {
  return {
    machineId: "codesense-test-1",
    sessionId: "s-barb",
    deliveryId: "d-1",
    marker: "Check the build",
    caption: "Woken by Check the build",
    origin: { kind: "timer", timerId: "t1" },
    deliveredAtMs: 1,
    source: "push",
    replayed: false,
    ...patch,
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));
const state = () => useSessionTimersStore.getState();

beforeEach(() => {
  resetSessionTimersForTests();
  handlers.clear();
  sessionTimers.mockReset().mockResolvedValue([]);
  cancelSessionTimer.mockReset().mockResolvedValue(true);
  remindSession.mockReset();
  wakeHistory.mockReset().mockResolvedValue([]);
  terminalFor.mockReset().mockReturnValue(null);
  resetWakeMarkersForTests();
});
afterEach(() => {
  vi.useRealTimers();
  resetSessionTimersForTests();
});

describe("session timers store", () => {
  it("reads a remote tab's timers from its machine, and a local tab's from this Mac", async () => {
    sessionTimers.mockResolvedValueOnce([row("t1"), row("t2")]);
    await state().refresh(REMOTE);
    expect(sessionTimers).toHaveBeenLastCalledWith("s-barb", "codesense-test-1");
    expect(state().bySession["codesense-test-1|s-barb"].rows?.map((r) => r.timerId)).toEqual([
      "t1",
      "t2",
    ]);
    await state().refresh(LOCAL);
    expect(sessionTimers).toHaveBeenLastCalledWith("s-claude", undefined);
  });

  it("Cancel hides the row now and cancels on the tab's machine only when its Undo closes", async () => {
    sessionTimers.mockResolvedValueOnce([row("t1"), row("t2")]);
    await state().refresh(REMOTE);
    const key = "codesense-test-1|s-barb";
    const shown = () =>
      visibleTimers(state().bySession[key].rows, state().cancelling[key])?.map((r) => r.timerId);
    state().cancel(REMOTE, "t1");
    expect(shown()).toEqual(["t2"]);
    expect(cancelSessionTimer).not.toHaveBeenCalled();
    sessionTimers.mockResolvedValueOnce([row("t2")]);
    await state().commitCancel(REMOTE, "t1");
    expect(cancelSessionTimer).toHaveBeenCalledWith("s-barb", "t1", "codesense-test-1");
    expect(shown()).toEqual(["t2"]);
  });

  it("Undo shows the row again and nothing is cancelled", async () => {
    sessionTimers.mockResolvedValueOnce([row("t1"), row("t2")]);
    await state().refresh(REMOTE);
    state().cancel(REMOTE, "t1");
    state().undoCancel(REMOTE, "t1");
    // The toast closing after an Undo commits nothing.
    await state().commitCancel(REMOTE, "t1");
    expect(cancelSessionTimer).not.toHaveBeenCalled();
    const key = "codesense-test-1|s-barb";
    expect(visibleTimers(state().bySession[key].rows, state().cancelling[key])).toHaveLength(2);
  });

  it("a pending cancel is committed when the page goes away (the toast never closes)", async () => {
    sessionTimers.mockResolvedValueOnce([row("t1")]);
    await state().refresh(REMOTE);
    const stop = watchSessionTimers();
    state().cancel(REMOTE, "t1");
    window.dispatchEvent(new Event("pagehide"));
    await flush();
    expect(cancelSessionTimer).toHaveBeenCalledWith("s-barb", "t1", "codesense-test-1");
    stop();
  });

  it("reminds on the tab's machine, then re-reads its timers", async () => {
    remindSession.mockResolvedValue({ timerId: "t9", nextAt: "2026-10-05T16:20:00Z" });
    sessionTimers.mockResolvedValueOnce([row("t9", { createdBy: "you" })]);
    await state().remind(REMOTE, { in: "20m", prompt: "check" });
    expect(remindSession).toHaveBeenCalledWith(
      "s-barb",
      { in: "20m", prompt: "check" },
      "codesense-test-1",
    );
    expect(state().bySession["codesense-test-1|s-barb"].rows?.[0].createdBy).toBe("you");
  });

  it("an offline machine's message is kept; an old server is told apart", async () => {
    sessionTimers.mockRejectedValueOnce(
      "Update the terminal server on codesense-test-1 to use wakes and timers.",
    );
    await state().refresh(REMOTE);
    expect(state().bySession["codesense-test-1|s-barb"].oldServer).toBe(true);
    sessionTimers.mockRejectedValueOnce("codesense-test-1 is offline: its timers keep firing");
    await state().refresh(REMOTE);
    const entry = state().bySession["codesense-test-1|s-barb"];
    expect(entry.oldServer).toBe(false);
    expect(entry.error).toMatch(/offline/);
  });

  it("forgets a tab whose session ended: no stale rows", async () => {
    sessionTimers.mockResolvedValueOnce([row("t1")]);
    await state().refresh(REMOTE);
    state().forget(REMOTE);
    expect(state().bySession["codesense-test-1|s-barb"]).toBeUndefined();
  });

  it("a timers-changed event re-reads a shown tab's timers, and only a shown tab's", async () => {
    await state().refresh(REMOTE);
    const stop = watchSessionTimers();
    await flush();
    sessionTimers.mockClear().mockResolvedValueOnce([row("t1")]);
    handlers.get(SESSION_TIMERS_CHANGED_EVENT)!({
      payload: { machineId: "codesense-test-1", sessionId: "s-barb" },
    });
    await flush();
    expect(sessionTimers).toHaveBeenCalledWith("s-barb", "codesense-test-1");
    expect(state().bySession["codesense-test-1|s-barb"].rows).toHaveLength(1);
    handlers.get(SESSION_TIMERS_CHANGED_EVENT)!({
      payload: { machineId: "local", sessionId: "s-shell" },
    });
    await flush();
    expect(sessionTimers).toHaveBeenCalledTimes(1);
    expect(state().bySession["local|s-shell"]).toBeUndefined();
    stop();
  });

  it("a delivery wakes the tab for 10 s, re-reads its timers and its history", async () => {
    vi.useFakeTimers();
    await state().refresh(REMOTE);
    sessionTimers.mockClear();
    const stop = watchSessionTimers();
    await vi.advanceTimersByTimeAsync(0);
    handlers.get(WAKE_DELIVERED_EVENT)!({ payload: delivered() });
    expect(state().woken["codesense-test-1|s-barb"]?.marker).toBe("Check the build");
    await vi.advanceTimersByTimeAsync(0);
    expect(sessionTimers).toHaveBeenCalledWith("s-barb", "codesense-test-1");
    expect(wakeHistory).toHaveBeenCalledWith("s-barb", "codesense-test-1");
    await vi.advanceTimersByTimeAsync(WOKEN_CAPTION_MS - 1);
    expect(state().woken["codesense-test-1|s-barb"]).toBeDefined();
    await vi.advanceTimersByTimeAsync(1);
    expect(state().woken["codesense-test-1|s-barb"]).toBeUndefined();
    stop();
  });

  it("a delivery into a background tab marks that tab's parked display, naming who set the timer", async () => {
    sessionTimers.mockResolvedValueOnce([row("t1", { createdBy: "agent:Barb" })]);
    await state().refresh(REMOTE);
    const registerMarker = vi.fn(() => ({ line: 0, isDisposed: false, onDispose: vi.fn() }));
    let painted: HTMLElement | null = null;
    terminalFor.mockReturnValue({
      buffer: {
        active: {
          length: 3,
          baseY: 0,
          cursorY: 2,
          getLine: (y: number) => ({
            translateToString: () => (y === 1 ? "> [timer: Check the build | fire 1]" : "> "),
          }),
        },
      },
      registerMarker,
      registerDecoration: () => ({
        onRender: (cb: (el: HTMLElement) => void) => {
          painted = document.createElement("div");
          cb(painted);
        },
      }),
      scrollToLine: vi.fn(),
    });
    const stop = watchSessionTimers();
    await flush();
    handlers.get(WAKE_DELIVERED_EVENT)!({
      payload: delivered({ marker: "Check the build (set by Barb)" }),
    });
    expect(terminalFor).toHaveBeenCalledWith("codesense-test-1", "s-barb");
    expect(registerMarker).toHaveBeenCalled();
    expect(painted!.textContent).toMatch(/^Check the build · timer set by Barb · /);
    stop();
  });

  it("a replayed delivery is shown in history but never captioned anew", async () => {
    const stop = watchSessionTimers();
    await flush();
    handlers.get(WAKE_DELIVERED_EVENT)!({ payload: delivered({ replayed: true }) });
    expect(state().woken["codesense-test-1|s-barb"]).toBeUndefined();
    await flush();
    expect(wakeHistory).toHaveBeenCalled();
    stop();
  });

  it("history is newest first and capped at 20", async () => {
    wakeHistory.mockResolvedValueOnce(
      Array.from({ length: 25 }, (_, i) => ({
        status: "delivered",
        deliveredAtMs: i,
        deliveryId: `d${i}`,
        marker: `m${i}`,
        origin: { kind: "timer", timerId: null },
        atMs: i,
      })),
    );
    await state().loadHistory(REMOTE);
    const history = state().historyBySession["codesense-test-1|s-barb"];
    expect(history).toHaveLength(20);
    expect(history[0].deliveryId).toBe("d24");
  });
});
