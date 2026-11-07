// SPDX-License-Identifier: MIT
import { useViewerStore, type TerminalAiStatus } from "@/shared/stores/viewer-store";

/**
 * SW-DX-10 (AC-6) — the session-scoped "working…" liveness signal for the chat
 * surface.
 *
 * `working` is a projection of the agent's OWN hook-driven activity status:
 * `aiStatus === "running"` ⇒ the agent is mid-turn / producing output ⇒ working;
 * anything else (`done` / `error` / `needs-permission` / absent) ⇒ idle. That
 * status is reliable for the DX-7-launched WORKTREE sessions DX-10 chats with —
 * the managed worktree installs the notify hook that maintains it — and it flips
 * to `done` on the agent's Stop hook / `signal_done` (idle), exactly the
 * transitions AC-6 wants.
 *
 * Deliberately NOT a new signal: it introduces no streaming verb, taps no
 * terminal output for chat text, and adds no backend emit/command — it reads an
 * already-maintained store slice, so the `commands.contract` count stays green
 * (AC-9/AC-10). (Design note: the story's original `appended_seq` output-cadence
 * idea is unreliable on the FE — output rides per-viewport streams that exist
 * only while a pane is attached, so cadence reads false-idle; `aiStatus` is the
 * correct reused signal.)
 */
export function selectSessionWorking(sessionId: string | null) {
  return (s: { aiStatusBySession: Record<string, TerminalAiStatus> }): boolean =>
    sessionId ? s.aiStatusBySession[sessionId] === "running" : false;
}

/** React hook form: re-renders only when THIS session's working flag flips. */
export function useSessionWorking(sessionId: string | null): boolean {
  return useViewerStore(selectSessionWorking(sessionId));
}
