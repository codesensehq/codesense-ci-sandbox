// SPDX-License-Identifier: MIT
import { TerminalActivityUpgradeNotice } from "./terminal-activity-upgrade";
import { useEffect, useState } from "react";

import { terminalServerOverview } from "@/tauri/commands";

import { InlineAlert } from "@/components/ui/inline-alert";

import {
  heldCopy,
  noticeCopy,
  reopen,
  restart,
  restartCopy,
  useTerminalServer,
} from "../lib/terminal-server";

/**
 * Where the terminal panel says why new terminals can't open while the user
 * has put the terminal-server restart off, with the way back to it. Renders
 * nothing while the terminal server is compatible or the prompt is showing.
 */
export function TerminalServerNotice() {
  const { server, open } = useTerminalServer();
  if (!server || open) return <TerminalActivityUpgradeNotice />;
  return (
    <InlineAlert
      data-testid="terminal-server-notice"
      className="max-w-md text-left"
      action={
        <button
          type="button"
          onClick={() => void reopen()}
          className="shrink-0 rounded border border-current/30 px-2 py-1 text-xs whitespace-nowrap hover:bg-current/10"
        >
          Restart terminal server…
        </button>
      }
    >
      {noticeCopy(server.liveSessions)}
    </InlineAlert>
  );
}

/**
 * SW-RT-6d: a workspace's new terminals the incompatible terminal server
 * refused are held — no tab, no pane, nothing typed into them — and start on
 * their own once it restarts. Says so, with the way to the restart, for as
 * long as any is held (never a silent wait).
 */
export function TerminalHeldNotice({ count }: { count: number }) {
  const { server } = useTerminalServer();
  if (count === 0) return null;
  return (
    <InlineAlert
      data-testid="terminal-held-notice"
      className="text-left"
      action={
        <button
          type="button"
          onClick={() => void reopen()}
          className="shrink-0 rounded border border-current/30 px-2 py-1 text-xs whitespace-nowrap hover:bg-current/10"
        >
          Restart terminal server…
        </button>
      }
    >
      {server ? `${heldCopy(count)} ${noticeCopy(server.liveSessions)}` : heldCopy(count)}
    </InlineAlert>
  );
}

/** How long the terminal area waits on an unanswering terminal server before
 *  it says so and offers the restart. */
export const HOSTS_ESCALATE_AFTER_MS = 30_000;

/** How long a lost connection to This Mac's terminal server waits before
 *  asking whether the server still runs, and how often it asks again. */
const UNREACHABLE_CHECK_MS = 3_000;
const UNREACHABLE_RECHECK_MS = 15_000;

/** What the app knows of This Mac's terminal server while it has lost it:
 *  `unknown` until the server is asked, then whether it runs unconnected. */
export type LostServer = "connected" | "unknown" | "unreachable" | "notRunning";

/**
 * Whether This Mac's terminal server runs but this app is not connected to it:
 * asked of the app while `lost` (its admission is unavailable), so a server
 * stopped, restarted or updated on purpose never reads as "reconnecting".
 * A brief blip that reconnects within {@link UNREACHABLE_CHECK_MS} is never asked about.
 */
export function useLocalServerUnreachable(lost: boolean): LostServer {
  const [answer, setAnswer] = useState<"unknown" | "unreachable" | "notRunning">("unknown");
  useEffect(() => {
    if (!lost) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const check = () => {
      terminalServerOverview()
        .then(
          (overview) => {
            if (!cancelled) {
              setAnswer(overview.state === "unreachable" ? "unreachable" : "notRunning");
            }
          },
          () => {},
        )
        .finally(() => {
          if (!cancelled) timer = setTimeout(check, UNREACHABLE_RECHECK_MS);
        });
    };
    timer = setTimeout(check, UNREACHABLE_CHECK_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      setAnswer("unknown");
    };
  }, [lost]);
  return lost ? answer : "connected";
}

