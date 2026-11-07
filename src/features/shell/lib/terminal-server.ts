// SPDX-License-Identifier: MIT
import { useSyncExternalStore } from "react";

import {
  restartTerminalServer,
  terminalServerStatus,
  type IncompatibleTerminalServer,
  type RestartTerminalServerError,
} from "@/tauri/commands";

/** The backend's events: an incompatible terminal server was found; the user
 *  tried to open a terminal and it refused; the terminal server restarted
 *  under the app. */
export const TERMINAL_SERVER_INCOMPATIBLE_EVENT = "terminal-server-incompatible";
export const TERMINAL_SERVER_REFUSED_EVENT = "terminal-server-refused";
export const TERMINAL_SERVER_RESTARTED_EVENT = "terminal-server-restarted";

/** Where a restart stands: in progress, or failed — `oldStopped` when the old
 *  server is gone and only the new one did not start. */
export type RestartPhase =
  | { kind: "idle" }
  | { kind: "restarting" }
  | { kind: "failed"; oldStopped: boolean };

export interface TerminalServerState {
  /** The incompatible terminal server, while there is one. */
  server: IncompatibleTerminalServer | null;
  /** Whether the restart prompt is showing. */
  open: boolean;
  /** The user put the restart off ("Not now"): only their next attempt to
   *  open a terminal, or the inline notice, offers it again. */
  declined: boolean;
  phase: RestartPhase;
}

const INITIAL: TerminalServerState = {
  server: null,
  open: false,
  declined: false,
  phase: { kind: "idle" },
};

let state: TerminalServerState = INITIAL;
const listeners = new Set<() => void>();

function set(next: Partial<TerminalServerState>) {
  state = { ...state, ...next };
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The terminal server's state, for the prompt and the inline notice. */
export function useTerminalServer(): TerminalServerState {
  return useSyncExternalStore(subscribe, () => state);
}

/** The backend found an incompatible terminal server: prompt, unless the user
 *  already put the restart off. */
export function found(server: IncompatibleTerminalServer) {
  set({ server, open: !state.declined || state.open });
}

/** The user tried to open a terminal and the incompatible server refused it:
 *  offer the restart again, even after "Not now". */
export function refused(server: IncompatibleTerminalServer) {
  set({ server, open: true, declined: false });
}

/** "Not now": the prompt closes; the inline notice keeps the way back —
 *  unless the old server already stopped, which settles it (nothing
 *  incompatible runs any more). */
export function decline() {
  if (state.phase.kind === "failed" && state.phase.oldStopped) {
    settled();
    return;
  }
  set({ open: false, declined: true, phase: { kind: "idle" } });
}

function settled() {
  set({ ...INITIAL });
}

/** Offer the restart again — unless the server was replaced meanwhile, which
 *  settles it. */
export async function reopen() {
  try {
    const server = await terminalServerStatus();
    if (server) set({ server, open: true, phase: { kind: "idle" } });
    else settled();
  } catch {
    set({ open: true });
  }
}

function oldStopped(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as RestartTerminalServerError).oldStopped === true
  );
}

/** Restart the terminal server: resolves to how many terminals it closed
 *  (`null` when the old server did not say, or nothing incompatible was left
 *  to end), or `undefined` when it failed — the phase then says whether the
 *  old server already stopped. Safe to retry: the backend only ever ends an
 *  incompatible server, so a retry after an attempt that finished late just
 *  connects to the server that attempt started. */
export async function restart(): Promise<number | null | undefined> {
  set({ phase: { kind: "restarting" } });
  try {
    const ended = await restartTerminalServer();
    settled();
    return ended;
  } catch (error) {
    set({ phase: { kind: "failed", oldStopped: oldStopped(error) } });
    return undefined;
  }
}

/** How the backend's refusal of a new terminal by an incompatible terminal
 *  server begins (the Rust `Skew` display, which a Rust test pins). */
