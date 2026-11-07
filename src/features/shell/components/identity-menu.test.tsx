// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { screen, act, fireEvent, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderWithProviders as render } from "../../../../tests/support/helpers/render-with-providers";

import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import type { Org, Team, CurrentUser } from "@/shared/types/domain";
import { installMachinesBackend } from "@/features/machines/__test-utils__/machines-backend";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";

// SW-OC-28: the title bar's identity menu — one trigger after the traffic lights,
// one menu holding context repo, team, what's yours, and the app's destinations. Seam-level: rows drive the REAL org / ui / viewer stores; only the
// tab openers (other features' surfaces) are stubbed so we can see which one fires.

const {
  mockToastInfo,
  mockOpenWorkspaces,
  mockPickFolder,
  mockAttach,
  mockOpenScratchSpace,
  mockOpenAgents,
  mockOpenRoutines,
  mockOpenMachines,
  mockOpenDashboards,
  mockOpenAppSettings,
} = vi.hoisted(() => ({
  mockToastInfo: vi.fn(),
  mockOpenWorkspaces: vi.fn(),
  mockPickFolder: vi.fn(),
  mockAttach: vi.fn(),
  mockOpenScratchSpace: vi.fn(),
  mockOpenAgents: vi.fn(),
  mockOpenRoutines: vi.fn(),
  mockOpenMachines: vi.fn(),
  mockOpenDashboards: vi.fn(),
  mockOpenAppSettings: vi.fn(),
}));

vi.mock("@/features/scratch-space/scratch-space-virtual-tab", () => ({
  openScratchSpace: mockOpenScratchSpace,
}));
vi.mock("@/features/agents/agents-virtual-tab", () => ({ openAgents: mockOpenAgents }));
vi.mock("@/features/routines/routines-virtual-tab", () => ({ openRoutines: mockOpenRoutines }));
vi.mock("@/features/dashboard-publishing/dashboards-virtual-tab", () => ({
  openDashboards: mockOpenDashboards,
}));
vi.mock("@/features/shell/machines-virtual-tab", () => ({ openMachines: mockOpenMachines }));
vi.mock("@/features/settings/lib/open-settings", () => ({ openAppSettings: mockOpenAppSettings }));
vi.mock(import("@/features/routines/stores/routines-store"), async (importOriginal) => ({
  ...(await importOriginal()),
  // The count is seeded straight into the store; no backend read.
  watchRoutineCounts: () => () => {},
}));

vi.mock("@/features/org/lib/open-workspaces", () => ({
  openWorkspaces: mockOpenWorkspaces,
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mockPickFolder }));

vi.mock(import("@/shared/lib/team-key"), async (importOriginal) => {
  const actual = await importOriginal();
  return actual;
});

vi.mock("sonner", () => ({
  toast: { info: mockToastInfo, error: vi.fn(), success: vi.fn() },
}));

// The machine registry commands stay real: the machines backend answers them
// through the invoke seam.
vi.mock(import("@/tauri/commands"), async (importOriginal) => ({
  ...(await importOriginal()),
  loadAppState: vi.fn(() => Promise.resolve("{}")),
  saveAppState: vi.fn(() => Promise.resolve()),
  listContextAttachments: vi.fn(() => Promise.resolve([])),
  attachContextRepository: mockAttach,
}));

let currentUserValue: CurrentUser | null = null;
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

import { IdentityMenu } from "./identity-menu";
import { CODESENSE_DOCS_DISPLAY_NAME, CODESENSE_DOCS_SLUG } from "@/shared/lib/codesense-docs";

const TEST_USER: CurrentUser = {
  principal: "42",
  github_user_id: 42,
  github_username: "alice",
  display_name: "Alice",
  avatar_url: null,
};

function makeTeam(name: string): Team {
  return { name, source_repos: [] };
}

function makeOrg(slug: string, displayName: string, teams: Team[] = []): Org {
  return {
    slug,
    display_name: displayName,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {} as Record<string, never>,
    people: [],
    teams,
    org_root: "",
  };
}

function resetStores(
  options: { orgs?: Org[]; activeOrgSlug?: string | null; activeTeamName?: string | null } = {},
) {
  currentUserValue = TEST_USER;
  useOrgStore.setState({
    orgs: options.orgs ?? [],
    activeOrgSlug: options.activeOrgSlug ?? null,
    activeTeamName: options.activeTeamName ?? null,
    shownTeamsByOrgSlug: {},
    disabledOrgSlugs: [],
    stateLoaded: true,
    hasUserIntent: false,
  });
  useUiStore.getState().closeIdentityMenu();
  useUiStore.setState({
    settingsPanelOpen: false,
    settingsInitialSection: null,
    noTeamTabs: [],
    noTeamActiveTab: null,
  });
  useRoutinesStore.setState({ needsYouCount: 0 });
}

