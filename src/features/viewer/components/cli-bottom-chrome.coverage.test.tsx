// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { screen, act, fireEvent } from "@testing-library/react";
import { renderWithProviders as render } from "../../../../tests/support/helpers/render-with-providers";

import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore, defaultForegroundLayout } from "@/shared/stores/viewer-store";
import { useUiStore } from "@/shared/stores/ui-store";
import type { Org, CurrentUser } from "@/shared/types/domain";

// SW-EW-35: the CLI controls (layout / full-screen / epic-repos) relocated from
// the retired bottom StatusBar onto the CLI's own bottom chrome (the tray
// strip). These tests migrate the StatusBar terminal-zone coverage onto the new
// `CliBottomChrome`, adjusted for the shared-mode-only gate (D5).

vi.mock(import("@/shared/lib/team-key"), async (importOriginal) => {
  const actual = await importOriginal();
  return actual;
});

vi.mock("@/tauri/commands", () => ({
  loadAppState: vi.fn(() => Promise.resolve("{}")),
  saveAppState: vi.fn(() => Promise.resolve()),
}));

let currentUserValue: CurrentUser | null = null;
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

import { CliBottomChrome } from "./cli-bottom-chrome";

const TEST_USER: CurrentUser = {
  github_user_id: 42,
  github_username: "alice",
  display_name: "Alice",
  avatar_url: null,
};

const realSetForegroundPreset = useViewerStore.getState().setForegroundPreset;
const realToggleConsoleShared = useViewerStore.getState().toggleConsoleShared;

function makeOrg(slug: string, displayName: string): Org {
  return {
    slug,
    display_name: displayName,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {} as Record<string, never>,
    people: [],
    teams: [],
    org_root: "",
  };
}

function resetStores(options: { orgs?: Org[]; activeOrgSlug?: string | null } = {}) {
  currentUserValue = TEST_USER;
  useOrgStore.setState({
    orgs: options.orgs ?? [],
    activeOrgSlug: options.activeOrgSlug ?? null,
    activeTeamName: options.activeOrgSlug ? "default" : null,
    stateLoaded: true,
    hasUserIntent: false,
  });
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    terminalVisible: false,
    rightPaneOpen: false,
    layoutMode: null,
    lastTerminalMode: "shared",
    foregroundLayout: {},
    epicReposOpen: {},
    openedTeams: options.activeOrgSlug ? [`${options.activeOrgSlug}:default`] : [],
    setForegroundPreset: realSetForegroundPreset,
    toggleConsoleShared: realToggleConsoleShared,
  });
  useUiStore.setState({ settingsPanelOpen: false });
}

// A split (N≥2) terminal that is foreground in SHARED mode under "solo:default".
function seedSplitTerminal(teamKey = "solo:default") {
  useViewerStore.setState({
    openedTeams: [teamKey],
    workspaces: {
      [teamKey]: [
        { id: "ws-a", kind: "story", cwd: "/a", status: "ready" },
        { id: "ws-b", kind: "story", cwd: "/b", status: "ready" },
      ],
    },
    activeWorkspace: { [teamKey]: "ws-a" },
    terminalVisible: true,
    layoutMode: "shared",
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  resetStores();
});

afterEach(() => {
  resetStores();
});

describe("CliBottomChrome — with two open teams it is the active team's", () => {
  const active = "solo:default";
  const other = "solo:other";

  it("another team's focused workspace shows nothing; after a switch its picker arranges that team", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    const setForegroundPreset = vi.fn();
    act(() => {
      seedSplitTerminal(other);
      useViewerStore.setState({
        openedTeams: [active, other],
        terminalVisible: true,
        layoutMode: "shared",
        setForegroundPreset,
      });
    });
    render(<CliBottomChrome />);
    expect(screen.queryByTestId("cli-bottom-chrome")).not.toBeInTheDocument();
    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    fireEvent.click(screen.getByTestId("cli-chrome-layout"));
    fireEvent.click(screen.getByTestId("cli-chrome-layout-rows"));
    expect(setForegroundPreset).toHaveBeenCalledWith(other, "rows");
  });

  it("offers the epic-repos toggle for the active team's focused epic terminal, and toggles it", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      useViewerStore.setState({
        openedTeams: [active, other],
        workspaces: {
          [active]: [
            { id: "epic-a", kind: "epic", cwd: "/e", status: "ready", epicDirPath: "/epic" },
          ],
          [other]: [],
        },
        activeWorkspace: { [active]: "epic-a" },
        terminalVisible: true,
        layoutMode: "shared",
      });
    });
    render(<CliBottomChrome />);
    fireEvent.click(screen.getByTestId("cli-chrome-epic-repos-toggle"));
    expect(useViewerStore.getState().epicReposOpen["solo:default|epic-a"]).toBe(true);
  });

  it("follows the one column setting: hidden, it shows nothing", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      seedSplitTerminal(active);
      useViewerStore.setState({
        openedTeams: [active, other],
        terminalVisible: false,
        layoutMode: "shared",
      });
    });
    render(<CliBottomChrome />);
    expect(screen.queryByTestId("cli-bottom-chrome")).not.toBeInTheDocument();
  });
});

