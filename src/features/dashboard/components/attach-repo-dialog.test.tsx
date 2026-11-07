// SPDX-License-Identifier: MIT
import { vi, describe, it, expect, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { AttachRepoDialog } from "./attach-repo-dialog";
import { useOrgStore } from "@/shared/stores/org-store";
import type { CurrentUser } from "@/shared/types/domain";

// F-25: user identity is read from `getCurrentUser` (the active local profile).
let currentUserValue: CurrentUser | null = null;
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

const mockAddSourceRepo = vi.fn();

/** The Rust core's answer for a tracked repo. */
function tracked(owner: string, repo: string, localPath: string | null = null) {
  const local = owner === "local";
  return {
    owner,
    repo,
    provider: local ? "local" : "github",
    host: local ? "local" : "github.com",
    remoteKey: `${local ? "local" : "github.com"}/${owner}/${repo}`,
    localPath,
    remoteUrl: null,
  };
}

vi.mock("@/tauri/commands", () => ({
  addSourceRepoToTeam: (...args: unknown[]) => mockAddSourceRepo(...args),
  isAddRepoError: (e: unknown) => e instanceof Error && "kind" in e,
  saveAppState: vi.fn(() => Promise.resolve()),
  loadAppState: vi.fn(() => Promise.resolve("{}")),
  getRepoPermissions: vi.fn(),
}));

const mockPickFolder = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => mockPickFolder(...args),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
}));

/** Reveal the URL path (the dialog defaults to the local-folder path). */
async function switchToUrlTab() {
  await userEvent.click(screen.getByRole("tab", { name: /from a url/i }));
}

describe("AttachRepoDialog controlled mode", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    seedOrgStore();
  });
  it("renders dialog content when controlled open=true, no trigger button", () => {
    render(<AttachRepoDialog open={true} onOpenChange={() => {}} />);
    expect(screen.getByText("Add a source repo")).toBeInTheDocument();
    // Both paths are offered.
    expect(screen.getByRole("tab", { name: /local folder/i })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /from a url/i })).toBeInTheDocument();
  });

  it("does not render dialog content when controlled open=false", () => {
    render(<AttachRepoDialog open={false} onOpenChange={() => {}} />);
    expect(screen.queryByText("Add a source repo")).not.toBeInTheDocument();
  });

  it("defaults to the local-folder path (no URL required)", () => {
    render(<AttachRepoDialog open={true} onOpenChange={() => {}} />);
    expect(screen.getByRole("button", { name: /choose folder/i })).toBeInTheDocument();
    // The URL input is not shown until the user picks the URL path.
    expect(screen.queryByPlaceholderText("https://github.com/owner/repo")).not.toBeInTheDocument();
  });

  it("calls onOpenChange when dialog is dismissed", async () => {
    const onOpenChange = vi.fn();
    render(<AttachRepoDialog open={true} onOpenChange={onOpenChange} />);
    await userEvent.keyboard("{Escape}");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });
});

describe("AttachRepoDialog uncontrolled mode", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    seedOrgStore();
  });
  it("renders trigger button and self-manages open state", async () => {
    render(<AttachRepoDialog />);
    const trigger = screen.getByRole("button", { name: /add source repo/i });
    expect(trigger).toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: /local folder/i })).not.toBeInTheDocument();
    await userEvent.click(trigger);
    expect(screen.getByRole("tab", { name: /local folder/i })).toBeInTheDocument();
  });
});

function seedOrgStore(overrides: Record<string, unknown> = {}) {
  currentUserValue = {
    github_user_id: 1,
    github_username: "test",
    display_name: "Test",
    avatar_url: null,
  };
  useOrgStore.setState({
    orgs: [
      {
        slug: "acme",
        display_name: "Acme",
        created_at: "2026-01-01T00:00:00Z",
        org_root: "/fake",
        remote_url: null,
        config_flags: {},
        teams: [{ name: "default", source_repos: [{ owner: "alderco", repo: "codesense" }] }],
        people: [],
      },
      {
        slug: "other-ws",
        display_name: "Other WS",
        created_at: "2026-01-01T00:00:00Z",
        org_root: "/fake-other",
        remote_url: null,
        config_flags: {},
        teams: [{ name: "eng", source_repos: [] }],
        people: [],
      },
    ],
    activeOrgSlug: "acme",
    activeTeamName: "default",
    workspaceRepos: {},
    ...overrides,
  });
}

