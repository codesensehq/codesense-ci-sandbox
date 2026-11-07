// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";

import { CheckCircle2, SendHorizontal, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { sendTextToSession } from "@/shared/lib/send-to-terminal";

import type { InboxItem } from "../lib/inbox-model";

/**
 * SLW-TL-7 — the Inbox's durable Reply composer (north-star §2.3, §8.14). A post is tied
 * to the RUN, not the ephemeral session, so the SAME "Reply" degrades by liveness under
 * ONE constant label — live-vs-exited is the system's problem, never the human's (§8.5):
 *  - **run still live** → deliver inline: the reply types straight into the waiting agent
 *    through the SAME durable path the in-app chat uses
 *    (`sendTextToSession(..., { submit: true })` → settle → write → confirm → nudge).
 *  - **run exited** (the common cron case) → the reply RE-DISPATCHES the automation as a
 *    continuation run carrying the reply + original context (`onRedispatch`, wired by the
 *    panel). v1 re-dispatches a fresh continuation — no mid-run resume (SW-AU-92 later).
 *
 * The label stays "Reply" either way, no action resolves to nothing, and pressing Reply
 * ALWAYS acknowledges it landed — a confirmation on the durable send, or "Sent — the agent
 * will pick this up" on the re-dispatch — so a silent re-dispatch never looks dead. A
 * runtime failure surfaces the error and WITHHOLDS the confirmation (no phantom "sent").
 * It invents no transport of its own; it reuses the shipped mechanisms.
 */
export interface InboxAgentReplyComposerProps {
  item: InboxItem;
  /** Whether the run's session is live AND reachable here (the panel resolves this from
   *  the same roster/tab signals the CLI door uses). Drives inline-vs-redispatch routing. */
  live: boolean;
  /** The live agent session id when `live`; `null`/absent on the exited path. */
  liveSessionId: string | null;
  /** Re-dispatch the automation as a continuation carrying the reply + original context.
   *  Called only on the exited path. Rejects → the composer shows the error, no phantom. */
  onRedispatch: (item: InboxItem, reply: string) => Promise<void>;
  /** Collapse the composer (the row owns the open state). */
  onClose: () => void;
  /** Fired after a CONFIRMED landing (live send or re-dispatch) — lets the item clear. */
  onSent?: () => void;
  className?: string;
}

export function InboxAgentReplyComposer({
  item,
  live,
  liveSessionId,
  onRedispatch,
  onClose,
  onSent,
  className,
}: InboxAgentReplyComposerProps): React.JSX.Element {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const sendingRef = useRef(false);
  // A confirmed landing — a live-inline send or an exited re-dispatch — each with its own
  // honest acknowledgement copy (never a runtime word in the button itself).
  const [sentKind, setSentKind] = useState<"inline" | "redispatch" | null>(null);
  const [sendError, setSendError] = useState<string | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // Land focus on the input the moment the composer opens — a keyboard user starts typing.
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const sendable = text.trim().length > 0 && !sending;

  const send = async () => {
    if (!sendable || sendingRef.current) return;
    const original = text;
    const message = original.trim();
    sendingRef.current = true;
    setSending(true);
    setSendError(null);
    setSentKind(null);
    try {
      if (live && liveSessionId) {
        // The durable message path — confirm on a CONFIRMED landing (await before the ack
        // so a dropped write never shows a phantom "sent").
        await sendTextToSession(liveSessionId, message, {
          submit: true,
          focusVisibleTerminal: false,
        });
        setSentKind("inline");
      } else {
        // The run has exited: the SAME Reply re-dispatches a continuation carrying the
        // reply + original context. Invisible to the human — the label did not change.
        await onRedispatch(item, message);
        setSentKind("redispatch");
      }
      // Clear only if the box still holds exactly what we sent.
      setText((cur) => (cur === original ? "" : cur));
      onSent?.();
    } catch (e) {
      // Keep the composed text and surface the failure — no phantom confirmation.
      setSendError(e instanceof Error ? e.message : String(e));
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  };

  return (
    <div
      data-testid="inbox-reply-composer"
      // A composer click is its own — never bubble into an ancestor row's drill.
      onClick={(e) => e.stopPropagation()}
      className={cn("flex flex-col gap-1 px-3 pb-2 pl-6", className)}
    >
      <div className="flex items-end gap-2">
        <textarea
          ref={inputRef}
          data-testid="inbox-reply-input"
          // The CONSTANT label — always "Reply", whatever the liveness (§8.5).
          aria-label="Reply"
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            if (sendError) setSendError(null);
            if (sentKind) setSentKind(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send();
            } else if (e.key === "Escape") {
              e.preventDefault();
              onClose();
            }
          }}
          placeholder="Reply to the agent…"
          rows={2}
          className="flex-1 resize-none rounded-md border border-border bg-background px-2 py-1.5 text-sm outline-none focus-visible:border-ring"
        />
        <div className="flex flex-col gap-1">
          <Button
            size="sm"
            data-testid="inbox-reply-send"
            onClick={() => void send()}
            disabled={!sendable}
            aria-label="Send reply"
          >
            <SendHorizontal className="size-4" aria-hidden />
          </Button>
          <Button
            size="sm"
            variant="ghost"
            data-testid="inbox-reply-close"
            onClick={onClose}
            aria-label="Close reply"
          >
            <X className="size-4" aria-hidden />
          </Button>
        </div>
      </div>
      {/* §8.14 — Reply ALWAYS acknowledges it landed. Inline sends confirm "Sent to the
          agent."; a re-dispatch confirms "Sent — the agent will pick this up." so a silent
          continuation never looks like the button did nothing. */}
      {sentKind && (
        <p
          role="status"
          data-testid="inbox-reply-sent"
          className="flex items-center gap-1 text-[0.7rem] text-muted-foreground"
        >
          <CheckCircle2 className="size-3" aria-hidden />
          {sentKind === "inline" ? "Sent to the agent." : "Sent — the agent will pick this up."}
        </p>
      )}
      {sendError && (
        <p role="alert" className="text-[0.7rem] text-destructive">
          Couldn&rsquo;t send — {sendError}. Your reply is still here; try again.
        </p>
      )}
    </div>
  );
}
