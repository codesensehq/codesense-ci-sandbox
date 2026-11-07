// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import type { CurrentUser, Org, Team, Person } from "@/shared/types/domain";

// ---------- Mocks ----------

let currentUserValue: CurrentUser | null = null;
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

vi.mock("sonner", () => ({
  toast: { info: vi.fn(), success: vi.fn(), error: vi.fn() },
}));

const mockListAttachmentsByRemote = vi.fn().mockResolvedValue(new Map());
const mockDeleteSourceRepoAttachment = vi.fn().mockResolvedValue(undefined);
const mockCommitRegistryEditLocal = vi.fn().mockResolvedValue({ commit_message: "ok" });
const mockCommitRegistryEditAsPr = vi.fn();
// SS-FU-17: SourceRepoPill probes path existence on mount; default to
// "present" so attached pills render the normal (non-gone) state.
const mockCheckPathExists = vi.fn().mockResolvedValue(true);

vi.mock("@/tauri/commands", () => ({
  listAttachmentsByRemote: (...args: unknown[]) => mockListAttachmentsByRemote(...args),
  deleteSourceRepoAttachment: (...args: unknown[]) => mockDeleteSourceRepoAttachment(...args),
  commitRegistryEditLocal: (...args: unknown[]) => mockCommitRegistryEditLocal(...args),
  commitRegistryEditAsPr: (...args: unknown[]) => mockCommitRegistryEditAsPr(...args),
  checkPathExists: (...args: unknown[]) => mockCheckPathExists(...args),
  parseGitRemote: vi.fn(),
  saveAppState: vi.fn(() => Promise.resolve()),
  loadAppState: vi.fn(() => Promise.resolve("{}")),
  getRepoPermissions: vi.fn(),
  legacyRepositoryStatuses: vi.fn(() => Promise.resolve([])),
  retryLegacyRepositoryJob: vi.fn(() => Promise.resolve()),
}));

// SS-FU-17: the pill listens for `sync-repo-looks-gone`.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock("@/features/attachment/components/attach-local-clone-dialog", () => ({
  AttachLocalCloneDialog: () => <div data-testid="mock-attach-local-clone-dialog" />,
}));

vi.mock("@/features/dashboard/components/attach-repo-dialog", () => ({
  AttachRepoDialog: () => null,
}));

const { toast } = await import("sonner");
const { useOrgStore } = await import("@/shared/stores/org-store");
const { TeamsCard } = await import("./teams-card");

// ---------- Fixtures ----------

function makeTeam(overrides: Partial<Team>): Team {
  return {
    name: "backend",
    source_repos: [],
    ...overrides,
  };
}

function makeOrg(teams: Team[] = [], people: Person[] = []): Org {
  return {
    slug: "acme",
    display_name: "Acme",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: "https://github.com/acme/context.git",
    config_flags: {},
    people,
    teams,
    org_root: "",
  };
}

beforeEach(() => {
  vi.mocked(toast.info).mockClear();
  mockListAttachmentsByRemote.mockClear();
  mockDeleteSourceRepoAttachment.mockClear();
  mockCommitRegistryEditLocal.mockClear();
  mockListAttachmentsByRemote.mockResolvedValue(new Map());

  currentUserValue = {
    github_user_id: 1,
    github_username: "test",
    display_name: "Test",
    avatar_url: null,
  };
  useOrgStore.setState({ attachmentsVersion: 0 });
});

// ---------- Card layout (AC1, AC3) ----------

describe("TeamsCard — card layout", () => {
  it("renders inside a SettingsCard with data-testid", () => {
    render(<TeamsCard org={makeOrg([])} query="" />);
    expect(screen.getByTestId("settings-card-teams")).toBeInTheDocument();
  });

  it("card header shows 'Teams' title with count", () => {
    const team = makeTeam({ name: "backend" });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const card = screen.getByTestId("settings-card-teams");
    expect(card).toHaveTextContent("Teams");
    expect(card).toHaveTextContent("(1)");
  });
});

// ---------- Team tiles (AC3) ----------

describe("TeamsCard — team tiles", () => {
  it("renders each team with name + repo count", () => {
    const team = makeTeam({
      name: "backend",
      source_repos: [{ owner: "acme", repo: "api" }],
    });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const tile = screen.getByTestId("settings-team-pill-backend");
    expect(tile).toHaveTextContent("backend");
    expect(tile).toHaveTextContent("1 repos");
  });

  it("zero-teams empty state", () => {
    render(<TeamsCard org={makeOrg([])} query="" />);
    expect(screen.getByTestId("settings-teams-empty")).toBeInTheDocument();
    expect(screen.getByTestId("settings-teams-empty")).toHaveTextContent(
      "No teams yet. Create your first team.",
    );
  });
});