const trigger = () => screen.getByTestId("identity-menu-trigger");
const menu = () => screen.getByRole("menu");
const sectionIds = () =>
  Array.from(menu().querySelectorAll("[data-menu-section]")).map((el) =>
    el.getAttribute("data-menu-section"),
  );

async function openByClick() {
  await act(async () => {
    fireEvent.click(trigger());
  });
  return menu();
}

/** Two teams in one org, plus a second context repo: the everyday shape. */
function seedEveryday() {
  resetStores({
    orgs: [
      makeOrg("acme", "Acme", [makeTeam("default"), makeTeam("platform")]),
      makeOrg("bolt", "Bolt", [makeTeam("core")]),
    ],
    activeOrgSlug: "acme",
    activeTeamName: "default",
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  resetStores();
});

afterEach(() => {
  delete window.__CODESENSE_DASHBOARD_PUBLISH__;
  resetStores();
});

describe("IdentityMenu — trigger", () => {
  it("names whose space you're in: context repo / team, with the org droppable when narrow", () => {
    seedEveryday();
    render(<IdentityMenu />);
    expect(trigger()).toHaveAccessibleName("Acme / default");
    expect(trigger()).toHaveAttribute("aria-haspopup", "menu");
    // The org segment hides below the titlebar container's 75rem; the team never does.
    expect(screen.getByTestId("identity-menu-org").className).toContain("@[75rem]/titlebar:inline");
    expect(screen.getByTestId("identity-menu-team").className).not.toContain("hidden");
  });

  it("0 context repos: the trigger still opens a menu that offers Connect and Create", async () => {
    // SW-OC-B10: never a dead label on the first-run floor.
    render(<IdentityMenu />);
    expect(trigger()).toHaveTextContent("No context repo");
    await openByClick();
    expect(screen.getByTestId("context-repo-connect")).toHaveTextContent("Connect a context repo…");
    expect(screen.getByTestId("context-repo-create")).toHaveTextContent(
      "Create a new context repo…",
    );
    expect(sectionIds()).not.toContain("team");
  });

  it("1 context repo, 0 teams: just the repo — no team in the trigger or the menu", async () => {
    resetStores({ orgs: [makeOrg("solo", "Solo")], activeOrgSlug: "solo" });
    render(<IdentityMenu />);
    expect(trigger()).toHaveAccessibleName("Solo");
    expect(screen.queryByTestId("identity-menu-team")).toBeNull();
    await openByClick();
    expect(sectionIds()).toEqual(["context-repo", "you"]);
  });

  it("the read-only CodeSense docs show their book glyph on the trigger", () => {
    resetStores({
      orgs: [makeOrg(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_DISPLAY_NAME)],
      activeOrgSlug: CODESENSE_DOCS_SLUG,
    });
    render(<IdentityMenu />);
    expect(trigger()).toHaveAttribute("data-context-repo", "codesense-docs");
    expect(trigger().querySelector("svg")).not.toBeNull();
    expect(trigger()).toHaveAccessibleName("CodeSense Docs");
  });

  it("the Needs you mark (and the accessible name) say when routines need you; none at zero", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    expect(screen.queryByTestId("identity-menu-attention")).toBeNull();
    act(() => useRoutinesStore.setState({ needsYouCount: 2 }));
    // the identity-menu's aggregate needs-you glyph is `question`
    // (the generic needs-you mark; the specific kind is only known at a
    // per-item level, not for a count roll-up).
    expect(screen.getByTestId("identity-menu-attention")).toHaveAttribute("data-mark", "question");
    expect(trigger()).toHaveAccessibleName("Acme / default — 2 need you");
    await openByClick();
    expect(screen.getByTestId("identity-menu-routines")).toHaveAccessibleName(
      "Schedule — 2 need you",
    );
    expect(screen.getByTestId("identity-menu-routines-count")).toHaveTextContent("2 need you");
    act(() => useRoutinesStore.setState({ needsYouCount: 1 }));
    expect(screen.getByTestId("identity-menu-routines-count")).toHaveTextContent("1 needs you");
    expect(trigger()).toHaveAccessibleName("Acme / default — 1 needs you");
  });
});

