// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

// Full-screen CLI (`layoutMode === "console"`) is never left on the user's behalf:
// closing a workspace — the last of an epic, of the team, or of the team the CLI
// shows — keeps it, and the CLI shows its empty state. Only a user action leaves full
// screen. The rail-scoped zoom-clear still follows the close (SW-FX-94).

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { selectExpandedWorkspaceId, useViewerStore } from "./viewer-store";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

const TEAM = "acme:default";
const EPIC_A = "epic-a";
const EPIC_B = "epic-b";
const WS_A = "epic-a-story-1";
const WS_A2 = "epic-a-story-2";
const WS_B = "epic-b-story-1";
const WS_B2 = "epic-b-story-2";

beforeEach(() => {
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    layoutMode: null,
    lastTerminalMode: "shared",
    trayMode: {},
    selectedEpicDirName: {},
    railKeyByWorkspaceId: {},
    trayFullOrderByTeam: {},
    foregroundLayout: {},
    foregroundLayoutByRail: {},
    softClosedWorkspaces: {},
    cliExpanded: false,
  });
  vi.clearAllMocks();
});

describe("a close keeps full-screen CLI", () => {
  it("AC-1/3/5: grouped, two epics — closing the last tile of the selected epic keeps console, follows focus, and keeps expanded", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          { id: WS_A, kind: "story", cwd: "/a", status: "ready" },
          { id: WS_B, kind: "story", cwd: "/b", status: "ready" },
        ],
      },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "console",
      lastTerminalMode: "console",
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: EPIC_A },
      railKeyByWorkspaceId: { [TEAM]: { [WS_A]: EPIC_A, [WS_B]: EPIC_B } },
      trayFullOrderByTeam: { [TEAM]: [WS_A, WS_B] },
      // Expanded is the global chrome's: a close never changes it.
      cliExpanded: true,
    });

    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    const s = useViewerStore.getState();

    // AC-1: console preserved.
    expect(s.layoutMode).toBe("console");
    // AC-5: lastTerminalMode likewise preserved.
    expect(s.lastTerminalMode).toBe("console");
    // Follow-focus target (SW-EW-38): active moves to the surviving epic-B tile.
    expect(s.activeWorkspace[TEAM]).toBe(WS_B);
    // Expanded stays set; it zooms the focused workspace once its slot has tiles to zoom over.
    expect(s.cliExpanded).toBe(true);
  });

  it("AC-1/3: grouped — a rail left with one tile shows no zoom while console stays", () => {
    // After closing the expanded WS_A, the SELECTED rail A keeps 1 tile (WS_A2) —
    // below SPLIT_MIN_TILES, so nothing is zoomed there — while the TEAM keeps 3 tiles.
    // A regression that counted the team's tiles (3 ≥ 2) would still zoom.
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          { id: WS_A, kind: "story", cwd: "/a", status: "ready" },
          { id: WS_A2, kind: "story", cwd: "/a2", status: "ready" },
          { id: WS_B, kind: "story", cwd: "/b", status: "ready" },
          { id: WS_B2, kind: "story", cwd: "/b2", status: "ready" },
        ],
      },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "console",
      lastTerminalMode: "console",
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: EPIC_A },
      railKeyByWorkspaceId: {
        [TEAM]: { [WS_A]: EPIC_A, [WS_A2]: EPIC_A, [WS_B]: EPIC_B, [WS_B2]: EPIC_B },
      },
      trayFullOrderByTeam: { [TEAM]: [WS_A, WS_A2, WS_B, WS_B2] },
      cliExpanded: true,
    });

    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    const s = useViewerStore.getState();

    // AC-1: console preserved.
    expect(s.layoutMode).toBe("console");
    // AC-3: rail A dropped to 1 tile (< SPLIT_MIN_TILES) → nothing is zoomed, even though
    // the team's count (3) is ≥ 2; expanded itself stays set.
    expect(selectExpandedWorkspaceId(s, TEAM)).toBeNull();
    expect(s.cliExpanded).toBe(true);
  });

  it("AC-5: preserving lastTerminalMode across the drain re-enters console on a later viewer→terminal toggle", () => {
    // End-to-end for AC-5's actual promise: the reason we keep lastTerminalMode is
    // so a later viewer→terminal toggle (`expandTerminalOnOpen`, which reads it)
    // returns to full-screen. Pre-fix the close clobbered lastTerminalMode to
    // "shared", so this re-entry would land in "shared" instead.
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          { id: WS_A, kind: "story", cwd: "/a", status: "ready" },
          { id: WS_B, kind: "story", cwd: "/b", status: "ready" },
        ],
      },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "console",
      lastTerminalMode: "console",
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: EPIC_A },
      railKeyByWorkspaceId: { [TEAM]: { [WS_A]: EPIC_A, [WS_B]: EPIC_B } },
      trayFullOrderByTeam: { [TEAM]: [WS_A, WS_B] },
    });

    // Close the last tile of the selected epic — console + lastTerminalMode preserved.
    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    expect(useViewerStore.getState().layoutMode).toBe("console");

    // User drops to the viewer (does NOT touch lastTerminalMode), then re-enters
    // the terminal — expandTerminalOnOpen restores from lastTerminalMode.
    useViewerStore.getState().setLayoutMode("viewer");
    expect(useViewerStore.getState().layoutMode).toBe("viewer");
    useViewerStore.getState().expandTerminalOnOpen();

    // AC-5 promise: re-enters CONSOLE, not shared.
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });

  it("grouped, single epic — closing the last remaining foreground tile keeps console on the empty CLI", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WS_A, kind: "story", cwd: "/a", status: "ready" }] },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "console",
      lastTerminalMode: "console",
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: EPIC_A },
      railKeyByWorkspaceId: { [TEAM]: { [WS_A]: EPIC_A } },
      trayFullOrderByTeam: { [TEAM]: [WS_A] },
    });

    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    const s = useViewerStore.getState();

    expect(s.workspaces[TEAM] ?? []).toHaveLength(0);
    expect(s.layoutMode).toBe("console");
    expect(s.lastTerminalMode).toBe("console");
  });

  it("with only a minimized agent left, closing the last visible tile keeps console on the empty CLI", () => {
    const AGENT = "test-agent";
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WS_A, kind: "story", cwd: "/a", status: "ready" }] },
      softClosedWorkspaces: {
        [TEAM]: {
          [AGENT]: {
            workspace: { id: AGENT, kind: "generic", cwd: "/ctx", status: "ready" },
          },
        },
      },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "console",
      lastTerminalMode: "console",
      trayMode: { [TEAM]: "flat" },
      trayFullOrderByTeam: { [TEAM]: [WS_A] },
    });

    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    const s = useViewerStore.getState();

    expect(s.workspaces[TEAM] ?? []).toHaveLength(0);
    expect(s.layoutMode).toBe("console");
    expect(s.lastTerminalMode).toBe("console");
  });

  it("flat tray — closing the last foreground tile keeps console on the empty CLI", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WS_A, kind: "story", cwd: "/a", status: "ready" }] },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "console",
      lastTerminalMode: "console",
      trayMode: { [TEAM]: "flat" },
      trayFullOrderByTeam: { [TEAM]: [WS_A] },
    });

    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    const s = useViewerStore.getState();

    expect(s.workspaces[TEAM] ?? []).toHaveLength(0);
    expect(s.layoutMode).toBe("console");
    expect(s.lastTerminalMode).toBe("console");
  });

  it("AC-4b: flat tray — closing a non-last tile keeps console", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          { id: WS_A, kind: "story", cwd: "/a", status: "ready" },
          { id: WS_B, kind: "story", cwd: "/b", status: "ready" },
        ],
      },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "console",
      lastTerminalMode: "console",
      trayMode: { [TEAM]: "flat" },
      trayFullOrderByTeam: { [TEAM]: [WS_A, WS_B] },
    });

    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    const s = useViewerStore.getState();

    expect(s.layoutMode).toBe("console");
    expect(s.lastTerminalMode).toBe("console");
    expect(s.activeWorkspace[TEAM]).toBe(WS_B);
  });

  it("closing the last tile in 'viewer' mode leaves layoutMode untouched", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WS_A, kind: "story", cwd: "/a", status: "ready" }] },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "viewer",
      lastTerminalMode: "console",
      trayMode: { [TEAM]: "flat" },
      trayFullOrderByTeam: { [TEAM]: [WS_A] },
    });

    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    const s = useViewerStore.getState();

    expect(s.layoutMode).toBe("viewer");
    expect(s.lastTerminalMode).toBe("console");
  });
});

