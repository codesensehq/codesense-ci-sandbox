// SPDX-License-Identifier: MIT
import agentListHtml from "@/features/shell/slots/__fixtures__/agent-list-pane.html?raw";
import { StickyNote } from "lucide-react";
import { usePanelRegistry } from "@/features/shell/right-pane/panel-registry";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { rebuildMenu, toastWarning, commandsSync } = vi.hoisted(() => ({
  rebuildMenu: vi.fn(),
  toastWarning: vi.fn(),
  commandsSync: vi.fn(() => Promise.resolve()),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  rebuildMenu: (...args: unknown[]) => {
    rebuildMenu(...args);
    return Promise.resolve();
  },
  loadAppState: () => Promise.resolve("{}"),
  saveAppState: () => Promise.resolve(),
  agentListIdentity: () => Promise.resolve({}),
  pushViewerState: () => Promise.resolve(),
  commandsSync: (...args: unknown[]) => commandsSync(...(args as [])),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { warning: toastWarning }) }));

import { useDashboardProfileStore } from "./dashboard-profile";
import {
  runDashboardCommand,
  setActiveDashboardCommands,
  syncDashboardCommands,
} from "./dashboard-commands";
import {
  markFrameLive,
  markFrameReady,
  registerFrame,
  COMMAND_QUEUE_BACKSTOP_MS,
} from "./frame-lifecycle";
import { dashboardRefKey } from "./dashboard-ref";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import {
  defaultRightPaneLayout,
  INBOX_PANEL_ID,
} from "@/features/shell/right-pane/right-pane-layout";
import { matchShortcut } from "@/shared/hooks/use-shortcut-handler";
import {
  allShortcutActions,
  findShortcutAction,
  resolveBindings,
  setDynamicShortcutActions,
} from "@/shared/lib/shortcuts/registry";
import { buildNavMenuEntries } from "@/features/shell/right-pane/panel-menu";
import { findBindingCollisions, useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { DashboardCommand, DashboardRef, PaneEntry } from "@/tauri/commands";

import { watchAgentListInputs } from "@/features/viewer/lib/agent-list-inputs";
import { liveHub } from "./live-hub";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useWeftJumpStore } from "@/features/weft/stores/weft-jump-store";

const inboxRef: DashboardRef = { scope: "team", org: "acme", team: "dev", id: "inbox" };
const boardRef: DashboardRef = { scope: "team", org: "acme", team: "dev", id: "board" };
const INBOX = dashboardRefKey(inboxRef);
const BOARD = dashboardRefKey(boardRef);
const id = (dashboard: string, command: string) => `${dashboard}/${command}`;

function pane(ref: DashboardRef, title: string, commands: DashboardCommand[]): PaneEntry {
  return {
    id: ref.id,
    title,
    scope: "team",
    team: "dev",
    path: `/ctx/panes/${ref.id}.html`,
    panesDir: "/ctx/panes",
    panelId: `pane:acme:dev/${ref.id}`,
    dashboardRef: ref,
    declaration: { slots: ["right-pane"], capabilities: [], commands },
    status: "ready",
  };
}

const ARCHIVE: DashboardCommand = {
  id: "archive",
  label: "Archive thread",
  icon: "archive",
  defaultChord: "CmdOrCtrl+Shift+Y",
  scope: "global",
};

function setSource(entry: PaneEntry) {
  const id = dashboardRefKey(entry.dashboardRef!);
  usePaneStore.setState((s) => ({
    sourceRows: {
      ...s.sourceRows,
      [id]: {
        id,
        dashboardRef: entry.dashboardRef!,
        entry,
        contentRoot: "/ctx",
        sourceRevision: (s.sourceRows[id]?.sourceRevision ?? 0) + 1,
        listed: false,
      },
    },
  }));
  syncDashboardCommands();
}

function install(panes: PaneEntry[], slotted = panes.map((p) => p.dashboardRef!)) {
  usePanelRegistry.setState({
    panels: Object.fromEntries(
      panes.map((p) => [
        p.panelId,
        { id: p.panelId, title: p.title, icon: StickyNote, render: () => null },
      ]),
    ),
  });
  usePaneStore.setState({
    teamKey: "acme:dev",
    listing: { contentRoot: "/ctx", chain: ["dev"], panes },
    knownEntries: {},
  });
  useDashboardProfileStore.setState({
    org: "acme",
    profile: {
      id: "default",
      name: "Default",
      posture: "",
      slots: { "right-pane": slotted.length ? slotted : "native" },
      cliLayout: "default",
      chrome: {},
    },
  });
  syncDashboardCommands();
}

const gate = {
  settingsPanelOpen: false,
  hasActiveWorkspace: true,
  hasTeam: true,
  rowPopupFocused: false,
};
function key(k: string, init: KeyboardEventInit = {}, target?: Element) {
  const e = new KeyboardEvent("keydown", { key: k, code: `Key${k.toUpperCase()}`, ...init });
  if (target) Object.defineProperty(e, "target", { value: target });
  return matchShortcut(e, useShortcutsStore.getState().shortcuts, gate)?.id;
}

