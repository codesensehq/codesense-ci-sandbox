// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { ATTACH_TIMEOUT_MS, attachTerminal, type AttachDeps } from "./terminal-attach";
import type { TerminalSnapshot, TerminalSubscription } from "@/tauri/commands";
import type { LiveStream } from "@/shared/terminal/live-stream";

/**
 * The FE viewport attach model: a pane only ATTACHES to the session its host
 * placed (open its stream → snapshot + live events) and detaches on unmount. It never
 * spawns and never kills.
 */

function snap(vt = "", seq = 0): TerminalSnapshot {
  return { cols: 80, rows: 24, seq, resize_gen: 0, scrollback: 1000, vt };
}

function sub(streamId: number, snapshot = snap()): TerminalSubscription {
  return {
    snapshot,
    stream_id: streamId,
    story_id: null,
    workspace_id: null,
    epic_id: null,
  };
}

function liveStream(subscription: TerminalSubscription): LiveStream {
  return {
    streamId: subscription.stream_id,
    subscription,
    begin: vi.fn(),
    close: vi.fn(),
  };
}

function makeDeps(over: Partial<AttachDeps> = {}): AttachDeps {
  return {
    open: vi.fn(async () => liveStream(sub(1))),
    isAborted: () => false,
    ...over,
  };
}

describe("attachTerminal", () => {
  it("attaches to the session: its stream and snapshot", async () => {
    const stream = liveStream(sub(7, snap("history-bytes", 42)));
    const deps = makeDeps({ open: vi.fn(async () => stream) });
    const res = await attachTerminal("sess-a", deps);
    expect(deps.open).toHaveBeenCalledWith("sess-a");
    expect(res).toEqual({ attached: true, stream, snapshot: snap("history-bytes", 42) });
    expect(stream.close).not.toHaveBeenCalled();
  });

  it("a session the host no longer holds reports ended (read-only 'session ended')", async () => {
    const deps = makeDeps({
      open: vi.fn(async () => Promise.reject("subscribe_terminal: session not found: sess-gone")),
    });
    expect(await attachTerminal("sess-gone", deps)).toEqual({ attached: false, ended: true });
  });

  it("a transport failure is not an ended session: it carries the reason for Retry", async () => {
    const deps = makeDeps({
      open: vi.fn(async () => Promise.reject(new Error("terminal server did not reply"))),
    });
    expect(await attachTerminal("sess-a", deps)).toEqual({
      attached: false,
      ended: false,
      incompatible: false,
      error: "terminal server did not reply",
    });
  });

  it("a refusal by an incompatible terminal server is marked incompatible", async () => {
    const error = "restart terminal server (ends 2 sessions): it speaks protocol 6, this app 7";
    const deps = makeDeps({ open: vi.fn(async () => Promise.reject(new Error(error))) });
    expect(await attachTerminal("sess-a", deps)).toEqual({
      attached: false,
      ended: false,
      incompatible: true,
      error,
    });
  });

  it("no snapshot in time gives up, and closes the stream when it opens late", async () => {
    vi.useFakeTimers();
    try {
      const stream = liveStream(sub(9));
      let open!: (s: LiveStream) => void;
      const deps = makeDeps({ open: vi.fn(() => new Promise<LiveStream>((r) => (open = r))) });
      const pending = attachTerminal("sess-a", deps);
      await vi.advanceTimersByTimeAsync(ATTACH_TIMEOUT_MS);
      expect(await pending).toMatchObject({ attached: false, ended: false, incompatible: false });
      open(stream);
      await vi.advanceTimersByTimeAsync(0);
      expect(stream.close).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it("AC-24: disposed while subscribing detaches the viewport it registered, never kills", async () => {
    let aborted = false;
    const stream = liveStream(sub(3));
    const deps = makeDeps({
      open: vi.fn(async () => {
        aborted = true;
        return stream;
      }),
      isAborted: () => aborted,
    });
    expect(await attachTerminal("sess-a", deps)).toBeNull();
    expect(stream.close).toHaveBeenCalledOnce();
  });

  it("disposed before a failed subscribe resolves reports nothing", async () => {
    let aborted = false;
    const deps = makeDeps({
      open: vi.fn(async () => {
        aborted = true;
        throw new Error("gone");
      }),
      isAborted: () => aborted,
    });
    expect(await attachTerminal("sess-a", deps)).toBeNull();
  });
});

it("closes and drops a private stream whose authority retired before the attach settled", async () => {
  const stream = { ...liveStream(sub(19, snap("private history", 15))), isCurrent: () => false };
  const result = await attachTerminal("private", makeDeps({ open: async () => stream }));
  expect(result).toBeNull();
  expect(stream.close).toHaveBeenCalledOnce();
});
