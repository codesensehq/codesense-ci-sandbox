// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { toast } from "sonner";

import { RowAction } from "@/features/shell/components/connections-reserve";
import { StrayTerminalServers } from "@/features/machines/components/stray-terminal-servers";
import { restart as restartIncompatibleServer } from "@/features/shell/lib/terminal-server";
import { getErrorMessage } from "@/shared/lib/error-message";
import {
  endUnownedTerminalSession,
  replaceTerminalServer,
  startTerminalServer,
  stopTerminalServer,
  terminalServerOverview,
  type MachineSummary,
  type TerminalServerOverview,
  type UnownedTerminalSession,
} from "@/tauri/commands";

/** How often the section re-reads the terminal server while it shows. */
const POLL_MS = 5_000;

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

/** Whether it runs, in the person's words. */
export function terminalServerStateLine(overview: TerminalServerOverview): string {
  switch (overview.state) {
    case "running":
      return "Running";
    case "stopped":
      return "Not running";
    case "incompatible":
      return "Running, from another CodeSense version: restart it to use it";
    case "unreachable":
      return "Running, but not answering";
    case "notApplicable":
      return "This app hosts its terminals itself: there is no terminal server to manage";
  }
}

/** How long it has run, from `startedAtMs` to `now`. */
export function uptime(startedAtMs: number, now = Date.now()): string {
  const minutes = Math.max(0, Math.floor((now - startedAtMs) / 60_000));
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${minutes % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

/** What it holds: its sessions, the running agents among them, the exited kept. */
export function sessionsLine(overview: TerminalServerOverview): string | null {
  if (overview.sessions === null) return null;
  const parts = [plural(overview.sessions, "session")];
  if (overview.runningAgents !== null && overview.runningAgents > 0) {
    parts.push(plural(overview.runningAgents, "running agent"));
  }
  if (overview.exitedKept !== null && overview.exitedKept > 0) {
    parts.push(`${overview.exitedKept} exited, kept`);
  }
  return parts.join(" · ");
}

/** What a stop or restart ends, for its confirmation: the running sessions it
 *  ends (an unknown count is said to be unknown, never 0), the agents among
 *  them, and the exited ones kept, apart. */
export function endsLine(overview: TerminalServerOverview): string {
  const agents = overview.runningAgents ?? 0;
  const kept = overview.exitedKept ?? 0;
  const keptNote =
    kept === 0
      ? ""
      : kept === 1
        ? " Its exited session goes too."
        : ` Its ${kept} exited sessions go too.`;
  if (overview.ends === null) return `It ends an unknown number of sessions.${keptNote}`;
  if (overview.ends === 0) return `It runs no sessions.${keptNote}`;
  const ends = `It ends ${plural(overview.ends, "running session")}`;
  return `${agents > 0 ? `${ends}, ${agents} of them running agents.` : `${ends}.`}${keptNote}`;
}

/** Other instances whose tabs end too, for the confirmation; `null` with none.
 *  A server that cannot say is said not to. */
export function othersLine(overview: TerminalServerOverview): string | null {
  const others = overview.otherInstances;
  if (others === null) {
    return overview.state === "running"
      ? "Can't tell whether other CodeSense instances are connected to it; if any are, their tabs end too."
      : null;
  }
  if (others === 0) return null;
  return `${plural(others, "other CodeSense instance")} ${others === 1 ? "is" : "are"} connected to it: ${others === 1 ? "its" : "their"} tabs end too.`;
}

/** Whether a newer read means a confirmation opened on `reviewed` no longer
 *  says what would end. */
function changedSince(reviewed: TerminalServerOverview, now: TerminalServerOverview): boolean {
  return (
    reviewed.state !== now.state ||
    reviewed.otherInstances !== now.otherInstances ||
    JSON.stringify(reviewed.expected) !== JSON.stringify(now.expected)
  );
}

const WHEN = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** A confirmation as it opened: what to do and the read it names. */
interface Review {
  action: "stop" | "restart";
  overview: TerminalServerOverview;
}

/** SV-7 — this instance's terminal server (it hosts every terminal tab and
 *  agent, and outlives the app): whether it runs, what it holds and which
 *  apps are connected, read live; Stop, Start and Restart, each confirmed
 *  with what ends, as read when the confirmation opened; the sessions no app
 *  holds, each ended after its own confirmation; and the other terminal
 *  servers found on This Mac. */
export function TerminalServerSection() {
  const [overview, setOverview] = useState<TerminalServerOverview | null>(null);
  const [failed, setFailed] = useState(false);
  const [revision, setRevision] = useState(0);
  // The stray list re-reads whenever this changes: a new list each poll.
  const [strayTick, setStrayTick] = useState<MachineSummary[]>([]);
  const [review, setReview] = useState<Review | null>(null);
  const [changed, setChanged] = useState(false);
  const [busy, setBusy] = useState(false);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const reviewRef = useRef<Review | null>(null);
  useEffect(() => {
    reviewRef.current = review;
  }, [review]);

  useEffect(() => {
    let live = true;
    // Through `Promise.resolve().then`, so a missing backend leaves it empty.
    const read = () =>
      Promise.resolve()
        .then(() => terminalServerOverview())
        .then(
          (next) => {
            if (!live || !next) return;
            setFailed(false);
            setStrayTick([]);
            const open = reviewRef.current;
            // While a confirmation is open the shown read stays the one it
            // names; a newer read that differs asks for a fresh review.
            if (open) {
              if (changedSince(open.overview, next)) setChanged(true);
              return;
            }
            setOverview(next);
          },
          () => {
            if (live) setFailed(true);
          },
        );
    void read();
    const timer = setInterval(() => void read(), POLL_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [revision]);

  useEffect(() => {
    if (review) cancelRef.current?.focus();
  }, [review]);

  const refresh = () => setRevision((r) => r + 1);
  const closeReview = () => {
    setReview(null);
    setChanged(false);
  };
  const settle = () => {
    setBusy(false);
    closeReview();
    refresh();
  };

  const stop = (reviewed: TerminalServerOverview) => {
    if (!reviewed.expected) return;
    setBusy(true);
    stopTerminalServer(reviewed.expected).then(
      (stopped) => {
        settle();
        toast.success(
          stopped.ended === 0
            ? "Stopped the terminal server."
            : `Stopped the terminal server; ${plural(stopped.ended, "running session")} ended.`,
        );
      },
      (e: unknown) => {
        settle();
        toast.error(`Couldn't stop the terminal server: ${getErrorMessage(e)}`);
      },
    );
  };

  const restart = (reviewed: TerminalServerOverview) => {
    setBusy(true);
    if (reviewed.state === "incompatible") {
      // The restart prompt's own path, so its state stays in step.
      void restartIncompatibleServer().then((ended) => {
        settle();
        if (ended === undefined) toast.error("Couldn't restart the terminal server.");
        else
          toast.success(
            ended === null
              ? "Restarted the terminal server."
              : `Restarted the terminal server; ${plural(ended, "session")} ended.`,
          );
      });
      return;
    }
    if (!reviewed.expected) return;
    replaceTerminalServer(reviewed.expected).then(
      (ended) => {
        settle();
        toast.success(
          ended === 0
            ? "Restarted the terminal server."
            : `Restarted the terminal server; ${plural(ended, "running session")} ended.`,
        );
      },
      (e: unknown) => {
        settle();
        toast.error(`Couldn't restart the terminal server: ${getErrorMessage(e)}`);
      },
    );
  };

  const start = () => {
    setBusy(true);
    startTerminalServer().then(
      (next) => {
        setBusy(false);
        setOverview(next);
        // A read started before the start must not show it stopped again.
        refresh();
        toast.success("Started the terminal server.");
      },
      (e: unknown) => {
        setBusy(false);
        toast.error(`Couldn't start the terminal server: ${getErrorMessage(e)}`);
        refresh();
      },
    );
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape" && !busy) {
      event.stopPropagation();
      closeReview();
      refresh();
    }
  };

  if (!overview) {
    return (
      <p data-testid="terminal-server-loading" className="text-xs text-muted-foreground">
        {failed ? "Couldn't read the terminal server." : "Reading the terminal server…"}
      </p>
    );
  }

  const shown = review?.overview ?? overview;
  const running = shown.state === "running";
  const restartable = shown.state === "incompatible" || (running && shown.expected !== null);
  const sessions = sessionsLine(shown);
  const showsPid = (running || shown.state === "incompatible") && shown.pid !== null;
  const others = review ? othersLine(review.overview) : null;
  return (
    <div className="space-y-3" data-testid="terminal-server-section">
      <div className="space-y-0.5">
        <p data-testid="terminal-server-state" className="text-sm">
          {terminalServerStateLine(shown)}
        </p>
        {showsPid && (
          <p data-testid="terminal-server-identity" className="text-xs text-muted-foreground">
            pid {shown.pid}
            {shown.version ? ` · ${shown.version}` : ""}
            {shown.startedAtMs !== null ? ` · up ${uptime(shown.startedAtMs)}` : ""}
          </p>
        )}
        {sessions && (
          <p data-testid="terminal-server-sessions" className="text-xs text-muted-foreground">
            {sessions}
          </p>
        )}
        {shown.connectedInstances !== null && (
          <p data-testid="terminal-server-instances" className="text-xs text-muted-foreground">
            {plural(shown.connectedInstances, "CodeSense instance")} connected
            {shown.otherInstances ? ` (${shown.otherInstances} besides this one)` : ""}
          </p>
        )}
        {shown.detail && shown.state !== "notApplicable" && (
          <p data-testid="terminal-server-detail" className="text-xs text-muted-foreground">
            {shown.detail}
          </p>
        )}
        {failed && (
          <p
            role="alert"
            data-testid="terminal-server-read-failed"
            className="text-xs text-destructive"
          >
            Couldn't read the terminal server just now; this is what it said last.
          </p>
        )}
      </div>

      {review ? (
        <div
          role="group"
          aria-label={review.action === "stop" ? "Confirm stop" : "Confirm restart"}
          data-testid="terminal-server-confirm"
          className="space-y-2 rounded-md border border-border p-2"
          onKeyDown={onKeyDown}
        >
          <p className="text-xs">
            {review.action === "stop"
              ? "Stop the terminal server? "
              : "Restart the terminal server? "}
            {endsLine(review.overview)}
          </p>
          {others && (
            <p data-testid="terminal-server-confirm-others" className="text-xs text-destructive">
              {others}
            </p>
          )}
          {changed && (
            <p role="alert" data-testid="terminal-server-confirm-changed" className="text-xs">
              The terminal server changed since you opened this — review again.
            </p>
          )}
          <div className="flex items-center gap-1">
            {changed ? (
              <RowAction
                testid="terminal-server-review-again"
                label="Review again"
                onClick={() => {
                  closeReview();
                  refresh();
                }}
              />
            ) : (
              <RowAction
                testid={`terminal-server-${review.action}-yes`}
                label={
                  busy
                    ? review.action === "stop"
                      ? "Stopping…"
                      : "Restarting…"
                    : review.action === "stop"
                      ? "Stop"
                      : "Restart"
                }
                destructive
                disabled={busy}
                onClick={() =>
                  review.action === "stop" ? stop(review.overview) : restart(review.overview)
                }
              />
            )}
            <RowAction
              ref={cancelRef}
              testid="terminal-server-confirm-cancel"
              label="Cancel"
              disabled={busy}
              onClick={() => {
                closeReview();
                refresh();
              }}
            />
          </div>
        </div>
      ) : (
        <div className="flex items-center gap-1">
          {shown.state === "stopped" && (
            <RowAction
              testid="terminal-server-start"
              label={busy ? "Starting…" : "Start"}
              disabled={busy}
              onClick={start}
            />
          )}
          {running && shown.expected && (
            <RowAction
              testid="terminal-server-stop"
              label="Stop…"
              destructive
              onClick={() => setReview({ action: "stop", overview: shown })}
            />
          )}
          {restartable && (
            <RowAction
              testid="terminal-server-restart"
              label="Restart…"
              onClick={() => setReview({ action: "restart", overview: shown })}
            />
          )}
        </div>
      )}

      {shown.unowned.length > 0 && (
        <div className="space-y-1" data-testid="terminal-server-unowned">
          <p className="text-xs font-medium">Sessions no app holds</p>
          <p className="text-[11px] leading-snug text-muted-foreground">
            Opened by a CodeSense instance that has been gone for 5 minutes or more (it crashed or
            was force-quit), and shown nowhere. They keep running until ended.
          </p>
          <ul className="space-y-1">
            {shown.unowned.map((left) => (
              <UnownedRow key={left.session} left={left} onEnded={refresh} />
            ))}
          </ul>
        </div>
      )}

      {shown.state !== "notApplicable" && (
        <ul className="space-y-1" data-testid="terminal-server-strays">
          <StrayTerminalServers machines={strayTick} />
        </ul>
      )}
    </div>
  );
}

function UnownedRow({ left, onEnded }: { left: UnownedTerminalSession; onEnded: () => void }) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const what = left.running ? (left.command ?? "shell") : "exited";
  const end = () => {
    setBusy(true);
    endUnownedTerminalSession(left.session).then(
      () => {
        setBusy(false);
        setConfirming(false);
        toast.success("Ended the session.");
        onEnded();
      },
      (e: unknown) => {
        setBusy(false);
        setConfirming(false);
        toast.error(`Couldn't end it: ${getErrorMessage(e)}`);
        onEnded();
      },
    );
  };
  return (
    <li
      data-testid={`terminal-server-unowned-${left.session}`}
      className="flex items-center gap-3 rounded-md border border-dashed border-border px-3 py-1.5"
    >
      <div className="min-w-0 flex-1">
        <p className="truncate font-mono text-xs">{what}</p>
        <p className="text-[11px] text-muted-foreground">
          started {WHEN.format(new Date(left.startedAtMs))}
          {left.runningAgent ? " · running an agent" : ""}
        </p>
      </div>
      {confirming ? (
        <div role="group" aria-label="Confirm end" className="flex shrink-0 items-center gap-1">
          <span
            data-testid={`terminal-server-unowned-confirm-${left.session}`}
            className="max-w-72 text-xs"
          >
            End {left.running ? <code>{what}</code> : "this exited session"}?
            {left.runningAgent ? " The agent it runs stops." : ""}
          </span>
          <RowAction
            testid={`terminal-server-unowned-end-yes-${left.session}`}
            label={busy ? "Ending…" : "End"}
            destructive
            disabled={busy}
            onClick={end}
          />
          <RowAction
            testid={`terminal-server-unowned-end-no-${left.session}`}
            label="Cancel"
            disabled={busy}
            onClick={() => setConfirming(false)}
          />
        </div>
      ) : (
        <RowAction
          testid={`terminal-server-unowned-end-${left.session}`}
          label="End…"
          destructive
          onClick={() => setConfirming(true)}
        />
      )}
    </li>
  );
}
