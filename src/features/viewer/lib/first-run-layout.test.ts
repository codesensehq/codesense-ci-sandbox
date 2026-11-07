// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// SW-LN-B37 / SW-LN-B38: the first-run layout seams — the terminal pane stays
// collapsed until there is a terminal, launching an agent never changes the
// user's layout mode, and nav openers always bring their surface into view.

const TEAM = "org:team";

vi.mock("@/shared/lib/team-key", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/team-key")>();
  return {
    ...actual,
    getActiveTeamKey: () => TEAM,
    teamKeyHasTeam: () => true,
  };
});

import { useViewerStore, type LayoutMode, type Workspace } from "@/shared/stores/viewer-store";
import { makeRealDashboardLaunchDeps } from "@/features/dashboard/components/dashboard-launch-controller";
import { openSettingsTab, SETTINGS_VIRTUAL_PATH } from "@/features/settings/lib/open-settings-tab";
import { openWorkspaces, WORKSPACES_VIRTUAL_PATH } from "@/features/org/lib/open-workspaces";
import { openRoutines } from "@/features/routines/routines-virtual-tab";
import { AGENTS_VIRTUAL_PATH } from "@/features/agents/agents-virtual-tab";
import {
  openSourceRepos,
  SOURCE_REPOS_VIRTUAL_PATH,
} from "@/features/dashboard/source-repos-virtual-tab";

function resetLayout(layoutMode?: LayoutMode) {
  useViewerStore.setState({
    layoutMode: layoutMode ?? null,
    terminalVisible: layoutMode ? layoutMode !== "viewer" : false,
    lastTerminalMode: "shared",
    workspaces: {},
    activeTab: {},
    openTabs: {},
  });
}

/** Is the main (viewer) pane — where Settings/Workspaces/Source repos render — on screen? */
function mainPaneVisible(): boolean {
  return (useViewerStore.getState().layoutMode ?? "shared") !== "console";
}

function launchAgent() {
  makeRealDashboardLaunchDeps("/ctx/dash.md").focus(TEAM, "session-1", "/ctx/epic/S-1-story.md");
}

describe("SW-LN-B37 — no terminal pane before there is a terminal", () => {
  beforeEach(() => resetLayout());

  it("a first-run team with no workspace starts with the terminal pane collapsed", () => {
    useViewerStore.getState().settleInitialLayout(TEAM);
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("viewer");
    expect(s.terminalVisible).toBe(false);
  });

  it("opening the first workspace expands the pane", () => {
    useViewerStore.getState().settleInitialLayout(TEAM);
    useViewerStore.getState().expandTerminalOnOpen();
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("shared");
    expect(s.terminalVisible).toBe(true);
  });

  it("a team whose pane already has a foreground workspace starts with it shown", () => {
    const ws: Workspace = { id: "S-1", kind: "story", cwd: "/wt/S-1", status: "ready" };
    useViewerStore.setState({ workspaces: { [TEAM]: [ws] } });
    useViewerStore.getState().settleInitialLayout(TEAM);
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("shared");
    expect(s.terminalVisible).toBe(true);
  });

  it("a persisted user-chosen layout is kept as-is", () => {
    for (const mode of ["shared", "console", "viewer"] as const) {
      resetLayout(mode);
      useViewerStore.getState().settleInitialLayout(TEAM);
      expect(useViewerStore.getState().layoutMode).toBe(mode);
      expect(useViewerStore.getState().terminalVisible).toBe(mode !== "viewer");
    }
  });

  it("a second team's first settle keeps the layout another team already set", () => {
    const ws: Workspace = { id: "S-1", kind: "story", cwd: "/wt/S-1", status: "ready" };
    useViewerStore.setState({ workspaces: { [TEAM]: [ws] } });
    useViewerStore.getState().settleInitialLayout(TEAM);
    // The other team has no workspace, but the CLI chrome is one setting (D39):
    // its first settle does not collapse the pane the first team opened.
    useViewerStore.getState().settleInitialLayout("org:other");
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("shared");
    expect(s.terminalVisible).toBe(true);
  });
});