// ---------- Expand/collapse (AC3) ----------

describe("TeamsCard — expand/collapse", () => {
  it("clicking a team tile toggles the expanded content", () => {
    const team = makeTeam({
      name: "backend",
      source_repos: [{ owner: "acme", repo: "api" }],
    });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    expect(screen.queryByTestId("settings-team-expanded-backend")).not.toBeInTheDocument();
    const tileBtn = screen.getByTestId("settings-team-pill-backend").querySelector("button")!;
    fireEvent.click(tileBtn);
    const expanded = screen.getByTestId("settings-team-expanded-backend");
    // SW-TM-3: no Members section — the expanded tile shows source repos.
    expect(expanded).toHaveTextContent("acme/api");
    fireEvent.click(tileBtn);
    expect(screen.queryByTestId("settings-team-expanded-backend")).not.toBeInTheDocument();
  });
});

// ---------- [+ Create team] (AC8) ----------

describe("TeamsCard — [+ Create team]", () => {
  // SW-OB-7: capability is open — the create-team button shows at every tier,
  // including the former read-only tier (gate is always-allow).
  it("button visible (capability open)", () => {
    render(<TeamsCard org={makeOrg([])} query="" />);
    expect(screen.getByTestId("settings-teams-create-button")).toBeInTheDocument();
  });

  it("default click fires the C.10a stub toast", () => {
    render(<TeamsCard org={makeOrg([])} query="" />);
    fireEvent.click(screen.getByTestId("settings-teams-create-button"));
    expect(toast.info).toHaveBeenCalledWith("Create team is coming in C.10a");
  });

  it("override prop replaces the stub handler", () => {
    const onCreate = vi.fn();
    render(<TeamsCard org={makeOrg([])} query="" onCreateTeamClick={onCreate} />);
    fireEvent.click(screen.getByTestId("settings-teams-create-button"));
    expect(onCreate).toHaveBeenCalledOnce();
    expect(toast.info).not.toHaveBeenCalled();
  });
});

// ---------- Search filter ----------

describe("TeamsCard — search filter", () => {
  it("hides the entire card when query doesn't match", () => {
    const team = makeTeam({ name: "backend" });
    render(<TeamsCard org={makeOrg([team])} query="oauth" />);
    expect(screen.queryByTestId("settings-card-teams")).not.toBeInTheDocument();
  });

  it("shows card when source-repos keywords match", () => {
    const team = makeTeam({ name: "backend" });
    render(<TeamsCard org={makeOrg([team])} query="source repos" />);
    expect(screen.getByTestId("settings-team-pill-backend")).toBeInTheDocument();
  });
});

// ---------- Source repo pills (AC4) ----------

describe("TeamsCard — source repo pills", () => {
  it("shows 'No tracked repos' when team has no source repos", () => {
    const team = makeTeam({ name: "default", source_repos: [] });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const tileBtn = screen.getByTestId("settings-team-pill-default").querySelector("button")!;
    fireEvent.click(tileBtn);
    expect(screen.getByTestId("settings-source-repos-empty")).toHaveTextContent("No tracked repos");
  });

  it("renders source repo pills when expanded", () => {
    const team = makeTeam({
      name: "default",
      source_repos: [
        { owner: "acme", repo: "api" },
        { owner: "acme", repo: "web" },
      ],
    });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const tileBtn = screen.getByTestId("settings-team-pill-default").querySelector("button")!;
    fireEvent.click(tileBtn);
    expect(screen.getByTestId("settings-source-repos-list")).toBeInTheDocument();
    expect(screen.getByTestId("settings-source-repo-acme/api")).toBeInTheDocument();
    expect(screen.getByTestId("settings-source-repo-acme/web")).toBeInTheDocument();
  });

  it("unattached repos are labelled not attached", () => {
    const team = makeTeam({
      name: "default",
      source_repos: [{ owner: "acme", repo: "api" }],
    });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const tileBtn = screen.getByTestId("settings-team-pill-default").querySelector("button")!;
    fireEvent.click(tileBtn);
    expect(screen.getByTestId("settings-source-repo-acme/api")).toHaveAccessibleName(
      "acme/api: not attached",
    );
  });

  it("attached repos are labelled attached locally", async () => {
    const attachmentMap = new Map([
      [
        "github.com/acme/api",
        {
          repo_path: "/Users/dev/api",
          remote_url: "https://github.com/acme/api.git",
          attached_at: "2026-01-01T00:00:00Z",
          tracking_prompt_dismissed_at: null,
          credential_ref: null,
        },
      ],
    ]);
    mockListAttachmentsByRemote.mockResolvedValue(attachmentMap);

    const team = makeTeam({
      name: "default",
      source_repos: [{ owner: "acme", repo: "api" }],
    });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const tileBtn = screen.getByTestId("settings-team-pill-default").querySelector("button")!;
    fireEvent.click(tileBtn);
    expect(
      await screen.findByRole("button", { name: "acme/api: attached locally" }),
    ).toBeInTheDocument();
  });

  // SS-FU-17 AC-7: a gone path is labelled unavailable and offers Forget/Re-attach.
  it("a gone repo is labelled unavailable and offers Forget/Re-attach affordances", async () => {
    mockCheckPathExists.mockResolvedValue(false); // path no longer resolves
    const attachmentMap = new Map([
      [
        "github.com/acme/api",
        {
          repo_path: "/Users/dev/api",
          remote_url: "https://github.com/acme/api.git",
          attached_at: "2026-01-01T00:00:00Z",
          tracking_prompt_dismissed_at: null,
          credential_ref: null,
        },
      ],
    ]);
    mockListAttachmentsByRemote.mockResolvedValue(attachmentMap);

    const team = makeTeam({ name: "default", source_repos: [{ owner: "acme", repo: "api" }] });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    fireEvent.click(screen.getByTestId("settings-team-pill-default").querySelector("button")!);

    expect(
      await screen.findByRole("button", { name: "acme/api: folder unavailable" }),
    ).toBeInTheDocument();

    // Popover surfaces the notice + Re-attach + Forget (never auto-prunes).
    fireEvent.click(screen.getByTestId("settings-source-repo-acme/api"));
    expect(screen.getByRole("status")).toHaveTextContent("This folder is unavailable");
    expect(screen.getByTestId("attach-clone-acme/api")).toHaveTextContent("Re-attach folder");
    expect(screen.getByRole("button", { name: "Forget attachment" })).toBeInTheDocument();

    mockCheckPathExists.mockResolvedValue(true); // restore default for later tests
  });
});

