// SPDX-License-Identifier: MIT
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { openNativeMenu } from "@/features/shell/native-menu-openers";
vi.mock("@/shared/lib/team-key", async (original) => ({
  ...(await original<typeof import("@/shared/lib/team-key")>()),
  getActiveTeamKey: () => "acme:default",
}));
/**
 * A Hidden tab whose process ends, seen from the tray. The real status listener
 * (`pty-exit` → the agent-state store), the real viewer store over the test
 * terminal host (a Hidden tab is a Background entry), the real close verbs and
 * the real tray; only the event bus and the IPC are faked.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, renderHook, screen, waitFor, within } from "@/test-utils";
import userEvent from "@testing-library/user-event";

const h = vi.hoisted(() => ({
  handlers: new Map<string, Set<(e: { payload: unknown }) => void>>(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    let set = h.handlers.get(name);
    if (!set) h.handlers.set(name, (set = new Set()));
    set.add(cb);
    return () => set.delete(cb);
  }),
}));
// This suite exercises other inputs; ActivityV1 recovery has its own seam tests.
vi.mock("@/shared/hooks/use-terminal-activity", () => ({ useTerminalActivity: () => {} }));
vi.mock("@/tauri/commands", () => ({
  LOCAL_MACHINE_ID: "local",
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  sessionsHaveLiveAgent: vi.fn(async (queries: { ptyId: string }[]) =>
    queries.map((q) => ({ ptyId: q.ptyId, alive: true })),
  ),
  sessionsRunningProcesses: vi.fn(async (ids: string[]) =>
    ids.map(() => ({ foreground: null, background: [] })),
  ),
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() } }));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import { resetLivenessPollerForTests } from "@/shared/hooks/use-agent-liveness-monitor";
import {
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import {
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  useViewerStore,
  type Workspace,
} from "@/shared/stores/viewer-store";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { hideTab, restartHiddenTab } from "../lib/terminal-close";
import { WorkspaceTray } from "./workspace-tray";
import { toast } from "sonner";

const TEAM = "acme:default";
const WS = "ws-story";
const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

installTerminalLayoutHarness();

function emit(name: string, payload: unknown) {
  const set = h.handlers.get(name);
  if (!set?.size) throw new Error(`no handler registered for ${name}`);
  act(() => {
    for (const cb of [...set]) cb({ payload });
  });
}

/** `id`'s process ends: its host marks the entry exited and emits `pty-exit`. */
function processEnds(id: string, code: number | null, signal: string | null) {
  act(() => terminalHost().exit(id, code, signal));
  emit("pty-exit", { session_id: id, exit_code: code, signal, requested: false });
}

const hiddenIn = () =>
  selectHiddenTerminalTabs(useViewerStore.getState(), TEAM, WS).map((t) => t.id);
const tabsIn = () => selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS).map((t) => t.id);

/** Every trace a session leaves in the stores. */
function traces(id: string) {
  const s = useViewerStore.getState();
  const a = useAgentStateStore.getState();
  return {
    aiStatus: s.aiStatusBySession[id],
    progress: s.progressBySession[id],
    lives: s.terminalLives[id],
    agent: a.bySession[id],
    exit: a.exitBySession[id],
    launch: a.launchBySession[id],
    relaunch: a.relaunchBySession[id],
  };
}
const NO_TRACES = {
  aiStatus: undefined,
  progress: undefined,
  lives: undefined,
  agent: undefined,
  exit: undefined,
  launch: undefined,
  relaunch: undefined,
};

/** Give the session state in every store it can leave some in. */
function leaveTraces(id: string) {
  emit("terminal-status-update", {
    tab_session_id: id,
    status: "running",
    hook_event: "UserPromptSubmit",
  });
  useViewerStore.setState((s) => ({
    progressBySession: { ...s.progressBySession, [id]: { phase: "dev", note: "working" } },
    terminalLives: { ...s.terminalLives, [id]: 1 },
  }));
  const a = useAgentStateStore.getState();
  a.recordLaunch(id, { line: "'claude'", agent: "claude", spec: { agentPath: "claude" } });
  projectSessionStatus(id, "done", { crashId: "dismissed" });
  a.setRelaunch(id, { busy: false, failure: { action: "Restart", reason: "x" } });
}

const unmounts: (() => void)[] = [];

async function setup(workspace: Workspace = story(WS)) {
  useViewerStore.setState({
    workspaces: { [TEAM]: [workspace] },
    activeWorkspace: { [TEAM]: WS },
    softClosedWorkspaces: { [TEAM]: {} },
    foregroundLayout: {},
    aiStatusBySession: {},
    progressBySession: {},
    terminalLives: {},
  });
  seedTerminalTabs(
    TEAM,
    WS,
    ["t1", "t2", "t3"].map((id) => ({ session: id, name: id })),
    { active: "t1" },
  );
  const status = renderHook(() => useTerminalStatusEvents());
  const tray = render(
    <WorkspaceTray teamKey={TEAM} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />,
  );
  unmounts.push(() => {
    tray.unmount();
    status.unmount();
  });
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
  hideTab(TEAM, WS, "t1");
  await waitFor(() => expect(hiddenIn()).toEqual(["t1"]));
}

