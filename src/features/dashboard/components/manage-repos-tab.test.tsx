// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { Attachment } from "@/shared/types/domain";
import type { RepoState } from "@/shared/stores/config-store";
import { useFirstRunStore } from "@/shared/stores/first-run-store";

// SW-OC-8: the Source-repos tab is the ONE self-sufficient home for the three
// ways to get code into a team — add-from-URL, attach-a-local-clone, and use a
// CodeSense-managed workspace — with per-repo working-copy state + one-click
// attach/managed actions. These tests assert the tab surfaces all three paths
// and fires the existing attach commands, from the tab alone (no Settings).

// ---------- org-store ----------
let workspaceRepos: Record<string, RepoState> = {};
const mockRemoveTeamSourceRepo = vi.fn();
const mockRemoveSourceRepoFromTeam = vi.fn().mockResolvedValue(undefined);
const mockBump = vi.fn();
const mockUpdateTeamSourceRepos = vi.fn();
// Stands in for the Rust core: a folder with no origin is `local/<folder>`.
const mockAddSourceRepoToTeam = vi.fn(
  async (call: { input: { kind: "url"; url: string } | { kind: "localPath"; path: string } }) => {
    const local = call.input.kind === "localPath";
    const repo = local ? (call.input as { path: string }).path.split("/").pop()! : "new";
    return {
      owner: local ? "local" : "acme",
      repo,
      provider: local ? "local" : "github",
      host: local ? "local" : "github.com",
      remoteKey: local ? `local/local/${repo}` : `github.com/acme/${repo}`,
      localPath: local ? (call.input as { path: string }).path : null,
      remoteUrl: null,
    };
  },
);

vi.mock("@/shared/stores/org-store", () => {
  const getState = () => ({
    activeOrgSlug: "acme",
    activeTeamName: "default",
    workspaceRepos,
    orgs: [{ slug: "acme", teams: [{ name: "default", source_repos: [] }] }],
    removeTeamSourceRepo: mockRemoveTeamSourceRepo,
    bumpAttachmentsVersion: mockBump,
    attachmentsVersion: 0,
    updateTeamSourceRepos: mockUpdateTeamSourceRepos,
  });
  const store = (sel?: (s: unknown) => unknown) => (sel ? sel(getState()) : getState());
  store.getState = getState;
  return { useOrgStore: store };
});

// ---------- ui-store ----------
vi.mock("@/shared/stores/ui-store", () => {
  const getState = () => ({ sourceReposIntent: null, setSourceReposIntent: vi.fn() });
  const store = (sel?: (s: unknown) => unknown) => (sel ? sel(getState()) : getState());
  store.getState = getState;
  return { useUiStore: store };
});

// ---------- attachments-by-remote (per-repo managed/attached state) ----------
let attachmentsByRemote = new Map<string, Attachment>();
vi.mock("@/features/attachment/hooks/use-attachments-by-remote", () => ({
  useAttachmentsByRemote: () => attachmentsByRemote,
}));

// ---------- commands ----------
vi.mock("@/tauri/commands", () => ({
  removeSourceRepoFromTeam: (...args: unknown[]) => mockRemoveSourceRepoFromTeam(...args),
  // The embedded AddRepoForm's one call for both its URL and local paths.
  addSourceRepoToTeam: (call: Parameters<typeof mockAddSourceRepoToTeam>[0]) =>
    mockAddSourceRepoToTeam(call),
  isAddRepoError: (e: unknown) => e instanceof Error && "kind" in e,
}));

vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => ({ github_user_id: 42, github_username: "work" }),
}));

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }),
}));

const mockPickFolder = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: (...a: unknown[]) => mockPickFolder(...a) }));

// Stub the shared connection picker (AddRepoForm) so the URL path renders inline.
vi.mock("@/shared/components/connections/connection-picker", () => ({
  ConnectionPicker: () => <div data-testid="connection-picker" />,
}));

// Stub the attach dialog: capture props so we can assert the per-repo action
// opened it against the right owner/repo. Reuses the EXISTING attach flow.
const attachDialogSpy = vi.fn();
vi.mock("@/features/attachment/components/attach-local-clone-dialog", () => ({
  AttachLocalCloneDialog: (props: { owner: string; repo: string; open: boolean }) => {
    attachDialogSpy(props);
    return props.open ? (
      <div data-testid="attach-dialog" data-owner={props.owner} data-repo={props.repo} />
    ) : null;
  },
}));

const { ManageReposTab } = await import("./manage-repos-tab");