// ---------- Action button visibility (AC4, AC8) ----------

describe("TeamsCard — source repo action buttons", () => {
  it("shows attach/remove buttons in popover for writers", () => {
    const team = makeTeam({
      name: "default",
      source_repos: [{ owner: "acme", repo: "api" }],
    });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const tileBtn = screen.getByTestId("settings-team-pill-default").querySelector("button")!;
    fireEvent.click(tileBtn);
    // Click pill to open popover
    fireEvent.click(screen.getByTestId("settings-source-repo-acme/api"));
    expect(screen.getByTestId("attach-clone-acme/api")).toBeInTheDocument();
    expect(screen.getByTestId("remove-repo-acme/api")).toBeInTheDocument();
  });

  it("SW-OB-7: shows action buttons even at the former read-only tier (capability open)", () => {
    const team = makeTeam({
      name: "default",
      source_repos: [{ owner: "acme", repo: "api" }],
    });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const tileBtn = screen.getByTestId("settings-team-pill-default").querySelector("button")!;
    fireEvent.click(tileBtn);
    // Click pill to open popover
    fireEvent.click(screen.getByTestId("settings-source-repo-acme/api"));
    expect(screen.getByTestId("attach-clone-acme/api")).toBeInTheDocument();
    expect(screen.getByTestId("remove-repo-acme/api")).toBeInTheDocument();
  });
});

// ---------- Track repo button ----------

describe("TeamsCard — track repo button", () => {
  it("shows Track repo button for writers", () => {
    const team = makeTeam({ name: "default", source_repos: [] });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const tileBtn = screen.getByTestId("settings-team-pill-default").querySelector("button")!;
    fireEvent.click(tileBtn);
    expect(screen.getByTestId("settings-track-repo-default")).toBeInTheDocument();
  });

  it("SW-OB-7: shows Track repo button even at the former read-only tier (capability open)", () => {
    const team = makeTeam({ name: "default", source_repos: [] });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    const tileBtn = screen.getByTestId("settings-team-pill-default").querySelector("button")!;
    fireEvent.click(tileBtn);
    expect(screen.getByTestId("settings-track-repo-default")).toBeInTheDocument();
  });
});

// ---------- NB-6: virtualized path (above 15-team threshold) ----------

describe("TeamsCard — virtualized list above threshold", () => {
  it("activates the virtualized branch when count > 15", () => {
    const teams = Array.from({ length: 16 }, (_, i) =>
      makeTeam({ name: `team-${i + 1}`, source_repos: [] }),
    );
    const { container } = render(<TeamsCard org={makeOrg(teams)} query="" />);

    // Container is queryable by testid in both branches.
    const list = screen.getByTestId("settings-teams-list");
    expect(list).toBeInTheDocument();

    // Virtualized branch wraps the scroll container in `max-h-[400px]`; the
    // non-virtualized branch uses `flex flex-col`. Distinguishing via class
    // signals which branch is mounted without coupling to internal DOM.
    expect(list.className).toContain("max-h-[400px]");

    // We don't assert all 16 tiles are in the DOM — TeamTile uses
    // `measureElement` (variable-height TileBody when expanded), which
    // happy-dom reports as 0px via `getBoundingClientRect`. The virtualizer
    // then computes a degenerate visible range. Production layout works
    // because real measurements are non-zero; in tests we assert the
    // branch is mounted plus that SOME tiles materialize.
    const tiles = container.querySelectorAll('[data-testid^="settings-team-pill-"]');
    expect(tiles.length).toBeGreaterThan(0);
  });
});