// D68: minimized records contain workspace memory only. Reopen uses the
// current global chrome, including an explicit layout change while minimized.
describe("SW-FX-99 — layout mode survives the soft-close→reopen round-trip", () => {
  it("a workspace minimized from console respects the current shared layout", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WS_A, kind: "story", cwd: "/a", status: "ready" }] },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "console",
      lastTerminalMode: "console",
      trayMode: { [TEAM]: "flat" },
      trayFullOrderByTeam: { [TEAM]: [WS_A] },
    });
    // A live tab is required for a soft-close to stash a shadow (keepRunning).
    seedTerminalTabs(TEAM, WS_A, ["t1"]);

    // Minimize the last tile: console stays, and the shadow stores only the workspace.
    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    expect(useViewerStore.getState().layoutMode).toBe("console");
    expect(Object.keys(useViewerStore.getState().softClosedWorkspaces[TEAM][WS_A])).toEqual([
      "workspace",
    ]);
    // The user leaves full screen meanwhile; reopening respects that choice.
    useViewerStore.getState().toggleConsoleShared();
    expect(useViewerStore.getState().layoutMode).toBe("shared");

    // Reopen uses the current global mode.
    useViewerStore.getState().reopenSoftClosedWorkspace(TEAM, WS_A);
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("shared");
    expect(s.lastTerminalMode).toBe("shared");
  });

  it("AC-2: a workspace soft-closed from shared reopens in shared (no spurious console)", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WS_A, kind: "story", cwd: "/a", status: "ready" }] },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "shared",
      lastTerminalMode: "shared",
      trayMode: { [TEAM]: "flat" },
      trayFullOrderByTeam: { [TEAM]: [WS_A] },
    });
    seedTerminalTabs(TEAM, WS_A, ["t1"]);

    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    useViewerStore.getState().reopenSoftClosedWorkspace(TEAM, WS_A);
    expect(useViewerStore.getState().layoutMode).toBe("shared");
  });

  it("a workspace soft-closed from shared, reopened in console, leaves full screen on", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WS_A, kind: "story", cwd: "/a", status: "ready" }] },
      activeWorkspace: { [TEAM]: WS_A },
      layoutMode: "shared",
      lastTerminalMode: "shared",
      trayMode: { [TEAM]: "flat" },
      trayFullOrderByTeam: { [TEAM]: [WS_A] },
    });
    seedTerminalTabs(TEAM, WS_A, ["t1"]);

    useViewerStore.getState().closeWorkspace(TEAM, WS_A);
    useViewerStore.getState().toggleConsoleShared();
    useViewerStore.getState().reopenSoftClosedWorkspace(TEAM, WS_A);
    expect(useViewerStore.getState()).toMatchObject({
      layoutMode: "console",
      lastTerminalMode: "console",
      terminalVisible: true,
    });
  });

  it("AC-3: a legacy shadow with no captured mode reopens without crashing and leaves layout unchanged", () => {
    // Pins the `!== undefined` skip branch: a shadow stashed before this fix (or by
    // an alternate path) has no layoutMode/lastTerminalMode — reopen must not crash
    // and must not silently change the team's current mode.
    useViewerStore.setState({
      workspaces: { [TEAM]: [] },
      activeWorkspace: {},
      layoutMode: "shared",
      lastTerminalMode: "shared",
      trayFullOrderByTeam: { [TEAM]: [] },
      softClosedWorkspaces: {
        [TEAM]: {
          [WS_A]: {
            workspace: { id: WS_A, kind: "story", cwd: "/a", status: "ready" },
            // no layoutMode / lastTerminalMode (legacy shape)
          },
        },
      },
    });
    seedTerminalTabs(TEAM, WS_A, ["t1"]);

    expect(() => useViewerStore.getState().reopenSoftClosedWorkspace(TEAM, WS_A)).not.toThrow();
    expect(useViewerStore.getState().layoutMode).toBe("shared");
  });
});