const badge = () => screen.getByTestId(`tray-chip-hidden-${WS}`);

beforeEach(() => {
  resetLivenessPollerForTests();
  resetPrunedAgentSessionsForTests();
  h.handlers.clear();
  useAgentStateStore.setState({
    bySession: {},
    exitBySession: {},
    launchBySession: {},
    relaunchBySession: {},
  });
});

afterEach(() => {
  for (const u of unmounts.splice(0)) u();
  resetLivenessPollerForTests();
});

describe("a Hidden tab whose process ends by itself", () => {
  it("a crash keeps it listed, marked crashed with how it ended, on the badge and in the menu", async () => {
    const user = userEvent.setup();
    await setup();
    processEnds("t1", null, "Segmentation fault: 11");

    expect(hiddenIn()).toEqual(["t1"]);
    expect(badge()).toHaveAccessibleName("1 hidden tab — restore, Crashed, SIGSEGV");
    expect(screen.getByTestId(`tray-chip-hidden-ended-${WS}`)).toHaveAttribute(
      "data-mark",
      "crashed",
    );

    await user.click(badge());
    expect(screen.getByTestId("hidden-tab-ended-t1")).toHaveTextContent("“t1” · Crashed · SIGSEGV");
    expect(screen.getByText("Restore “t1”")).toBeInTheDocument();
    expect(screen.getByText("Restart “t1”")).toBeInTheDocument();
    expect(screen.getByText("Dismiss “t1”")).toBeInTheDocument();
    expect(screen.queryByText("End “t1”")).toBeNull();
  });

  it("after a reload (only the layout's exit left) it is still listed and marked", async () => {
    await setup();
    act(() => terminalHost().exit("t1", null, "Killed: 9"));
    expect(hiddenIn()).toEqual(["t1"]);
    expect(screen.getByTestId(`tray-chip-hidden-ended-${WS}`)).toHaveAttribute(
      "data-mark",
      "crashed",
    );
  });

  it("the badge ranks by the shared roll-up: a wait outranks a crash, and both are named", async () => {
    await setup();
    hideTab(TEAM, WS, "t2");
    await waitFor(() => expect(hiddenIn()).toEqual(["t1", "t2"]));
    emit("terminal-status-update", { tab_session_id: "t2", status: "needs-permission" });
    act(() => projectSessionStatus("t2", "paused"));
    processEnds("t1", null, "Killed: 9");
    expect(screen.getByTestId(`tray-chip-hidden-status-${WS}`)).toHaveAttribute(
      "data-mark",
      "permission",
    );
    expect(badge()).toHaveAccessibleName("2 hidden tabs — restore, 1 needs approval, 1 crashed");
  });

  it("a plain exit (even nonzero) is marked ended", async () => {
    await setup();
    processEnds("t1", 1, null); // `false; exit`
    expect(hiddenIn()).toEqual(["t1"]);
    expect(screen.getByTestId(`tray-chip-hidden-ended-${WS}`)).toHaveAttribute(
      "data-mark",
      "ended",
    );
  });

  it("Dismiss removes it and every trace of its session", async () => {
    const user = userEvent.setup();
    await setup();
    leaveTraces("t1");
    processEnds("t1", 1, null);
    expect(traces("t1").exit).toBeDefined();

    await user.click(badge());
    await user.click(screen.getByText("Dismiss “t1”"));

    await waitFor(() => expect(hiddenIn()).toEqual([]));
    expect(terminalHost().contains("t1")).toBe(false);
    expect(traces("t1")).toEqual(NO_TRACES);
    expect(screen.queryByTestId(`tray-chip-hidden-${WS}`)).toBeNull();
    // A late event for it lands nowhere.
    emit("pty-exit", { session_id: "t1", exit_code: 1, signal: null, requested: true });
    expect(traces("t1")).toEqual(NO_TRACES);
  });

  it("Restore puts it back on its tile with its exit kept, so the pane shows the ended bar", async () => {
    const user = userEvent.setup();
    await setup();
    processEnds("t1", 1, null);
    await user.click(badge());
    await user.click(screen.getByText("Restore “t1”"));
    await waitFor(() => expect(tabsIn()).toEqual(["t2", "t3", "t1"]));
    expect(hiddenIn()).toEqual([]);
    expect(traces("t1").exit?.code).toBe(1);
  });

  it("Restart starts a new process in the same entry, then puts it back on its tile", async () => {
    const user = userEvent.setup();
    await setup();
    processEnds("t1", null, "Killed: 9");
    await user.click(badge());
    await user.click(screen.getByText("Restart “t1”"));
    await waitFor(() => expect(tabsIn()).toEqual(["t2", "t3", "t1"]));
    expect(hiddenIn()).toEqual([]);
    const entry = Object.values(terminalHost().snapshot().workspaces)
      .flatMap((w) => w.tabs)
      .find((t) => t.session === "t1");
    expect(entry?.state).toEqual({ kind: "live" });
    // The ended life's exit went with it; the pane remounts on the new one.
    expect(traces("t1").exit).toBeUndefined();
    expect(traces("t1").lives).toBe(1);
  });
});