let frames: (() => void)[] = [];
function mountFrame(dashboard: string, frameId = "f1") {
  const post = vi.fn();
  frames.push(
    registerFrame({
      frameId,
      dashboard,
      panelId: dashboard === INBOX ? INBOX_PANEL_ID : undefined,
      slotId: "right-pane",
      post: (message) => {
        if (message.type === "codesense:command") post(message);
      },
      hide: () => {},
    }),
  );
  return post;
}

beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "dev" });
  useViewerStore.setState({ rightPaneOpen: false, rightPaneLayout: defaultRightPaneLayout() });
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: true });
  rebuildMenu.mockClear();
  toastWarning.mockClear();
});
afterEach(() => {
  usePaneStore.setState({ sourceRows: {} });
  frames.forEach((dispose) => dispose());
  frames = [];
  install([]);
  setDynamicShortcutActions([]);
  vi.useRealTimers();
  vi.restoreAllMocks();
  document.body.innerHTML = "";
});

describe("declaration and registry", () => {
  it("lands a slot dashboard's commands in the one registry and matches their chords", () => {
    install([pane(inboxRef, "Inbox", [ARCHIVE])]);
    const action = allShortcutActions().find((a) => a.id === id(INBOX, "archive"));
    expect(action?.command).toMatchObject({ dashboardTitle: "Inbox", active: true });
    expect(key("y", { metaKey: true, shiftKey: true })).toBe(id(INBOX, "archive"));
  });

  it("re-registering a subset deactivates the rest", () => {
    const reply = { ...ARCHIVE, id: "reply", label: "Reply", defaultChord: "CmdOrCtrl+Shift+X" };
    install([pane(inboxRef, "Inbox", [ARCHIVE, reply])]);
    setActiveDashboardCommands(INBOX, ["reply"]);
    expect(key("y", { metaKey: true, shiftKey: true })).toBeUndefined();
    expect(key("x", { metaKey: true, shiftKey: true })).toBe(id(INBOX, "reply"));
    const active = allShortcutActions().filter((a) => a.command?.active);
    expect(active.map((a) => a.id)).toEqual([id(INBOX, "reply")]);
  });

  it("registering no ids deactivates every command", () => {
    install([pane(inboxRef, "Inbox", [ARCHIVE])]);
    setActiveDashboardCommands(INBOX, []);
    expect(allShortcutActions().some((a) => a.command?.active)).toBe(false);
    expect(key("y", { metaKey: true, shiftKey: true })).toBeUndefined();
  });

  it.each([
    ["CmdOrCtrl+Q", "Quit"],
    ["CmdOrCtrl+W", "CodeSense uses ⌘W for Close Tab"],
    ["CmdOrCtrl+H", "macOS uses ⌘H for Hide"],
    ["CmdOrCtrl+M", "Minimize"],
    ["CmdOrCtrl+Tab", "Switch Apps"],
    ["CmdOrCtrl+Space", "Spotlight"],
    ["CmdOrCtrl+A", "Select All"],
    ["CmdOrCtrl+C", "Copy"],
    ["CmdOrCtrl+V", "Paste"],
    ["CmdOrCtrl+X", "Cut"],
    ["CmdOrCtrl+Z", "Undo"],
  ])("refuses the reserved chord %s as a default and as a rebind", (chord, why) => {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
    install([pane(inboxRef, "Inbox", [{ ...ARCHIVE, defaultChord: chord }])]);
    const binding = resolveBindings({}).get(id(INBOX, "archive"));
    expect(binding?.keystroke).toBe("");
    expect(binding?.unbound).toMatchObject({ kind: "reserved", wanted: chord });
    expect(binding?.unbound?.kind === "reserved" && binding.unbound.message).toContain(why);
    const result = useShortcutsStore.getState().setShortcut(id(INBOX, "archive"), chord);
    expect(result).toMatchObject({ ok: false, reason: "reserved" });
  });

  it("with a terminal focused only a ⌘ chord fires; other chords reach the PTY", () => {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
    const chords: [string, string][] = [
      ["ctrl", "CmdOrCtrl+Y"],
      ["alt", "Alt+J"],
      ["fkey", "F7"],
      ["esc", "Escape"],
    ];
    install([
      pane(
        inboxRef,
        "Inbox",
        chords.map(([cid, chord]) => ({ ...ARCHIVE, id: cid, defaultChord: chord })),
      ),
    ]);
    const xterm = document.createElement("div");
    xterm.className = "xterm";
    const textarea = document.createElement("textarea");
    xterm.append(textarea);
    document.body.append(xterm);
    textarea.focus();
    expect(key("y", { ctrlKey: true }, textarea)).toBeUndefined();
    expect(key("j", { altKey: true }, textarea)).toBeUndefined();
    expect(key("F7", {}, textarea)).toBeUndefined();
    expect(key("Escape", {}, textarea)).toBeUndefined();
    expect(key("y", { metaKey: true }, textarea)).toBe(id(INBOX, "ctrl"));
    textarea.blur();
    expect(key("F7")).toBe(id(INBOX, "fkey"));
    expect(key("j", { altKey: true })).toBe(id(INBOX, "alt"));
  });
});

