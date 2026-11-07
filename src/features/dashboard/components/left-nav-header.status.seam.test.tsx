// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import type { AgentContextPivot as PivotState } from "@/features/agent-context/hooks/use-agent-context";
import { resetContextGitStoreForTests } from "@/features/git-sync/store/use-context-git-store";
import type { RepoState } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { LeftNavHeader } from "./left-nav-header";

// The status button driven from the real read: the one source hook calls the real command
// wrappers (answered here with the shapes the Rust commands serialize), and the button, its
// sentence and its cards come from that answer. Settling a conflict round-trips: the resolve
// command runs, the status is read again, and the conflict is gone from the button.

const TEAM = "acme:default";

const cmd = vi.hoisted(() => ({
  contextRepoStatus: vi.fn(),
  getPublishSet: vi.fn(),
  resolveContextConflict: vi.fn(),
  handContextConflictToAgent: vi.fn(),
  runContextSyncAction: vi.fn(),
}));

vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, ...cmd, saveAppState: vi.fn().mockResolvedValue(undefined) };
});
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@/shared/lib/pipeline-graph", () => ({ useTeamRepoGlyphMap: () => new Map() }));
vi.mock("@/shared/hooks/use-avatar-packs", () => ({
  useAvatarPacks: () => ({ faces: undefined }),
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

/** What `context_repo_status` serializes: main, one commit ahead, a merge stopped on a
 *  conflict whose last writer here was an agent. */
const rustStatus = (conflicts: boolean) => ({
  branch: "main",
  headSha: "5a7a83a",
  hasUpstream: true,
  upstream: "origin/main",
  remoteProvider: "github",
  ahead: 1,
  behind: 2,
  sync: { state: "ok", lastSyncedAt: Date.now() - 3 * 60_000, noSender: false },
  conflicts: conflicts
    ? [
        {
          path: "default/content/roadmap.html",
          mine: {
            who: "claude",
            subject: "Add the Q4 table",
            at: Date.now() - 40 * 60_000,
            deleted: false,
          },
          theirs: {
            who: "maya",
            subject: "Move the table",
            at: Date.now() - 20 * 60_000,
            deleted: false,
          },
          lastWriter: "claude",
          heldBack: ["default/content/team.yaml"],
        },
      ]
    : [],
});

const PUBLISH_SET = {
  buckets: [],
  general: {
    count: 2,
    files: [
      { path: "default/content/a.md", status: "modified", added: 2, removed: 1 },
      { path: "default/content/b.md", status: "added", added: 4 },
    ],
  },
};

function pivot(): PivotState {
  return {
    agents: [{ key: "acme/default/claude", name: "claude", avatar: null }],
    repos: [],
    agent: null,
    repo: null,
    select: vi.fn(),
    view: null,
    loading: false,
    error: null,
    reload: vi.fn(),
    membership: () => ({ state: "out" }),
    plan: () => null,
    edit: vi.fn(),
  };
}

const button = () => screen.getByTestId("git-status-button");

beforeEach(() => {
  resetContextGitStoreForTests();
  vi.clearAllMocks();
  useOrgStore.setState({
    orgs: [],
    activeOrgSlug: "acme",
    activeTeamName: "default",
    workspaceRepos: {
      "/checkout": {
        repoPath: "/checkout",
        contextRepoPath: "/org",
        teamContentPath: "/org/default/content",
      } as unknown as RepoState,
    },
  });
  useViewerStore.setState({ navLens: {}, openTabs: {}, activeTab: {}, tabDiff: {} });
  cmd.getPublishSet.mockResolvedValue(PUBLISH_SET);
  cmd.contextRepoStatus.mockResolvedValue(rustStatus(true));
  cmd.resolveContextConflict.mockResolvedValue(undefined);
  cmd.handContextConflictToAgent.mockResolvedValue({ status: "sent", messageId: 7 });
});

describe("the status button, from the real status read", () => {
  it("shows the branch, ↑ edits plus commits, ↓ GitHub's, and the conflict count", async () => {
    render(<LeftNavHeader teamKey={TEAM} navRepoPath="/checkout" pivot={pivot()} />);
    await waitFor(() => expect(screen.getByTestId("git-conflicts")).toHaveTextContent("1"));
    expect(cmd.contextRepoStatus).toHaveBeenCalledWith("/org");
    // 2 changed files in the team + 1 commit not pushed.
    expect(screen.getByTestId("git-up")).toHaveTextContent("3");
    expect(screen.getByTestId("git-down")).toHaveTextContent("2");
    expect(button().getAttribute("aria-label")).toContain(
      "3 changes not on GitHub (2 uncommitted edits + 1 commit not pushed); 2 new on GitHub; 1 conflict to settle",
    );
  });

  it("settling a conflict resolves it, reads the status again, and the count goes", async () => {
    render(<LeftNavHeader teamKey={TEAM} navRepoPath="/checkout" pivot={pivot()} />);
    await waitFor(() => expect(screen.getByTestId("git-conflicts")).toBeInTheDocument());
    fireEvent.click(button());
    const card = screen.getByTestId("git-conflict-card");
    expect(card).toHaveTextContent(
      "claude: Add the Q4 table, 40m ago. maya: Move the table, 20m ago.",
    );
    expect(card).toHaveTextContent("Held back: 1 change from GitHub (team.yaml)");
    cmd.contextRepoStatus.mockResolvedValue(rustStatus(false));
    fireEvent.click(within(card).getByRole("button", { name: "Use theirs" }));
    expect(cmd.resolveContextConflict).toHaveBeenCalledWith(
      "/org",
      "default/content/roadmap.html",
      "useTheirs",
    );
    await waitFor(() => expect(screen.queryByTestId("git-conflicts")).toBeNull());
    expect(screen.queryByTestId("git-conflict-card")).toBeNull();
  });

  it("hands a conflict to its last writer through the inbox", async () => {
    render(<LeftNavHeader teamKey={TEAM} navRepoPath="/checkout" pivot={pivot()} />);
    await waitFor(() => expect(screen.getByTestId("git-conflicts")).toBeInTheDocument());
    fireEvent.click(button());
    fireEvent.click(screen.getByRole("button", { name: "Hand to claude" }));
    expect(cmd.handContextConflictToAgent).toHaveBeenCalledWith(
      { org: "acme", team: "default" },
      "/org",
      "default/content/roadmap.html",
      "claude",
    );
  });

  it("a failure the git state shows opens its card", async () => {
    cmd.contextRepoStatus.mockResolvedValue({
      ...rustStatus(false),
      sync: {
        state: "failed",
        lastSyncedAt: Date.now() - 47 * 60_000,
        reason: "Rebasing 3 of 7 stopped partway",
        noSender: false,
      },
    });
    render(<LeftNavHeader teamKey={TEAM} navRepoPath="/checkout" pivot={pivot()} />);
    await waitFor(() => expect(button()).toHaveAttribute("data-state-kind", "failed"));
    fireEvent.click(button());
    expect(screen.getByTestId("git-failure-card")).toHaveTextContent(
      "Rebasing 3 of 7 stopped partway. Last synced 47m ago.",
    );
  });

  it("an org kept on this Mac with nothing changed opens Changes, not a '0 not sent' card", async () => {
    cmd.getPublishSet.mockResolvedValue({ buckets: [], general: { count: 0, files: [] } });
    cmd.contextRepoStatus.mockResolvedValue({
      ...rustStatus(false),
      hasUpstream: false,
      upstream: null,
      ahead: 0,
      behind: 0,
      sync: { state: "ok", lastSyncedAt: null, noSender: true },
    });
    render(<LeftNavHeader teamKey={TEAM} navRepoPath="/checkout" pivot={pivot()} />);
    await waitFor(() => expect(cmd.contextRepoStatus).toHaveBeenCalled());
    await waitFor(() => expect(button()).toHaveAttribute("data-state-kind", "ok"));
    expect(button().getAttribute("aria-label")).not.toContain("will send");
    fireEvent.click(button());
    expect(screen.queryByTestId("git-status-card")).toBeNull();
    expect(useViewerStore.getState().navLens[TEAM]).toMatchObject({ view: "changes" });
  });

  it("changes with no remote to send them open the Set up card, which links to Changes", async () => {
    cmd.contextRepoStatus.mockResolvedValue({
      ...rustStatus(false),
      hasUpstream: false,
      upstream: null,
      ahead: 0,
      behind: 0,
      sync: { state: "ok", lastSyncedAt: null, noSender: true },
    });
    render(<LeftNavHeader teamKey={TEAM} navRepoPath="/checkout" pivot={pivot()} />);
    await waitFor(() =>
      expect(button().getAttribute("aria-label")).toContain(
        "nothing on this Mac will send these changes",
      ),
    );
    fireEvent.click(button());
    expect(screen.getByTestId("git-failure-card")).toHaveTextContent("2 not sent");
    fireEvent.click(screen.getByRole("button", { name: "Show changes" }));
    expect(useViewerStore.getState().navLens[TEAM]).toMatchObject({ view: "changes" });
    expect(screen.queryByTestId("git-status-card")).toBeNull();
  });

  it("the button closes the card it opened", async () => {
    render(<LeftNavHeader teamKey={TEAM} navRepoPath="/checkout" pivot={pivot()} />);
    await waitFor(() => expect(screen.getByTestId("git-conflicts")).toBeInTheDocument());
    fireEvent.click(button());
    expect(screen.getByTestId("git-status-card")).toBeInTheDocument();
    // The card listens for outside presses from the next tick on.
    await act(() => new Promise((r) => setTimeout(r, 10)));
    // A real press: pointerdown first (Radix's outside-click check), then the click.
    fireEvent.pointerDown(button());
    fireEvent.mouseDown(button());
    fireEvent.pointerUp(button());
    fireEvent.mouseUp(button());
    fireEvent.click(button());
    await waitFor(() => expect(screen.queryByTestId("git-status-card")).toBeNull());
  });

  it("once settled, the next conflict waits for a click instead of opening the card itself", async () => {
    render(<LeftNavHeader teamKey={TEAM} navRepoPath="/checkout" pivot={pivot()} />);
    await waitFor(() => expect(screen.getByTestId("git-conflicts")).toBeInTheDocument());
    fireEvent.click(button());
    cmd.contextRepoStatus.mockResolvedValue(rustStatus(false));
    fireEvent.click(screen.getByRole("button", { name: "Use theirs" }));
    await waitFor(() => expect(screen.queryByTestId("git-conflicts")).toBeNull());
    expect(screen.queryByTestId("git-status-card")).toBeNull();
    // An agent's next pull stops on another conflict.
    cmd.contextRepoStatus.mockResolvedValue(rustStatus(true));
    act(() => {
      window.dispatchEvent(new Event("online"));
    });
    await waitFor(() => expect(screen.getByTestId("git-conflicts")).toBeInTheDocument());
    expect(screen.queryByTestId("git-status-card")).toBeNull();
  });
});

it("private source reads the scratch root and never opens the no-remote setup card", async () => {
  useViewerStore.getState().setNavLens(TEAM, { source: { kind: "private" } });
  cmd.contextRepoStatus.mockResolvedValue({
    ...rustStatus(false),
    hasUpstream: false,
    ahead: 4,
    sync: { state: "ok", noSender: true },
  });
  render(
    <LeftNavHeader
      teamKey={TEAM}
      navRepoPath="/checkout"
      pivot={pivot()}
      resolvedSource={{
        cloneRoot: "/clone",
        source: { kind: "private" },
        content: {
          repoPath: "/scratch/orgs/acme/ctx/content",
          repoName: "Private",
          teamContentPath: "/scratch/orgs/acme/ctx/content",
          contextRepoPath: "/scratch",
          mainDoc: "",
        },
        orgRoot: "/scratch",
        prefix: "orgs/acme/ctx/content",
        privateSource: null,
        note: "",
        retryBaseline: () => {},
      }}
    />,
  );
  await waitFor(() => expect(cmd.contextRepoStatus).toHaveBeenCalledWith("/scratch"));
  expect(button()).toHaveTextContent("Private");
  expect(button()).toHaveTextContent("local only");
  fireEvent.click(button());
  expect(screen.queryByRole("button", { name: "Set up" })).toBeNull();
  expect(screen.queryByTestId("git-status-card")).toBeNull();
  expect(useViewerStore.getState().navLens[TEAM].view).toBe("changes");
});
