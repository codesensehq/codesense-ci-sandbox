// SPDX-License-Identifier: MIT
import { listen } from "@tauri-apps/api/event";

import { safeUnlisten } from "./tauri-listeners";

/**
 * SW-GS-2d (AC-3b) — the ping/pong handshake that gates "connected" on a proven
 * round-trip, not raw process liveness. The prime (`agent-chat-prime`) is the
 * ping: it asks the agent to echo a one-shot token through the
 * `post_agent_message` MCP verb. This module awaits that pong on the existing
 * `mcp:agent-message` channel, matched by `sessionId` + token, within a bounded
 * timeout — so the full loop (UI→PTY→agent→`post_agent_message`→UI) is what
 * proves the connection. No git-sync/Claude literal (AC-4a) — any workspace
 * agent surface reuses it.
 *
 * The pong is a LIGHTWEIGHT ack — NOT 2c's blocking `ask_user`/reply channel.
 * Correlation rides the message TEXT (the `mcp:agent-message` payload carries no
 * correlation field, by design), so this stays FE-only and doesn't preempt 2c.
 */

/** The `mcp:agent-message` event payload (mirror of the Rust emit + the chat
 *  listener). */
export interface AgentMessagePayload {
  sessionId: string;
  role?: string;
  text: string;
  /** SW-WV-F6 — the render tripwire's routing decision, stamped Rust-side on the
   *  `mcp:agent-message` emit. Narrowed to the three valid values by the
   *  listener before it reaches the chat store. */
  renderTarget?: string;
}

/** Register a handler on the agent→UI message channel; resolves to an unlisten
 *  fn. Injectable so the handshake unit-tests without Tauri. */
export type AgentMessageSubscribe = (
  handler: (payload: AgentMessagePayload) => void,
) => Promise<() => void>;

/** The real Tauri-backed subscription to `mcp:agent-message`. */
export const subscribeAgentMessages: AgentMessageSubscribe = (handler) =>
  listen<AgentMessagePayload>("mcp:agent-message", (event) => handler(event.payload));

/** Timer seam so the timeout path is deterministic under test. */
export interface HandshakeTimer {
  set: (cb: () => void, ms: number) => unknown;
  clear: (handle: unknown) => void;
}

const realTimer: HandshakeTimer = {
  set: (cb, ms) => setTimeout(cb, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export interface AwaitPongOptions {
  /** The bound session the pong must self-report. */
  sessionId: string;
  /** The one-shot token the agent must echo in its pong text. */
  token: string;
  /** Bounded wait — no pong by then ⇒ resolves false (G4: never an indefinite wait). */
  timeoutMs: number;
  /** Event-channel registration (default: real `mcp:agent-message`). */
  subscribe?: AgentMessageSubscribe;
  /** Invoked AFTER the listener is live — the caller primes here, so a fast pong
   *  can't arrive before we're listening (no lost-pong race). */
  onListening?: () => void | Promise<void>;
  /** Invoked every `nudgeMs` until pong/timeout — used to send a discrete Enter
   *  that submits a prime the agent batched as a paste (held the trailing Enter
   *  as content). No-op when `nudgeMs` is unset. */
  onNudge?: () => void | Promise<void>;
  nudgeMs?: number;
  /** Timer seam (default: real setTimeout). */
  timer?: HandshakeTimer;
}

/**
 * Resolve `true` when a `post_agent_message` whose `sessionId` matches AND whose
 * `text` contains the `token` arrives within `timeoutMs`; otherwise `false`.
 * Always cleans up the listener + timer. The listener is registered BEFORE
 * `onListening` runs, so the caller's prime is guaranteed to fire into a live
 * subscription.
 */
export async function awaitPong(opts: AwaitPongOptions): Promise<boolean> {
  const subscribe = opts.subscribe ?? subscribeAgentMessages;
  const timer = opts.timer ?? realTimer;

  return new Promise<boolean>((resolve) => {
    let settled = false;
    let unlisten: (() => void) | null = null;
    let nudgeHandle: unknown = null;

    const finish = (result: boolean) => {
      if (settled) return;
      settled = true;
      timer.clear(timerHandle);
      if (nudgeHandle !== null) timer.clear(nudgeHandle);
      if (unlisten) safeUnlisten(unlisten);
      resolve(result);
    };

    // Arm the timeout FIRST (G4): no `subscribe` hang/rejection or `onListening`
    // throw can wedge the wait — the bounded timer always resolves it to false.
    const timerHandle = timer.set(() => finish(false), opts.timeoutMs);

    // Recurring Enter-nudge: re-fires `onNudge` every `nudgeMs` until the pong
    // (or timeout) settles us, so a paste-held prime gets a discrete Enter that
    // submits it. Self-reschedules; cleared in `finish`.
    const scheduleNudge = () => {
      if (!opts.onNudge || !opts.nudgeMs) return;
      nudgeHandle = timer.set(() => {
        if (settled) return;
        void Promise.resolve(opts.onNudge?.())
          .catch(() => {})
          .finally(() => {
            if (!settled) scheduleNudge();
          });
      }, opts.nudgeMs);
    };

    subscribe((payload) => {
      if (payload.sessionId === opts.sessionId && payload.text.includes(opts.token)) {
        finish(true);
      }
    })
      .then(async (un) => {
        // If the timeout already fired during registration, tear down immediately.
        if (settled) {
          safeUnlisten(un);
          return;
        }
        unlisten = un;
        await opts.onListening?.();
        scheduleNudge();
      })
      // A failed subscription (or a throwing prime) must NOT hang the surface —
      // resolve to a not-verified result so the AC-3a recovery path engages.
      .catch(() => finish(false));
  });
}
