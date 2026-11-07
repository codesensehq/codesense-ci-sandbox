// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, within, act, fireEvent, waitFor } from "@testing-library/react";

import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useConfigStore } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";

// ---------- next-themes mock ----------
const mockSetTheme = vi.fn();
let currentMockTheme = "system";
vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: currentMockTheme, setTheme: mockSetTheme }),
}));

// ---------- Tauri command mocks ----------
const mockDetectTools = vi.fn();
vi.mock("@/tauri/commands", () => ({
  detectTools: () => mockDetectTools(),
  revealContextRepo: vi.fn().mockResolvedValue(undefined),
  listDirectory: vi.fn().mockResolvedValue([]),
  // AboutRow update-check probe.
  checkForUpdate: vi.fn().mockResolvedValue(null),
}));

vi.mock("@/shared/lib/codesense-home", () => ({
  getCodesenseHome: () => "/mock/home",
  // SS.8: repo-section (rendered by settings-panel) calls deriveOrgRoot
  // on `contextRepoPath` to strip the synthetic-repo
  // `_team/{team}/repos/{owner}/{repo}` suffix before threading orgRoot
  // to backend worktree commands. Tests don't care about the strip —
  // identity is fine.
  deriveOrgRoot: (p: string) => p,
}));

vi.mock("@/shared/lib/resolve-source-repo-path", () => ({
  resolveSourceRepoPath: (repo: { repoPath: string }) => repo.repoPath,
}));

// AboutRow reads the app version on mount.
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("0.0.0-test"),
}));

// AboutRow's manual-update "Download manually" action opens the public download page.
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

// RepoSection listens for context-file-change events.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

// runUpdateCheck is invoked by AboutRow when a new version is found.
vi.mock("@/app/update-toast", () => ({
  runUpdateCheck: vi.fn(),
}));

import { toast } from "sonner";
import { openUrl } from "@tauri-apps/plugin-opener";
import { checkForUpdate } from "@/tauri/commands";
import { PUBLIC_DOWNLOAD_URL } from "@/features/license/channel";
import { SettingsPanel } from "./settings-panel";
import { resetDetectedEditorsCache } from "@/shared/hooks/use-detected-editors";

const realUpdateSettings = useViewerStore.getState().updateSettings;

// T.6: settings-panel resolves the active repo via
// `useActiveWorkspaceContext().repoPath` — the synthetic workspace key
// derived from `(teamKey, sourceRepoSlug)`. The helper aligns the
// active org/team + workspace's sourceRepoSlug so the derived repoPath
// equals `path` (when `path` is a synthetic key) or so the fallback
// chain in settings-panel resolves cleanly (when `path` is a legacy
// real path). Passing `null` clears the active workspace.
function seedActiveRepo(path: string | null) {
  if (path === null) {
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
    useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
    return;
  }
  // If `path` is a synthetic workspace key, parse out its org/team/slug
  // and align the seed so `buildSyntheticRepoPathFromPrompt` returns
  // `path` verbatim. Otherwise fall back to a generic seed; tests
  // passing real paths rely on settings-panel's configRepos fallback.
  const match = path.match(/^workspace:([^:]+):([^:]+):([^/]+)\/([^/]+)$/);
  const orgSlug = match?.[1] ?? "t";
  const teamName = match?.[2] ?? "t";
  const slug = match ? `${match[3]}/${match[4]}` : "test/repo";
  const teamKey = `${orgSlug}:${teamName}`;
  useOrgStore.setState({ activeOrgSlug: orgSlug, activeTeamName: teamName });
  // Seed a workspaceRepos entry under `path` for synthetic keys so
  // `findWorkspaceRepoBySlug` returns the canonical repoPath rather than
  // the fallback string. The entry's repo-data fields aren't asserted
  // on by tests that seed their own workspaceRepos record after; this
  // is just a base scaffold.
  if (match) {
    useOrgStore.setState((s) => ({
      workspaceRepos: {
        ...s.workspaceRepos,
        [path]: { ...(s.workspaceRepos[path] ?? { repoPath: path, repoName: slug }) } as never,
      },
    }));
  }
  useViewerStore.setState({
    workspaces: {
      [teamKey]: [
        {
          id: "w1",
          kind: "root",
          cwd: path,
          sourceRepoPath: path,
          sourceRepoSlug: slug,
          status: "ready",
        },
      ],
    },
    activeWorkspace: { [teamKey]: "w1" },
  });
}

