// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SW-GS-2b (B) — the generalized, per-session agent↔UI chat transcript.
 *
 * Keyed by `sessionId` (a workspace's agent tab), so ANY session can have a
 * transcript — git-sync is the first consumer, story/epic agents reuse it
 * unchanged. Holds only messages that are deliberately surfaced: inbound agent
 * messages (the `post_agent_message` MCP verb → `mcp:agent-message` event, via
 * `useAgentMessageListener`) and outbound user messages (the chat surface). It
 * is NOT a scrape of the raw PTY stream (AC-5a) — the terminal remains the full
 * watchable stream; this is the curated conversation.
 *
 * Selector discipline: read via `useAgentChatStore((s) => s.field)`; never
 * destructure the whole store.
 */
export type ChatRole = "agent" | "user";

/** SW-WV-F6 — where the controller's reply was routed by the render tripwire.
 *  Set once on append from the `mcp:agent-message` payload; never mutated. */
export type RenderTarget = "terminal" | "drill_doc" | "artifact";

export interface ChatMessage {
  id: string;
  role: ChatRole;
  text: string;
  timestamp: number;
  /** SW-WV-F6 — the tripwire routing decision; drives the `[doc]`/`[artifact]`
   *  bubble suffix. Absent (or `"terminal"`) → no suffix. */
  renderTarget?: RenderTarget;
}

interface AgentChatState {
  /** Per-session transcript, keyed by `sessionId`. */
  transcripts: Record<string, ChatMessage[]>;
  /** Append a message to a session's transcript. `renderTarget` (SW-WV-F6) is
   *  optional — omitted for user messages and pre-F6 callers. */
  appendMessage: (
    sessionId: string,
    role: ChatRole,
    text: string,
    renderTarget?: RenderTarget,
  ) => void;
  /** Drop a session's transcript (e.g. on hard-close). */
  clearTranscript: (sessionId: string) => void;
}

// Module-monotonic id source — stable React keys without Date/Math.random in
// the hot path; identity only, not ordering (timestamp carries ordering).
let seq = 0;

/** Cap per-session history so a long-lived, chatty agent can't grow a
 *  transcript without bound. Keeps the most recent messages (the visible tail);
 *  older ones drop off. Generous enough that normal use never trims. */
const MAX_MESSAGES_PER_SESSION = 500;

export const useAgentChatStore = create<AgentChatState>((set) => ({
  transcripts: {},

  appendMessage: (sessionId, role, text, renderTarget) =>
    set((s) => {
      const message: ChatMessage = {
        id: `m${++seq}`,
        role,
        text,
        timestamp: Date.now(),
        ...(renderTarget ? { renderTarget } : {}),
      };
      const prev = s.transcripts[sessionId] ?? [];
      const next = [...prev, message];
      // Trim to the most recent cap so the transcript can't grow unbounded.
      const capped =
        next.length > MAX_MESSAGES_PER_SESSION ? next.slice(-MAX_MESSAGES_PER_SESSION) : next;
      return { transcripts: { ...s.transcripts, [sessionId]: capped } };
    }),

  clearTranscript: (sessionId) =>
    set((s) => {
      if (!(sessionId in s.transcripts)) return s;
      const next = { ...s.transcripts };
      delete next[sessionId];
      return { transcripts: next };
    }),
}));

/** Test-only reset. */
export function resetAgentChatStoreForTests(): void {
  seq = 0;
  useAgentChatStore.setState({ transcripts: {} });
}
