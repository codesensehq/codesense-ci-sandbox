// SPDX-License-Identifier: MIT
import { isAgentRuntime } from "@/shared/lib/agent-runtimes";
import type { HandlerRegistry } from "../mock-router";
import { SESSION_NOT_FOUND } from "@/features/viewer/lib/terminal-attach";
import type { MockEventBus } from "../mock-events";
import { MockTerminalHost } from "../terminal-layout-host";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME, TEAM_CONTENT_PATH, mockAppState } from "../fixtures";
import { workspaceKey } from "@/shared/lib/workspace-key";
import {
  AGENT_TEAM_PROCESSES,
  AGENT_TEAM_STORY_WORKSPACES,
  agentTeamHostSnapshots,
  agentTeamTabs,
  isAgentTeamWorld,
  isAgentTeamsWorld,
  SECOND_ORG_SLUG,
  SECOND_TEAM_CONTENT_PATH,
  SECOND_TEAM_KEY,
  SECOND_TEAM_NAME,
  SECOND_TEAM_TABS,
  SECOND_TEAM_WORKSPACES,
} from "./agents-team";
import {
  isLeftNavDemo,
  LEFT_NAV_DEMO_WORKSPACES,
  leftNavDemoProcesses,
  leftNavDemoTabs,
} from "./left-nav-demo";
import type {
  TerminalCloseMode,
  TerminalGridState,
  TerminalOpenTabRequest,
  TerminalPlacement,
  TerminalRespawnScope,
  TerminalTabSpawn,
  TerminalWorkspaceDescriptor,
  RuntimeAdmissionRow,
} from "@/tauri/commands";

const TEAM_KEY = `${MOCK_ORG_SLUG}:${MOCK_TEAM_NAME}`;
const CONTENT_WS = `generic:${TEAM_CONTENT_PATH}`;
const SCRATCH_DIR = `${TEAM_CONTENT_PATH}/scratch`;
const SCRATCH_WS = `generic:${SCRATCH_DIR}`;

/** Canned scrollback per session: the mock runs no shell, so a terminal shows
 *  what its session would have printed. */
const OUTPUT: Record<string, string> = {
  "demo-shell":
    "\x1b[32m~/widgets/content\x1b[0m $ ls\r\ndocs  epics  scratch  README.md\r\n\x1b[32m~/widgets/content\x1b[0m $ ",
  "demo-tests":
    "\x1b[32m~/widgets/content\x1b[0m $ npm test\r\n\r\n> widgets@1.0.0 test\r\n> vitest run\r\n\r\n \x1b[31mFAIL\x1b[0m  src/cart.test.ts > applies the discount\r\n   AssertionError: expected 90 to be 81\r\n\r\n Test Files  \x1b[31m1 failed\x1b[0m | 12 passed (13)\r\n",
};

/** How long after a resize a mock agent TUI repaints its frame. */
const REPAINT_DELAY_MS = 150;

/** Output written after boot (a wake's paste in the session-timers demo). */
const appended = new Map<string, string>();

const output = (session: string): string =>
  (OUTPUT[session] ?? "\x1b[32m~\x1b[0m $ ") + (appended.get(session) ?? "");

/** A mock session's canned output as the plain lines an emulator would hold. */
const textLines = (session: string): string[] =>
  output(session)
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "")
    .split("\n")
    .map((line) => line.replace(/\r$/, ""));

/** The viewer's share of the main area beside the CLI (`cliChrome.splitRatio`) in the agent team
 *  worlds: the CLI gets most of the height so the Agents pane shows the team's workspaces. */
const AGENT_TEAM_VIEWER_SHARE = 0.38;
/** The same with two teams open: the CLI gets nearly all of it, so the Agents pane lists both
 *  teams' workspaces without scrolling in a review capture. */
const AGENT_TEAMS_VIEWER_SHARE = 0.12;

/** The `?terminals=demo` world's persisted viewer state: two workspaces of the
 *  fixture team, one whose terminals the host still runs, one whose ended, with the CLI
 *  shown. The agent team worlds add their story workspaces and a split, and
 *  `?agents=teams` its second team, open with its own active workspace and workspace order. */
