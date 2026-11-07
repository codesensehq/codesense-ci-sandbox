// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";

import { verifyAgentLive } from "@/shared/lib/agent-liveness";
import type { AgentMessagePayload } from "@/shared/lib/agent-handshake";

/** Controllable subscribe seam: capture the handler so a test can push a simulated
 *  pong, and record unlisten cleanup. Mirrors the agent-handshake test harness. */
function fakeChannel() {
  let handler: ((p: AgentMessagePayload) => void) | null = null;
  const unlisten = vi.fn();
  const subscribe = vi.fn(async (h: (p: AgentMessagePayload) => void) => {
    handler = h;
    return unlisten;
  });
  return { subscribe, unlisten, emit: (p: AgentMessagePayload) => handler?.(p) };
}

/** By-ms timer seam: `verifyAgentLive` arms BOTH a timeout (at `timeoutMs`) and a
 *  self-rescheduling nudge (at `nudgeMs`), so we key the captured callback by ms to
 *  fire the timeout deterministically without the nudge clobbering it. */
function manualTimer(timeoutMs: number) {
  let timeoutCb: (() => void) | null = null;
  return {
    timer: {
      set: (fn: () => void, ms: number) => {
        if (ms === timeoutMs) timeoutCb = fn;
        return ms;
      },
      clear: vi.fn(),
    },
    fire: () => timeoutCb?.(),
  };
}

describe("@P1 verifyAgentLive (SW-GS-24)", () => {
  it("mints a token, primes with it, and resolves TRUE on a matching pong", async () => {
    const ch = fakeChannel();
    const prime = vi.fn(async () => {});
    const p = verifyAgentLive("sid-1", {
      subscribe: ch.subscribe,
      prime,
      mintToken: () => "cs-ack-fixed",
      nudge: vi.fn(),
    });
    await Promise.resolve();
    await Promise.resolve();
    // Primed with the same session + minted token the pong must echo.
    expect(prime).toHaveBeenCalledWith("sid-1", "cs-ack-fixed");
    ch.emit({ sessionId: "sid-1", text: "hi — cs-ack-fixed" });
    expect(await p).toBe(true);
    expect(ch.unlisten).toHaveBeenCalledTimes(1); // cleaned up
  });

  it("resolves FALSE on timeout (dead/drifted session) without hanging", async () => {
    const ch = fakeChannel();
    const mt = manualTimer(1000);
    const p = verifyAgentLive("sid-1", {
      timeoutMs: 1000,
      subscribe: ch.subscribe,
      timer: mt.timer,
      prime: vi.fn(async () => {}),
      nudge: vi.fn(),
      mintToken: () => "cs-ack-fixed",
    });
    await Promise.resolve();
    await Promise.resolve();
    mt.fire(); // no pong within the budget
    expect(await p).toBe(false);
    expect(ch.unlisten).toHaveBeenCalledTimes(1);
  });

  it("ignores an uncorrelated pong (wrong token) — the stale reply can't fake liveness", async () => {
    const ch = fakeChannel();
    const mt = manualTimer(1000);
    const p = verifyAgentLive("sid-1", {
      timeoutMs: 1000,
      subscribe: ch.subscribe,
      timer: mt.timer,
      prime: vi.fn(async () => {}),
      nudge: vi.fn(),
      mintToken: () => "cs-ack-fixed",
    });
    await Promise.resolve();
    await Promise.resolve();
    ch.emit({ sessionId: "sid-1", text: "hello — cs-ack-STALE" }); // wrong token
    mt.fire();
    expect(await p).toBe(false);
  });
});