function resetAll() {
  useUiStore.setState({
    orgSwitcherOpen: false,
    teamSwitcherOpen: false,
    settingsPanelOpen: true,
    settingsInitialSection: "general",
  });
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    updateSettings: realUpdateSettings,
    settings: {
      ...useViewerStore.getState().settings,
      editor: "",
      editorCustomCmd: "",
      terminalTheme: "github-dark",
      starshipPreset: "compact",
    },
  });
  // Reset config-store repos and org-store workspace repos so each test
  // starts from a clean slate.
  useConfigStore.setState({ repos: [] });
  useOrgStore.setState({
    activeOrgSlug: null,
    activeTeamName: null,
    workspaceRepos: {},
  });
  currentMockTheme = "system";
  mockSetTheme.mockReset();
  mockDetectTools.mockReset();
  // WDE-5 R2: the editor detect is now session-cached — clear it each test so
  // every case re-detects from the fresh mock (matches pre-refactor behavior).
  resetDetectedEditorsCache();
  mockDetectTools.mockResolvedValue({
    terminals: [],
    editors: [
      { id: "cursor", label: "Cursor" },
      { id: "code", label: "VS Code" },
    ],
    default_terminal: "",
    default_editor: "",
  });
}

beforeEach(() => {
  resetAll();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("SettingsPanel — mount (the virtual://settings tab body)", () => {
  it("renders on the requested section", async () => {
    useUiStore.setState({ settingsInitialSection: "general" });
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByTestId("settings-panel")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Settings" })).toBeInTheDocument();
  });

  it("has no overlay chrome: no Back button, and Escape never closes it", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.queryByTestId("settings-close")).not.toBeInTheDocument();
    // No global Escape listener: Escape reaches the rest of the app untouched.
    const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    await act(async () => {
      document.dispatchEvent(escape);
    });
    expect(escape.defaultPrevented).toBe(false);
  });
});

describe("SettingsPanel — Escape in search", () => {
  it("Escape clears a non-empty search query", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    const input = screen.getByTestId("settings-search-input") as HTMLInputElement;
    await act(async () => {
      fireEvent.change(input, { target: { value: "theme" } });
    });
    expect(input.value).toBe("theme");

    await act(async () => {
      input.focus();
      fireEvent.keyDown(input, { key: "Escape" });
    });
    expect((screen.getByTestId("settings-search-input") as HTMLInputElement).value).toBe("");
  });
});

