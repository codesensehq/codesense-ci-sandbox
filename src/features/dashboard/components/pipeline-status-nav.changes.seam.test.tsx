// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, within } from "@/test-utils";

import { PipelineStatusNav } from "./pipeline-status-nav";
import { buildStoryRef, buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  resetContextGitStoreForTests,
  useContextGitStore,
} from "@/features/git-sync/store/use-context-git-store";
import type { NavChange } from "@/features/git-sync/lib/nav-changes";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";

// The Changes view and the Files view's tints, driven through the nav with the real stores:
// the changed files the context repo reports are exactly the rows Changes lists, each name
// tinted by its change, and a click opens the file's diff in an ordinary tab.

const TEAM = "acme:platform";

vi.mock("@/shared/lib/send-to-terminal", () => ({ sendTextToTerminal: vi.fn() }));
vi.mock("@/shared/lib/team-key", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/team-key")>();
  return { ...actual, useActiveTeamKey: () => TEAM, getActiveTeamKey: () => TEAM };
});
vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return {
    ...actual,
    listDirectory: () => Promise.resolve([]),
    getTeamNavPins: () => Promise.resolve([]),
  };
});
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

installTerminalLayoutHarness();

const story = (dir: string, id: string, status: string, title: string) => ({
  ...buildStoryRef(`${id}.md`, { absolutePath: `/ctx/${dir}/${id}.md`, status }),
  storyId: id,
  openTags: { title: [title] },
});

function tree() {
  return buildTree("root", {
    absoluteDirPath: "/ctx",
    docs: [{ fileName: "roadmap.html", filePath: "/ctx/roadmap.html" }],
    subEpics: [
      buildTree("26-inbox", {
        dirPath: "26-inbox",
        absoluteDirPath: "/ctx/26-inbox",
        mainDoc: { filePath: "/ctx/26-inbox/README.md", supportingDocs: null, purposeText: null },
        stories: [
          story("26-inbox", "AID-1", "done", "Inbox read model"),
          story("26-inbox", "AID-4", "in-progress", "Dashboards from the inbox"),
        ],
        docs: [{ fileName: "inbox-board.html", filePath: "/ctx/26-inbox/inbox-board.html" }],
      }),
      buildTree("27-nav", {
        dirPath: "27-nav",
        absoluteDirPath: "/ctx/27-nav",
        mainDoc: { filePath: "/ctx/27-nav/README.md", supportingDocs: null, purposeText: null },
        stories: [story("27-nav", "LN-1", "in-progress", "Story rows show agents")],
        docs: [{ fileName: "icons.md", filePath: "/ctx/27-nav/icons.md" }],
      }),
    ],
  });
}

const CHANGES: NavChange[] = [
  {
    absPath: "/ctx/26-inbox/AID-4.md",
    relPath: "c/26-inbox/AID-4.md",
    kind: "modified",
    added: 41,
    removed: 6,
  },
  {
    absPath: "/ctx/26-inbox/inbox-board.html",
    relPath: "c/26-inbox/inbox-board.html",
    kind: "added",
    added: 88,
  },
  { absPath: "/ctx/26-inbox/rail-mock.png", relPath: "c/26-inbox/rail-mock.png", kind: "deleted" },
  {
    absPath: "/ctx/27-nav/icons.md",
    relPath: "c/27-nav/icons.md",
    kind: "renamed",
    renamedFrom: "c/27-nav/icon-spec.md",
  },
  {
    absPath: "/ctx/repos/widgets/CLAUDE.md",
    relPath: "c/repos/widgets/CLAUDE.md",
    kind: "modified",
    added: 3,
    removed: 1,
  },
  { absPath: "/ctx/roadmap.html", relPath: "c/roadmap.html", kind: "conflict" },
];

function seedChanges(changes: NavChange[] = CHANGES) {
  useContextGitStore.getState().put(TEAM, {
    orgRoot: "/org",
    changes,
    byPath: new Map(changes.map((c) => [c.absPath, c])),
    status: null,
    loading: false,
    historyMoves: 0,
  });
}