describe("CliBottomChrome — controls", () => {
  const teamKey = "solo:default";

  it("shows the layout control + console toggle when foreground and split (N≥2)", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => seedSplitTerminal(teamKey));
    render(<CliBottomChrome />);
    expect(screen.getByTestId("cli-chrome-layout")).toBeInTheDocument();
    expect(screen.getByTestId("cli-chrome-console-toggle")).toBeInTheDocument();
  });

  it("renders nothing when the terminal is not foreground (terminal hidden)", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      seedSplitTerminal(teamKey);
      useViewerStore.setState({ terminalVisible: false });
    });
    render(<CliBottomChrome />);
    expect(screen.queryByTestId("cli-bottom-chrome")).not.toBeInTheDocument();
  });

  it("hides the layout control at N=1 (not split) but still shows the console toggle", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      seedSplitTerminal(teamKey);
      useViewerStore.setState({
        workspaces: { [teamKey]: [{ id: "ws-a", kind: "story", cwd: "/a", status: "ready" }] },
      });
    });
    render(<CliBottomChrome />);
    expect(screen.queryByTestId("cli-chrome-layout")).not.toBeInTheDocument();
    expect(screen.getByTestId("cli-chrome-console-toggle")).toBeInTheDocument();
  });

  it("reflects the active preset in the layout trigger's accessible name", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      seedSplitTerminal(teamKey);
      useViewerStore.setState({
        foregroundLayout: { [teamKey]: defaultForegroundLayout({ preset: "rows" }) },
      });
    });
    render(<CliBottomChrome />);
    expect(screen.getByTestId("cli-chrome-layout")).toHaveAttribute(
      "aria-label",
      "Terminal layout: Rows",
    );
  });

  it("selecting a preset calls setForegroundPreset", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    const setForegroundPreset = vi.fn();
    act(() => {
      seedSplitTerminal(teamKey);
      useViewerStore.setState({ setForegroundPreset });
    });
    render(<CliBottomChrome />);
    fireEvent.click(screen.getByTestId("cli-chrome-layout"));
    fireEvent.click(screen.getByTestId("cli-chrome-layout-rows"));
    expect(setForegroundPreset).toHaveBeenCalledWith(teamKey, "rows");
  });

  it("clicking the console toggle calls toggleConsoleShared", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    const toggleConsoleShared = vi.fn();
    act(() => {
      seedSplitTerminal(teamKey);
      useViewerStore.setState({ toggleConsoleShared });
    });
    render(<CliBottomChrome />);
    fireEvent.click(screen.getByTestId("cli-chrome-console-toggle"));
    expect(toggleConsoleShared).toHaveBeenCalledWith();
  });

  it("shows the epic-repos toggle only for an epic-integration workspace", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      useViewerStore.setState({
        workspaces: {
          [teamKey]: [
            { id: "epic-i", kind: "epic", cwd: "/e", status: "ready", epicDirPath: "/epic" },
          ],
        },
        activeWorkspace: { [teamKey]: "epic-i" },
        terminalVisible: true,
        layoutMode: "shared",
      });
    });
    render(<CliBottomChrome />);
    expect(screen.getByTestId("cli-chrome-epic-repos-toggle")).toBeInTheDocument();
  });
});