describe("conflicts and take-over", () => {
  it("⌘K is an ordinary conflict: unbound by default, taken in Settings with the native accelerator dropped", () => {
    install([pane(inboxRef, "Inbox", [{ ...ARCHIVE, defaultChord: "CmdOrCtrl+K" }])]);
    const archive = id(INBOX, "archive");
    expect(resolveBindings({}).get(archive)?.unbound).toEqual({
      kind: "conflict",
      wanted: "CmdOrCtrl+K",
      winner: "nav.open-workspace",
    });
    expect(key("k", { metaKey: true })).toBe("nav.open-workspace");

    const taken = useShortcutsStore.getState().takeShortcut(archive, "CmdOrCtrl+K");
    expect(taken).toMatchObject({ ok: true, displaced: "nav.open-workspace" });
    expect(useShortcutsStore.getState().shortcuts).toEqual({
      [archive]: "CmdOrCtrl+K",
      "nav.open-workspace": "",
    });
    expect(key("k", { metaKey: true })).toBe(archive);
    expect(rebuildMenu).toHaveBeenLastCalledWith(
      expect.objectContaining({ "nav.open-workspace": "" }),
      expect.anything(),
      expect.anything(),
    );

    // Reset gives the chord back: the built-in wins and the command is a conflict again.
    useShortcutsStore.getState().resetShortcut("nav.open-workspace");
    expect(useShortcutsStore.getState().shortcuts).toEqual({});
    expect(resolveBindings({}).get(archive)?.unbound?.kind).toBe("conflict");
  });

  it("⌘P is file quick-open's: a dashboard's ⌘P is a conflict it can take", () => {
    install([pane(inboxRef, "Inbox", [{ ...ARCHIVE, defaultChord: "CmdOrCtrl+P" }])]);
    const archive = id(INBOX, "archive");
    expect(resolveBindings({}).get(archive)?.unbound).toMatchObject({ winner: "nav.quick-open" });
    expect(key("p", { metaKey: true })).toBe("nav.quick-open");
    useShortcutsStore.getState().takeShortcut(archive, "CmdOrCtrl+P");
    expect(key("p", { metaKey: true })).toBe(archive);
  });

  it("the dashboard assigned first keeps a shared default chord; the newcomer conflicts", () => {
    const zuluRef: DashboardRef = { ...boardRef, id: "zulu" };
    const alphaRef: DashboardRef = { ...boardRef, id: "alpha" };
    const zulu = pane(zuluRef, "Zulu", [ARCHIVE]);
    const alpha = pane(alphaRef, "Alpha", [ARCHIVE]);
    // Discovery lists by id (Alpha first); the profile lists Zulu, then the newly added Alpha.
    install([alpha, zulu], [zuluRef, alphaRef]);
    const bindings = resolveBindings({});
    const ZULU = dashboardRefKey(zuluRef);
    expect(bindings.get(id(ZULU, "archive"))?.keystroke).toBe("CmdOrCtrl+Shift+Y");
    expect(bindings.get(id(dashboardRefKey(alphaRef), "archive"))?.unbound).toEqual({
      kind: "conflict",
      wanted: "CmdOrCtrl+Shift+Y",
      winner: id(ZULU, "archive"),
    });
    expect(key("y", { metaKey: true, shiftKey: true })).toBe(id(ZULU, "archive"));
  });

  it("a cleared built-in leaves the native Go menu without its accelerator", () => {
    useShortcutsStore.getState().clearShortcut("nav.context-back");
    expect(buildNavMenuEntries().find((e) => e.id === "nav.context-back")?.accelerator).toBe("");
  });

  it("a collision with a built-in and one between dashboards both register unbound with the right winner", () => {
    install([
      pane(inboxRef, "Inbox", [{ ...ARCHIVE, defaultChord: "CmdOrCtrl+B" }]),
      pane(boardRef, "Board", [
        { ...ARCHIVE, id: "move", label: "Move card", defaultChord: "CmdOrCtrl+Shift+Y" },
      ]),
    ]);
    const bindings = resolveBindings({});
    expect(bindings.get(id(INBOX, "archive"))?.unbound).toMatchObject({
      kind: "conflict",
      winner: "viewer.toggle-nav",
    });
    // The board's default matches no built-in, so it is the first to claim ⌘⇧Y.
    expect(bindings.get(id(BOARD, "move"))?.keystroke).toBe("CmdOrCtrl+Shift+Y");

    install([
      pane(inboxRef, "Inbox", [ARCHIVE]),
      pane(boardRef, "Board", [
        { ...ARCHIVE, id: "move", label: "Move card", defaultChord: "CmdOrCtrl+Shift+Y" },
      ]),
    ]);
    const next = resolveBindings({});
    expect(next.get(id(INBOX, "archive"))?.keystroke).toBe("CmdOrCtrl+Shift+Y");
    expect(next.get(id(BOARD, "move"))?.unbound).toMatchObject({
      kind: "conflict",
      winner: id(INBOX, "archive"),
    });
    expect(findBindingCollisions({})).toEqual([]);
  });
});

