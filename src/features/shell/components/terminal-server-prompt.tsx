// SPDX-License-Identifier: MIT
import { openMachinesPanel } from "@/features/machines/lib/machine-actions";
import { TerminalActivityUpgradePrompt } from "./terminal-activity-upgrade";
import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { buttonVariants } from "@/components/ui/button";
import { InlineAlert } from "@/components/ui/inline-alert";
import {
  terminalServerStatus,
  type IncompatibleTerminalServer,
  type TerminalServerRestarted,
} from "@/tauri/commands";
import {
  decline,
  found,
  refused,
  restart,
  restartCopy,
  terminals,
  useTerminalServer,
  TERMINAL_SERVER_INCOMPATIBLE_EVENT,
  TERMINAL_SERVER_REFUSED_EVENT,
  TERMINAL_SERVER_RESTARTED_EVENT,
} from "../lib/terminal-server";
import {
  closeMachineTerminalServerPrompt,
  confirmMachineTerminalServerReplacement,
  useMachineTerminalServerPrompt,
} from "../lib/machine-terminal-server";

const INTRO =
  "A terminal server from another version of CodeSense is still running on this machine, so new terminals can't open.";

/** The next step after a failed restart. */
const RESTART_FAILED =
  "Couldn't restart the terminal server. Try again, or quit and reopen CodeSense.";

/** What putting the restart off means once the old server is gone: new
 *  terminals are hosted by the app itself, so they end with it; the app
 *  launches the server again at its next start (and at a new terminal once the
 *  launch backoff passes). */
const WONT_SURVIVE_QUIT =
  "Terminals will open, but they'll close when you quit CodeSense. The terminal server will start again next time you open CodeSense.";

/**
 * The app-wide "restart terminal server" prompt. A terminal server (the
 * session-server sidecar) left running by another CodeSense version speaks a
 * protocol this app cannot use, so the backend refuses new terminals rather
 * than silently opening them beside it, and reports the server here — when it
 * is found (`terminal-server-incompatible`, or `terminalServerStatus` for a
 * view that mounts later) and whenever the user tries to open a terminal
 * (`terminal-server-refused`).
 *
 * Restarting closes the terminals the old server runs (counted by the server
 * itself) and starts this version's. "Not now" closes the prompt: the terminal
 * panel's notice, and the user's next attempt to open a terminal, offer it
 * again. A server that restarted on its own under the app is reported too.
 */
export function TerminalServerPrompt() {
  return (
    <>
      <TerminalActivityUpgradePrompt />
      <LegacyTerminalServerPrompt />
    </>
  );
}