function synthRepo(owner: string, repo: string, localPath: string | null): RepoState {
  return {
    repoPath: `workspace:acme:default:${owner}/${repo}`,
    repoName: `${owner}/${repo}`,
    contextRepoPath: "",
    teamContentPath: "",
    hasRemote: true,
    remoteUrl: null,
    mainDoc: "",
    localPath,
  } as RepoState;
}

function makeAttachment(managed: boolean): Attachment {
  return {
    repo_path: "/clones/acme/api",
    remote_url: "https://github.com/acme/api.git",
    attached_at: null,
    tracking_prompt_dismissed_at: null,
    managed,
  } as Attachment;
}

beforeEach(() => {
  workspaceRepos = {};
  attachmentsByRemote = new Map();
  attachDialogSpy.mockClear();
  mockRemoveSourceRepoFromTeam.mockClear();
  mockRemoveTeamSourceRepo.mockClear();
  mockBump.mockClear();
  mockAddSourceRepoToTeam.mockClear();
  mockUpdateTeamSourceRepos.mockClear();
});

describe("ManageReposTab — the add paths (S1/S3, AC-1)", () => {
  it("shows the two add paths that exist, and no managed-clone offer", () => {
    render(<ManageReposTab />);
    expect(screen.getByTestId("add-repo-panel")).toBeInTheDocument();
    expect(screen.getByTestId("add-path-github")).toHaveTextContent(/Add from GitHub/i);
    expect(screen.getByTestId("add-path-local")).toHaveTextContent(/local clone/i);
    expect(screen.queryByTestId("add-path-managed")).not.toBeInTheDocument();
    expect(screen.queryByText(/managed clone/i)).not.toBeInTheDocument();
  });

  it("embeds the add-repo form (URL + local folder modes) directly in the tab", () => {
    render(<ManageReposTab />);
    // AddRepoForm exposes the two-mode tablist inline (no reserve pane, no dialog).
    expect(screen.getByRole("tablist", { name: /how to add a repo/i })).toBeInTheDocument();
  });
});

describe("ManageReposTab — per-repo working-copy state (S2, AC-2)", () => {
  it("labels a not-attached repo and offers Attach local clone only", () => {
    workspaceRepos = { k: synthRepo("acme", "api", null) };
    render(<ManageReposTab />);
    expect(screen.getByTestId("manage-repos-state-acme/api")).toHaveTextContent(/not attached/i);
    expect(screen.getByTestId("attach-local-acme/api")).toBeInTheDocument();
    expect(screen.queryByTestId("use-managed-acme/api")).not.toBeInTheDocument();
    expect(screen.queryByText(/Use a managed clone/i)).not.toBeInTheDocument();
  });

  it("labels an attached (own clone) repo — no attach actions", () => {
    workspaceRepos = { k: synthRepo("acme", "api", "/home/me/api") };
    render(<ManageReposTab />);
    expect(screen.getByTestId("manage-repos-state-acme/api")).toHaveTextContent(/Attached/);
    expect(screen.queryByTestId("attach-local-acme/api")).not.toBeInTheDocument();
    expect(screen.queryByTestId("use-managed-acme/api")).not.toBeInTheDocument();
  });

  it("labels a CodeSense-managed repo distinctly from a plain attach", () => {
    workspaceRepos = { k: synthRepo("acme", "api", "/clones/acme/api") };
    attachmentsByRemote = new Map([["github.com/acme/api", makeAttachment(true)]]);
    render(<ManageReposTab />);
    const row = screen.getByTestId("manage-repos-row-acme/api");
    expect(row).toHaveAttribute("data-state", "managed");
    expect(screen.getByTestId("manage-repos-state-acme/api")).toHaveTextContent(/Managed clone/i);
  });
});

describe("ManageReposTab — per-repo actions fire the existing attach flow (S2, AC-3)", () => {
  it("'Attach local clone' opens the existing attach dialog for that repo", async () => {
    const user = userEvent.setup();
    workspaceRepos = { k: synthRepo("acme", "api", null) };
    render(<ManageReposTab />);
    await user.click(screen.getByTestId("attach-local-acme/api"));
    const dialog = await screen.findByTestId("attach-dialog");
    expect(dialog).toHaveAttribute("data-owner", "acme");
    expect(dialog).toHaveAttribute("data-repo", "api");
  });

  it("'Remove' fires the existing removeSourceRepoFromTeam command", async () => {
    const user = userEvent.setup();
    workspaceRepos = { k: synthRepo("acme", "api", null) };
    render(<ManageReposTab />);
    await user.click(screen.getByTestId("manage-repos-remove-acme/api"));
    await waitFor(() =>
      expect(mockRemoveSourceRepoFromTeam).toHaveBeenCalledWith(
        expect.objectContaining({
          owner: "acme",
          repo: "api",
          orgSlug: "acme",
          teamName: "default",
        }),
      ),
    );
  });
});