describe("agent-list commands", () => {
  installTerminalLayoutHarness();
  const ref: DashboardRef = { ...boardRef, id: "agents" };
  const dashboard = dashboardRefKey(ref);
  function installAgents() {
    const doc = new DOMParser().parseFromString(agentListHtml, "text/html");
    const commands = JSON.parse(
      doc.querySelector('meta[name="codesense:commands"]')?.getAttribute("content") ?? "[]",
    ) as DashboardCommand[];
    const dock = pane(ref, "Agents", commands);
    dock.declaration!.slots = ["dock"];
    dock.declaration!.capabilities =
      doc
        .querySelector('meta[name="codesense:capabilities"]')
        ?.getAttribute("content")
        ?.split(/\s+/) ?? [];
    install([dock], []);
    useDashboardProfileStore.setState((s) => ({
      profile: { ...s.profile!, slots: { dock: ref } },
    }));
    setSource(dock);
    return dock;
  }

  it("registers the fixture's commands with one free default chord and open agent access", () => {
    const dock = installAgents();
    const ids = dock.declaration!.commands?.map((c) => c.id) ?? [];
    expect(ids[0]).toBe("focus-list");
    expect(ids).toEqual(
      expect.arrayContaining([
        "focus-list",
        "reveal-current",
        "collapse-all",
        "expand-all",
        "next-needs-you",
        "previous-needs-you",
        "open-row-menu",
        "focus-search",
        "switch-arrangement",
      ]),
    );
    expect(new Set(ids).size).toBe(ids.length);
    const bindings = resolveBindings({});
    expect(bindings.get(id(dashboard, "focus-list"))?.keystroke).toBe("CmdOrCtrl+Alt+A");
    for (const command of dock.declaration!.commands!.slice(1)) {
      expect(bindings.get(id(dashboard, command.id))?.unbound).toEqual({ kind: "none" });
      expect(command.humanOnly).not.toBe(true);
    }
    expect(dock.declaration!.capabilities).toEqual(
      expect.arrayContaining([
        "sessions:read",
        "inbox:read:team",
        "inbox:read:all-teams",
        "commands:run",
      ]),
    );
    expect(key("a")).toBeUndefined();
    const terminal = document.createElement("textarea");
    expect(key("a", {}, terminal)).toBeUndefined();
    expect(key("a", { metaKey: true, altKey: true }, terminal)).toBe(id(dashboard, "focus-list"));
  });

  it("keeps store shortcuts and the real ui:dock selection mirror working with the dashboard registered", async () => {
    installAgents();
    const team = "acme:dev";
    const workspaces = ["ws1", "ws2", "ws3"].map((id) => ({
      id,
      kind: "story" as const,
      status: "ready" as const,
      cwd: "/" + id,
    }));
    useViewerStore.setState({
      stateLoaded: true,
      workspaces: { [team]: workspaces },
      activeWorkspace: { [team]: "ws1" },
      softClosedWorkspaces: {},
      openedTeams: [team],
      trayOrderByTeam: { [team]: ["ws1", "ws2", "ws3"] },
      trayFullOrderByTeam: { [team]: ["ws1", "ws2", "ws3"] },
      trayMode: { [team]: "flat" },
      mruWorkspaceStack: { [team]: ["ws3", "ws1"] },
      agentsPaneArrange: "teams",
      agentsPaneFolds: {},
      agentPins: {},
      aiStatusBySession: { s3: "needs-permission" },
    });
    for (let n = 1; n <= 3; n++) seedTerminalTabs(team, "ws" + n, ["s" + n], { active: "s" + n });
    useAgentStateStore.setState({ stateSince: { s3: 1 }, seenAt: {} });
    useWeftJumpStore.getState().clear();
    const write = vi.spyOn(liveHub, "writeUi").mockResolvedValue({
      topic: "ui:dock",
      viewKey: "all",
      rev: 1,
      snapshot: [],
    });
    vi.spyOn(liveHub, "snapshot").mockResolvedValue({
      topic: "ui:dock",
      viewKey: "all",
      rev: 1,
      snapshot: [{ id: "ui:dock", arrange: "teams", pins: {}, folds: {} }],
    });
    vi.spyOn(liveHub, "watch").mockReturnValue(() => {});
    const stop = watchAgentListInputs();
    try {
      for (const [command, expected] of [
        ["viewer.cycle-workspace-next", "s2"],
        ["viewer.cycle-workspace-prev", "s3"],
        ["viewer.jump-workspace-2", "s2"],
        ["viewer.flip-last-workspace", "s3"],
        ["weft.jump-to-next-needs-you", "s3"],
        ["viewer.close-workspace", "s2"],
      ]) {
        useViewerStore.setState({
          activeWorkspace: { [team]: "ws1" },
          mruWorkspaceStack: { [team]: ["ws3", "ws1"] },
        });
        write.mockClear();
        findShortcutAction(command)!.run();
        await vi.waitFor(() =>
          expect(write, command).toHaveBeenCalledWith("ui:dock", "all", { selected: expected }),
        );
      }
    } finally {
      stop();
      useViewerStore.setState({ stateLoaded: false });
    }
  });

  it("focuses an already visible dock when its global chord is pressed from the terminal", () => {
    installAgents();
    useViewerStore.setState({
      layoutMode: "shared",
      terminalVisible: true,
      workspacePanelCollapsed: false,
    });
    const element = document.createElement("iframe");
    const terminal = document.createElement("textarea");
    document.body.append(element, terminal);
    const post = vi.fn();
    frames.push(
      registerFrame({
        frameId: "visible-agents",
        panelId: "slot:dock",
        dashboard,
        slotId: "dock",
        element,
        post,
        hide: () => {},
      }),
    );
    markFrameReady("visible-agents");
    markFrameLive("visible-agents");
    terminal.focus();
    expect(document.activeElement).toBe(terminal);
    const action = key("a", { metaKey: true, altKey: true }, terminal);
    expect(action).toBe(id(dashboard, "focus-list"));
    findShortcutAction(action!)!.run();
    expect(document.activeElement).toBe(element);
    expect(post).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ type: "codesense:command", id: "focus-list" }),
    );
  });

  it.each(["hidden", "folded", "unmounted"])(
    "reveals and focuses a %s dock, then delivers a rebound key",
    (state) => {
      installAgents();
      useViewerStore.setState({
        layoutMode: "shared",
        terminalVisible: state === "folded",
        workspacePanelCollapsed: state !== "hidden",
      });
      const element = document.createElement("iframe");
      document.body.append(element);
      const post = vi.fn();
      const mount = () => {
        frames.push(
          registerFrame({
            frameId: "agents-frame",
            panelId: "slot:dock",
            dashboard,
            slotId: "dock",
            element,
            post,
            hide: () => {},
          }),
        );
        markFrameReady("agents-frame");
        markFrameLive("agents-frame");
      };
      if (state !== "unmounted") mount();
      useShortcutsStore
        .getState()
        .setShortcut(id(dashboard, "focus-list"), "CmdOrCtrl+Alt+Shift+A");
      const action = key("a", { metaKey: true, altKey: true, shiftKey: true });
      expect(action).toBe(id(dashboard, "focus-list"));
      findShortcutAction(action!)!.run();
      if (state === "unmounted") mount();
      expect(useViewerStore.getState().workspacePanelCollapsed).toBe(false);
      expect(useViewerStore.getState().terminalVisible).toBe(true);
      expect(document.activeElement).toBe(element);
      expect(post).toHaveBeenCalledExactlyOnceWith(
        expect.objectContaining({ type: "codesense:command", id: "focus-list" }),
      );
    },
  );
});