function LegacyTerminalServerPrompt() {
  const { server, open, phase } = useTerminalServer();
  const remote = useMachineTerminalServerPrompt();
  const notNow = useRef<HTMLButtonElement>(null);
  const failed = phase.kind === "failed";

  // After a failure, keyboard focus returns to "Not now", the safe way out.
  useEffect(() => {
    if (failed) notNow.current?.focus();
  }, [failed]);

  useEffect(() => {
    let active = true;
    terminalServerStatus()
      .then((incompatible) => {
        if (active && incompatible) found(incompatible);
      })
      .catch(() => {});
    const subscriptions = [
      listen<IncompatibleTerminalServer>(TERMINAL_SERVER_INCOMPATIBLE_EVENT, (event) =>
        found(event.payload),
      ),
      listen<IncompatibleTerminalServer>(TERMINAL_SERVER_REFUSED_EVENT, (event) =>
        refused(event.payload),
      ),
      listen<TerminalServerRestarted>(TERMINAL_SERVER_RESTARTED_EVENT, (event) => {
        const closed = event.payload.endedSessions;
        if (closed > 0) {
          toast.warning("The terminal server restarted", {
            description: `${terminals(closed)} that ${closed === 1 ? "was" : "were"} open closed with it.`,
          });
        }
      }),
    ].map((subscribed) => subscribed.catch(() => null));
    return () => {
      active = false;
      for (const subscribed of subscriptions) {
        subscribed.then((unlisten) => unlisten?.()).catch(() => {});
      }
    };
  }, []);

  if (remote.target) {
    const { target, phase: remotePhase } = remote;
    const restarting = remotePhase.kind === "restarting";
    const failed = remotePhase.kind === "failed";
    const oldStopped = failed && remotePhase.oldStopped;
    const unknownOutcome = failed && remotePhase.oldStopped === null;
    const countCopy = target.compatible
      ? `${target.update ? `Updating restarts the terminal server on ${target.label}.` : "Restarting starts the installed terminal server version."} ${
          target.count === null || target.count === 0
            ? "All terminals and agents running on this server when it stops will end"
            : `The ${terminals(target.count)} and agents running on it now will end`
        }, including sessions started after this prompt opened. Their scrollback will be lost, and timers and scheduled work running on the server end too.${
          target.reinstall
            ? " It is too old to hand its sessions over, so CodeSense stops it and installs this app's terminal server."
            : ""
        }`
      : target.count === null
        ? "It is too old to report how many sessions it owns. Restarting may end sessions and lose their scrollback."
        : target.count === 0
          ? "It isn't running any terminals, so nothing is lost."
          : `Restarting closes ${terminals(target.count)}. Anything still running in ${target.count === 1 ? "it stops" : "them stops"}, and ${target.count === 1 ? "its" : "their"} scrollback is lost.`;
    // This Mac: the confirmation is bound to the sessions it runs now (a
    // changed set is refused, nothing stopped), and they are this app's own.
    const localCopy = `Updating restarts This Mac's terminal server. ${
      target.count === null || target.count === 0
        ? "Every terminal in this app ends"
        : `The ${terminals(target.count)} it runs now end — every terminal in this app`
    }, including agents connected to CodeSense and the agent you may be talking to. Tabs reopen afterwards as shells; scrollback, timers and scheduled work on the server are lost. If a session starts or ends before you confirm, nothing is stopped and you're asked to review again.`;
    const fullCopy = `${
      target.local
        ? localCopy
        : !target.compatible && target.reinstall
          ? `${countCopy} It can't hand its sessions over, so CodeSense stops it and installs this app's terminal server.`
          : countCopy
    }${
      target.local?.otherBuildPath
        ? ` It is another CodeSense build's (${target.local.otherBuildPath}); updating replaces it with this app's.`
        : ""
    }${
      target.shared
        ? ` ${target.label} is shared with your team: sessions teammates run on it under the same login end too.`
        : ""
    }`;
    return (
      <AlertDialog
        open
        onOpenChange={(next) => {
          if (!next && !restarting) closeMachineTerminalServerPrompt();
        }}
      >
        <AlertDialogContent
          data-testid="terminal-server-prompt"
          className="sm:max-w-md"
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            notNow.current?.focus();
          }}
        >
          <AlertDialogHeader>
            <AlertDialogTitle>
              {target.update ? "Update" : "Restart"} the terminal server on {target.label}?
            </AlertDialogTitle>
            <AlertDialogDescription>{fullCopy}</AlertDialogDescription>
          </AlertDialogHeader>
          {target.countChanged && !failed && (
            <InlineAlert severity="warning">
              The session count changed: {terminals(target.count ?? 0)} run there now.
            </InlineAlert>
          )}
          {failed && (
            <InlineAlert severity="error">
              {unknownOutcome
                ? remotePhase.repair
                : oldStopped && target.local
                  ? `The old terminal server stopped, but the new one didn't start. Quit and reopen CodeSense to start it; your tabs are offered to reopen in Machines. ${remotePhase.repair}`
                  : oldStopped
                    ? `The old terminal server stopped, but ${target.label} isn't ready. ${remotePhase.repair}`
                    : target.reinstall
                      ? `The terminal server was not stopped. ${remotePhase.repair}`
                      : `The terminal server was not stopped. ${target.compatible ? `${target.label} remains connected.` : `${target.label} remains read-only.`} ${remotePhase.repair}`}
            </InlineAlert>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel ref={notNow} disabled={restarting}>
              {oldStopped || unknownOutcome
                ? "Close"
                : target.compatible
                  ? "Not now"
                  : "Keep read-only"}
            </AlertDialogCancel>
            {(oldStopped || unknownOutcome) && (
              <button
                type="button"
                className={buttonVariants({ variant: "outline" })}
                onClick={() => {
                  closeMachineTerminalServerPrompt();
                  openMachinesPanel(null);
                }}
              >
                Open Machines
              </button>
            )}
            {!oldStopped && !unknownOutcome && (
              <AlertDialogAction
                className={buttonVariants({
                  variant: !target.compatible && target.count === 0 ? "default" : "destructive",
                })}
                disabled={restarting}
                onClick={(event) => {
                  event.preventDefault();
                  void confirmMachineTerminalServerReplacement();
                }}
              >
                {restarting
                  ? target.update
                    ? "Updating…"
                    : "Restarting…"
                  : target.update
                    ? "Update terminal server"
                    : "Restart terminal server"}
              </AlertDialogAction>
            )}
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    );
  }

  if (!server) return null;
  const restarting = phase.kind === "restarting";
  const partial = phase.kind === "failed" && phase.oldStopped;
  const copy = restartCopy(server.liveSessions);
  const destructive = copy.destructive && !partial;

  const onRestart = async () => {
    const closed = await restart();
    if (closed === undefined) return;
    toast.success("Terminal server restarted", {
      description:
        closed && closed > 0
          ? `${terminals(closed)} closed. New terminals open normally.`
          : "New terminals open normally.",
    });
  };

  return (
    <AlertDialog
      open={open}
      onOpenChange={(next) => {
        if (next || restarting) return;
        if (partial) toast.warning(WONT_SURVIVE_QUIT);
        decline();
      }}
    >
      <AlertDialogContent data-testid="terminal-server-prompt" className="sm:max-w-md">
        <AlertDialogHeader>
          <AlertDialogTitle>
            {partial ? "Couldn't start the terminal server" : "Restart the terminal server?"}
          </AlertDialogTitle>
          <AlertDialogDescription>
            {partial
              ? "The old terminal server stopped, but the new one didn't start."
              : `${INTRO} ${copy.sentence}`}
          </AlertDialogDescription>
        </AlertDialogHeader>
        {failed && !partial && <InlineAlert severity="error">{RESTART_FAILED}</InlineAlert>}
        <AlertDialogFooter>
          <AlertDialogCancel ref={notNow} disabled={restarting}>
            Not now
          </AlertDialogCancel>
          <AlertDialogAction
            className={buttonVariants({ variant: destructive ? "destructive" : "default" })}
            data-destructive={destructive}
            disabled={restarting}
            onClick={(event) => {
              event.preventDefault();
              void onRestart();
            }}
          >
            {restarting ? "Restarting…" : partial ? "Try again" : copy.action}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