describe("a Hidden tab's Restart the host refuses", () => {
  it("says so in a toast and keeps it listed, ended", async () => {
    const user = userEvent.setup();
    await setup();
    processEnds("t1", null, "Killed: 9");
    vi.spyOn(terminalHost(), "respawnTab").mockImplementation(() => {
      throw new Error(
        "Restart needs the updated terminal server — restart it from Restart terminal server…",
      );
    });
    await user.click(badge());
    await user.click(screen.getByText("Restart “t1”"));
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("Couldn't restart “t1”", {
        description:
          "Restart needs the updated terminal server — restart it from Restart terminal server…",
      }),
    );
    expect(hiddenIn()).toEqual(["t1"]);
    expect(traces("t1").exit?.outcome).toBe("crashed");
  });
});

describe("a Hidden tab's Restart while one is in flight", () => {
  it("asks the host once and says the first is still going", async () => {
    await setup();
    processEnds("t1", null, "Killed: 9");
    const respawn = vi.spyOn(terminalHost(), "respawnTab");
    act(() => restartHiddenTab(TEAM, WS, "t1"));
    expect(toast.info).not.toHaveBeenCalled();
    act(() => restartHiddenTab(TEAM, WS, "t1"));
    expect(toast.info).toHaveBeenCalledTimes(1);
    expect(toast.info).toHaveBeenCalledWith("Restart already in progress for “t1”");
    await waitFor(() => expect(tabsIn()).toContain("t1"));
    expect(respawn).toHaveBeenCalledTimes(1);
  });
});

describe("a Hidden tab CodeSense ends", () => {
  it("End from the tray removes it and every trace of its session, with no ended mark", async () => {
    const user = userEvent.setup();
    await setup();
    leaveTraces("t1");

    await user.click(badge());
    await user.click(screen.getByText("End “t1”"));
    await waitFor(() => expect(hiddenIn()).toEqual([]));
    expect(terminalHost().contains("t1")).toBe(false);
    expect(traces("t1")).toEqual(NO_TRACES);

    // The exit that kill causes records nothing.
    emit("pty-exit", { session_id: "t1", exit_code: null, signal: "Killed: 9", requested: true });
    expect(traces("t1")).toEqual(NO_TRACES);
    expect(screen.queryByTestId(`tray-chip-hidden-${WS}`)).toBeNull();
  });
});

describe("a root workspace's Hidden tabs", () => {
  const root: Workspace = {
    id: WS,
    kind: "root",
    cwd: "/repo",
    status: "ready",
    sourceRepoSlug: "a/b",
  };
  const pill = () => screen.getByRole("button", { name: /^Workspace a\/b/ });

  it("declines an empty native root menu and settles it when its last item disappears", async () => {
    await setup(root);
    const onClose = vi.fn();
    const request = {
      teamKey: TEAM,
      contentRoot: "/repo",
      anchorRect: { x: 20, y: 30, width: 0, height: 0 },
      owner: {},
      onClose,
      row: { kind: "workspace", workspaceId: WS },
    };
    let dismiss: (() => void) | null = null;
    act(() => {
      dismiss = openNativeMenu(request);
    });
    expect(dismiss).not.toBeNull();
    expect(screen.getByRole("menu", { name: "Workspace actions" })).toBeInTheDocument();
    await act(async () => {
      await useViewerStore.getState().adoptTerminalSession(TEAM, "t1", WS);
    });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("menu")).toBeNull();
    expect(openNativeMenu(request)).toBeNull();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("a right-click on the pill opens a menu that restores them, with no Hide or End", async () => {
    const user = userEvent.setup();
    await setup(root);
    fireEvent.contextMenu(pill(), { clientX: 10, clientY: 20 });
    const menu = screen.getByRole("menu", { name: "Workspace actions" });
    expect(within(menu).queryByText("Hide")).toBeNull();
    expect(within(menu).queryByText("End")).toBeNull();
    await user.click(within(menu).getByText("Restore “t1”"));
    await waitFor(() => expect(hiddenIn()).toEqual([]));
  });

  it("its menu closes once its last Hidden tab comes back from anywhere else", async () => {
    await setup(root);
    fireEvent.contextMenu(pill(), { clientX: 10, clientY: 20 });
    expect(screen.getByRole("menu", { name: "Workspace actions" })).toBeInTheDocument();
    await act(async () => {
      await useViewerStore.getState().adoptTerminalSession(TEAM, "t1", WS);
    });
    await waitFor(() => expect(hiddenIn()).toEqual([]));
    expect(screen.queryByRole("menu", { name: "Workspace actions" })).toBeNull();
  });

  it("Shift+F10 on the pill opens the same menu from the keyboard", async () => {
    await setup(root);
    fireEvent.keyDown(pill(), { key: "F10", shiftKey: true });
    const menu = screen.getByRole("menu", { name: "Workspace actions" });
    expect(within(menu).getByText("Restore “t1”")).toBeInTheDocument();
  });
});
