// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { toast } from "sonner";
import { Server } from "lucide-react";

import { RowAction } from "@/features/shell/components/connections-reserve";
import { getErrorMessage } from "@/shared/lib/error-message";
import {
  listStrayTerminalServers,
  stopStrayTerminalServer,
  type MachineSummary,
  type StrayServer,
} from "@/tauri/commands";

import { plural } from "../lib/machine-status";

/** What a stray is, in the person's words. */
export function strayKindLine(stray: StrayServer): string {
  switch (stray.kind) {
    case "bundled":
      return "this app's terminal server, for another CodeSense folder";
    case "otherBuild":
      return "another CodeSense build's terminal server";
    case "ownRoot":
      return "another terminal server on this app's folder (a replacement may still be starting)";
  }
}

const WHEN = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** When a stray started, for the person (the raw start time is its identity). */
export function strayStarted(stray: StrayServer): string {
  return stray.startedAtMs ? WHEN.format(new Date(stray.startedAtMs)) : stray.started;
}

/** The sessions a stray runs, for its row and its confirmation. */
export function straySessions(stray: StrayServer): string {
  if (stray.sessions === null) return "sessions unknown";
  return stray.sessions === 0 ? "no sessions" : plural(stray.sessions, "session");
}

/** SW-AGR-19 PR-7 — terminal servers on This Mac besides the one this app
 *  uses (a leaked test fixture, a dev worktree's): each named with its
 *  executable, folder, start time and sessions, and stopped only after its
 *  own confirmation. Re-read whenever the machine list is. */
export function StrayTerminalServers({ machines }: { machines: MachineSummary[] }) {
  const [strays, setStrays] = useState<StrayServer[]>([]);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let live = true;
    listStrayTerminalServers().then(
      (next) => {
        if (live) setStrays(next);
      },
      () => {
        if (live) setStrays([]);
      },
    );
    return () => {
      live = false;
    };
  }, [machines, revision]);

  if (strays.length === 0) return null;
  return (
    <>
      {strays.map((stray) => (
        <StrayRow
          key={`${stray.pid}-${stray.started}`}
          stray={stray}
          onStopped={() => setRevision((r) => r + 1)}
        />
      ))}
    </>
  );
}

function StrayRow({ stray, onStopped }: { stray: StrayServer; onStopped: () => void }) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const stopRef = useRef<HTMLButtonElement>(null);
  const wasConfirming = useRef(false);
  // The confirmation takes keyboard focus on its safe way out; closing it
  // gives focus back to "Stop…".
  useEffect(() => {
    if (confirming) cancelRef.current?.focus();
    else if (wasConfirming.current) stopRef.current?.focus();
    wasConfirming.current = confirming;
  }, [confirming]);
  const id = stray.pid;
  const stop = () => {
    setBusy(true);
    stopStrayTerminalServer({ pid: stray.pid, started: stray.started, root: stray.root }).then(
      (result) => {
        setBusy(false);
        setConfirming(false);
        if (result.status === "stopped") toast.success("Stopped the other terminal server.");
        else if (result.status === "gone")
          toast.info(
            "It had already stopped, or is no longer the same process; nothing was stopped.",
          );
        else if (result.status === "attached")
          toast.error("That is this app's own terminal server; it was not stopped.");
        else toast.error(`Couldn't stop it: ${result.message}`);
        onStopped();
      },
      (e: unknown) => {
        setBusy(false);
        toast.error(`Couldn't stop it: ${getErrorMessage(e)}`);
      },
    );
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape" && !busy) {
      event.stopPropagation();
      setConfirming(false);
    }
  };
  const ending =
    stray.sessions === null
      ? "Any sessions it runs end"
      : stray.sessions === 0
        ? "It runs no sessions"
        : `Its ${plural(stray.sessions, "session")} end`;
  return (
    <li
      data-testid={`machine-stray-${id}`}
      className="flex items-center gap-3 rounded-md border border-dashed border-border px-3 py-2"
    >
      <Server size={14} aria-hidden="true" className="shrink-0 text-muted-foreground" />
      <div className="min-w-0 flex-1">
        <p className="text-sm">
          Another terminal server on This Mac{" "}
          <span className="text-muted-foreground">· {straySessions(stray)}</span>
        </p>
        <p className="truncate text-xs text-muted-foreground">
          {strayKindLine(stray)} · started {strayStarted(stray)}
        </p>
        {/* Truncated from the start: the folder's own name stays in view. */}
        <p
          data-testid={`machine-stray-root-${id}`}
          dir="rtl"
          className="truncate text-left text-xs text-muted-foreground"
          title={stray.root}
        >
          <bdi>{stray.root}</bdi>
        </p>
        <p
          data-testid={`machine-stray-exe-${id}`}
          className="truncate font-mono text-[11px] text-muted-foreground"
          title={stray.exe}
        >
          {stray.exe}
        </p>
      </div>
      {confirming ? (
        <div
          role="group"
          aria-label="Confirm stop"
          className="flex shrink-0 items-center gap-1"
          onKeyDown={onKeyDown}
        >
          <span className="max-w-72 text-xs">
            Stop it? {ending}
            {stray.sessions === 0
              ? "."
              : ", including those of another CodeSense window (a dev or sandbox app) that uses it."}
          </span>
          <RowAction
            testid={`machine-stray-stop-yes-${id}`}
            label={busy ? "Stopping…" : "Stop"}
            destructive
            disabled={busy}
            onClick={stop}
          />
          <RowAction
            ref={cancelRef}
            testid={`machine-stray-stop-no-${id}`}
            label="Cancel"
            disabled={busy}
            onClick={() => setConfirming(false)}
          />
        </div>
      ) : (
        <div className="flex shrink-0 items-center gap-1">
          <RowAction
            ref={stopRef}
            testid={`machine-stray-stop-${id}`}
            label="Stop…"
            destructive
            onClick={() => setConfirming(true)}
          />
        </div>
      )}
    </li>
  );
}