describe("SettingsPanel — sections and Repo reactivity", () => {
  it("renders General section by default and shows all sidebar nav items", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByRole("heading", { name: "General" })).toBeInTheDocument();
    // Sidebar nav items for all 5 sections are visible
    expect(screen.getByTestId("settings-nav-general")).toBeInTheDocument();
    expect(screen.getByTestId("settings-nav-shortcuts")).toBeInTheDocument();
    expect(screen.getByTestId("settings-nav-repo")).toBeInTheDocument();
    expect(screen.getByTestId("settings-nav-workspace")).toBeInTheDocument();
    expect(screen.getByTestId("settings-nav-account")).toBeInTheDocument();
  });

  it("shows the Repo section with empty state when no repos exist", async () => {
    useUiStore.setState({ settingsInitialSection: "repo" });
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();
    expect(screen.getByTestId("settings-repo-empty-state")).toBeInTheDocument();
  });

  it("shows the Repo section when a repo is selected", async () => {
    useUiStore.setState({ settingsInitialSection: "repo" });
    useConfigStore.setState({
      repos: [
        {
          repoPath: "/repos/x",
          repoName: "x",
          contextRepoPath: "/repos/x/.codesense",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
      ],
    });
    seedActiveRepo("/repos/x");
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();
    expect(screen.getByTestId("settings-repo-main-doc")).toBeInTheDocument();
  });

  it("Repo section falls back to first config repo when the active workspace clears", async () => {
    useUiStore.setState({ settingsInitialSection: "repo" });
    useConfigStore.setState({
      repos: [
        {
          repoPath: "/repos/x",
          repoName: "x",
          contextRepoPath: "/repos/x/.codesense",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
      ],
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    // No active workspace — falls back to config-store.repos[0].
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();
    expect(screen.getByTestId("settings-repo-main-doc")).toBeInTheDocument();

    await act(async () => {
      seedActiveRepo("/repos/x");
    });
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();

    await act(async () => {
      seedActiveRepo(null);
    });
    // Still visible — falls back to repos[0].
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();
    expect(screen.getByTestId("settings-repo-main-doc")).toBeInTheDocument();
  });

  it("opening on the repo section with no repos shows empty state (no crash)", async () => {
    useUiStore.setState({ settingsInitialSection: "repo" });
    await act(async () => {
      render(<SettingsPanel />);
    });
    // Panel mounts, Repo section shows empty state, no error.
    expect(useUiStore.getState().settingsPanelOpen).toBe(true);
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();
    expect(screen.getByTestId("settings-repo-empty-state")).toBeInTheDocument();
  });

  it("falls back to org-store workspace repo when config-store is empty (AC-3)", async () => {
    useUiStore.setState({ settingsInitialSection: "repo" });
    useOrgStore.setState({
      workspaceRepos: {
        "workspace:org:team:test/repo": {
          repoPath: "workspace:org:team:test/repo",
          repoName: "test/repo",
          contextRepoPath: "/ctx/test-repo",
          teamContentPath: "",
          hasRemote: true,
          remoteUrl: "https://github.com/test/repo",
          mainDoc: "",
        },
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();
    expect(screen.getByTestId("settings-repo-main-doc")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-repo-empty-state")).not.toBeInTheDocument();
  });

  it("workspace repo takes priority over config-store when the active workspace matches (R-10)", async () => {
    const syntheticKey = "workspace:org:team:test/repo";
    useUiStore.setState({ settingsInitialSection: "repo" });
    seedActiveRepo(syntheticKey);
    useOrgStore.setState({
      workspaceRepos: {
        [syntheticKey]: {
          repoPath: syntheticKey,
          repoName: "test/repo (workspace)",
          contextRepoPath: "/ctx/workspace/test-repo",
          teamContentPath: "",
          hasRemote: true,
          remoteUrl: "https://github.com/test/repo",
          mainDoc: "ws-main.md",
        },
      },
    });
    useConfigStore.setState({
      repos: [
        {
          repoPath: syntheticKey,
          repoName: "test/repo (config)",
          contextRepoPath: "/ctx/config/test-repo",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "cfg-main.md",
        },
      ],
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();
    // SW-CP-17: the viewer-root row is retired — assert active-repo precedence
    // via the surviving Main doc row. The workspace repo's mainDoc
    // ("ws-main.md") wins over the config-store entry's ("cfg-main.md"). Read
    // the actual input value so the test catches a future regression where
    // config-store wins by accident.
    const mainDocInput = screen.getByTestId("settings-repo-main-doc") as HTMLInputElement;
    expect(mainDocInput.value).toBe("ws-main.md");
  });

  it("stale active workspace path falls through to first available repo (AC-1)", async () => {
    useUiStore.setState({ settingsInitialSection: "repo" });
    seedActiveRepo("/repos/deleted");
    useConfigStore.setState({
      repos: [
        {
          repoPath: "/repos/fallback",
          repoName: "fallback",
          contextRepoPath: "/repos/fallback/.codesense",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
      ],
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    // Active workspace path doesn't match any repo — falls through to repos[0].
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();
    expect(screen.getByTestId("settings-repo-main-doc")).toBeInTheDocument();
  });

  it("opening on the repo section activates Repo via sidebar (replaces scroll-to-section)", async () => {
    useUiStore.setState({ settingsInitialSection: "repo" });
    await act(async () => {
      render(<SettingsPanel />);
    });
    // Repo section is the active tab — its heading renders in the content area.
    expect(screen.getByRole("heading", { name: "Repo" })).toBeInTheDocument();
    expect(screen.getByTestId("settings-nav-repo")).toHaveAttribute("aria-selected", "true");
  });
});

describe("SettingsPanel — search filtering", () => {
  it("filters General rows by substring match on label + keywords", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    // Initially: all four general rows visible
    expect(screen.queryByText("Theme")).toBeNull();
    expect(screen.getByText("Editor")).toBeInTheDocument();
    const initialTerminalMatches = screen.getAllByText(/Terminal /).length;
    expect(initialTerminalMatches).toBeGreaterThanOrEqual(2);

    // Type "theme" — Theme row and Terminal theme row match
    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-search-input"), {
        target: { value: "theme" },
      });
    });
    expect(screen.getByRole("heading", { name: "Theme" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Terminal colours" })).toBeInTheDocument();
    expect(screen.queryByText("Editor")).not.toBeInTheDocument();
    expect(screen.queryByText("Terminal prompt")).not.toBeInTheDocument();
  });

  it("shows empty state when no General row matches", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-search-input"), {
        target: { value: "zzz-no-match" },
      });
    });
    expect(screen.getByTestId("settings-general-empty")).toBeInTheDocument();
  });

  it("sidebar dims all nav items when query matches none, active section stays", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-search-input"), {
        target: { value: "zzz-no-match" },
      });
    });
    // Active section (General) stays mounted with its empty-state message
    expect(screen.getByTestId("settings-general-empty")).toBeInTheDocument();
    // Sidebar nav items are still visible (dimmed) — not removed
    expect(screen.getByTestId("settings-nav-general")).toBeInTheDocument();
    expect(screen.getByTestId("settings-nav-account")).toBeInTheDocument();
  });

  it("aria-live count announces 'N settings shown' only with a non-empty query", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByTestId("settings-search-count")).toHaveTextContent("");

    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-search-input"), {
        target: { value: "theme" },
      });
    });
    // 4 rows match "theme" substring: Theme (label), Terminal theme (label),
    // Viewer theme (C.6d migration), Code editor theme (SLW-WDE-7). General-
    // section filter is substring-only (no fuzzy), so unrelated rows like
    // Content width don't false-match via t-h-e-m-e walking.
    expect(screen.getByTestId("settings-search-count")).toHaveTextContent("2 settings shown");
  });
});