describe("SW-LN-B38 — launching an agent keeps the user's layout", () => {
  it.each(["shared", "console"] as const)("keeps %s layout on launch", (mode) => {
    resetLayout(mode);
    launchAgent();
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe(mode);
    expect(s.terminalVisible).toBe(true);
  });

  it("a collapsed pane expands to the bottom pane (never console) on launch", () => {
    resetLayout("viewer");
    useViewerStore.setState({ lastTerminalMode: "console" });
    launchAgent();
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("shared");
    expect(s.terminalVisible).toBe(true);
    // The user's saved terminal mode is not overwritten by the launch.
    expect(s.lastTerminalMode).toBe("console");
  });

  it("a hidden terminal in an unset/shared layout is shown on launch", () => {
    resetLayout();
    launchAgent();
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("shared");
    expect(s.terminalVisible).toBe(true);
  });

  describe("focus", () => {
    const original = useViewerStore.getState().setActiveTerminalTab;
    afterEach(() => useViewerStore.setState({ setActiveTerminalTab: original }));

    it("the launched session's tab becomes the active terminal tab", () => {
      resetLayout("shared");
      const setActiveTerminalTab = vi.fn();
      useViewerStore.setState({ setActiveTerminalTab });
      launchAgent();
      expect(setActiveTerminalTab).toHaveBeenCalledWith(TEAM, "session-1", "S-1-story");
    });
  });
});

describe("SW-LN-B38 — nav clicks after a launch bring their surface into view", () => {
  const surfaces = [
    ["Settings", () => openSettingsTab("general"), SETTINGS_VIRTUAL_PATH],
    ["Workspaces", () => openWorkspaces("home"), WORKSPACES_VIRTUAL_PATH],
    ["Source repos", () => openSourceRepos("list"), SOURCE_REPOS_VIRTUAL_PATH],
  ] as const;

  for (const startMode of ["shared", "console", "viewer"] as const) {
    for (const [name, open, path] of surfaces) {
      it(`${name} is visible after launching an agent (started in ${startMode})`, () => {
        resetLayout(startMode);
        launchAgent();
        open();
        const s = useViewerStore.getState();
        expect(s.activeTab[TEAM]).toBe(path);
        expect(mainPaneVisible()).toBe(true);
      });
    }
  }

  it("an open the user did not ask for keeps full-screen CLI through the real openers", () => {
    for (const [open, path] of [
      [() => openWorkspaces("join", { keepLayout: true }), WORKSPACES_VIRTUAL_PATH],
      [() => openRoutines(null, { keepLayout: true }), AGENTS_VIRTUAL_PATH],
    ] as const) {
      resetLayout("console");
      open();
      const s = useViewerStore.getState();
      expect(s.activeTab[TEAM]).toBe(path);
      expect(s.layoutMode).toBe("console");
    }
  });

  it("opening a file from console layout reveals the viewer without hiding the terminal", () => {
    resetLayout("console");
    useViewerStore.getState().openFile(TEAM, "", "/ctx/epic/S-1-story.md");
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("shared");
    expect(s.terminalVisible).toBe(true);
  });

  it("preview-browsing never changes the layout", () => {
    resetLayout("console");
    useViewerStore.getState().openFilePreview(TEAM, "", "/ctx/epic/S-1-story.md");
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });

  it("opening a file from console keeps the user's saved terminal mode", () => {
    resetLayout("console");
    useViewerStore.setState({ lastTerminalMode: "console" });
    useViewerStore.getState().openFile(TEAM, "", "/ctx/epic/S-1-story.md");
    expect(useViewerStore.getState().lastTerminalMode).toBe("console");
  });

  it("opening a file leaves shared and viewer layouts alone", () => {
    for (const mode of ["shared", "viewer"] as const) {
      resetLayout(mode);
      useViewerStore.getState().openFile(TEAM, "", "/ctx/epic/S-1-story.md");
      expect(useViewerStore.getState().layoutMode).toBe(mode);
    }
  });
});