describe("IdentityMenu — the menu", () => {
  it("opens Dashboards from the App footer only in publishing builds", async () => {
    window.__CODESENSE_DASHBOARD_PUBLISH__ = true;
    seedEveryday();
    render(<IdentityMenu />);
    await openByClick();
    const app = screen.getByRole("group", { name: "App" });
    expect(
      within(app)
        .getAllByRole("menuitem")
        .map((el) => el.textContent),
    ).toEqual(["Machines⌘⌥C", "Dashboards", "Settings⌘,", "Keyboard shortcuts"]);
    await userEvent.click(screen.getByTestId("identity-menu-dashboards"));
    expect(mockOpenDashboards).toHaveBeenCalledOnce();
    expect(useUiStore.getState().identityMenuOpen).toBe(false);
  });
  it("hides Dashboards when the backend build flag is off", async () => {
    window.__CODESENSE_DASHBOARD_PUBLISH__ = false;
    seedEveryday();
    render(<IdentityMenu />);
    await openByClick();
    expect(screen.queryByTestId("identity-menu-dashboards")).toBeNull();
  });

  it("a click opens ONE menu with the groups in order: context repo, team, you, app", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    await openByClick();
    expect(useUiStore.getState().identityMenuOpen).toBe(true);
    expect(sectionIds()).toEqual(["context-repo", "team", "you"]);
    // The app group (Machines, Settings, Keyboard shortcuts) closes the menu.
    const rows = within(menu())
      .getAllByRole("menuitem")
      .map((el) => el.textContent);
    expect(rows.slice(-3)).toEqual(["Machines⌘⌥C", "Settings⌘,", "Keyboard shortcuts"]);
    // Click-opened, focus lands on the context repo you're in.
    expect(screen.getByTestId("org-switcher-row-acme")).toHaveFocus();
  });

  it("context repos: alphabetical, the active one checked IN PLACE (never hoisted)", async () => {
    resetStores({
      orgs: [makeOrg("a", "Acme"), makeOrg("z", "Zebra"), makeOrg("b", "Bolt")],
      activeOrgSlug: "z",
    });
    render(<IdentityMenu />);
    await openByClick();
    const rows = screen.getAllByTestId(/^org-switcher-row-/);
    expect(rows.map((r) => r.getAttribute("data-testid"))).toEqual([
      "org-switcher-row-a",
      "org-switcher-row-b",
      "org-switcher-row-z",
    ]);
    expect(rows[2]).toHaveAttribute("role", "menuitemradio");
    expect(rows[2]).toHaveAttribute("aria-checked", "true");
  });

  it("SW-OC-16: the CodeSense docs are pinned first with a book, read-only, and can't be disabled", async () => {
    resetStores({
      orgs: [
        makeOrg("a", "Acme"),
        makeOrg(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_DISPLAY_NAME),
        makeOrg("z", "Zebra"),
      ],
      activeOrgSlug: "z",
    });
    render(<IdentityMenu />);
    await openByClick();
    const rows = screen.getAllByTestId(/^org-switcher-row-/);
    expect(rows[0]).toHaveAttribute("data-testid", `org-switcher-row-${CODESENSE_DOCS_SLUG}`);
    expect(rows[0]).toHaveTextContent("CodeSense Docs");
    expect(rows[0]).toHaveTextContent("Read-only");
    expect(screen.queryByTestId(`org-switcher-disable-${CODESENSE_DOCS_SLUG}`)).toBeNull();
    expect(screen.getByTestId("org-switcher-disable-a")).toBeInTheDocument();
  });

  it("disable / enable a context repo: its own Disable item, one click back", async () => {
    resetStores({ orgs: [makeOrg("a", "Acme"), makeOrg("b", "Bolt")], activeOrgSlug: "a" });
    render(<IdentityMenu />);
    await openByClick();
    const disable = screen.getByTestId("org-switcher-disable-b");
    expect(disable).toHaveAttribute("role", "menuitem");
    fireEvent.click(disable);
    expect(useOrgStore.getState().disabledOrgSlugs).toEqual(["b"]);
    expect(screen.getByRole("group", { name: "Disabled" })).toContainElement(
      screen.getByTestId("org-switcher-enable-b"),
    );
    fireEvent.click(screen.getByTestId("org-switcher-enable-b"));
    expect(useOrgStore.getState().disabledOrgSlugs).toEqual([]);
  });

  it("switching context repo through the menu reaches the org store and closes the menu", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    await openByClick();
    fireEvent.click(screen.getByTestId("org-switcher-row-bolt"));
    expect(useOrgStore.getState().activeOrgSlug).toBe("bolt");
    expect(useUiStore.getState().identityMenuOpen).toBe(false);
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  });

  it("switching team through the menu reaches the org store's setActiveTeam and closes", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    await openByClick();
    fireEvent.click(screen.getByTestId("team-switcher-row-platform"));
    expect(useOrgStore.getState().activeTeamName).toBe("platform");
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    expect(trigger()).toHaveAccessibleName("Acme / platform");
  });

  it("You and app rows open their tabs (same openers the old title-bar buttons used)", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    const cases: [string, () => void][] = [
      ["identity-menu-private", () => expect(mockOpenScratchSpace).toHaveBeenCalled()],
      ["identity-menu-agents", () => expect(mockOpenAgents).toHaveBeenCalled()],
      ["identity-menu-routines", () => expect(mockOpenRoutines).toHaveBeenCalled()],
      ["identity-menu-machines", () => expect(mockOpenMachines).toHaveBeenCalled()],
      ["identity-menu-settings", () => expect(mockOpenAppSettings).toHaveBeenCalledWith("general")],
      [
        "identity-menu-shortcuts",
        () => expect(mockOpenAppSettings).toHaveBeenCalledWith("shortcuts"),
      ],
    ];
    for (const [testId, assert] of cases) {
      await openByClick();
      fireEvent.click(screen.getByTestId(testId));
      assert();
      expect(useUiStore.getState().identityMenuOpen).toBe(false);
    }
    // User-facing word is "Private", never "scratch".
    await openByClick();
    expect(screen.getByTestId("identity-menu-private")).toHaveTextContent("PrivateOnly you see it");
  });

  it("SW-OC-16: Connect picks a folder, confirms inline with the exact copy, then connects and closes", async () => {
    mockPickFolder.mockResolvedValue("/Users/me/acme-context");
    mockAttach.mockResolvedValue({
      org_slug: "acme/context",
      org_root: "/Users/me/acme-context",
      clone_outcome: "attached",
      loaded_org: {
        org: {
          ...makeOrg("acme/context", "Acme context", [makeTeam("core")]),
          org_root: "/Users/me/acme-context",
        },
      },
    });
    resetStores({
      orgs: [makeOrg(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_DISPLAY_NAME)],
      activeOrgSlug: CODESENSE_DOCS_SLUG,
    });
    useUiStore.setState({ orgSwitcherOpen: true });
    render(<IdentityMenu />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("context-repo-connect"));
    });
    const confirm = await screen.findByTestId("context-repo-connect-confirm");
    expect(confirm).toHaveTextContent("Connect acme-context?");
    expect(confirm).toHaveTextContent(
      "CodeSense will read and write files in /Users/me/acme-context. It won't run git — you or your agents do.",
    );
    expect(mockAttach).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.click(screen.getByTestId("context-repo-connect-submit"));
    });
    await waitFor(() => expect(useOrgStore.getState().activeOrgSlug).toBe("acme/context"));
    expect(mockAttach).toHaveBeenCalledWith("/Users/me/acme-context");
    expect(useUiStore.getState().orgSwitcherOpen).toBe(false);
  });
});