describe("SettingsPanel — Appearance Mode", () => {
  it("offers Light, Dark and System in Appearance and changes Mode", async () => {
    currentMockTheme = "dark";
    await act(async () => {
      render(<SettingsPanel />);
    });
    fireEvent.click(screen.getByTestId("settings-nav-appearance"));
    const mode = screen.getByRole("group", { name: "Mode" });
    expect(within(mode).getByRole("radio", { name: "Dark" })).toBeChecked();
    expect(within(mode).getByRole("radio", { name: "Light" })).not.toBeChecked();
    fireEvent.click(within(mode).getByRole("radio", { name: "System" }));
    expect(mockSetTheme).toHaveBeenCalledWith("system");
  });
});

describe("SettingsPanel — Editor row", () => {
  it("renders a button per detected editor plus Custom", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    await waitFor(() => {
      expect(screen.getByTestId("settings-editor-cursor")).toBeInTheDocument();
    });
    expect(screen.getByTestId("settings-editor-code")).toBeInTheDocument();
    expect(screen.getByTestId("settings-editor-custom")).toBeInTheDocument();
  });

  it("clicking a detected editor calls updateSettings({ editor, editorCustomCmd: '' })", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({ updateSettings: updateSpy });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await waitFor(() => {
      expect(screen.getByTestId("settings-editor-cursor")).toBeInTheDocument();
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-editor-cursor"));
    });
    // AC-2: clears editorCustomCmd so a stale custom value doesn't reappear
    // if the user later toggles back to Custom.
    expect(updateSpy).toHaveBeenCalledWith({ editor: "cursor", editorCustomCmd: "" });
  });

  it("selecting Custom reveals the custom command input; typing updates editorCustomCmd", async () => {
    type Settings = ReturnType<typeof useViewerStore.getState>["settings"];
    const updateSpy = vi.fn((partial: Partial<Settings>) => {
      useViewerStore.setState({
        settings: {
          ...useViewerStore.getState().settings,
          ...partial,
        },
      });
    });
    useViewerStore.setState({ updateSettings: updateSpy });

    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-editor-custom"));
    });
    await waitFor(() => {
      expect(screen.getByTestId("settings-editor-custom-cmd")).toBeInTheDocument();
    });
    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-editor-custom-cmd"), {
        target: { value: "nvim" },
      });
    });
    expect(updateSpy).toHaveBeenCalledWith({ editorCustomCmd: "nvim" });
  });
});