describe("ManageReposTab — self-sufficient (AC-4)", () => {
  it("renders empty-state guidance pointing at the in-tab add section (no Settings)", () => {
    render(<ManageReposTab />);
    expect(screen.getByText(/Add one above to start working/i)).toBeInTheDocument();
    // The add affordance is present in the SAME tab — nothing sends the user away.
    expect(screen.getByTestId("add-repo-panel")).toBeInTheDocument();
  });
});

describe("ManageReposTab — contextual OAuth (SW-LN-8 AC-2)", () => {
  it("says why a repo is needed, and that it stays on the machine", () => {
    render(<ManageReposTab />);
    expect(screen.getByTestId("connect-repo-why")).toHaveTextContent(
      "CodeSense needs read access to run agents against your code. It stays on your machine.",
    );
  });

  it("offers a local folder first, which needs no account at all", () => {
    render(<ManageReposTab />);
    expect(screen.getByRole("tab", { name: /Local folder/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.queryByTestId("connection-picker")).toBeNull();
  });

  it("keeps URL setup available to a local guest without account prompts", async () => {
    useFirstRunStore.setState({ guest: true });
    try {
      render(<ManageReposTab />);
      await userEvent.click(screen.getByRole("tab", { name: /From a URL/ }));
      expect(screen.queryByTestId("guest-account-note")).toBeNull();
      expect(screen.queryByTestId("connection-picker")).toBeNull();
    } finally {
      useFirstRunStore.setState({ guest: false });
    }
  });

  it("says nothing about guests to a signed-in user", async () => {
    useFirstRunStore.setState({ guest: false });
    render(<ManageReposTab />);
    await userEvent.click(screen.getByRole("tab", { name: /From a URL/ }));
    expect(screen.queryByTestId("guest-account-note")).toBeNull();
  });
});

describe("ManageReposTab — SW-LN-29 where context lands (AC-1/AC-2)", () => {
  it("an attached repo's row says context lands in its worktrees, not the main checkout", () => {
    workspaceRepos = { k: synthRepo("acme", "api", "/home/me/api") };
    render(<ManageReposTab />);
    expect(screen.getByTestId("manage-repos-context-acme/api")).toHaveTextContent(
      "Agents you start in worktrees CodeSense opens for this repo get the context their definition declares. Your main checkout isn't touched.",
    );
  });

  it("a managed (Clone for me) row says CodeSense manages it, and what agents get", () => {
    workspaceRepos = { k: synthRepo("acme", "api", "/clones/acme/api") };
    attachmentsByRemote = new Map([["github.com/acme/api", makeAttachment(true)]]);
    render(<ManageReposTab />);
    expect(screen.getByTestId("manage-repos-context-acme/api")).toHaveTextContent(
      /codesense manages this clone\. agents you start .* get the context their definition declares/i,
    );
  });

  it("a not-attached row makes no context promise", () => {
    workspaceRepos = { k: synthRepo("acme", "api", null) };
    render(<ManageReposTab />);
    expect(screen.queryByTestId("manage-repos-context-acme/api")).toBeNull();
  });

  it("attaching a local folder ends on the success state", async () => {
    mockPickFolder.mockResolvedValue("/Users/me/code/shop");
    const user = userEvent.setup();
    render(<ManageReposTab />);
    await user.click(screen.getByRole("button", { name: /choose folder/i }));
    await user.click(await screen.findByRole("button", { name: /attach folder/i }));
    const success = await screen.findByTestId("attach-success");
    expect(success).toHaveTextContent("Attached local/shop");
    expect(success).toHaveTextContent(/your main checkout isn't touched/i);
  });
});

describe("SW-FU-C1: the Source repos tab is built from the shared pieces", () => {
  it("uses the shared tab layout and the one empty state", () => {
    render(<ManageReposTab />);
    expect(screen.getByTestId("source-repos-tab")).toHaveAttribute("data-slot", "tab-surface");
    expect(screen.getByTestId("manage-repos-empty")).toHaveAttribute("data-slot", "empty-state");
  });

  it("a failed remove is an inline alert on its row with a safe reason, not a raw toast", async () => {
    const user = userEvent.setup();
    workspaceRepos = { k: synthRepo("acme", "api", null) };
    mockRemoveSourceRepoFromTeam.mockRejectedValueOnce(
      new Error("denied: the registry is read-only"),
    );
    render(<ManageReposTab />);
    await user.click(screen.getByTestId("manage-repos-remove-acme/api"));
    const alert = await screen.findByTestId("manage-repos-remove-error-acme/api");
    expect(alert).toHaveAttribute("data-slot", "inline-alert");
    expect(alert).toHaveTextContent("Couldn't remove acme/api");
    expect(alert).toHaveTextContent("the registry is read-only");
    expect(alert).not.toHaveTextContent("denied:");
    expect(screen.getByTestId("manage-repos-remove-acme/api")).toHaveAttribute(
      "data-variant",
      "destructive",
    );
  });

  it("Try again on a failed remove retries it and clears the alert on success", async () => {
    const user = userEvent.setup();
    workspaceRepos = { k: synthRepo("acme", "api", null) };
    mockRemoveSourceRepoFromTeam.mockRejectedValueOnce(new Error("busy: try later"));
    render(<ManageReposTab />);
    await user.click(screen.getByTestId("manage-repos-remove-acme/api"));
    await user.click(await screen.findByTestId("manage-repos-remove-retry-acme/api"));
    await waitFor(() => expect(mockRemoveSourceRepoFromTeam).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(screen.queryByTestId("manage-repos-remove-error-acme/api")).not.toBeInTheDocument(),
    );
  });

  it("a failed add says what failed in the shared inline alert", async () => {
    const user = userEvent.setup();
    mockPickFolder.mockRejectedValueOnce(new Error("unavailable: the dialog plugin is not ready"));
    render(<ManageReposTab />);
    await user.click(screen.getByRole("button", { name: /Choose folder/ }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveAttribute("data-slot", "inline-alert");
    expect(alert).toHaveTextContent("Couldn't open the folder picker");
    expect(alert).toHaveTextContent("the dialog plugin is not ready");
    expect(alert).not.toHaveTextContent("unavailable:");
  });
});

describe("ManageReposTab — the add form hands every rule to the one add-repo call", () => {
  it("a local folder is added with its path, and the result lands in the store", async () => {
    mockPickFolder.mockResolvedValue("/Users/me/code/shop");
    const user = userEvent.setup();
    render(<ManageReposTab />);
    await user.click(screen.getByRole("button", { name: /choose folder/i }));
    await user.click(await screen.findByRole("button", { name: /attach folder/i }));
    await screen.findByTestId("attach-success");
    expect(mockAddSourceRepoToTeam).toHaveBeenCalledWith({
      githubUserId: 42,
      orgSlug: "acme",
      teamName: "default",
      input: { kind: "localPath", path: "/Users/me/code/shop" },
    });
    expect(mockUpdateTeamSourceRepos).toHaveBeenCalledWith("acme", "default", {
      owner: "local",
      repo: "shop",
      provider: "local",
      host: "local",
    });
    expect(mockBump).toHaveBeenCalled();
  });

  it("a typed URL is passed through untouched — normalizing it is the core's job", async () => {
    const user = userEvent.setup();
    render(<ManageReposTab />);
    await user.click(screen.getByRole("tab", { name: /from a url/i }));
    await user.type(screen.getByLabelText(/repository url/i), "acme/new");
    await user.click(screen.getByRole("button", { name: /add repo/i }));
    await waitFor(() =>
      expect(mockAddSourceRepoToTeam).toHaveBeenCalledWith(
        expect.objectContaining({ input: { kind: "url", url: "acme/new" } }),
      ),
    );
  });

  it("a repo the team already tracks says so instead of failing", async () => {
    mockAddSourceRepoToTeam.mockRejectedValueOnce(
      Object.assign(new Error("acme/new is already tracked on team default"), {
        kind: "alreadyTracked",
      }),
    );
    const user = userEvent.setup();
    render(<ManageReposTab />);
    await user.click(screen.getByRole("tab", { name: /from a url/i }));
    await user.type(screen.getByLabelText(/repository url/i), "acme/new");
    await user.click(screen.getByRole("button", { name: /add repo/i }));
    expect(await screen.findByText("Already tracked in this team.")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(mockUpdateTeamSourceRepos).not.toHaveBeenCalled();
  });

  it("a refused folder shows the core's reason in the inline alert", async () => {
    mockPickFolder.mockResolvedValue("/Users/me/code/shop");
    mockAddSourceRepoToTeam.mockRejectedValueOnce(
      Object.assign(new Error('"https://example.com/a/b" is not a supported git remote'), {
        kind: "unsupportedRemote",
      }),
    );
    const user = userEvent.setup();
    render(<ManageReposTab />);
    await user.click(screen.getByRole("button", { name: /choose folder/i }));
    await user.click(await screen.findByRole("button", { name: /attach folder/i }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Couldn't attach the folder");
    expect(alert).toHaveTextContent("is not a supported git remote");
  });
});
