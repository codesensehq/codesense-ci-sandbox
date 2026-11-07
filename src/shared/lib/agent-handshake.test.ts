// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { awaitPong, type AgentMessagePayload } from "@/shared/lib/agent-handshake";

/** A controllable subscribe seam: captures the handler so a test can push a
 *  simulated `post_agent_message` pong, and records unlisten cleanup. */
function fakeChannel() {
  let handler: ((p: AgentMessagePayload) => void) | null = null;
  const unlisten = vi.fn();
  const subscribe = vi.fn(async (h: (p: AgentMessagePayload) => void) => {
    handler = h;
    return unlisten;
  });
  return {
    subscribe,
    unlisten,
    emit: (p: AgentMessagePayload) => handler?.(p),
    get handler() {
      return handler;
    },
  };
}

/** A manual timer seam so the timeout path fires deterministically. */
function manualTimer() {
  let cb: (() => void) | null = null;
  return {
    timer: {
      set: (fn: () => void) => {
        cb = fn;
        return 1;
      },
      clear: vi.fn(),
    },
    fire: () => cb?.(),
  };
}

describe("awaitPong (AC-3b ping/pong)", () => {
  it("primes ONLY after the listener is live (no lost-pong race), then resolves true on a matching pong", async () => {
    const ch = fakeChannel();
    const order: string[] = [];
    const onListening = vi.fn(async () => {
      // The listener must already be registered when we prime.
      expect(ch.handler).not.toBeNull();
      order.push("prime");
    });

    const pong = awaitPong({
      sessionId: "shell-1",
      token: "cs-ack-xyz",
      timeoutMs: 1000,
      subscribe: ch.subscribe,
      onListening,
    });
    // Let the subscribe + onListening microtasks flush, then deliver the pong.
    await Promise.resolve();
    await Promise.resolve();
    order.push("pong");
    ch.emit({ sessionId: "shell-1", text: "hello! cs-ack-xyz" });

    expect(await pong).toBe(true);
    expect(onListening).toHaveBeenCalledTimes(1);
    expect(order).toEqual(["prime", "pong"]);
    expect(ch.unlisten).toHaveBeenCalledTimes(1); // cleaned up
  });

  it("ignores an uncorrelated message — wrong session id OR missing token does NOT satisfy the handshake", async () => {
    const ch = fakeChannel();
    const mt = manualTimer();
    const pong = awaitPong({
      sessionId: "shell-1",
      token: "cs-ack-xyz",
      timeoutMs: 1000,
      subscribe: ch.subscribe,
      timer: mt.timer,
    });
    await Promise.resolve();
    await Promise.resolve();
    ch.emit({ sessionId: "shell-OTHER", text: "hello cs-ack-xyz" }); // wrong session
    ch.emit({ sessionId: "shell-1", text: "hello, no token here" }); // missing token
    mt.fire(); // timeout
    expect(await pong).toBe(false);
    expect(ch.unlisten).toHaveBeenCalledTimes(1);
  });

  it("resolves false (never hangs) if the subscription rejects — G4 wedge guard", async () => {
    const mt = manualTimer();
    const subscribe = vi.fn(async () => {
      throw new Error("listen failed");
    });
    const pong = awaitPong({
      sessionId: "shell-1",
      token: "cs-ack-xyz",
      timeoutMs: 1000,
      subscribe,
      timer: mt.timer,
    });
    expect(await pong).toBe(false);
  });

  it("resolves false (never hangs) if onListening throws — a failed prime can't wedge the wait", async () => {
    const ch = fakeChannel();
    const mt = manualTimer();
    const pong = awaitPong({
      sessionId: "shell-1",
      token: "cs-ack-xyz",
      timeoutMs: 1000,
      subscribe: ch.subscribe,
      onListening: async () => {
        throw new Error("prime failed");
      },
      timer: mt.timer,
    });
    expect(await pong).toBe(false);
  });

  it("nudges Enter every nudgeMs until the pong lands, then stops (paste-held submit)", async () => {
    const ch = fakeChannel();
    // A by-interval timer seam: track the latest cb per interval (the timeout vs
    // the self-rescheduling nudge), keyed by the ms passed.
    const nudge: { cb: (() => void) | null } = { cb: null };
    const timer = {
      set: (fn: () => void, ms: number) => {
        if (ms === 1500) nudge.cb = fn;
        return ms;
      },
      clear: vi.fn(),
    };
    const onNudge = vi.fn();
    const flush = async () => {
      await Promise.resolve();
      await Promise.resolve();
    };

    const pong = awaitPong({
      sessionId: "shell-1",
      token: "cs-ack-xyz",
      timeoutMs: 99_999,
      subscribe: ch.subscribe,
      onNudge,
      nudgeMs: 1500,
      timer,
    });
    await flush();

    nudge.cb?.(); // 1st nudge
    await flush();
    nudge.cb?.(); // 2nd nudge (rescheduled)
    await flush();
    expect(onNudge).toHaveBeenCalledTimes(2);

    ch.emit({ sessionId: "shell-1", text: "hello cs-ack-xyz" }); // pong
    expect(await pong).toBe(true);

    // After the pong, further timer fires must NOT nudge (settled).
    nudge.cb?.();
    await flush();
    expect(onNudge).toHaveBeenCalledTimes(2);
  });

  it("resolves false on timeout when no pong arrives, and cleans up", async () => {
    const ch = fakeChannel();
    const mt = manualTimer();
    const pong = awaitPong({
      sessionId: "shell-1",
      token: "cs-ack-xyz",
      timeoutMs: 1000,
      subscribe: ch.subscribe,
      timer: mt.timer,
    });
    await Promise.resolve();
    await Promise.resolve();
    mt.fire();
    expect(await pong).toBe(false);
    expect(ch.unlisten).toHaveBeenCalledTimes(1);
  });
});