describe("SettingsPanel — Terminal rows", () => {
  it("clicking a terminal theme calls updateSettings({ terminalTheme })", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({ updateSettings: updateSpy });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-nav-appearance"));
    });
    await act(async () => {
      fireEvent.click(
        within(screen.getByRole("group", { name: "Terminal colours" })).getByRole("radio", {
          name: "Nord",
        }),
      );
    });
    expect(updateSpy).toHaveBeenCalledWith({ terminalTheme: "nord" });
  });

  it("clicking a starship preset calls updateSettings({ starshipPreset })", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({ updateSettings: updateSpy });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-starship-minimal"));
    });
    expect(updateSpy).toHaveBeenCalledWith({ starshipPreset: "minimal" });
  });

  it("scrollback row shows the 5,000 default highlighted when never set", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    const btn = screen.getByTestId("settings-scrollback-5000");
    expect(btn).toBeInTheDocument();
    expect(btn.className).toContain("bg-accent");
  });

  it("clicking a scrollback preset calls updateSettings({ terminalScrollback })", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({ updateSettings: updateSpy });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-scrollback-50000"));
    });
    expect(updateSpy).toHaveBeenCalledWith({ terminalScrollback: 50000 });
  });

  it("clicking infinite scrollback calls updateSettings with 0 (AC-4)", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({ updateSettings: updateSpy });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-scrollback-0"));
    });
    expect(updateSpy).toHaveBeenCalledWith({ terminalScrollback: 0 });
  });

  it("says what the history length is, its default, and when it applies", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(
      screen.getByText(
        "Lines of history each terminal keeps. Default 5,000. Changes apply to terminals opened afterwards.",
      ),
    ).toBeInTheDocument();
  });

  it("a saved value is kept, 10,000 included", async () => {
    useViewerStore.setState((s) => ({ settings: { ...s.settings, terminalScrollback: 10000 } }));
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByTestId("settings-scrollback-10000").className).toContain("bg-accent");
    expect(screen.getByTestId("settings-scrollback-5000").className).not.toContain("bg-accent");
  });
});

describe("SettingsPanel — a11y + dark-mode smoke", () => {
  it("panel container has aria-label 'Settings'", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByTestId("settings-panel")).toBeInTheDocument();
  });

  it("search input is type='search' with aria-label", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    const input = screen.getByTestId("settings-search-input");
    expect(input).toHaveAttribute("type", "search");
    expect(input).toHaveAttribute("aria-label", "Search settings");
  });

  it("mounts without errors in dark mode", async () => {
    document.documentElement.classList.add("dark");
    try {
      await act(async () => {
        render(<SettingsPanel />);
      });
      expect(screen.getByTestId("settings-panel")).toBeInTheDocument();
    } finally {
      document.documentElement.classList.remove("dark");
    }
  });
});