describe("AttachRepoDialog — local folder path (no URL required)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    seedOrgStore();
  });

  it("picks a folder and attaches it without asking for a URL", async () => {
    mockPickFolder.mockResolvedValue("/Users/me/code/widgets");
    mockAddSourceRepo.mockResolvedValue(tracked("acme", "widgets", "/Users/me/code/widgets"));

    render(<AttachRepoDialog open={true} onOpenChange={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: /choose folder/i }));
    // The picked folder is shown.
    await waitFor(() => expect(screen.getByText("/Users/me/code/widgets")).toBeInTheDocument());
    await userEvent.click(screen.getByRole("button", { name: /attach folder/i }));

    // One call adds and binds the picked folder; the core reads its origin.
    await waitFor(() => {
      expect(mockAddSourceRepo).toHaveBeenCalledWith({
        githubUserId: 1,
        orgSlug: "acme",
        teamName: "default",
        input: { kind: "localPath", path: "/Users/me/code/widgets" },
      });
    });
    const team = useOrgStore.getState().orgs[0].teams[0];
    expect(team.source_repos).toContainEqual(
      expect.objectContaining({ owner: "acme", repo: "widgets" }),
    );
  });

  it("attaches a folder with no git origin, naming the repo from the folder", async () => {
    mockPickFolder.mockResolvedValue("/Users/me/code/scratch");
    mockAddSourceRepo.mockResolvedValue(tracked("local", "scratch", "/Users/me/code/scratch"));

    render(<AttachRepoDialog open={true} onOpenChange={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: /choose folder/i }));
    await userEvent.click(screen.getByRole("button", { name: /attach folder/i }));

    expect(await screen.findByTestId("attach-success")).toHaveTextContent("local/scratch");
  });
});

describe("AttachRepoDialog — SW-LN-29 local attach success state", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    seedOrgStore();
  });

  it("stays open on where context lands", async () => {
    mockPickFolder.mockResolvedValue("/Users/me/code/widgets");
    mockAddSourceRepo.mockResolvedValue(tracked("acme", "widgets", "/Users/me/code/widgets"));
    const onOpenChange = vi.fn();

    render(<AttachRepoDialog open={true} onOpenChange={onOpenChange} />);
    await userEvent.click(screen.getByRole("button", { name: /choose folder/i }));
    await userEvent.click(screen.getByRole("button", { name: /attach folder/i }));

    expect(await screen.findByTestId("attach-success")).toHaveTextContent(
      /your main checkout isn't touched/i,
    );
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
  });
});

describe("AttachRepoDialog — URL path", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    seedOrgStore();
  });

  it("uses workspaceTarget instead of active store state", async () => {
    mockAddSourceRepo.mockResolvedValue(tracked("foo", "bar"));

    render(
      <AttachRepoDialog
        open={true}
        onOpenChange={() => {}}
        workspaceTarget={{ orgSlug: "other-ws", teamName: "eng" }}
      />,
    );
    await switchToUrlTab();
    await userEvent.type(screen.getByPlaceholderText("https://github.com/owner/repo"), "foo/bar");
    await userEvent.keyboard("{Enter}");

    await waitFor(() => {
      expect(mockAddSourceRepo).toHaveBeenCalledWith({
        githubUserId: 1,
        orgSlug: "other-ws",
        teamName: "eng",
        input: { kind: "url", url: "foo/bar" },
      });
    });
  });

  it("falls back to active store state when workspaceTarget is omitted", async () => {
    mockAddSourceRepo.mockResolvedValue(tracked("new", "repo"));

    render(<AttachRepoDialog open={true} onOpenChange={() => {}} />);
    await switchToUrlTab();
    await userEvent.type(screen.getByPlaceholderText("https://github.com/owner/repo"), "new/repo");
    await userEvent.keyboard("{Enter}");

    await waitFor(() => {
      expect(mockAddSourceRepo).toHaveBeenCalledWith(
        expect.objectContaining({ orgSlug: "acme", teamName: "default" }),
      );
    });
  });

  it("duplicate guard: the core's already-tracked refusal shows as info", async () => {
    mockAddSourceRepo.mockRejectedValue(
      Object.assign(new Error("alderco/codesense is already tracked on team default"), {
        kind: "alreadyTracked",
      }),
    );

    render(<AttachRepoDialog open={true} onOpenChange={() => {}} />);
    await switchToUrlTab();
    await userEvent.type(
      screen.getByPlaceholderText("https://github.com/owner/repo"),
      "alderco/codesense",
    );
    await userEvent.keyboard("{Enter}");

    await waitFor(() => {
      expect(screen.getByText("Already tracked in this team.")).toBeInTheDocument();
    });
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