describe("IdentityMenu — keyboard", () => {
  it("⌘⇧G / ⌘⌥⇧T open the menu focused in their section; the same chord again closes it", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    const run = (id: string) => act(() => SHORTCUT_ACTIONS.find((a) => a.id === id)!.run());

    run("nav.switch-team");
    expect(await screen.findByRole("menu")).toBeInTheDocument();
    expect(screen.getByTestId("team-switcher-row-default")).toHaveFocus();

    // Moving an open menu to another section moves focus with it.
    run("nav.switch-org");
    await waitFor(() => expect(screen.getByTestId("org-switcher-row-acme")).toHaveFocus());

    run("nav.switch-org");
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  });

  it("arrows roam every section (wrapping), Home/End jump, typing jumps, Esc closes", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    await openByClick();
    const user = userEvent.setup();
    expect(screen.getByTestId("org-switcher-row-acme")).toHaveFocus();
    // Each row's Disable action is its own stop, right after the row.
    await user.keyboard("{ArrowDown}");
    expect(screen.getByTestId("org-switcher-disable-acme")).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByTestId("org-switcher-row-bolt")).toHaveFocus();
    await user.keyboard("{ArrowUp}");
    await user.keyboard("{ArrowUp}{ArrowUp}");
    // Wraps from the first row to the last.
    expect(screen.getByTestId("identity-menu-shortcuts")).toHaveFocus();
    await user.keyboard("{Home}");
    expect(screen.getByTestId("org-switcher-row-acme")).toHaveFocus();
    await user.keyboard("{End}");
    expect(screen.getByTestId("identity-menu-shortcuts")).toHaveFocus();
    // Type-ahead: "pl" lands on the platform team; Enter switches to it.
    await user.keyboard("pl");
    expect(screen.getByTestId("team-switcher-row-platform")).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(useOrgStore.getState().activeTeamName).toBe("platform");

    await openByClick();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    expect(useUiStore.getState().identityMenuOpen).toBe(false);
  });
});