const REFUSAL_PREFIX = "restart terminal server";

/** Whether `error` is a new terminal refused by an incompatible terminal
 *  server — answered by the restart prompt, never shown as an error. */
export function isTerminalServerRefusal(error: unknown): boolean {
  return String(error).startsWith(REFUSAL_PREFIX);
}

/** How long a held terminal waits to try again when the terminal server's
 *  status could not be read. */
const STATUS_RETRY_MS = 5000;

/** A terminal the user opened was refused by the incompatible terminal
 *  server: offer the restart again, and run `retry` once the server has been
 *  restarted — at once if it already was. Returns a cancel. */
export function holdForTerminalServer(retry: () => void): () => void {
  let active = true;
  let seen = false;
  const listener = () => {
    if (state.server) seen = true;
    else if (seen) release();
  };
  function release() {
    listeners.delete(listener);
    if (active) retry();
    active = false;
  }
  listeners.add(listener);
  terminalServerStatus()
    .then((server) => {
      if (!active) return;
      if (server) refused(server);
      else release();
    })
    .catch(() => {
      if (!active) return;
      if (state.server) refused(state.server);
      // Nothing known to wait on: try the terminal again shortly.
      else setTimeout(release, STATUS_RETRY_MS);
    });
  return () => {
    active = false;
    listeners.delete(listener);
  };
}

/** Test-only: forget everything. */
export function resetTerminalServerForTests() {
  state = INITIAL;
  listeners.clear();
}

/** "3 terminals", "1 terminal". */
export function terminals(count: number): string {
  return count === 1 ? "1 terminal" : `${count} terminals`;
}

/** The prompt's words for what restarting costs, from the server's own count
 *  (`null` when it is too old to count). */
export function restartCopy(liveSessions: number | null): {
  /** The restart action's label. */
  action: string;
  /** Whether restarting loses anything (the destructive style). */
  destructive: boolean;
  /** The sentence saying what restarting does. */
  sentence: string;
} {
  if (liveSessions === 0) {
    return {
      action: "Restart terminal server",
      destructive: false,
      sentence: "It isn't running any terminals, so nothing is lost.",
    };
  }
  const action = `Restart terminal server (closes ${liveSessions === null ? "its terminals" : terminals(liveSessions)})`;
  if (liveSessions === 1) {
    return {
      action,
      destructive: true,
      sentence:
        "Restarting it closes the terminal it's running. Anything still running in it stops, and its scrollback is lost.",
    };
  }
  return {
    action,
    destructive: true,
    sentence: `Restarting it closes the ${liveSessions === null ? "terminals" : terminals(liveSessions)} it's running. Anything still running in them stops, and their scrollback is lost.`,
  };
}

/** The inline notice's words while the restart is put off. */
/** What held terminals — ones the server refused, waiting for it — add to the
 *  notice: they wait, and start on their own once the terminal server restarts
 *  (nothing typed meanwhile runs in them: they have no pane until then). */
const HELD_TERMINAL_COPY = "This terminal will start once the terminal server restarts.";

/** The held notice's lead sentence for `count` held terminals. */
export function heldCopy(count: number): string {
  return count === 1
    ? HELD_TERMINAL_COPY
    : `These ${count} terminals will start once the terminal server restarts.`;
}

export function noticeCopy(liveSessions: number | null): string {
  if (liveSessions === 0) {
    return "The terminal server from another CodeSense version is still running. Restart it to open new terminals.";
  }
  if (liveSessions === 1) {
    return "The terminal server from another CodeSense version is still running your terminal. Restart it to open new ones. This closes it.";
  }
  if (liveSessions === null) {
    return "The terminal server from another CodeSense version is still running your terminals. Restart it to open new ones. This closes them.";
  }
  return `The terminal server from another CodeSense version is still running your ${liveSessions} terminals. Restart it to open new ones. This closes those ${liveSessions}.`;
}