/** Milliseconds since this component mounted, updated once a second. */
function useWaitedMs(): number {
  const [since] = useState(() => Date.now());
  const [now, setNow] = useState(since);
  useEffect(() => {
    const tick = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(tick);
  }, []);
  return now - since;
}

/**
 * The app lost its connection to This Mac's terminal server while the panel
 * still shows its tabs: says it is reconnecting and that the terminals keep
 * running, so nobody restarts a healthy server (which would end every agent).
 * Never offers the restart; past {@link HOSTS_ESCALATE_AFTER_MS} it names the
 * safe way out instead.
 */
export function TerminalReconnectingBanner() {
  const waited = useWaitedMs();
  return (
    <div
      data-testid="terminal-reconnecting-banner"
      role="status"
      className="max-w-md rounded-md border border-border bg-card/95 px-3 py-2 text-xs text-muted-foreground shadow-sm"
    >
      {waited >= HOSTS_ESCALATE_AFTER_MS ? (
        <>
          Still reconnecting to the terminal server
          {/* The count is not announced every second; the sentence is, once. */}
          <span aria-hidden="true"> ({Math.floor(waited / 1_000)}s)</span>. Your terminals and
          agents keep running. If this doesn&apos;t clear, quit and reopen CodeSense; that
          reconnects without stopping them.
        </>
      ) : (
        "Reconnecting to the terminal server… Your terminals keep running there."
      )}
    </div>
  );
}

/**
 * The terminal hosts have not answered for their layout yet — a terminal
 * server that runs but does not answer: the terminal area says it is
 * reconnecting (and keeps asking) instead of showing nothing. Past
 * {@link HOSTS_ESCALATE_AFTER_MS} it says the server is still not answering,
 * counting, and offers to restart it, saying first what that does to the
 * terminals it runs. An incompatible server's restart stays reachable through
 * its notice.
 */
export function TerminalHostsReconnecting() {
  const waited = useWaitedMs();
  const [confirming, setConfirming] = useState(false);
  const { phase } = useTerminalServer();
  const escalated = waited >= HOSTS_ESCALATE_AFTER_MS;
  const restarting = phase.kind === "restarting";
  const { sentence } = restartCopy(null);
  return (
    <div
      data-testid="terminal-hosts-reconnecting"
      role="status"
      className="flex max-w-md flex-col items-center gap-2 text-center"
    >
      <TerminalServerNotice />
      {escalated ? (
        <>
          <p className="text-sm text-[#c9d1d9]">
            Terminal server still not answering ({Math.floor(waited / 1_000)}s)
          </p>
          {confirming ? (
            <>
              <p className="text-xs text-[#8b949e]">{sentence}</p>
              <div className="flex gap-2">
                <button
                  type="button"
                  disabled={restarting}
                  onClick={() => void restart()}
                  className="rounded bg-[#da3633] px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-[#f85149] disabled:opacity-60"
                >
                  {restarting ? "Restarting…" : "Restart terminal server"}
                </button>
                <button
                  type="button"
                  disabled={restarting}
                  onClick={() => setConfirming(false)}
                  className="rounded border border-[#30363d] px-3 py-1.5 text-xs text-[#c9d1d9] transition-colors hover:bg-[#ffffff10]"
                >
                  Cancel
                </button>
              </div>
            </>
          ) : (
            <button
              type="button"
              onClick={() => setConfirming(true)}
              className="rounded bg-[#1f6feb] px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-[#388bfd]"
            >
              Restart terminal server…
            </button>
          )}
          {phase.kind === "failed" && (
            <p role="alert" className="text-xs text-[#f85149]">
              {phase.oldStopped
                ? "The old terminal server stopped, and its terminals with it, but a new one didn't start. Try again, or quit and reopen CodeSense."
                : "Couldn't restart the terminal server. Try again, or quit and reopen CodeSense."}
            </p>
          )}
        </>
      ) : (
        <>
          <p className="text-sm text-[#8b949e]">Reconnecting to the terminal server…</p>
          <p className="text-xs text-[#6e7681]">
            Your terminals keep running there; they show here once it answers.
          </p>
        </>
      )}
    </div>
  );
}