function renderNav(agentContext?: NavAgentContext) {
  return render(
    <PipelineStatusNav
      tree={tree()}
      repoPath="/ctx"
      rootPath="/ctx"
      selectedPath={null}
      onFileSelect={() => {}}
      agentContext={agentContext}
    />,
  );
}

/** Every file row's path, in list order. */
const fileRows = () =>
  [...document.querySelectorAll<HTMLElement>("[data-filepath]")].map((el) => el.dataset.filepath);

beforeEach(() => {
  resetContextGitStoreForTests();
  useViewerStore.setState({
    workspaces: {},
    navLens: {},
    openTabs: {},
    activeTab: {},
    tabDiff: {},
    statusNavExpandedPaths: { [TEAM]: ["/ctx/26-inbox", "/ctx/27-nav"] },
  });
  seedChanges();
});

describe("the Changes view", () => {
  it("lists exactly the changed files, in their folders", () => {
    useViewerStore.getState().setNavLens(TEAM, { view: "changes" });
    renderNav();
    expect(fileRows().sort()).toEqual(CHANGES.map((c) => c.absPath).sort());
    // Unchanged files and stories are gone; their folders stay as the frame.
    expect(screen.queryByText("Inbox read model")).toBeNull();
    expect(screen.getByText("26-inbox")).toBeInTheDocument();
    // A chain of plain folders folds into one breadcrumb.
    expect(screen.getByText("repos › widgets")).toBeInTheDocument();
    // The filter field says which view this is, with its count.
    expect(screen.getByTestId("left-nav-changes-chip")).toHaveTextContent("Changes6");
  });

  it("tints each name by its change and says renames and conflicts in words", () => {
    useViewerStore.getState().setNavLens(TEAM, { view: "changes" });
    renderNav();
    const kindOf = (path: string) =>
      document
        .querySelector(`[data-filepath="${path}"] [data-change]`)
        ?.getAttribute("data-change");
    expect(kindOf("/ctx/26-inbox/AID-4.md")).toBe("modified");
    expect(kindOf("/ctx/26-inbox/inbox-board.html")).toBe("added");
    expect(kindOf("/ctx/26-inbox/rail-mock.png")).toBe("deleted");
    expect(kindOf("/ctx/roadmap.html")).toBe("conflict");
    const icons = document.querySelector<HTMLElement>('[data-filepath="/ctx/27-nav/icons.md"]')!;
    expect(within(icons).getByText("← icon-spec")).toBeInTheDocument();
    const roadmap = document.querySelector<HTMLElement>('[data-filepath="/ctx/roadmap.html"]')!;
    expect(within(roadmap).getByText("conflict")).toBeInTheDocument();
    // Line counts ride the Changes view.
    const aid4 = document.querySelector<HTMLElement>('[data-filepath="/ctx/26-inbox/AID-4.md"]')!;
    expect(within(aid4).getByTestId("nav-change-lines")).toHaveTextContent("+41 −6");
  });

  it("a click opens the file's diff in an ordinary tab; a double-click pins it", () => {
    useViewerStore.getState().setNavLens(TEAM, { view: "changes" });
    renderNav();
    fireEvent.click(screen.getByText("inbox-board"));
    let s = useViewerStore.getState();
    expect(s.activeTab[TEAM]).toBe("/ctx/26-inbox/inbox-board.html");
    expect(s.openTabs[TEAM]).toEqual([
      expect.objectContaining({ path: "/ctx/26-inbox/inbox-board.html", preview: true }),
    ]);
    expect(s.tabDiff[TEAM]).toEqual({ "/ctx/26-inbox/inbox-board.html": true });

    fireEvent.doubleClick(screen.getByText("Dashboards from the inbox"));
    s = useViewerStore.getState();
    expect(s.activeTab[TEAM]).toBe("/ctx/26-inbox/AID-4.md");
    expect(s.tabDiff[TEAM]?.["/ctx/26-inbox/AID-4.md"]).toBe(true);
  });

  it("says when GitHub has everything", () => {
    seedChanges([]);
    useViewerStore.getState().setNavLens(TEAM, { view: "changes" });
    renderNav();
    expect(screen.getByTestId("left-nav-empty")).toHaveTextContent("GitHub has everything here.");
    fireEvent.click(screen.getByText("Show all files"));
    expect(useViewerStore.getState().navLens[TEAM]?.view).toBe("files");
  });

  it("in an agent's context lists only what it gets, and says how much it leaves out", () => {
    const barb: NavAgentContext = {
      agentName: "Barb",
      repoName: null,
      relOf: (abs) => (abs.startsWith("/ctx/") ? abs.slice(5) : null),
      editRelOf: () => null,
      defaulted: false,
      membership: (rel) =>
        (rel.startsWith("26-inbox") ? { state: "in", tier: "team" } : { state: "out" }) as never,
      plan: () => null,
      edit: () => {},
    };
    useViewerStore.getState().setNavLens(TEAM, { view: "changes" });
    renderNav(barb);
    expect(fileRows().sort()).toEqual(
      [
        "/ctx/26-inbox/AID-4.md",
        "/ctx/26-inbox/inbox-board.html",
        "/ctx/26-inbox/rail-mock.png",
      ].sort(),
    );
    expect(screen.getByTestId("nav-section-heading")).toHaveTextContent("Team context · acme3");
    expect(screen.getByTestId("nav-section-note")).toHaveTextContent(
      "3 more changes are outside what Barb gets. Switch to Team to see them.",
    );
  });
});