function demoAppState(
  storyWorkspaces: readonly object[] = [],
  splitRatio?: number,
  secondTeam = false,
): string {
  const state = JSON.parse(mockAppState) as { viewer: Record<string, unknown> };
  const secondContent = SECOND_TEAM_WORKSPACES[0].id;
  state.viewer = {
    ...state.viewer,
    schemaVersion: 10,
    ...(secondTeam && {
      openedTeams: [...(state.viewer.openedTeams as string[]), SECOND_TEAM_KEY],
      workspaceOrderByTeam: { [SECOND_TEAM_KEY]: SECOND_TEAM_WORKSPACES.map((ws) => ws.id) },
    }),
    workspaces: {
      [TEAM_KEY]: [
        { id: CONTENT_WS, kind: "generic", cwd: TEAM_CONTENT_PATH, status: "ready" },
        { id: SCRATCH_WS, kind: "generic", cwd: SCRATCH_DIR, status: "ready" },
        ...storyWorkspaces,
      ],
      ...(secondTeam && { [SECOND_TEAM_KEY]: SECOND_TEAM_WORKSPACES }),
    },
    activeWorkspace: {
      [TEAM_KEY]: CONTENT_WS,
      ...(secondTeam && { [SECOND_TEAM_KEY]: secondContent }),
    },
    // The CLI's chrome is saved as one `cliChrome` value; flat chrome keys would be read as
    // a pre-`cliChrome` state file and dropped, losing the shown CLI and the split.
    cliChrome: { visible: true, ...(splitRatio !== undefined && { splitRatio }) },
  };
  return JSON.stringify(state);
}

function descriptor(
  displayName: string,
  kind: string,
  dir: string,
  org: string = MOCK_ORG_SLUG,
  team: string = MOCK_TEAM_NAME,
): TerminalWorkspaceDescriptor {
  return { displayName, kind, org, team, worktreePath: dir };
}

/** Seed the host as if it had kept running while the app was closed: the
 *  content workspace's live shell and its exited test run. */
function seedDemo(host: MockTerminalHost): void {
  const content = workspaceKey(TEAM_KEY, { id: CONTENT_WS });
  const contentDescriptor = descriptor("content", "generic", TEAM_CONTENT_PATH);
  host.seed(content, contentDescriptor, { session: "demo-shell", name: "zsh" });
  host.seed(content, contentDescriptor, {
    session: "demo-tests",
    name: "npm test",
    exit: { code: 1 },
  });
}

/**
 * The terminal hosts' layout, served by one in-memory host whose changes are
 * pushed on `terminal-runtime-event` under its admitted target — so the viewer
 * store is the same
 * projection it is against the real hosts. A new terminal opens empty (no
 * shell runs); `?terminals=demo` boots a world the host kept running.
 */