describe("lifecycle", () => {
  it("refreshes edited dock commands and retains them when a sibling listing omits the page", () => {
    install([]);
    const original = pane(boardRef, "Agents", [
      ARCHIVE,
      { ...ARCHIVE, id: "removed", label: "Old command" },
    ]);
    original.declaration!.slots = ["dock"];
    useDashboardProfileStore.setState((s) => ({
      profile: { ...s.profile!, slots: { dock: boardRef } },
    }));
    setSource(original);
    const commands = () =>
      allShortcutActions()
        .filter((action) => action.command?.dashboard === BOARD)
        .map(({ id, label, defaultKeystroke }) => ({ id, label, defaultKeystroke }));
    expect(commands().map((command) => command.id)).toContain(id(BOARD, "removed"));

    const edited = pane(boardRef, "Agents", [
      { ...ARCHIVE, label: "Archive agent", defaultChord: "CmdOrCtrl+Alt+Shift+A" },
      { ...ARCHIVE, id: "inspect", label: "Inspect agent", defaultChord: "CmdOrCtrl+Alt+Shift+I" },
    ]);
    edited.declaration!.slots = ["dock"];
    usePaneStore.getState().setListing("acme:dev", {
      contentRoot: "/ctx",
      chain: ["dev"],
      panes: [edited],
    });
    setSource(edited);
    const expected = [
      {
        id: id(BOARD, "archive"),
        label: "Archive agent",
        defaultKeystroke: "CmdOrCtrl+Alt+Shift+A",
      },
      {
        id: id(BOARD, "inspect"),
        label: "Inspect agent",
        defaultKeystroke: "CmdOrCtrl+Alt+Shift+I",
      },
    ];
    expect(commands()).toEqual(expected);

    useOrgStore.setState({ activeTeamName: "ops" });
    usePaneStore.getState().setListing("acme:ops", {
      contentRoot: "/ctx/ops",
      chain: ["ops"],
      panes: [],
    });
    syncDashboardCommands();
    expect(commands()).toEqual(expected);
  });

  it("keeps the resolved dock commands on a sibling team and reveals a live hidden dock", () => {
    install([]);
    const dock = pane(boardRef, "Agents", [ARCHIVE]);
    dock.declaration!.slots = ["dock"];
    useDashboardProfileStore.setState((s) => ({
      profile: { ...s.profile!, slots: { dock: boardRef } },
    }));
    setSource(dock);
    useOrgStore.setState({ activeTeamName: "ops" });
    usePaneStore.getState().setListing("acme:ops", null);
    syncDashboardCommands();
    const post = vi.fn();
    frames.push(
      registerFrame({
        frameId: "dock-frame",
        panelId: "slot:dock",
        dashboard: BOARD,
        slotId: "dock",
        post,
        hide: () => {},
      }),
    );
    markFrameLive("dock-frame");
    const revealTerminalInPlace = vi.fn(() => useViewerStore.setState({ terminalVisible: true }));
    useViewerStore.setState({
      layoutMode: "shared",
      terminalVisible: false,
      workspacePanelCollapsed: true,
      revealTerminalInPlace,
    });
    runDashboardCommand(id(BOARD, "archive"));
    expect(revealTerminalInPlace).toHaveBeenCalled();
    expect(useViewerStore.getState().workspacePanelCollapsed).toBe(false);
    expect(post).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({
        type: "codesense:command",
        id: "archive",
      }),
    );
  });

  it("a rebind survives uninstall and reinstall, and the commands leave with the dashboard", () => {
    const inbox = pane(inboxRef, "Inbox", [ARCHIVE]);
    install([inbox]);
    useShortcutsStore.getState().setShortcut(id(INBOX, "archive"), "CmdOrCtrl+Alt+A");
    install([inbox], []);
    expect(allShortcutActions().some((a) => a.command)).toBe(true);
    install([], []);
    expect(allShortcutActions().some((a) => a.command)).toBe(false);
    expect(useShortcutsStore.getState().shortcuts[id(INBOX, "archive")]).toBe("CmdOrCtrl+Alt+A");
    install([inbox]);
    expect(key("a", { metaKey: true, altKey: true })).toBe(id(INBOX, "archive"));
  });
});

