// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";

import { CheckCircle2, SendHorizontal, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { sendTextToSession } from "@/shared/lib/send-to-terminal";

/**
 * SW-AV-9 — the Answer / Steer composer: the human's explicit place to TYPE a
 * message to an agent from a Weft Board row. Opened by a "waiting on your answer"
 * (question/blocked) row's **Answer** verb or a running row's **Steer** verb, it
 * lands focus on the input, sends through the SAME durable message path the in-app
 * chat uses (`sendTextToSession(..., { submit: true })` → `submit_pty_line`: settle
 * → write → confirm → nudge), and confirms only after a CONFIRMED landing.
 *
 * It invents no transport. Send-enablement (`canSend` / `disabledReason`) is decided
 * by the caller from the row's reachability, so a gone target degrades honestly —
 * the box is disabled and explains why, never a silent send into the void. Even when
 * enabled, a runtime send failure surfaces the error and withholds the confirmation
 * (no phantom "sent"). Deliberately leaner than `AgentChatSurface` (no transcript):
 * this is a one-shot compose-and-send affordance grafted onto a board row.
 */
export interface WeftAnswerComposerProps {
  /** The row's live agent session id; `null`/absent disables sending (no target). */
  sessionId: string | null;
  /** Whether the target is reachable (caller derives from the row's resolvability). */
  canSend: boolean;
  /** Shown when `!canSend` — the honest "why you can't send" explanation. */
  disabledReason?: string;
  /** The verb that opened the composer ("Answer" / "Steer") — labels the surface. */
  verb: string;
  /** Collapse the composer (the row's toggle owns the open state). */
  onClose: () => void;
  className?: string;
}

export function WeftAnswerComposer({
  sessionId,
  canSend,
  disabledReason,
  verb,
  onClose,
  className,
}: WeftAnswerComposerProps): React.JSX.Element {
  const [text, setText] = useState("");
  // SW-AV-9: in-flight guard (no double-send) — `sending` drives the disabled UI,
  // `sendingRef` is the SYNCHRONOUS guard that closes the double-submit race a state
  // flag can't (two Enter keydowns in one render tick both read the pre-update flag),
  // mirroring `AgentChatSurface`'s discipline.
  const [sending, setSending] = useState(false);
  const sendingRef = useRef(false);
  const [sent, setSent] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // SW-AV-9: land focus on the input the moment the composer opens (the "Answer
  // lands on the input" contract) — a keyboard user starts typing immediately.
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const sendable = canSend && sessionId !== null && text.trim().length > 0 && !sending;

  const send = async () => {
    if (!sendable || sessionId === null || sendingRef.current) return;
    // Capture the exact value we're sending — the submit can take a beat and the box
    // stays editable meanwhile, so we only clear if the user hasn't typed anew.
    const original = text;
    const message = original.trim();
    sendingRef.current = true;
    setSending(true);
    setSendError(null);
    setSent(false);
    try {
      // The DURABLE message path: `{ submit: true }` routes through `submit_pty_line`
      // (settle → write → confirm-from-output → discrete-Enter nudge) so the turn
      // reliably registers on the agent's input. `focusVisibleTerminal: false` keeps
      // focus in this composer (the target isn't necessarily the visible xterm). Await
      // before confirming so a dropped/gone-session write never shows a phantom "sent".
      await sendTextToSession(sessionId, message, { submit: true, focusVisibleTerminal: false });
      setSent(true);
      // Clear only if the box still holds exactly what we sent — never wipe a fresh
      // draft typed during the in-flight submit.
      setText((cur) => (cur === original ? "" : cur));
    } catch (e) {
      // Keep the composed text and surface the failure — no phantom confirmation, no
      // silent send into the void.
      setSendError(e instanceof Error ? e.message : String(e));
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  };

  return (
    <div
      data-testid="weft-answer-composer"
      // A composer click is its own — never bubble up into an ancestor row's drill.
      onClick={(e) => e.stopPropagation()}
      className={cn("flex flex-col gap-1 px-3 pb-2 pl-10", className)}
    >
      <div className="flex items-end gap-2">
        <textarea
          ref={inputRef}
          data-testid="weft-answer-composer-input"
          aria-label={`${verb} the agent`}
          value={text}
          disabled={!canSend}
          onChange={(e) => {
            setText(e.target.value);
            if (sendError) setSendError(null);
            if (sent) setSent(false);
          }}
          onKeyDown={(e) => {
            // Enter sends; Shift+Enter inserts a newline; Escape collapses.
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send();
            } else if (e.key === "Escape") {
              e.preventDefault();
              onClose();
            }
          }}
          placeholder={
            canSend ? `Message the agent — ${verb.toLowerCase()}…` : "Can't reach this agent"
          }
          rows={2}
          className={cn(
            "flex-1 resize-none rounded-md border border-border bg-background px-2 py-1.5 text-sm outline-none focus-visible:border-ring",
            !canSend && "opacity-50",
          )}
        />
        <div className="flex flex-col gap-1">
          <Button
            size="sm"
            data-testid="weft-answer-composer-send"
            onClick={() => void send()}
            disabled={!sendable}
            aria-label="Send message"
          >
            <SendHorizontal className="size-4" aria-hidden />
          </Button>
          <Button
            size="sm"
            variant="ghost"
            data-testid="weft-answer-composer-close"
            onClick={onClose}
            aria-label="Close composer"
          >
            <X className="size-4" aria-hidden />
          </Button>
        </div>
      </div>
      {/* SW-AV-9: the confirmation — a sent/acknowledged signal, rendered ONLY after a
          confirmed landing on the durable path. */}
      {sent && (
        <p
          role="status"
          data-testid="weft-answer-composer-sent"
          className="flex items-center gap-1 text-[0.7rem] text-muted-foreground"
        >
          <CheckCircle2 className="size-3" aria-hidden />
          Sent to the agent.
        </p>
      )}
      {sendError && (
        <p role="alert" className="text-[0.7rem] text-destructive">
          Couldn't send — {sendError}. Your message is still here; try again.
        </p>
      )}
      {!canSend && disabledReason && (
        <p
          data-testid="weft-answer-composer-disabled"
          className="text-[0.7rem] text-muted-foreground"
        >
          {disabledReason}
        </p>
      )}
    </div>
  );
}
