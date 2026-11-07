// SPDX-License-Identifier: MIT
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentContextPivot as PivotState } from "@/features/agent-context/hooks/use-agent-context";
import type { NavChange } from "@/features/git-sync/lib/nav-changes";
import {
  resetContextGitStoreForTests,
  useContextGitStore,
} from "@/features/git-sync/store/use-context-git-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { Org } from "@/shared/types/domain";
import type { ContextRepoStatus } from "@/tauri/commands";

// The source hook reads the context repo; here the store is seeded directly instead.
vi.mock("@/features/git-sync/lib/use-context-git-source", () => ({
  useContextGitSource: () => ({ refreshStatus: () => {} }),
}));
const cmd = vi.hoisted(() => ({
  resolveContextConflict: vi.fn().mockResolvedValue(undefined),
  handContextConflictToAgent: vi.fn().mockResolvedValue({ status: "sent", messageId: 1 }),
  runContextSyncAction: vi.fn().mockResolvedValue({ status: "sent", messageId: 2 }),
  contextRepoStatus: vi.fn(),
}));
vi.mock("@/shared/lib/pipeline-graph", () => ({
  useTeamRepoGlyphMap: () => new Map(),
}));
vi.mock("@/shared/hooks/use-avatar-packs", () => ({
  useAvatarPacks: () => ({ faces: undefined }),
}));
vi.mock("@/tauri/commands", () => ({
  ...cmd,
  saveAppState: vi.fn().mockResolvedValue(undefined),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

import { LeftNavHeader } from "./left-nav-header";

const TEAM_KEY = "acme:platform";
const BARB = { key: "acme/platform/barb", name: "Barb", avatar: null };
const NOW = Date.now();

function org(): Org {
  return {
    slug: "acme",
    display_name: "Acme Widgets",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {} as Record<string, never>,
    people: [],
    teams: [{ name: "platform", source_repos: [] }],
    org_root: "",
  };
}

function pivot(over: Partial<PivotState> = {}): PivotState {
  return {
    agents: [BARB],
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
    ...over,
  };
}

function status(over: Partial<ContextRepoStatus> = {}): ContextRepoStatus {
  return {
    branch: "main",
    headSha: "394fa6d",
    hasUpstream: true,
    upstream: "origin/main",
    remoteProvider: "github",
    ahead: 0,
    behind: 0,
    sync: { state: "ok", lastSyncedAt: NOW - 120_000 },
    conflicts: [],
    ...over,
  };
}

const changed = (n: number): NavChange[] =>
  Array.from({ length: n }, (_, i) => ({
    absPath: `/org/c/f${i}.md`,
    relPath: `c/f${i}.md`,
    kind: "modified" as const,
  }));

function seed(s: ContextRepoStatus | null, changes: NavChange[] = []) {
  useContextGitStore.getState().put(TEAM_KEY, {
    orgRoot: "/org",
    changes,
    byPath: new Map(changes.map((c) => [c.absPath, c])),
    status: s,
    loading: false,
    historyMoves: 0,
  });
}

function renderHeader(p: PivotState = pivot()) {
  return render(<LeftNavHeader teamKey={TEAM_KEY} navRepoPath="/checkout" pivot={p} />);
}

const button = () => screen.getByTestId("git-status-button");

beforeEach(() => {
  resetContextGitStoreForTests();
  useOrgStore.setState({
    orgs: [org()],
    activeOrgSlug: "acme",
    activeTeamName: "platform",
    shownTeamsByOrgSlug: {},
    stateLoaded: true,
    hasUserIntent: false,
  });
  useViewerStore.setState({ navLens: {}, openTabs: {}, activeTab: {}, tabDiff: {} });
  vi.clearAllMocks();
});

describe("LeftNavHeader", () => {
  it("never restates the org/team identity the title bar owns", () => {
    seed(status());
    renderHeader();
    const row = screen.getByTestId("left-nav-scope-row");
    expect(row).not.toHaveTextContent("Acme Widgets");
    expect(within(row).getByTestId("agent-context-trigger")).toHaveTextContent("Team (everything)");
  });

  it("shows no status button until the context repo's status is read", () => {
    seed(null);
    renderHeader();
    expect(screen.queryByTestId("git-status-button")).toBeNull();
  });

  it("hides the scope picker when there is no active team", () => {
    useOrgStore.setState({ activeOrgSlug: null });
    seed(status());
    renderHeader();
    expect(screen.queryByTestId("pipeline-nav-projection-header")).toBeNull();
    expect(button()).toBeInTheDocument();
  });
});

describe("the status button", () => {
  it("clean: the branch, then 'synced'; the full sentence is its name and tooltip", () => {
    seed(status());
    renderHeader();
    expect(button()).toHaveTextContent("mainsynced");
    expect(button()).toHaveAccessibleName("On main; last synced 2m ago.");
    expect(button()).toHaveAttribute("title", "On main; last synced 2m ago.");
  });

  it("↑ is uncommitted edits plus unpushed commits, ↓ is new on GitHub", () => {
    seed(status({ ahead: 2, behind: 3 }), changed(5));
    renderHeader();
    expect(screen.getByTestId("git-up")).toHaveTextContent("7");
    expect(screen.getByTestId("git-down")).toHaveTextContent("3");
    expect(button()).toHaveAccessibleName(
      "On main; 7 changes not on GitHub (5 uncommitted edits + 2 commits not pushed); 3 new on GitHub; last synced 2m ago.",
    );
  });

  it("in an agent's context ↑ counts only the changes that agent gets", () => {
    seed(status(), changed(5));
    render(
      <LeftNavHeader
        teamKey={TEAM_KEY}
        navRepoPath="/checkout"
        pivot={pivot({ agent: BARB })}
        agentContext={{
          agentName: "Barb",
          repoName: null,
          relOf: (abs) => abs.slice("/org/c/".length),
          editRelOf: () => null,
          defaulted: false,
          membership: (rel) => ({ state: rel === "f0.md" || rel === "f1.md" ? "in" : "out" }),
          plan: () => null,
          edit: () => {},
        }}
      />,
    );
    expect(screen.getByTestId("git-up")).toHaveTextContent("2");
  });

  it.each([
    [
      "branch",
      status({ branch: "gs/_bmad-output-d735ccb" }),
      "On gs/_bmad-output-d735ccb, not main",
    ],
    ["detached", status({ branch: null }), "On no branch (detached at 394fa6d)"],
  ])("draws a %s branch differently and says so", (look, s, words) => {
    seed(s);
    renderHeader();
    expect(screen.getByTestId("git-status-branch")).toHaveAttribute("data-look", look);
    expect(button().getAttribute("aria-label")).toContain(words);
    // A detached HEAD never claims "synced".
    if (look === "detached") expect(button()).not.toHaveTextContent("synced");
  });

  it.each([
    ["failed", "failed", "sync failed"],
    ["offline", "offline", "offline"],
    ["signin", "sign in", "signed out of GitHub"],
  ] as const)("the %s state takes an icon and a short word", (state, word, sentence) => {
    seed(status({ sync: { state, lastSyncedAt: NOW - 120_000 } }), changed(3));
    renderHeader();
    expect(button()).toHaveAttribute("data-state-kind", state);
    expect(button()).toHaveTextContent(word);
    expect(button().getAttribute("aria-label")).toContain(sentence);
  });

  it("retrying shows its attempt; syncing a spinner; neither opens a card", () => {
    seed(status({ sync: { state: "retrying", lastSyncedAt: null, attempt: 2, maxAttempts: 5 } }));
    renderHeader();
    expect(button()).toHaveTextContent("2/5");
    fireEvent.click(button());
    expect(screen.queryByTestId("git-status-card")).toBeNull();
    expect(useViewerStore.getState().navLens[TEAM_KEY]?.view).toBe("changes");
  });

  it("a click with nothing to decide opens the Changes view", () => {
    seed(status(), changed(2));
    renderHeader();
    fireEvent.click(button());
    expect(useViewerStore.getState().navLens[TEAM_KEY]?.view).toBe("changes");
  });

  it("a failure opens its card, which says why and asks an agent to retry", () => {
    seed(
      status({
        sync: { state: "failed", lastSyncedAt: NOW - 47 * 60_000, reason: "Push refused" },
      }),
      changed(3),
    );
    renderHeader();
    fireEvent.click(button());
    const card = screen.getByTestId("git-failure-card");
    expect(card).toHaveTextContent("Sync failed");
    expect(card).toHaveTextContent(
      "Push refused. Last synced 47m ago. Your 3 changes are safe here.",
    );
    expect(card).toHaveTextContent("Asks Barb to find out why and sync again.");
    fireEvent.click(within(card).getByRole("button", { name: "Retry" }));
    expect(cmd.runContextSyncAction).toHaveBeenCalledWith(
      { org: "acme", team: "platform" },
      "/org",
      "retry",
      "Barb",
      "Push refused",
    );
    expect(useViewerStore.getState().navLens[TEAM_KEY]?.view).toBeUndefined();
  });

  it("signed out offers Sign in again; no sender offers Set up", () => {
    seed(status({ sync: { state: "signin", lastSyncedAt: null } }));
    const { unmount } = renderHeader();
    fireEvent.click(button());
    fireEvent.click(screen.getByRole("button", { name: "Sign in again" }));
    expect(cmd.runContextSyncAction).toHaveBeenCalledWith(
      { org: "acme", team: "platform" },
      "/org",
      "signIn",
      "Barb",
      null,
    );
    unmount();
    seed(status({ sync: { state: "ok", lastSyncedAt: null, noSender: true } }), changed(4));
    renderHeader();
    fireEvent.click(button());
    expect(screen.getByTestId("git-failure-card")).toHaveTextContent("4 not sent");
    fireEvent.click(screen.getByRole("button", { name: "Set up" }));
    expect(cmd.runContextSyncAction).toHaveBeenLastCalledWith(
      { org: "acme", team: "platform" },
      "/org",
      "setUp",
      "Barb",
      null,
    );
  });

  it("worktree conflicts and Git operations stay visible with only Compare and Hand actions", () => {
    const worktree = {
      id: "cw_merge",
      name: "Merge",
      path: "/merge",
      mergeBase: "abc1234",
      uncommittedPaths: ["c/a.md"],
    };
    seed(
      status({
        worktree,
        conflicts: [
          {
            path: "c/a.md",
            mine: { who: null, subject: null, at: null, deleted: false },
            theirs: { who: null, subject: null, at: null, deleted: false },
            lastWriter: null,
            heldBack: [],
          },
        ],
      }),
    );
    renderHeader();
    expect(screen.getByTestId("git-conflicts")).toHaveTextContent("1");
    expect(button()).toHaveAccessibleName(expect.stringContaining("1 conflict to settle"));
    fireEvent.click(button());
    const card = screen.getByTestId("git-status-card");
    expect(card).toHaveTextContent("1 conflict to settle");
    expect(
      within(card)
        .getAllByRole("button")
        .map((b) => b.textContent),
    ).toEqual(["Compare", "Hand to Barb"]);
    act(() =>
      seed(
        status({
          worktree,
          sync: { state: "syncing", progress: "Merging", lastSyncedAt: null },
        }),
      ),
    );
    expect(button()).toHaveAttribute("data-state-kind", "syncing");
    expect(card).toHaveTextContent("Merging");
    expect(cmd.runContextSyncAction).not.toHaveBeenCalled();
    expect(cmd.resolveContextConflict).not.toHaveBeenCalled();
  });

  it("a conflict opens the conflict card: settle it, compare it, or hand it to an agent", async () => {
    seed(
      status({
        conflicts: [
          {
            path: "c/roadmap.html",
            mine: { who: "claude", subject: "Q4 table", at: NOW - 40 * 60_000, deleted: false },
            theirs: {
              who: "maya",
              subject: "moved the table",
              at: NOW - 20 * 60_000,
              deleted: false,
            },
            lastWriter: "claude",
            heldBack: ["c/team.yaml", "c/metrics.csv"],
          },
        ],
      }),
    );
    renderHeader();
    expect(screen.getByTestId("git-conflicts")).toHaveTextContent("1");
    fireEvent.click(button());
    const card = screen.getByTestId("git-conflict-card");
    expect(card).toHaveTextContent("roadmap · changed here and on GitHub");
    expect(card).toHaveTextContent("claude: Q4 table, 40m ago. maya: moved the table, 20m ago.");
    expect(card).toHaveTextContent("Held back: 2 changes from GitHub (team.yaml, metrics.csv)");
    fireEvent.click(within(card).getByRole("button", { name: "Use mine" }));
    expect(cmd.resolveContextConflict).toHaveBeenCalledWith("/org", "c/roadmap.html", "useMine");
    // The card's buttons hold still while a decision is in flight.
    expect(within(card).getByRole("button", { name: "Use theirs" })).toBeDisabled();
    await act(async () => {});
    fireEvent.click(within(card).getByRole("button", { name: "Hand to claude" }));
    expect(cmd.handContextConflictToAgent).toHaveBeenCalledWith(
      { org: "acme", team: "platform" },
      "/org",
      "c/roadmap.html",
      "claude",
    );
    fireEvent.click(within(card).getByRole("button", { name: "Compare" }));
    const v = useViewerStore.getState();
    expect(v.activeTab[TEAM_KEY]).toBe("/org/c/roadmap.html");
    expect(v.tabDiff[TEAM_KEY]).toEqual({ "/org/c/roadmap.html": true });
  });
});