describe("scopes", () => {
  it("uses file-nav visibility independently of the dock and reveals a collapsed sidebar for global commands", () => {
    const ref: DashboardRef = { scope: "team", org: "acme", team: "dev", id: "files" };
    const dashboard = dashboardRefKey(ref);
    const entry = pane(ref, "Files", [
      { ...ARCHIVE, id: "visible", scope: "slot-visible" },
      { ...ARCHIVE, id: "global", scope: "global" },
    ]);
    entry.declaration!.slots = ["file-nav"];
    install([], []);
    useDashboardProfileStore.setState((s) => ({
      profile: {
        ...s.profile!,
        slots: { "file-nav": ref },
      },
    }));
    setSource(entry);
    const post = vi.fn();
    frames.push(
      registerFrame({
        frameId: "file-nav-frame",
        panelId: "slot:file-nav",
        dashboard,
        slotId: "file-nav",
        post,
        hide: () => {},
      }),
    );
    markFrameLive("file-nav-frame");
    const visible = findShortcutAction(id(dashboard, "visible"))!;
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
      workspacePanelCollapsed: true,
      navCollapsed: { "acme:dev": false },
    });
    expect(visible.available?.()).toBe(true);
    useViewerStore.setState({ layoutMode: "console", terminalVisible: true });
    expect(visible.available?.()).toBe(false);
    useViewerStore.setState({
      layoutMode: "shared",
      terminalVisible: true,
      workspacePanelCollapsed: false,
      navCollapsed: { "acme:dev": true },
    });
    expect(visible.available?.()).toBe(false);
    runDashboardCommand(id(dashboard, "global"));
    expect(useViewerStore.getState().navCollapsed["acme:dev"]).toBe(false);
    expect(post).toHaveBeenCalledWith(
      expect.objectContaining({ type: "codesense:command", id: "global" }),
    );
  });

  it("global fires with the slot closed, slot-visible only while shown, slot-focused only with frame focus", () => {
    install([
      pane(inboxRef, "Inbox", [
        { ...ARCHIVE, id: "g", defaultChord: "CmdOrCtrl+Alt+Shift+G", scope: "global" },
        { ...ARCHIVE, id: "v", defaultChord: "CmdOrCtrl+Alt+Shift+V", scope: "slot-visible" },
        { ...ARCHIVE, id: "f", defaultChord: "CmdOrCtrl+Alt+Shift+F", scope: "slot-focused" },
      ]),
    ]);
    const chord = (k: string) => key(k, { metaKey: true, altKey: true, shiftKey: true });
    expect(chord("g")).toBe(id(INBOX, "g"));
    expect(chord("v")).toBeUndefined();
    expect(chord("f")).toBeUndefined();

    useViewerStore.getState().revealPanelById(INBOX_PANEL_ID);
    expect(chord("v")).toBe(id(INBOX, "v"));

    // Any focusable element stands in for the frame: the gate compares identity.
    const iframe = document.createElement("button") as unknown as HTMLIFrameElement;
    document.body.append(iframe);
    frames.push(
      registerFrame({
        frameId: "focused",
        dashboard: INBOX,
        slotId: "right-pane",
        element: iframe,
        post: () => {},
        hide: () => {},
      }),
    );
    iframe.focus();
    expect(chord("f")).toBe(id(INBOX, "f"));
  });
});