describe("IdentityMenu — no machine lens", () => {
  it("has no Machine section and no machine switch, even with remote machines; Machines stays a row", async () => {
    installMachinesBackend("demo");
    resetStores({
      orgs: [makeOrg("solo", "Solo", [makeTeam("default")])],
      activeOrgSlug: "solo",
      activeTeamName: "default",
    });
    render(<IdentityMenu />);
    await openByClick();
    expect(sectionIds()).toEqual(["context-repo", "team", "you"]);
    expect(within(menu()).queryByText(/switch machine/i)).toBeNull();
    expect(screen.getByTestId("identity-menu-machines")).toBeInTheDocument();
    expect(SHORTCUT_ACTIONS.some((a) => a.id === "nav.switch-machine")).toBe(false);
  });

  it("agents waiting on the team's listed (not hidden) machines count on the Machines row and light the dot", async () => {
    installMachinesBackend("demo");
    resetStores({
      orgs: [makeOrg("solo", "Solo", [makeTeam("default")])],
      activeOrgSlug: "solo",
      activeTeamName: "default",
    });
    render(<IdentityMenu />);
    await screen.findByTestId("identity-menu-attention");
    await openByClick();
    expect(screen.getByTestId("identity-menu-machines-count")).toHaveTextContent("2 need you");
    expect(screen.getByTestId("identity-menu-machines")).toHaveAccessibleName(
      "Machines — 2 need you",
    );
  });
});

describe("IdentityMenu — the space switcher's look", () => {
  it("the trigger carries the active repo's mark beside repo / team, as a switcher chip", () => {
    seedEveryday();
    render(<IdentityMenu />);
    const mark = within(trigger()).getByTestId("context-repo-mark");
    expect(mark).toHaveAttribute("data-initials", "AC");
    // The mark is decoration: the trigger's name stays "Acme / default".
    expect(mark).toHaveAttribute("aria-hidden", "true");
    expect(trigger()).toHaveAccessibleName("Acme / default");
  });

  it("each repo row leads with its mark, yet type-ahead still matches the repo's name", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    await openByClick();
    const bolt = screen.getByTestId("org-switcher-row-bolt");
    expect(within(bolt).getByTestId("context-repo-mark")).toHaveAttribute("data-initials", "BO");
    // Initials are generated content, so the row's text is just its name.
    expect(bolt).toHaveTextContent(/^Bolt$/);
    const user = userEvent.setup();
    await user.keyboard("b");
    expect(bolt).toHaveFocus();
  });

  it("the active repo and team carry a resting wash and a trailing check, without leaving their place", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    await openByClick();
    const acme = screen.getByTestId("org-switcher-row-acme");
    const team = screen.getByTestId("team-switcher-row-default");
    for (const row of [acme, team]) {
      expect(row).toHaveAttribute("aria-checked", "true");
      expect(row.className).toContain("bg-foreground/[0.05]");
      expect(row.querySelector("svg.lucide-check")).not.toBeNull();
    }
    expect(
      screen.getByTestId("team-switcher-row-platform").querySelector("svg.lucide-check"),
    ).toBeNull();
  });

  it("the team group says whose teams they are, and the You group names you", async () => {
    seedEveryday();
    render(<IdentityMenu />);
    await openByClick();
    expect(screen.getByRole("group", { name: "Teams in Acme" })).toContainElement(
      screen.getByTestId("team-switcher-row-default"),
    );
    const you = menu().querySelector('[data-menu-section="you"]')!;
    expect(you).toHaveTextContent("Alice");
    expect(screen.getByRole("group", { name: "You" })).toContainElement(
      screen.getByTestId("identity-menu-private"),
    );
  });
});