describe("CliBottomChrome — gating + console-mode controls", () => {
  const teamKey = "solo:default";

  it("renders nothing when terminalVisible is true but layoutMode is 'viewer'", () => {
    // Edge: a direct setTerminalVisible(true) can leave terminalVisible true
    // while layoutMode is still "viewer" — the chrome must not show over the
    // viewer.
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      seedSplitTerminal(teamKey);
      useViewerStore.setState({ layoutMode: "viewer" });
    });
    render(<CliBottomChrome />);
    expect(screen.queryByTestId("cli-bottom-chrome")).not.toBeInTheDocument();
  });

  it("keeps the layout picker + full-screen toggle visible in console mode (split, N≥2)", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      seedSplitTerminal(teamKey);
      useViewerStore.setState({ layoutMode: "console" });
    });
    render(<CliBottomChrome />);
    expect(screen.getByTestId("cli-chrome-layout")).toBeInTheDocument();
    expect(screen.getByTestId("cli-chrome-console-toggle")).toBeInTheDocument();
  });

  it("flips the full-screen toggle to 'Back to bottom pane' in console mode", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      seedSplitTerminal(teamKey);
      useViewerStore.setState({ layoutMode: "console" });
    });
    render(<CliBottomChrome />);
    const toggle = screen.getByTestId("cli-chrome-console-toggle");
    expect(toggle).toHaveAttribute("aria-label", "Back to bottom pane");
    // Full-screen is just another state, not a pressed toggle (the bottom-pane
    // state isn't highlighted either) — no aria-pressed.
    expect(toggle).not.toHaveAttribute("aria-pressed");
  });

  it("shows 'Full screen' on the toggle in shared mode", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => seedSplitTerminal(teamKey));
    render(<CliBottomChrome />);
    const toggle = screen.getByTestId("cli-chrome-console-toggle");
    expect(toggle).toHaveAttribute("aria-label", "Full screen");
    expect(toggle).not.toHaveAttribute("aria-pressed");
  });

  it("keeps the epic-repos toggle reachable in console mode", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      useViewerStore.setState({
        workspaces: {
          [teamKey]: [
            { id: "epic-i", kind: "epic", cwd: "/e", status: "ready", epicDirPath: "/epic" },
          ],
        },
        activeWorkspace: { [teamKey]: "epic-i" },
        terminalVisible: true,
        layoutMode: "console",
      });
    });
    render(<CliBottomChrome />);
    expect(screen.getByTestId("cli-chrome-epic-repos-toggle")).toBeInTheDocument();
    // Full-screen toggle is always present when foreground; the layout picker is
    // hidden here because a lone epic-integration workspace is not a split (N=1).
    expect(screen.getByTestId("cli-chrome-console-toggle")).toBeInTheDocument();
    expect(screen.queryByTestId("cli-chrome-layout")).not.toBeInTheDocument();
  });

  it("in console keeps the full-screen toggle with no workspace focused in the CLI's team; in shared it shows nothing", () => {
    // Switching to a team whose CLI is empty keeps full-screen: the toggle is the way out.
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      useViewerStore.setState({
        workspaces: { [teamKey]: [] },
        activeWorkspace: {},
        terminalVisible: true,
        layoutMode: "console",
      });
    });
    const { unmount } = render(<CliBottomChrome />);
    expect(screen.getByTestId("cli-bottom-chrome")).toBeInTheDocument();
    expect(screen.getByTestId("cli-chrome-console-toggle")).toHaveAttribute(
      "aria-label",
      "Back to bottom pane",
    );
    unmount();

    act(() => useViewerStore.setState({ layoutMode: "shared" }));
    render(<CliBottomChrome />);
    expect(screen.queryByTestId("cli-bottom-chrome")).not.toBeInTheDocument();
  });

  it("renders on the Settings tab when the terminal is visible (the terminal stays on screen)", () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    act(() => {
      seedSplitTerminal(teamKey);
      useUiStore.setState({ settingsPanelOpen: true });
    });
    render(<CliBottomChrome />);
    expect(screen.getByTestId("cli-bottom-chrome")).toBeInTheDocument();
  });
});