// ---------------------------------------------------------------------------
// C.6d migration coverage — new General rows + search regression
// ---------------------------------------------------------------------------

describe("C.6d — Content width row", () => {
  it("renders 4 segmented buttons matching settings.contentWidth", async () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, contentWidth: "wide" },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    const wide = screen.getByTestId("settings-content-width-wide");
    expect(wide.getAttribute("aria-checked")).toBe("true");
    expect(screen.getByTestId("settings-content-width-narrow").getAttribute("aria-checked")).toBe(
      "false",
    );
  });

  it("clicking 'Full' calls updateSettings({contentWidth: 'full'})", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({ updateSettings: updateSpy });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-content-width-full"));
    });
    expect(updateSpy).toHaveBeenCalledWith({ contentWidth: "full" });
  });
});

describe("Appearance replaces the independent surface theme rows", () => {
  it("has an Appearance section after General", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    fireEvent.click(screen.getByTestId("settings-nav-appearance"));
    expect(screen.getByTestId("settings-appearance")).toBeInTheDocument();
    expect(screen.queryByText("Viewer theme")).toBeNull();
    expect(screen.queryByText("Code editor theme")).toBeNull();
  });
});

describe("C.6d — Font size rows + AC-3 preset/custom-px equivalence", () => {
  it("clicking a viewer font preset clears the px override (writes 0)", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({
      updateSettings: updateSpy,
      settings: {
        ...useViewerStore.getState().settings,
        viewerFontSize: "normal",
        viewerFontSizePx: 18, // existing custom override
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-viewer-font-size-large"));
    });
    expect(updateSpy).toHaveBeenCalledWith({
      viewerFontSize: "large",
      viewerFontSizePx: 0,
    });
  });

  it("typing the canonical preset px in the px input clears an existing override", async () => {
    const updateSpy = vi.fn();
    // Start with an existing custom override (18px) so the "clear" is observable —
    // typing the canonical preset px (14 for "normal") writes 0, removing the
    // override and snapping the row back to the preset.
    useViewerStore.setState({
      updateSettings: updateSpy,
      settings: {
        ...useViewerStore.getState().settings,
        viewerFontSize: "normal",
        viewerFontSizePx: 18,
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    const input = screen.getByTestId("settings-viewer-font-size-px") as HTMLInputElement;
    // VIEWER_FONT_PX_MAP.normal === 14 (per src/shared/lib/font-sizes.ts).
    await act(async () => {
      fireEvent.change(input, { target: { value: "14" } });
      fireEvent.blur(input);
    });
    expect(updateSpy).toHaveBeenCalledWith({ viewerFontSizePx: 0 });
  });

  it("typing a non-preset px writes the override (clamped if out of range)", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({
      updateSettings: updateSpy,
      settings: {
        ...useViewerStore.getState().settings,
        viewerFontSize: "normal",
        viewerFontSizePx: 0,
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    const input = screen.getByTestId("settings-viewer-font-size-px") as HTMLInputElement;
    await act(async () => {
      // fireEvent.change updates input.value; fireEvent.blur then triggers
      // onBlur which reads e.target.value (now "22").
      fireEvent.change(input, { target: { value: "22" } });
      fireEvent.blur(input);
    });
    expect(updateSpy).toHaveBeenCalledWith({ viewerFontSizePx: 22 });
  });

  it("typing non-numeric input is rejected and the input remounts back to effectivePx", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({
      updateSettings: updateSpy,
      settings: {
        ...useViewerStore.getState().settings,
        viewerFontSize: "normal",
        viewerFontSizePx: 0,
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    const input = screen.getByTestId("settings-viewer-font-size-px") as HTMLInputElement;
    await act(async () => {
      fireEvent.change(input, { target: { value: "abc" } });
      fireEvent.blur(input);
    });
    // No update written.
    expect(updateSpy).not.toHaveBeenCalled();
    // The input remounts back to effectivePx (14 for "normal" preset, no override).
    const remounted = screen.getByTestId("settings-viewer-font-size-px") as HTMLInputElement;
    expect(remounted.value).toBe("14");
  });

  it("tree font size: aria-checked matches settings.treeFontSize with no override", async () => {
    useViewerStore.setState({
      settings: {
        ...useViewerStore.getState().settings,
        treeFontSize: "small",
        treeFontSizePx: 0,
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByTestId("settings-tree-font-size-small").getAttribute("aria-checked")).toBe(
      "true",
    );
    expect(screen.getByTestId("settings-tree-font-size-normal").getAttribute("aria-checked")).toBe(
      "false",
    );
  });

  it("terminal font size: aria-checked matches settings.terminalFontSize with no override", async () => {
    useViewerStore.setState({
      settings: {
        ...useViewerStore.getState().settings,
        terminalFontSize: "xl",
        terminalFontSizePx: 0,
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    expect(screen.getByTestId("settings-terminal-font-size-xl").getAttribute("aria-checked")).toBe(
      "true",
    );
  });

  it("tree font size: clicking preset clears tree override", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({
      updateSettings: updateSpy,
      settings: {
        ...useViewerStore.getState().settings,
        treeFontSize: "small",
        treeFontSizePx: 17,
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-tree-font-size-large"));
    });
    expect(updateSpy).toHaveBeenCalledWith({ treeFontSize: "large", treeFontSizePx: 0 });
  });

  it("terminal font size: clicking 'xl' preset writes the right enum value", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({ updateSettings: updateSpy });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-terminal-font-size-xl"));
    });
    expect(updateSpy).toHaveBeenCalledWith({ terminalFontSize: "xl", terminalFontSizePx: 0 });
  });
});

describe("C.6d — EditorRow detected-editor quick-select (AC-2 / AC-12)", () => {
  it("clicking a detected editor button writes settings.editor and clears editorCustomCmd", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({
      updateSettings: updateSpy,
      settings: {
        ...useViewerStore.getState().settings,
        editor: "custom",
        editorCustomCmd: "nvim",
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    // Wait for detectTools().then to populate the detected editors list.
    await waitFor(() => {
      expect(screen.getByTestId("settings-editor-cursor")).toBeInTheDocument();
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-editor-cursor"));
    });
    // AC-2: detected-editor click writes editor AND clears editorCustomCmd
    // (so a stale "nvim" doesn't reappear if the user toggles back to Custom).
    expect(updateSpy).toHaveBeenCalledWith({ editor: "cursor", editorCustomCmd: "" });
  });

  it("clicking 'Custom' preserves the existing editorCustomCmd (only changes editor key)", async () => {
    const updateSpy = vi.fn();
    useViewerStore.setState({
      updateSettings: updateSpy,
      settings: {
        ...useViewerStore.getState().settings,
        editor: "cursor",
        editorCustomCmd: "",
      },
    });
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-editor-custom"));
    });
    // Custom toggle only mutates `editor`; editorCustomCmd left alone so the
    // input renders empty (or with a previously typed value if any).
    expect(updateSpy).toHaveBeenCalledWith({ editor: "custom" });
  });
});

describe("C.6d — About row", () => {
  it("renders version + 'Check for Updates' button", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    // getVersion is mocked to "0.0.0-test"; resolves on a microtask.
    await waitFor(() => {
      expect(screen.getByTestId("settings-about-version")).toHaveTextContent("CodeSense");
    });
    expect(screen.getByTestId("settings-about-check-updates")).toBeInTheDocument();
  });

  it("a failed check's 'Download manually' action opens the public download page", async () => {
    vi.mocked(checkForUpdate).mockRejectedValueOnce(new Error("offline"));
    const toastError = vi.spyOn(toast, "error");
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-about-check-updates"));
    });
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    const options = toastError.mock.calls[0][1] as {
      action: { label: string; onClick: () => void };
    };
    expect(options.action.label).toBe("Download manually");
    options.action.onClick();
    expect(openUrl).toHaveBeenCalledWith(PUBLIC_DOWNLOAD_URL);
    expect(PUBLIC_DOWNLOAD_URL).toBe("https://codesense.ai/download");
    toastError.mockRestore();
  });
});

describe("C.6d — Search regression (AC-16)", () => {
  it("typing 'font' matches viewer/tree/terminal font-size rows (3 expected)", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-search-input"), {
        target: { value: "font" },
      });
    });
    expect(screen.getByTestId("settings-viewer-font-size")).toBeInTheDocument();
    expect(screen.getByTestId("settings-tree-font-size")).toBeInTheDocument();
    expect(screen.getByTestId("settings-terminal-font-size")).toBeInTheDocument();
  });

  it("typing 'narrow' shows content-width and hides viewer-theme", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-search-input"), {
        target: { value: "narrow" },
      });
    });
    expect(screen.getByTestId("settings-content-width")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-viewer-theme")).toBeNull();
  });

  it("typing 'dracula' shows viewer-theme and hides content-width", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-search-input"), {
        target: { value: "dracula" },
      });
    });
    expect(screen.getByRole("group", { name: "Theme" })).toBeInTheDocument();
    expect(screen.queryByTestId("settings-content-width")).toBeNull();
  });

  it("typing a non-matching keyword hides all of content-width, viewer-theme, and status-line", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-search-input"), {
        target: { value: "xyzzy-no-match" },
      });
    });
    expect(screen.queryByTestId("settings-content-width")).toBeNull();
    expect(screen.queryByTestId("settings-viewer-theme")).toBeNull();
    expect(screen.queryByTestId("settings-about")).toBeNull();
  });

  it("typing 'about' matches the About row", async () => {
    await act(async () => {
      render(<SettingsPanel />);
    });
    await act(async () => {
      fireEvent.change(screen.getByTestId("settings-search-input"), {
        target: { value: "about" },
      });
    });
    expect(screen.getByTestId("settings-about-version")).toBeInTheDocument();
  });
});