describe("a background refresh of the Changes view", () => {
  it("keeps folded folders, scroll and the rows already there; a new change slots in", () => {
    useViewerStore.getState().setNavLens(TEAM, { view: "changes" });
    renderNav();
    fireEvent.click(screen.getByText("27-nav"));
    expect(fileRows()).not.toContain("/ctx/27-nav/icons.md");
    const aid4 = document.querySelector('[data-filepath="/ctx/26-inbox/AID-4.md"]');
    const list = aid4!.closest<HTMLElement>(".overflow-y-auto")!;
    list.scrollTop = 40;
    const order = fileRows();

    act(() =>
      seedChanges([
        ...CHANGES,
        { absPath: "/ctx/26-inbox/notes.md", relPath: "c/26-inbox/notes.md", kind: "added" },
      ]),
    );

    expect(fileRows()).toContain("/ctx/26-inbox/notes.md");
    expect(fileRows()).not.toContain("/ctx/27-nav/icons.md");
    expect(document.querySelector('[data-filepath="/ctx/26-inbox/AID-4.md"]')).toBe(aid4);
    expect(list.scrollTop).toBe(40);
    // Rows already listed keep their order; the new one takes its place among them.
    expect(fileRows().filter((p) => p !== "/ctx/26-inbox/notes.md")).toEqual(order);
  });
});

describe("the Files view", () => {
  it("tints changed names and leaves every other name in ink", () => {
    renderNav();
    expect(screen.getByText("Inbox read model")).not.toHaveAttribute("data-change");
    const aid4 = document.querySelector<HTMLElement>('[data-filepath="/ctx/26-inbox/AID-4.md"]')!;
    expect(within(aid4).getByTestId("nav-story-title")).toHaveAttribute("data-change", "modified");
    // Counts are the Changes view's; Files shows only the tint and the words.
    expect(within(aid4).queryByTestId("nav-change-lines")).toBeNull();
  });

  it("offers Files and Changes in the lens menu's View group", () => {
    renderNav();
    fireEvent.pointerDown(screen.getByTestId("left-nav-lens-button"), {
      button: 0,
      pointerType: "mouse",
    });
    const menu = screen.getByTestId("left-nav-lens-menu");
    expect(within(menu).getByTestId("left-nav-view-files")).toHaveAttribute("aria-checked", "true");
    fireEvent.click(within(menu).getByTestId("left-nav-view-changes"));
    expect(useViewerStore.getState().navLens[TEAM]?.view).toBe("changes");
  });
});

