// SPDX-License-Identifier: MIT
import { Fragment, useCallback, useState, type ReactNode } from "react";
import { DotPulse } from "ldrs/react";
import "ldrs/react/DotPulse.css";

import type { PaneStartupFailure, PaneStartupPhase } from "../hooks/use-pane-startup";

interface PaneStartupOverlayProps {
  phase: PaneStartupPhase;
  failure: PaneStartupFailure | null;
  /** The attach finished: the pane has its session, whether or not it painted. */
  started: boolean;
  /** The pane shows a terminal that ran before (not one this viewer just opened). */
  restoring?: boolean;
  background: string;
  onRetry: () => void;
}

// A pane that timed out after its session attached has a live session it could
// not draw (it never laid out, or never rendered): say that, not that the
// session failed.
function failedTitle(failure: PaneStartupFailure | null, started: boolean): string {
  if (failure?.title) return failure.title;
  return failure?.reason === "timeout" && started
    ? "Couldn't show the terminal"
    : "Failed to start";
}

function failedDetail(failure: PaneStartupFailure | null, started: boolean): string {
  if (failure?.reason === "error" && failure.detail) return failure.detail;
  return started ? "The terminal couldn't be shown." : "The terminal didn't start in time.";
}

/** What covers a terminal pane until it first paints, so it is never silently blank. */
export function PaneStartupOverlay({
  phase,
  failure,
  started,
  restoring = false,
  background,
  onRetry,
}: PaneStartupOverlayProps) {
  if (phase === "quiet" || phase === "painted") return null;
  return (
    <div
      data-testid="pane-startup-overlay"
      data-phase={phase}
      data-surface="terminal"
      className="absolute inset-0 flex flex-col items-center justify-center z-10 gap-3 p-4 text-center"
      style={{ backgroundColor: background }}
    >
      {phase === "failed" ? (
        <div role="alert" className="flex flex-col items-center gap-3">
          <p className="text-sm text-foreground">{failedTitle(failure, started)}</p>
          <p className="max-w-xs text-xs text-muted-foreground break-words">
            {failedDetail(failure, started)}
          </p>
          <button
            type="button"
            data-testid="pane-startup-retry"
            onClick={onRetry}
            className="rounded border border-input px-3 py-1.5 text-xs text-foreground transition-colors hover:bg-foreground/5"
          >
            Retry
          </button>
        </div>
      ) : (
        <>
          <DotPulse size={40} speed={1.3} color="var(--cs-status-busy)" />
          <p role="status" className="text-sm text-muted-foreground">
            {restoring
              ? "Restoring session…"
              : phase === "slow"
                ? "Still starting…"
                : started
                  ? "Starting shell…"
                  : "Initializing terminal…"}
          </p>
        </>
      )}
    </div>
  );
}

/** Retry for a pane: rebuilds its children from scratch on each call. */
export function RetryRemount({ children }: { children: (retry: () => void) => ReactNode }) {
  const [attempt, setAttempt] = useState(0);
  const retry = useCallback(() => setAttempt((n) => n + 1), []);
  return <Fragment key={attempt}>{children(retry)}</Fragment>;
}
