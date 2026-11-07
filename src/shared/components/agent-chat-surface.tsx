// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { SendHorizontal, Loader2, Circle } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { sendTextToSession } from "@/shared/lib/send-to-terminal";
import { useAgentChatStore } from "@/shared/stores/use-agent-chat-store";
import { beginViewingSession, endViewingSession } from "@/shared/lib/transcript-prune";

/**
 * SW-GS-2b (B/AC-7) — the reusable, two-way agent↔UI chat surface, bindable to
 * ANY workspace agent session by `sessionId`. It renders the per-session
 * transcript (agent + user messages, from `useAgentChatStore`) and a send box,
 * fully DECOUPLED from the terminal: the user converses without the xterm
 * focused, while the live terminal stays watchable in the tray.
 *
 * Generic by construction — no Claude specifics. A story/epic agent surface
 * mounts it bound to a per-instance session. Send-enablement (`canSend` /
 * `disabledReason`) is decided by the CALLER from its ownership/connection
 * state, so the surface itself carries no ownership semantics.
 */
export interface AgentChatSurfaceProps {
  /** The bound agent session id; `null` disables sending (no live session). */
  sessionId: string | null;
  /** Whether the send box is enabled (caller derives from ownership/connection). */
  canSend: boolean;
  /** Shown when `!canSend`. */
  disabledReason?: string;
  /**
   * SW-DX-10 (AC-6): the bound session's "working…" liveness. `true` → the agent
   * is mid-turn; `false` → idle. `undefined` (default) hides the lamp entirely,
   * so callers that don't derive liveness are unaffected.
   */
  working?: boolean;
  className?: string;
}

export function AgentChatSurface({
  sessionId,
  canSend,
  disabledReason,
  working,
  className,
}: AgentChatSurfaceProps): React.JSX.Element {
  const [text, setText] = useState("");
  // SW-GS-23: in-flight guard (no double-send) + a local send-failure surface.
  // `sending` drives the disabled UI; `sendingRef` is the SYNCHRONOUS guard that
  // closes the double-send race a state flag can't (two Enter keydowns in one
  // render tick both read the pre-update `sending` via closure) — parity with the
  // conflict path's `publishingRef`.
  const [sending, setSending] = useState(false);
  const sendingRef = useRef(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const messages = useAgentChatStore((s) => (sessionId ? s.transcripts[sessionId] : undefined));
  const scrollRef = useRef<HTMLDivElement | null>(null);

  // Keep the latest message in view as the transcript grows.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages]);

  // RF-perf-15 (F-5/OQ-2): register as a viewer while this surface shows the
  // session's transcript. A deferred prune is suppressed while viewed and armed
  // only once the last viewer leaves (and only if the session is dead) — so the
  // transcript can't vanish out from under a reader, even if the session dies
  // while the surface is open, and a live session's transcript is never dropped.
  useEffect(() => {
    if (!sessionId) return;
    beginViewingSession(sessionId);
    return () => endViewingSession(sessionId);
  }, [sessionId]);

  const sendable = canSend && sessionId !== null && text.trim().length > 0 && !sending;

  const send = async () => {
    if (!sendable || sessionId === null || sendingRef.current) return;
    // Capture the exact box value we're sending — the confirm/nudge submit can take
    // a beat, and the composer stays editable meanwhile (non-blocking), so we must
    // only clear if the user hasn't started composing something new in the interim.
    const original = text;
    const message = original.trim();
    sendingRef.current = true;
    setSending(true);
    setSendError(null);
    try {
      // UI→agent: submit into the BOUND session's PTY (not the active tab). SW-GS-23:
      // `{ submit }` routes through submit_pty_line (settle → write → confirm →
      // nudge), so the Enter reliably registers; pass the RAW message (it owns the
      // Enter). Await before recording/clearing so a dropped write never leaves a
      // phantom "sent" line in the transcript.
      await sendTextToSession(sessionId, message, { submit: true, focusVisibleTerminal: false });
      useAgentChatStore.getState().appendMessage(sessionId, "user", message);
      // Clear only if the box still holds exactly what we sent — never wipe a fresh
      // draft the user typed during the in-flight submit.
      setText((cur) => (cur === original ? "" : cur));
    } catch (e) {
      // Keep the composed text in the box and surface the error — no phantom entry.
      setSendError(e instanceof Error ? e.message : String(e));
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  };

  return (
    <div className={cn("flex min-h-0 flex-col gap-2", className)}>
      {/* SW-DX-10 (AC-6): the "working…" liveness lamp — a projection of the
          agent's own activity status, NOT a token stream. Hidden when the caller
          passes no `working` flag. */}
      {working !== undefined && (
        <div
          data-testid="agent-liveness-lamp"
          data-working={working}
          className="flex items-center gap-1.5 text-[0.7rem] text-muted-foreground"
        >
          {working ? (
            <>
              <Loader2 className="size-3 animate-spin" aria-hidden />
              <span>working…</span>
            </>
          ) : (
            <>
              <Circle className="size-3" aria-hidden />
              <span>idle</span>
            </>
          )}
        </div>
      )}
      <div
        ref={scrollRef}
        data-testid="agent-chat-transcript"
        className="flex-1 min-h-0 space-y-2 overflow-auto rounded-md border border-border bg-background/40 p-2 text-sm"
      >
        {!messages || messages.length === 0 ? (
          <p className="text-muted-foreground">No messages yet.</p>
        ) : (
          messages.map((m) => (
            <div
              key={m.id}
              data-role={m.role}
              className={cn(
                "max-w-[85%] rounded-md px-2 py-1",
                m.role === "user"
                  ? "ml-auto bg-primary/10 text-foreground"
                  : "mr-auto bg-muted text-foreground",
              )}
            >
              {m.text}
              {/* SW-WV-F6: render-tripwire footnote — a muted suffix showing the
                  reply was routed to a drill-down doc or an artifact. Absent for
                  terminal-target (or untagged) messages. */}
              {m.renderTarget === "drill_doc" && (
                <span className="ml-1 text-[0.65rem] text-muted-foreground/60">[doc]</span>
              )}
              {m.renderTarget === "artifact" && (
                <span className="ml-1 text-[0.65rem] text-muted-foreground/60">[artifact]</span>
              )}
            </div>
          ))
        )}
      </div>

      <div className="flex flex-col gap-1">
        <div className="flex items-end gap-2">
          <textarea
            aria-label="Message the agent"
            value={text}
            disabled={!canSend}
            onChange={(e) => {
              setText(e.target.value);
              if (sendError) setSendError(null);
            }}
            onKeyDown={(e) => {
              // Enter sends; Shift+Enter inserts a newline.
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void send();
              }
            }}
            placeholder={canSend ? "Message the agent…" : "Reconnect to steer the agent…"}
            rows={2}
            className={cn(
              "flex-1 resize-none rounded-md border border-border bg-background px-2 py-1.5 text-sm outline-none focus-visible:border-ring",
              !canSend && "opacity-50",
            )}
          />
          <Button
            size="sm"
            onClick={() => void send()}
            disabled={!sendable}
            aria-label="Send message"
          >
            <SendHorizontal className="size-4" aria-hidden />
          </Button>
        </div>
        {sendError && (
          <p role="alert" className="text-[0.7rem] text-destructive">
            Couldn't send — {sendError}. Your message is still here; try again.
          </p>
        )}
        {!canSend && disabledReason && (
          <p className="text-[0.7rem] text-muted-foreground">{disabledReason}</p>
        )}
      </div>
    </div>
  );
}