it("shows private Changes scoped by the source and returns to clone without stale private rows", () => {
  useViewerStore.getState().setNavLens(TEAM, { source: { kind: "private" }, view: "changes" });
  useContextGitStore.getState().put(TEAM, {
    source: { kind: "private" },
    orgRoot: "/scratch",
    prefix: "orgs/acme/ctx/content",
    changes: [
      {
        absPath: "/scratch/orgs/acme/ctx/content/secret.md",
        relPath: "orgs/acme/ctx/content/secret.md",
        kind: "modified",
        added: 1,
        removed: 1,
      },
    ],
    byPath: new Map(),
    status: null,
    loading: false,
    historyMoves: 0,
  });
  render(
    <PipelineStatusNav
      tree={buildTree("content", {
        absoluteDirPath: "/scratch/orgs/acme/ctx/content",
        docs: [{ fileName: "secret.md", filePath: "/scratch/orgs/acme/ctx/content/secret.md" }],
      })}
      repoPath="/ctx"
      rootPath="/scratch/orgs/acme/ctx/content"
      selectedPath={null}
      onFileSelect={() => {}}
    />,
  );
  expect(fileRows()).toContain("/scratch/orgs/acme/ctx/content/secret.md");
  fireEvent.click(screen.getByRole("button", { name: "Clear Private" }));
  expect(fileRows()).not.toContain("/scratch/orgs/acme/ctx/content/secret.md");
});

it("lists and opens worktree copies in Changes while Files keeps clone paths", () => {
  const changes = [
    {
      absPath: "/context-worktree/26-inbox/AID-4.md",
      relPath: "26-inbox/AID-4.md",
      kind: "modified" as const,
    },
    { absPath: "/context-worktree/new.md", relPath: "new.md", kind: "added" as const },
  ];
  useContextGitStore.getState().put(TEAM, {
    source: { kind: "worktree", id: "cw_sprint" },
    orgRoot: "/context-worktree",
    cloneRoot: "/ctx",
    changes,
    byPath: new Map(changes.map((c) => [c.absPath, c])),
    status: null,
    loading: false,
    historyMoves: 0,
  });
  useViewerStore
    .getState()
    .setNavLens(TEAM, { view: "changes", source: { kind: "worktree", id: "cw_sprint" } });
  renderNav();
  expect(fileRows().sort()).toEqual(changes.map((c) => c.absPath).sort());
  fireEvent.doubleClick(screen.getByText("Dashboards from the inbox"));
  expect(useViewerStore.getState().activeTab[TEAM]).toBe("/context-worktree/26-inbox/AID-4.md");
  act(() => useViewerStore.getState().setNavLens(TEAM, { view: "files" }));
  expect(fileRows()).toContain("/ctx/26-inbox/AID-4.md");
  expect(fileRows()).not.toContain("/context-worktree/new.md");
});

it("keeps the source heading and crew visible for a worktree with no changes", () => {
  seedChanges([]);
  const snapshot = useContextGitStore.getState().byTeam[TEAM]!;
  useContextGitStore.getState().put(TEAM, {
    ...snapshot,
    status: {
      worktree: { id: "cw_sprint", name: "Sprint", path: "/worktree" },
      uncommitted: 0,
      agents: [{ handle: "reviewer", name: "Reviewer" }],
    } as unknown as import("@/tauri/commands").ContextRepoStatus,
  });
  useViewerStore.getState().setNavLens(TEAM, { view: "changes" });
  renderNav();
  expect(screen.getByText("Sprint · vs main · 0 files · 0 uncommitted")).toBeInTheDocument();
  expect(screen.getByRole("img", { name: "Reviewer" })).toBeInTheDocument();
});

it("explains a vanished source even when the clone has no changes", () => {
  seedChanges([]);
  const snapshot = useContextGitStore.getState().byTeam[TEAM]!;
  useContextGitStore.getState().put(TEAM, {
    ...snapshot,
    sourceNote: "Sprint no longer exists; showing your clone.",
  });
  useViewerStore.getState().setNavLens(TEAM, { view: "changes" });
  renderNav();
  expect(screen.getByText("Sprint no longer exists; showing your clone.")).toBeInTheDocument();
});