describe("dispatch", () => {
  it("queues for the stable Inbox instead of a live independent copy of the same dashboard", () => {
    install([pane(inboxRef, "Inbox", [ARCHIVE])]);
    useViewerStore.getState().revealPanelById("pane:acme:dev/inbox");
    const ordinary = vi.fn();
    frames.push(
      registerFrame({
        frameId: "ordinary",
        dashboard: INBOX,
        panelId: "pane:acme:dev/inbox",
        slotId: "right-pane",
        post: ordinary,
        hide: () => {},
      }),
    );
    markFrameLive("ordinary");
    allShortcutActions()
      .find((action) => action.id === id(INBOX, "archive"))!
      .run();
    expect(
      ordinary.mock.calls.filter(([message]) => message.type === "codesense:command"),
    ).toHaveLength(0);
    const hosted = mountFrame(INBOX);
    markFrameLive("f1");
    expect(hosted).toHaveBeenCalledWith(
      expect.objectContaining({ type: "codesense:command", id: "archive" }),
    );
    expect(
      useViewerStore
        .getState()
        .rightPaneLayout.sections.some((section) => section.group.activePanelId === INBOX_PANEL_ID),
    ).toBe(true);
  });

  it("reveals a closed slot and delivers exactly once after the page's ready()", () => {
    install([pane(inboxRef, "Inbox", [ARCHIVE])]);
    const action = allShortcutActions().find((a) => a.id === id(INBOX, "archive"))!;
    action.run();
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    const post = mountFrame(INBOX);
    expect(post).not.toHaveBeenCalled();
    markFrameLive("f1");
    markFrameLive("f1");
    expect(post).toHaveBeenCalledTimes(1);
    expect(post).toHaveBeenCalledWith({
      type: "codesense:command",
      id: "archive",
      args: undefined,
      dispatchId: expect.any(String),
    });
    action.run();
    expect(post).toHaveBeenCalledTimes(2);
    expect(post.mock.calls[0][0].dispatchId).not.toBe(post.mock.calls[1][0].dispatchId);
  });

  it("revealing a hidden frame whose page is already ready delivers at once", () => {
    install([pane(inboxRef, "Inbox", [ARCHIVE])]);
    const post = mountFrame(INBOX);
    markFrameLive("f1");
    expect(useViewerStore.getState().rightPaneOpen).toBe(false);
    allShortcutActions()
      .find((a) => a.id === id(INBOX, "archive"))!
      .run();
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    expect(post).toHaveBeenCalledTimes(1);
    expect(post).toHaveBeenCalledWith(expect.objectContaining({ type: "codesense:command" }));
  });

  it("drops queued commands with one toast when the page never becomes ready", () => {
    vi.useFakeTimers();
    install([pane(inboxRef, "Inbox", [ARCHIVE, { ...ARCHIVE, id: "next", label: "Next" }])]);
    const run = (cid: string) =>
      allShortcutActions()
        .find((a) => a.id === id(INBOX, cid))!
        .run();
    run("archive");
    vi.advanceTimersByTime(COMMAND_QUEUE_BACKSTOP_MS);
    expect(toastWarning).toHaveBeenCalledWith(
      "Inbox pane didn't load — Archive thread didn't run.",
    );
    run("archive");
    run("next");
    vi.advanceTimersByTime(COMMAND_QUEUE_BACKSTOP_MS);
    expect(toastWarning).toHaveBeenLastCalledWith(
      "Inbox pane didn't load — 2 commands didn't run.",
    );
    expect(toastWarning).toHaveBeenCalledTimes(2);
    const post = mountFrame(INBOX);
    markFrameLive("f1");
    expect(post).not.toHaveBeenCalled();
  });
});

describe("the host registry (agents and page verbs)", () => {
  it("pushes the slot commands with their effective chords, and again after a rebind", () => {
    commandsSync.mockClear();
    install([pane(inboxRef, "Inbox", [{ ...ARCHIVE, humanOnly: true }])]);
    expect(commandsSync).toHaveBeenLastCalledWith([
      {
        id: id(INBOX, "archive"),
        dashboard: INBOX,
        dashboardTitle: "Inbox",
        dashboardScope: "team",
        slot: "right-pane",
        capabilities: [],
        command: "archive",
        label: "Archive thread",
        scope: "global",
        binding: "CmdOrCtrl+Shift+Y",
        humanOnly: true,
      },
    ]);
    const calls = commandsSync.mock.calls.length;
    syncDashboardCommands();
    expect(commandsSync).toHaveBeenCalledTimes(calls);
    useShortcutsStore.getState().setShortcut(id(INBOX, "archive"), "CmdOrCtrl+Alt+A");
    syncDashboardCommands();
    expect((commandsSync.mock.lastCall as unknown as [{ binding: string }[]])[0][0].binding).toBe(
      "CmdOrCtrl+Alt+A",
    );
  });

  it("runs a command the host asks for through the chord's dispatch, with its args", () => {
    install([pane(inboxRef, "Inbox", [ARCHIVE])]);
    const post = mountFrame(INBOX);
    markFrameLive("f1");
    runDashboardCommand(id(INBOX, "archive"), { thread: 7 });
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    expect(post).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ type: "codesense:command", id: "archive", args: { thread: 7 } }),
    );
    runDashboardCommand(id(INBOX, "gone"));
    expect(post).toHaveBeenCalledTimes(1);
  });
});