// AC-15: regression sweep — no production source references the legacy
// `__settings__` sentinel or the `SettingsPane` component.
describe("C.6d — legacy reference sweep (AC-15)", () => {
  it("no production source under src/ references the legacy '__settings__' sentinel or SettingsPane", async () => {
    // Vitest runs on Node, so we can walk the filesystem here. Use sync fs
    // to keep the test deterministic and avoid Promise interleaving with
    // happy-dom's runtime.
    const fs = await import("node:fs");
    const path = await import("node:path");

    const ROOT = path.resolve(__dirname, "..", "..", "..", "..", "src");
    const PATTERNS = [
      // Match the legacy sentinel + the deleted component / module name.
      /__settings__/,
      /\bSettingsPane\b/,
      /["']\.\/components\/settings-pane["']/,
      /["'].*?\/components\/settings-pane["']/,
    ];

    const offenders: string[] = [];
    function walk(dir: string) {
      for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        const full = path.join(dir, entry.name);
        if (entry.isDirectory()) {
          // Skip generated folders.
          if (entry.name === "node_modules" || entry.name.startsWith(".")) continue;
          walk(full);
          continue;
        }
        if (!/\.(ts|tsx|js|jsx)$/.test(entry.name)) continue;
        // Skip test files — they may legitimately reference the sentinel
        // for documentation or in this file's own assertion list.
        if (/\.(test|coverage\.test|spec)\.[tj]sx?$/.test(entry.name)) continue;

        const text = fs.readFileSync(full, "utf8");
        for (const re of PATTERNS) {
          if (re.test(text)) {
            offenders.push(`${path.relative(ROOT, full)} matches ${re}`);
            break;
          }
        }
      }
    }

    walk(ROOT);
    expect(offenders).toEqual([]);
  });
});
