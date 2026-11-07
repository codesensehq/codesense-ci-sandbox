// SPDX-License-Identifier: MIT
import type { CommandHandler, HandlerRegistry } from "../mock-router";
import type {
  IncompatibleTerminalServer,
  LocalActivityUpgrade,
  RestartTerminalServerError,
  StoppedTerminalServer,
  TerminalServerOverview,
  UnownedTerminalSession,
} from "@/tauri/commands";

/** A terminal server left running by an older CodeSense: protocol 4 against
 *  this app's 5, running `liveSessions` terminals a restart would close. Typed
 *  as the real `terminal_server_status` DTO, so a contract change breaks it at
 *  compile time. */
export function incompatibleTerminalServer(
  liveSessions: number | null,
): IncompatibleTerminalServer {
  const ends =
    liveSessions === null
      ? "its running sessions"
      : `${liveSessions} session${liveSessions === 1 ? "" : "s"}`;
  return {
    liveSessions,
    serverProtocol: 4,
    appProtocol: 5,
    reason: `restart terminal server (ends ${ends}): it speaks protocol 4, this app 5`,
  };
}

/** SV-7: the running terminal server a mock world starts with: 4 sessions (3
 *  running, 2 of them agents, 1 exited and kept), `otherInstances` other
 *  CodeSense instances connected and `unowned` sessions no app holds. */
export function runningTerminalServer(
  otherInstances = 0,
  unowned = 0,
  now = Date.now(),
): TerminalServerOverview {
  const left: UnownedTerminalSession[] = Array.from({ length: unowned }, (_, i) => ({
    session: `left-${i + 1}`,
    command: i === 0 ? "claude" : null,
    startedAtMs: now - (i + 2) * 3_600_000,
    running: true,
    runningAgent: i === 0,
  }));
  const running = ["tab-1", "tab-2", "tab-3", ...left.map((l) => l.session)];
  return {
    state: "running",
    pid: 4321,
    version: "0.0.25",
    startedAtMs: now - 3 * 3_600_000,
    sessions: 4 + unowned,
    running: running.length,
    runningAgents: 2,
    exitedKept: 1,
    ends: running.length,
    connectedInstances: 1 + otherInstances,
    otherInstances,
    unowned: left,
    expected: { serverId: "mock-server", epoch: "mock-epoch-1", pid: 4321, sessionIds: running },
    incompatible: null,
    detail: null,
  };
}

/** SV-7: no terminal server runs. */
export function stoppedTerminalServer(): TerminalServerOverview {
  return {
    state: "stopped",
    pid: null,
    version: null,
    startedAtMs: null,
    sessions: null,
    running: null,
    runningAgents: null,
    exitedKept: null,
    ends: null,
    connectedInstances: null,
    otherInstances: null,
    unowned: [],
    expected: null,
    incompatible: null,
    detail: null,
  };
}

/** A freshly started (or restarted) server: this app connected, nothing yet. */
function freshTerminalServer(epoch: string): TerminalServerOverview {
  return {
    ...runningTerminalServer(),
    pid: 5000,
    startedAtMs: Date.now(),
    sessions: 0,
    running: 0,
    runningAgents: 0,
    exitedKept: 0,
    ends: 0,
    expected: { serverId: "mock-server", epoch, pid: 5000, sessionIds: [] },
  };
}

function sameReview(a: LocalActivityUpgrade, b: LocalActivityUpgrade | null): boolean {
  return (
    b !== null &&
    a.serverId === b.serverId &&
    a.epoch === b.epoch &&
    a.pid === b.pid &&
    a.sessionIds.join("\n") === b.sessionIds.join("\n")
  );
}

/** How long the mock restart takes, so the "Restarting…" state is visible. */
const RESTART_MS = 800;

/** How the first restart attempt fails, per `?restart=`: the old server stops
 *  but the new one doesn't start (`partial`), or the old one can't be ended
 *  (`error`). The attempt after it succeeds. */
function firstFailure(kind: string | null): RestartTerminalServerError | null {
  if (kind === "partial") {
    return { oldStopped: true, message: "the new terminal server did not start" };
  }
  if (kind === "error") {
    return { oldStopped: false, message: "the restart did not finish within 10s" };
  }
  return null;
}