// ---------- Starter page ----------

describe("TeamsCard — starter page", () => {
  const expand = (name: string) =>
    fireEvent.click(screen.getByTestId(`settings-team-pill-${name}`).querySelector("button")!);

  beforeEach(() => {
    vi.mocked(toast.error).mockClear();
  });

  it("shows the README default, then sets a page through Change", async () => {
    const team = makeTeam({ name: "backend" });
    useOrgStore.setState({ orgs: [makeOrg([team])], activeOrgSlug: "acme" });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    expand("backend");
    expect(screen.getByTestId("settings-starter-page-current")).toHaveTextContent(
      "Default (README or index)",
    );
    expect(screen.queryByTestId("settings-starter-page-clear-backend")).toBeNull();
    fireEvent.click(screen.getByTestId("settings-starter-page-change-backend"));
    const input = screen.getByTestId("settings-starter-page-input-backend");
    fireEvent.change(input, { target: { value: ".env" } });
    expect(screen.getByTestId("settings-starter-page-save-backend")).toBeDisabled();
    expect(input).toHaveAccessibleDescription(/Hidden files and \.\. aren't allowed/);
    fireEvent.change(input, { target: { value: "guide/start.md" } });
    fireEvent.click(screen.getByTestId("settings-starter-page-save-backend"));
    await vi.waitFor(() =>
      expect(mockCommitRegistryEditLocal).toHaveBeenCalledWith(1, "acme", {
        type: "SetTeamLandingPage",
        team_name: "backend",
        landing_page: "guide/start.md",
      }),
    );
  });

  it("clears a declared page", async () => {
    const team = makeTeam({ name: "backend", landing_page: "guide/start.md" });
    useOrgStore.setState({ orgs: [makeOrg([team])], activeOrgSlug: "acme" });
    render(<TeamsCard org={makeOrg([team])} query="" />);
    expand("backend");
    expect(screen.getByTestId("settings-starter-page-current")).toHaveTextContent("guide/start.md");
    fireEvent.click(screen.getByTestId("settings-starter-page-clear-backend"));
    await vi.waitFor(() =>
      expect(mockCommitRegistryEditLocal).toHaveBeenCalledWith(1, "acme", {
        type: "SetTeamLandingPage",
        team_name: "backend",
        landing_page: null,
      }),
    );
  });
});

describe("TeamsCard — starter page, read-only and failures", () => {
  const expand = (name: string) =>
    fireEvent.click(screen.getByTestId(`settings-team-pill-${name}`).querySelector("button")!);

  it("the CodeSense Docs team shows its page read-only, with no Change", () => {
    const team = makeTeam({ name: "getting-started", landing_page: "README.html" });
    const docs = { ...makeOrg([team]), slug: "codesense/docs" };
    render(<TeamsCard org={docs} query="" />);
    expand("getting-started");
    expect(screen.getByTestId("settings-starter-page-getting-started")).toHaveTextContent(
      "CodeSense Docs are read-only.",
    );
    expect(screen.queryByTestId("settings-starter-page-change-getting-started")).toBeNull();
  });

  it("a refused save keeps the draft open and says why; Escape cancels", async () => {
    const team = makeTeam({ name: "backend" });
    useOrgStore.setState({ orgs: [makeOrg([team])], activeOrgSlug: "acme" });
    mockCommitRegistryEditLocal.mockRejectedValueOnce(new Error("not_found: no file"));
    render(<TeamsCard org={makeOrg([team])} query="" />);
    expand("backend");
    fireEvent.click(screen.getByTestId("settings-starter-page-change-backend"));
    const input = screen.getByTestId("settings-starter-page-input-backend");
    fireEvent.change(input, { target: { value: "missing.md" } });
    fireEvent.click(screen.getByTestId("settings-starter-page-save-backend"));
    await vi.waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(screen.getByTestId("settings-starter-page-input-backend")).toBeInTheDocument();
    fireEvent.keyDown(screen.getByTestId("settings-starter-page-input-backend"), { key: "Escape" });
    expect(screen.queryByTestId("settings-starter-page-input-backend")).toBeNull();
    await vi.waitFor(() =>
      expect(document.activeElement).toBe(
        screen.getByTestId("settings-starter-page-change-backend"),
      ),
    );
  });
});