export function terminalLayoutHandlers(search: string, events: MockEventBus): HandlerRegistry {
  const host: MockTerminalHost = new MockTerminalHost((change) =>
    events.emit("terminal-runtime-event", {
      target: host.target(),
      event: { LayoutChanged: change },
    }),
  );
  // The `?agents=team` world runs in the demo host, with its agents' story workspaces open.
  const agentTeam = isAgentTeamWorld(search);
  const navDemo = isLeftNavDemo(search);
  const processes: Readonly<Record<string, string>> = {
    ...(agentTeam ? AGENT_TEAM_PROCESSES : {}),
    ...(navDemo ? leftNavDemoProcesses() : {}),
  };
  const demo = agentTeam || navDemo || new URLSearchParams(search).get("terminals") === "demo";
  if (demo) seedDemo(host);
  // The `?nav=demo` world's story workspaces, each with its agents.
  if (navDemo) {
    for (const tab of leftNavDemoTabs()) {
      host.seed(
        workspaceKey(TEAM_KEY, { id: tab.workspaceId, kind: "story", storyId: tab.workspaceId }),
        descriptor(tab.workspaceId, "story", tab.dir),
        { session: tab.session, name: tab.name, exit: tab.exit },
      );
    }
  }
  const storyWorkspaces = [
    ...(agentTeam ? AGENT_TEAM_STORY_WORKSPACES : []),
    ...(navDemo ? LEFT_NAV_DEMO_WORKSPACES : []),
  ];
  if (agentTeam) {
    for (const tab of agentTeamTabs(search)) {
      host.seed(
        workspaceKey(TEAM_KEY, {
          id: tab.workspaceId,
          kind: tab.kind,
          ...(tab.kind === "story" && { storyId: tab.workspaceId }),
        }),
        descriptor(tab.kind === "story" ? tab.workspaceId : "content", tab.kind, tab.dir),
        { session: tab.session, name: tab.name },
      );
    }
  }
  // The `?agents=teams` world's second team, in its own org.
  const secondTeam = isAgentTeamsWorld(search);
  if (secondTeam) {
    for (const tab of SECOND_TEAM_TABS) {
      host.seed(
        workspaceKey(SECOND_TEAM_KEY, {
          id: tab.workspaceId,
          kind: tab.kind,
          ...(tab.kind === "story" && { storyId: tab.workspaceId }),
        }),
        descriptor(
          tab.kind === "story" ? tab.workspaceId : "content",
          tab.kind,
          tab.dir,
          SECOND_ORG_SLUG,
          SECOND_TEAM_NAME,
        ),
        { session: tab.session, name: tab.name },
      );
    }
  }
  let streams = 0;
  // Each session's open output stream: an agent's TUI repaints its current frame
  // on a resize, which is how a reattached agent tab paints (it never replays
  // its headless scrollback).
  const streamOf = new Map<string, number>();
  const reserved = new Set<number>();
  // SW-AGR-6 demo: write into a session as its terminal server would (a wake's
  // paste), so a delivered wake's scrollback marker has its header to find.
  (
    globalThis as { __mockTerminalWrite?: (session: string, text: string) => void }
  ).__mockTerminalWrite = (session, text) => {
    appended.set(session, (appended.get(session) ?? "") + text);
    const stream = streamOf.get(session);
    if (stream !== undefined)
      events.emit(`pty-output-stream-${stream}`, { data: text, seq: output(session).length });
  };
  // Each session's PTY size and how many times it changed.
  const sizeOf = new Map<string, { cols: number; rows: number; resizeGen: number }>();
  const sizeFor = (session: string) => sizeOf.get(session) ?? { cols: 80, rows: 24, resizeGen: 0 };
  const reference = (value: unknown, field: "session" | "workspace"): string => {
    if (typeof value === "string") return value;
    if (typeof value !== "object" || value === null) throw new Error("invalid runtime reference");
    const target = value as Record<string, unknown>;
    const { member, admissionRevision, namespace } = host.target();
    const got = target.namespace as Record<string, unknown> | undefined;
    const gotMember = target.member as Record<string, unknown> | undefined;
    if (
      gotMember?.machineId !== member.machineId ||
      gotMember.slot !== member.slot ||
      target.admissionRevision !== admissionRevision ||
      got?.generation !== namespace.generation ||
      got.serverId !== namespace.serverId ||
      got.epoch !== namespace.epoch ||
      typeof target[field] !== "string"
    )
      throw new Error("runtime admission changed");
    return target[field];
  };
  return {
    ...(demo && {
      load_app_state: () =>
        demoAppState(
          storyWorkspaces,
          storyWorkspaces.length === 0
            ? undefined
            : secondTeam
              ? AGENT_TEAMS_VIEWER_SHARE
              : AGENT_TEAM_VIEWER_SHARE,
          secondTeam,
        ),
      check_path_exists: (args: Record<string, unknown>) =>
        [TEAM_CONTENT_PATH, SCRATCH_DIR].includes(String(args.path)) ||
        storyWorkspaces.some((ws) => ws.cwd === String(args.path)) ||
        (secondTeam &&
          [SECOND_TEAM_CONTENT_PATH, ...SECOND_TEAM_WORKSPACES.map((ws) => ws.cwd)].includes(
            String(args.path),
          )),
    }),
    terminal_layout_admission: () => {
      const { member, admissionRevision: revision, namespace } = host.target();
      const row: RuntimeAdmissionRow = {
        member,
        revision,
        state: "live",
        namespace,
        historical: null,
      };
      return { rows: [row] };
    },
    terminal_layout_snapshot: () => [host.reply()],
    // The team worlds' host reports activity for the agents whose subagents run; the rest stay
    // uninstrumented, as a host with no snapshot for a session leaves it.
    ...(agentTeam && {
      terminal_activity_snapshot: () => ({
        target: host.target(),
        snapshots: agentTeamHostSnapshots(search).filter((a) => host.contains(a.session)),
      }),
    }),
    terminal_open_tab: (args) => host.openTab(args.request as TerminalOpenTabRequest),
    terminal_respawn_tab: (args) =>
      host.respawnTab(
        String(args.machineId),
        reference(args.session, "session"),
        args.spawn as TerminalTabSpawn,
        args.scope as TerminalRespawnScope | undefined,
      ),
    terminal_close_tab: (args) =>
      host.closeTab(reference(args.session, "session"), args.mode as TerminalCloseMode),
    terminal_place_tab: (args) =>
      host.placeTab(
        reference(args.session, "session"),
        String(args.workspace),
        args.placement as TerminalPlacement,
        args.index as number | undefined,
        args.descriptor as TerminalWorkspaceDescriptor | undefined,
      ),
    terminal_rename_tab: (args) =>
      host.renameTab(reference(args.session, "session"), String(args.name)),
    terminal_move_tab: (args) =>
      host.moveTab(reference(args.session, "session"), Number(args.toIndex)),
    terminal_set_active: (args) =>
      host.setActive(
        reference(args.workspace, "workspace"),
        args.session == null ? null : reference(args.session, "session"),
      ),
    terminal_set_grid: (args) =>
      host.setGrid(reference(args.workspace, "workspace"), args.grid as TerminalGridState),
    terminal_set_view_hint: (args) => {
      reference(args.session, "session");
    },
    terminal_set_tags: (args) =>
      host.setTags(reference(args.session, "session"), args.patch as Record<string, string | null>),
    terminal_close_workspace: (args) => host.closeWorkspace(reference(args.workspace, "workspace")),
    reserve_terminal_stream: () => {
      const stream = ++streams;
      reserved.add(stream);
      return stream;
    },
    subscribe_terminal: (args) => {
      const session = reference(args.ptyId, "session");
      if (!host.contains(session))
        throw new Error(`subscribe_terminal: ${SESSION_NOT_FOUND}: ${session}`);
      const snapshot = output(session);
      let stream: number;
      if (args.stream == null) {
        stream = ++streams;
      } else {
        stream = Number(args.stream);
        if (!reserved.delete(stream))
          throw new Error(`subscribe_terminal: stream ${stream} is not reserved`);
      }
      streamOf.set(session, stream);
      const size = sizeFor(session);
      return {
        snapshot: {
          cols: size.cols,
          rows: size.rows,
          seq: snapshot.length,
          resize_gen: size.resizeGen,
          scrollback: 5000,
          vt: snapshot,
        },
        stream_id: stream,
        story_id: null,
        workspace_id: null,
        epic_id: null,
      };
    },
    unsubscribe_terminal: (args) => {
      reserved.delete(Number(args.streamId));
    },
    write_pty: (args) => {
      reference(args.sessionId, "session");
    },
    submit_pty_line: (args) => {
      reference(args.sessionId, "session");
      return { submitted: true };
    },
    // Notes' "Send to…": every mock agent runs on this Mac.
    send_prompt_to_agent: (args) => {
      if (typeof args.handle !== "string" || !args.handle.trim()) {
        throw new Error("handle is required");
      }
      if (typeof args.text !== "string" || !args.text.trim()) {
        throw new Error("there is nothing to send");
      }
      if (
        args.sessionId != null &&
        (typeof args.sessionId !== "string" || !args.sessionId.trim())
      ) {
        throw new Error("the agent has no running session");
      }
      return {
        status: args.submit === false ? "typed" : "submitted",
        submitted: args.submit !== false,
        machine: null,
        code: null,
        message: null,
      };
    },
    session_has_live_agent: (args) => host.isAgent(reference(args.ptyId, "session")),
    resize_pty: (args) => {
      const session = reference(args.sessionId, "session");
      const stream = streamOf.get(session);
      const cols = Number(args.cols);
      const rows = Number(args.rows);
      const size = sizeFor(session);
      if (cols !== size.cols || rows !== size.rows) {
        const resizeGen = size.resizeGen + 1;
        sizeOf.set(session, { cols, rows, resizeGen });
        if (stream !== undefined) {
          events.emit(`pty-output-stream-${stream}`, {
            resized: { at_seq: output(session).length, resize_gen: resizeGen, cols, rows },
          });
        }
      }
      if (stream === undefined || !host.isAgent(session)) return;
      // The TUI repaints after the SIGWINCH lands, not inside the resize call.
      const frame = `\x1b[2J\x1b[H${output(session)}`;
      setTimeout(() => {
        events.emit(`pty-output-stream-${stream}`, {
          data: frame,
          seq: output(session).length + frame.length,
        });
      }, REPAINT_DELAY_MS);
    },
    read_terminal_text: (args) => {
      const session = reference(args.ptyId, "session");
      if (!host.contains(session))
        throw new Error(`read_terminal_text: ${SESSION_NOT_FOUND}: ${session}`);
      return {
        text: textLines(session).slice(-Number(args.lines)).join("\n"),
        seq: output(session).length,
        alt_screen: false,
      };
    },
    session_is_idle: (args) => {
      reference(args.ptyId, "session");
      return true;
    },
    // What each shell runs (the close verbs' Hide-or-End probe): every shell
    // sits at its prompt unless it has a mock process entry.
    sessions_running_processes: (args) =>
      (args.ptyIds as unknown[]).map((id) => {
        const session = reference(id, "session");
        return {
          foreground: processes[session] ?? null,
          background: [],
        };
      }),
    // The built-in agent CLIs the tab floor probes for.
    list_agent_signals: () =>
      [
        { basename: "claude", attention: "hooks", quietSecs: 10 },
        { basename: "codex", attention: "hooks", quietSecs: 10 },
        { basename: "opencode", attention: "hooks", quietSecs: 10 },
        { basename: "hermes", attention: null, quietSecs: 10 },
      ].filter((signal) => isAgentRuntime(signal.basename)),
    // Which agent CLI runs under each tab's shell: the team world's hand-started agents.
    sessions_have_live_agent: (args) =>
      (args.queries as { ptyId: string; agentName: string }[]).map((q) => ({
        ptyId: q.ptyId,
        alive: processes[q.ptyId] === q.agentName,
      })),
  };
}
