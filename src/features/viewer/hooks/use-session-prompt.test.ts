// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

const { listenMock, sessionPromptMock, layoutState, heldMock, targetMock } = vi.hoisted(() => ({
  listenMock: vi.fn(),
  sessionPromptMock: vi.fn(),
  layoutState: { terminalLayout: null as unknown },
  heldMock: vi.fn(() => false),
  targetMock: vi.fn((): unknown => undefined),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@/tauri/commands", () => ({
  sessionPrompt: (ref: unknown) => sessionPromptMock(ref),
}));
vi.mock("@/features/viewer/lib/cli-workspace/store/viewer-access", () => ({
  getViewerState: () => layoutState,
}));
vi.mock("@/shared/lib/terminal-layout-projection", () => ({
  heldRemotely: () => heldMock(),
  sessionTarget: () => targetMock(),
}));

import {
  refreshSessionPrompt,
  resetSessionPrompts,
  useSessionPrompt,
  type SessionPrompt,
} from "./use-session-prompt";

const prompt = (cwd: string): SessionPrompt => ({ cwd, home: "/h", git: null });

/** A promise whose resolution the test controls. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

let oscHandler: ((e: { payload: { session_id: string; kind: string } }) => void) | null = null;

beforeEach(() => {
  resetSessionPrompts();
  sessionPromptMock.mockReset();
  listenMock.mockReset();
  heldMock.mockReturnValue(false);
  targetMock.mockReturnValue(undefined);
  layoutState.terminalLayout = null;
  oscHandler = null;
  listenMock.mockImplementation(async (_event: string, handler: typeof oscHandler) => {
    oscHandler = handler;
    return () => undefined;
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useSessionPrompt", () => {
  it("shares one read in flight per session", async () => {
    const d = deferred<SessionPrompt>();
    sessionPromptMock.mockReturnValue(d.promise);
    const a = refreshSessionPrompt("s1");
    const b = refreshSessionPrompt("s1");
    expect(sessionPromptMock).toHaveBeenCalledTimes(1);
    d.resolve(prompt("/a"));
    await Promise.all([a, b]);
  });

  it("a read that settles after a reset never clears the newer read in flight", async () => {
    const old = deferred<SessionPrompt>();
    const fresh = deferred<SessionPrompt>();
    sessionPromptMock.mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    const first = refreshSessionPrompt("s1");
    resetSessionPrompts();
    const second = refreshSessionPrompt("s1");
    old.resolve(prompt("/stale"));
    await first;
    // The newer read is still the one in flight: a join must not start a third.
    void refreshSessionPrompt("s1");
    expect(sessionPromptMock).toHaveBeenCalledTimes(2);
    fresh.resolve(prompt("/new"));
    await second;
  });

  it("an OSC 133 command-end during a read runs one more read after it", async () => {
    vi.useFakeTimers();
    const first = deferred<SessionPrompt>();
    sessionPromptMock.mockReturnValueOnce(first.promise).mockResolvedValueOnce(prompt("/after-cd"));
    const { result } = renderHook(() => useSessionPrompt("s1"));
    await act(async () => {
      await Promise.resolve();
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(1);
    expect(oscHandler).not.toBeNull();
    // The shell finished `cd` while the mount read (started before it) is out.
    act(() => oscHandler!({ payload: { session_id: "s1", kind: "command-end" } }));
    await act(async () => {
      vi.advanceTimersByTime(200);
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(1);
    await act(async () => {
      first.resolve(prompt("/before-cd"));
      await vi.runOnlyPendingTimersAsync();
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(2);
    expect(result.current?.cwd).toBe("/after-cd");
  });

  it("a read that lands after its session was evicted caches nothing", async () => {
    vi.useFakeTimers();
    const slow = deferred<SessionPrompt>();
    sessionPromptMock.mockReturnValueOnce(slow.promise);
    const { unmount } = renderHook(() => useSessionPrompt("s1"));
    await act(async () => {
      await Promise.resolve();
    });
    unmount();
    // The eviction runs while the read is still out…
    await act(async () => {
      vi.advanceTimersByTime(31_000);
    });
    await act(async () => {
      slow.resolve(prompt("/late"));
      await Promise.resolve();
    });
    // …so a later caption starts empty and reads again instead of seeing it.
    sessionPromptMock.mockReturnValue(new Promise(() => undefined));
    const { result } = renderHook(() => useSessionPrompt("s1"));
    expect(result.current).toBeNull();
  });

  it("ignores OSC 133 marks other than prompt-start and command-end", async () => {
    vi.useFakeTimers();
    sessionPromptMock.mockResolvedValue(prompt("/a"));
    renderHook(() => useSessionPrompt("s1"));
    await act(async () => {
      await Promise.resolve();
    });
    const before = sessionPromptMock.mock.calls.length;
    act(() => oscHandler!({ payload: { session_id: "s1", kind: "command-start" } }));
    await act(async () => {
      vi.advanceTimersByTime(200);
    });
    expect(sessionPromptMock.mock.calls.length).toBe(before);
  });

  it("an OSC 133 mark on a caption that is off screen waits for the next on-screen poll", async () => {
    vi.useFakeTimers();
    sessionPromptMock.mockResolvedValue(prompt("/a"));
    const el = document.createElement("div");
    let laidOut = false;
    el.getClientRects = () => (laidOut ? [{}] : []) as unknown as DOMRectList;
    const ref = { current: el };
    renderHook(() => useSessionPrompt("s1", ref));
    await act(async () => {
      await Promise.resolve();
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(1);
    act(() => oscHandler!({ payload: { session_id: "s1", kind: "prompt-start" } }));
    await act(async () => {
      vi.advanceTimersByTime(200);
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(1);
    // Back on screen: the next poll tick reads, even inside the usual wait.
    laidOut = true;
    await act(async () => {
      vi.advanceTimersByTime(5000);
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(2);
  });

  it("a caption shown again after a mark while hidden reads at once, not at the next poll", async () => {
    vi.useFakeTimers();
    const observers: Array<() => void> = [];
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(private cb: () => void) {
          observers.push(() => this.cb());
        }
        observe() {}
        disconnect() {}
      },
    );
    try {
      sessionPromptMock.mockResolvedValue(prompt("/a"));
      const el = document.createElement("div");
      let laidOut = true;
      el.getClientRects = () => (laidOut ? [{}] : []) as unknown as DOMRectList;
      renderHook(() => useSessionPrompt("s1", { current: el }));
      await act(async () => {
        await Promise.resolve();
      });
      expect(sessionPromptMock).toHaveBeenCalledTimes(1);
      laidOut = false;
      act(() => observers.forEach((o) => o()));
      act(() => oscHandler!({ payload: { session_id: "s1", kind: "command-end" } }));
      await act(async () => {
        vi.advanceTimersByTime(200);
      });
      expect(sessionPromptMock).toHaveBeenCalledTimes(1);
      laidOut = true;
      await act(async () => {
        observers.forEach((o) => o());
        await Promise.resolve();
      });
      expect(sessionPromptMock).toHaveBeenCalledTimes(2);
      // Already fresh: another layout change doesn't read again.
      await act(async () => {
        observers.forEach((o) => o());
        await Promise.resolve();
      });
      expect(sessionPromptMock).toHaveBeenCalledTimes(2);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("a mark debounced on screen whose caption is hidden before it fires doesn't read off screen", async () => {
    vi.useFakeTimers();
    sessionPromptMock.mockResolvedValue(prompt("/a"));
    const el = document.createElement("div");
    let laidOut = true;
    el.getClientRects = () => (laidOut ? [{}] : []) as unknown as DOMRectList;
    renderHook(() => useSessionPrompt("s1", { current: el }));
    await act(async () => {
      await Promise.resolve();
    });
    act(() => oscHandler!({ payload: { session_id: "s1", kind: "command-end" } }));
    laidOut = false;
    act(() => oscHandler!({ payload: { session_id: "s1", kind: "prompt-start" } }));
    await act(async () => {
      vi.advanceTimersByTime(200);
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(1);
  });

  it("a follow-up read queued on screen waits when the caption is hidden before the read lands", async () => {
    vi.useFakeTimers();
    const first = deferred<SessionPrompt>();
    sessionPromptMock.mockReturnValueOnce(first.promise).mockResolvedValue(prompt("/after"));
    const el = document.createElement("div");
    let laidOut = true;
    el.getClientRects = () => (laidOut ? [{}] : []) as unknown as DOMRectList;
    renderHook(() => useSessionPrompt("s1", { current: el }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(1);
    // A mark on screen queues a fresh read behind the one in flight...
    act(() => oscHandler!({ payload: { session_id: "s1", kind: "command-end" } }));
    await act(async () => {
      vi.advanceTimersByTime(200);
    });
    // ...then the caption leaves the screen before the first read lands.
    laidOut = false;
    await act(async () => {
      first.resolve(prompt("/before"));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(1);
  });

  it("a reset while the OSC 133 listen is pending leaves no orphaned listener", async () => {
    const first = deferred<() => void>();
    const unlistenFirst = vi.fn();
    listenMock.mockImplementationOnce(() => first.promise);
    sessionPromptMock.mockResolvedValue(prompt("/a"));
    renderHook(() => useSessionPrompt("s1"));
    resetSessionPrompts();
    renderHook(() => useSessionPrompt("s2"));
    expect(listenMock).toHaveBeenCalledTimes(2);
    await act(async () => {
      first.resolve(unlistenFirst);
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(unlistenFirst).toHaveBeenCalledTimes(1);
  });

  it("a wall clock set backwards doesn't stall polling", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-05T12:00:00Z"));
    sessionPromptMock.mockResolvedValue(prompt("/a"));
    renderHook(() => useSessionPrompt("s1"));
    await act(async () => {
      await Promise.resolve();
    });
    vi.setSystemTime(new Date("2026-10-05T11:00:00Z"));
    await act(async () => {
      vi.advanceTimersByTime(5000);
    });
    expect(sessionPromptMock).toHaveBeenCalledTimes(2);
  });

  it("the reset seam also drops the OSC 133 listener", async () => {
    const unlisten = vi.fn();
    listenMock.mockImplementation(async () => unlisten);
    sessionPromptMock.mockResolvedValue(prompt("/a"));
    renderHook(() => useSessionPrompt("s1"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    resetSessionPrompts();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("asks a remote Machine by the session's admitted target, and skips it while unreachable", async () => {
    sessionPromptMock.mockResolvedValue(prompt("/remote"));
    layoutState.terminalLayout = {};
    heldMock.mockReturnValue(true);
    const target = { machineId: "m1", session: "s9" };
    targetMock.mockReturnValue(target);
    await refreshSessionPrompt("s9");
    expect(sessionPromptMock).toHaveBeenLastCalledWith(target);

    sessionPromptMock.mockClear();
    targetMock.mockReturnValue(undefined);
    await refreshSessionPrompt("s10");
    expect(sessionPromptMock).not.toHaveBeenCalled();
  });
});
