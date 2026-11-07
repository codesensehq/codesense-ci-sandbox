// SPDX-License-Identifier: MIT
import { useState } from "react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  type AskUserRequest,
  acceptAnswer,
  declineAnswer,
  cancelAnswer,
  handledAnswer,
} from "@/shared/lib/managed-session";
import { answerAskUser } from "@/shared/hooks/use-managed-session-bridge";
import { useOldestPendingAsk } from "@/shared/hooks/use-oldest-pending-ask";

/**
 * SW-AW-4 (A/D) — the "answer from the UI" surface. Renders the oldest pending
 * blocking question for a session so the human answers WITHOUT touching the
 * terminal (the product invariant), then routes the reply through `answerAskUser`
 * (which resolves the Rust oneshot by `correlationId` and returns the session to
 * `running`). Renders nothing when there is no pending ask.
 *
 * The store shape is inbox-ready (correlation-keyed, N sessions), so the future
 * multi-lane dashboard is a view layer over the same data (OQ-3) — v1 renders
 * inline for the single bound session.
 */
export function ManagedQuestionPanel({
  sessionId,
  className,
  showPendingCount = true,
}: {
  sessionId: string;
  className?: string;
  /** Show the "+N more" count (off when the host shows its own count). */
  showPendingCount?: boolean;
}): React.JSX.Element | null {
  const { ask, pendingCount } = useOldestPendingAsk(sessionId);

  if (!ask) return null;
  return (
    <QuestionCard
      key={ask.correlationId}
      ask={ask}
      pendingCount={showPendingCount ? pendingCount : 1}
      className={className}
    />
  );
}

function QuestionCard({
  ask,
  pendingCount,
  className,
}: {
  ask: AskUserRequest;
  pendingCount: number;
  className?: string;
}): React.JSX.Element {
  const [text, setText] = useState("");
  const send = (response: Parameters<typeof answerAskUser>[1]): void => {
    void answerAskUser(ask.correlationId, response);
  };

  return (
    <div
      className={cn(
        "flex flex-col gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-sm",
        className,
      )}
      role="group"
      aria-label="Agent question"
      data-testid="managed-question-panel"
    >
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs font-medium text-amber-600 dark:text-amber-400">
          {ask.category ? ask.category : "The agent needs your answer"}
        </span>
        {pendingCount > 1 ? (
          <span className="text-xs text-muted-foreground">+{pendingCount - 1} more</span>
        ) : null}
      </div>

      <p className="whitespace-pre-wrap">{ask.question}</p>

      {ask.options && ask.options.length > 0 ? (
        <div className="flex flex-col gap-1.5">
          <div className="flex flex-wrap gap-1.5">
            {ask.options.map((opt) => (
              <Button
                key={opt}
                variant="outline"
                size="xs"
                onClick={() => send(acceptAnswer(opt))}
                data-testid="ask-option"
              >
                {opt}
              </Button>
            ))}
          </div>
          <div className="flex items-center gap-1.5">
            <PutDownButtons send={send} />
          </div>
        </div>
      ) : (
        <div className="flex flex-col gap-1.5">
          <textarea
            className="min-h-[3rem] w-full resize-y rounded-sm border border-border bg-background p-2 text-sm"
            placeholder="Type your answer…"
            value={text}
            onChange={(e) => setText(e.target.value)}
            aria-label="Your answer"
          />
          <div className="flex items-center gap-1.5">
            <Button
              size="xs"
              disabled={text.trim().length === 0}
              onClick={() => send(acceptAnswer(text.trim()))}
              data-testid="ask-send"
            >
              Send answer
            </Button>
            <Button variant="outline" size="xs" onClick={() => send(declineAnswer())}>
              Decline
            </Button>
            <PutDownButtons send={send} />
          </div>
        </div>
      )}
    </div>
  );
}

/** Ways to put a question down without answering it: Handled (the human already
 *  settled it, e.g. in the pane) and Dismiss. Offered for every question. */
function PutDownButtons({
  send,
}: {
  send: (response: Parameters<typeof answerAskUser>[1]) => void;
}): React.JSX.Element {
  return (
    <>
      <Button
        variant="outline"
        size="xs"
        onClick={() => send(handledAnswer())}
        aria-label="I handled this myself"
        data-testid="ask-handled"
      >
        Handled it
      </Button>
      <Button
        variant="ghost"
        size="xs"
        onClick={() => send(cancelAnswer())}
        aria-label="Dismiss this question"
      >
        Dismiss
      </Button>
    </>
  );
}