describe("Notes built-in entry commands", () => {
  const notesRef: DashboardRef = { ...boardRef, id: "notes" };
  const NOTES = dashboardRefKey(notesRef);
  beforeEach(() => useUiStore.setState({ headingJumpOpen: false }));

  it("routes ⌘⇧O to the selected Notes dashboard, revealing it before delivery", () => {
    const notes = pane(notesRef, "Notes", [
      { id: "goto-heading", label: "Go to heading…", scope: "global" },
    ]);
    notes.surface = "notes";
    install([notes]);
    expect(key("o", { metaKey: true, shiftKey: true })).toBe("nav.goto-heading");
    findShortcutAction("nav.goto-heading")!.run();
    expect(useUiStore.getState().headingJumpOpen).toBe(false);
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    const post = mountFrame(NOTES);
    markFrameLive("f1");
    expect(post).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ type: "codesense:command", id: "goto-heading" }),
    );
  });

  it("keeps the native heading dialog when Notes is native", () => {
    install([]);
    findShortcutAction("nav.goto-heading")!.run();
    expect(useUiStore.getState().headingJumpOpen).toBe(true);
    expect(useViewerStore.getState().rightPaneOpen).toBe(false);
  });

  it("toggles the active right-pane panel full screen without a default chord", () => {
    install([pane(notesRef, "Notes", [])]);
    const viewer = useViewerStore.getState();
    viewer.revealPanelById("pane:acme:dev/notes");
    useViewerStore.setState({ maximizedPanel: {} });
    const action = findShortcutAction("viewer.toggle-pane-full-screen")!;
    expect(action).toMatchObject({
      label: "Toggle Pane Full Screen",
      menuPath: ["View", "Toggle Pane Full Screen"],
      scope: "team",
      defaultKeystroke: "",
    });
    action.run();
    expect(useViewerStore.getState().maximizedPanel["acme:dev"]).toBe("pane:acme:dev/notes");
    action.run();
    expect(useViewerStore.getState().maximizedPanel["acme:dev"]).toBeUndefined();
  });

  it("prefers the focused dashboard over the first expanded section", () => {
    install([pane(notesRef, "Notes", []), pane(boardRef, "Board", [])]);
    useViewerStore.setState({
      rightPaneOpen: true,
      maximizedPanel: {},
      rightPaneLayout: {
        sections: [
          {
            id: "notes",
            group: { panelIds: ["pane:acme:dev/notes"], activePanelId: "pane:acme:dev/notes" },
            collapsed: false,
            size: 0.5,
          },
          {
            id: "board",
            group: { panelIds: ["pane:acme:dev/board"], activePanelId: "pane:acme:dev/board" },
            collapsed: false,
            size: 0.5,
          },
        ],
      },
    });
    const iframe = document.createElement("iframe");
    document.body.append(iframe);
    frames.push(
      registerFrame({
        frameId: "board",
        panelId: "pane:acme:dev/board",
        dashboard: BOARD,
        slotId: "right-pane",
        element: iframe,
        post: () => {},
        hide: () => {},
      }),
    );
    markFrameLive("board");
    iframe.focus();
    findShortcutAction("viewer.toggle-pane-full-screen")!.run();
    expect(useViewerStore.getState().maximizedPanel["acme:dev"]).toBe("pane:acme:dev/board");
    iframe.blur();
    findShortcutAction("viewer.toggle-pane-full-screen")!.run();
    expect(useViewerStore.getState().maximizedPanel["acme:dev"]).toBeUndefined();
  });

  it.each([false, true])(
    "skips another team's hidden panel (whole section: %s)",
    (wholeSection) => {
      const notes = pane(notesRef, "Notes", []);
      install([notes]);
      const hidden = "pane:acme:other/absent";
      useViewerStore.setState({
        rightPaneOpen: true,
        maximizedPanel: { "acme:dev": hidden },
        rightPaneLayout: {
          sections: wholeSection
            ? [
                {
                  id: "hidden",
                  group: { panelIds: [hidden], activePanelId: hidden },
                  collapsed: false,
                  size: 0.5,
                },
                {
                  id: "notes",
                  group: { panelIds: [notes.panelId], activePanelId: notes.panelId },
                  collapsed: false,
                  size: 0.5,
                },
              ]
            : [
                {
                  id: "mixed",
                  group: { panelIds: [hidden, notes.panelId], activePanelId: hidden },
                  collapsed: false,
                  size: 1,
                },
              ],
        },
      });
      findShortcutAction("viewer.toggle-pane-full-screen")!.run();
      expect(useViewerStore.getState().maximizedPanel["acme:dev"]).toBe(notes.panelId);
      expect(useViewerStore.getState().rightPaneLayout.sections[0].group.activePanelId).toBe(
        hidden,
      );
    },
  );

  it("does nothing when the right pane is closed or every section is collapsed", () => {
    install([]);
    useViewerStore.setState({ maximizedPanel: {} });
    findShortcutAction("viewer.toggle-pane-full-screen")!.run();
    expect(useViewerStore.getState().maximizedPanel).toEqual({});
    useViewerStore.setState({ rightPaneOpen: true, rightPaneLayout: { sections: [] } });
    findShortcutAction("viewer.toggle-pane-full-screen")!.run();
    expect(useViewerStore.getState().maximizedPanel).toEqual({});
  });
});