/**
 * The terminal server's version skew. `?terminalServer=incompatible` boots a
 * world whose terminal server is from another CodeSense version, so the
 * "restart terminal server" prompt shows; `&liveSessions=N` sets how many
 * terminals it runs (default 3; `unknown` for a server too old to count) and
 * `&restart=partial|error` makes the first restart fail. While it runs, a new
 * terminal is refused with its reason (the prompt answers that); a successful
 * restart clears it, after which terminals `open` as usual. Any other world has
 * a compatible server (`null`).
 */
export function terminalServerHandlers(search: string, open: CommandHandler): HandlerRegistry {
  const params = new URLSearchParams(search);
  const count = params.get("liveSessions");
  const liveSessions = count === "unknown" ? null : Number(count ?? 3);
  let incompatible: IncompatibleTerminalServer | null =
    params.get("terminalServer") === "incompatible"
      ? incompatibleTerminalServer(liveSessions)
      : null;
  let failure = firstFailure(params.get("restart"));
  // SV-7: `?terminalServer=stopped` starts with none running; otherwise one
  // runs, with `&otherInstances=N` other instances and `&unowned=N` sessions
  // no app holds.
  const world = params.get("terminalServer");
  let server: TerminalServerOverview =
    world === "inProcess"
      ? { ...stoppedTerminalServer(), state: "notApplicable" }
      : world === "stopped"
        ? stoppedTerminalServer()
        : runningTerminalServer(
            Number(params.get("otherInstances") ?? 0),
            Number(params.get("unowned") ?? 0),
          );
  let epochs = 1;
  const changed: RestartTerminalServerError = {
    oldStopped: false,
    message: "The terminal server or its sessions changed. Review the update again.",
  };
  const overview = (): TerminalServerOverview =>
    incompatible
      ? {
          ...stoppedTerminalServer(),
          state: "incompatible",
          pid: 3210,
          ends: incompatible.liveSessions,
          incompatible,
          detail: incompatible.reason,
        }
      : server;
  return {
    terminal_server_overview: overview,
    // SV-8: the macOS menu-bar item is a native surface; the mock has none.
    set_terminal_server_menu_bar: () => undefined,
    stop_terminal_server: (args) => {
      const { expected } = args as { expected: LocalActivityUpgrade };
      if (!sameReview(expected, server.expected)) return Promise.reject(changed);
      server = stoppedTerminalServer();
      return Promise.resolve<StoppedTerminalServer>({ ended: expected.sessionIds.length });
    },
    start_terminal_server: () => {
      if (server.state === "notApplicable") {
        return Promise.reject("this app hosts its terminals itself");
      }
      if (server.state === "stopped") server = freshTerminalServer(`mock-epoch-${++epochs}`);
      return overview();
    },
    replace_terminal_server: (args) =>
      new Promise<number>((resolve, reject) => {
        const { expected } = args as { expected: LocalActivityUpgrade };
        setTimeout(() => {
          if (!sameReview(expected, server.expected)) {
            reject(changed);
            return;
          }
          server = freshTerminalServer(`mock-epoch-${++epochs}`);
          resolve(expected.sessionIds.length);
        }, RESTART_MS);
      }),
    end_unowned_terminal_session: (args) => {
      const { session } = args as { session: string };
      const left = server.unowned.find((u) => u.session === session);
      if (!left) {
        return Promise.reject(
          `${session} is not a session no app holds (any more); nothing was ended`,
        );
      }
      const sessionIds = server.expected?.sessionIds.filter((id) => id !== session) ?? [];
      server = {
        ...server,
        sessions: (server.sessions ?? 1) - 1,
        running: (server.running ?? 1) - 1,
        ends: sessionIds.length,
        unowned: server.unowned.filter((u) => u.session !== session),
        expected: server.expected ? { ...server.expected, sessionIds } : null,
      };
      return Promise.resolve();
    },
    terminal_open_tab: (args) => (incompatible ? Promise.reject(incompatible.reason) : open(args)),
    terminal_server_status: (): IncompatibleTerminalServer | null => incompatible,
    restart_terminal_server: () =>
      new Promise<number | null>((resolve, reject) => {
        setTimeout(() => {
          if (failure) {
            const failed = failure;
            if (failed.oldStopped) incompatible = null;
            failure = null;
            reject(failed);
            return;
          }
          const ended = incompatible?.liveSessions ?? null;
          incompatible = null;
          resolve(ended);
        }, RESTART_MS);
      }),
  };
}
