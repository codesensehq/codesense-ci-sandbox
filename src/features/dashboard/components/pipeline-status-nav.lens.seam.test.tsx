// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, screen, within } from "@/test-utils";

import * as sharedUiState from "@/features/dashboard/lib/shared-ui-state";
import { PipelineStatusNav } from "./pipeline-status-nav";
import { buildStoryRef, buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import {
  resetManagedSessionStoreForTests,
  useManagedSessionStore,
} from "@/shared/stores/use-managed-session-store";
import { NAV_LENSES } from "@/shared/lib/nav-lens";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";
import type { AgentStatus } from "@/shared/lib/agent-status-config";

// The lens menu, the needs-you badge, the stage arrangement and their counts, driven through
// the nav with real stores: a story's lens bucket comes from the same mark its pill draws and
// the same taxonomy role its rail draws, in the Team scope and in an agent's context.

const appState = vi.hoisted(() => ({ raw: "{}" }));
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
    loadAppState: async () => appState.raw,
    saveAppState: async (raw: string) => {
      appState.raw = raw;
    },
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
    subEpics: [
      buildTree("26-inbox", {
        dirPath: "26-inbox",
        absoluteDirPath: "/ctx/26-inbox",
        mainDoc: { filePath: "/ctx/26-inbox/README.md", supportingDocs: null, purposeText: null },
        stories: [
          story("26-inbox", "AID-1", "done", "Inbox read model"),
          story("26-inbox", "AID-3", "review", "Rail list"),
          story("26-inbox", "AID-4", "in-progress", "Dashboards from the inbox"),
        ],
        docs: [{ fileName: "flow.excalidraw", filePath: "/ctx/26-inbox/flow.excalidraw" }],
      }),
      buildTree("27-nav", {
        dirPath: "27-nav",
        absoluteDirPath: "/ctx/27-nav",
        mainDoc: { filePath: "/ctx/27-nav/README.md", supportingDocs: null, purposeText: null },
        stories: [
          story("27-nav", "LN-1", "in-progress", "Story rows show agents"),
          story("27-nav", "LN-3", "blocked", "One icon family"),
        ],
      }),
    ],
  });
}

/** A story workspace whose one agent reports `status`. */
function agentOn(id: string, dir: string, status: AgentStatus) {
  const session = `sess-${id}`;
  useViewerStore.setState((s) => ({
    workspaces: {
      [TEAM]: [
        ...(s.workspaces[TEAM] ?? []),
        {
          id,
          kind: "story",
          cwd: `/wt/${id}`,
          status: "ready",
          storyId: id,
          storyFilePath: `/ctx/${dir}/${id}.md`,
        },
      ],
    } as never,
  }));
  seedTerminalTabs(TEAM, id, [{ session, name: "claude" }]);
  projectSessionStatus(session, status);
}

/** Barb's context: the inbox epic only. */
const BARB: NavAgentContext = {
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

function renderNav(agentContext?: NavAgentContext) {
  return render(
    <PipelineStatusNav
      tree={tree()}
      repoPath="/ctx"
      rootPath="/ctx"
      selectedPath={null}
      onFileSelect={() => {}}
      onOpenStoryWorkspace={() => {}}
      agentContext={agentContext}
    />,
  );
}

function openMenu() {
  const trigger = screen.getByTestId("left-nav-lens-button");
  fireEvent.pointerDown(trigger, { button: 0, pointerType: "mouse" });
  return screen.getByTestId("left-nav-lens-menu");
}

/** The lens menu's live count for one lens. */
function countOf(menu: HTMLElement, lens: string): string {
  return within(menu).getByTestId(`left-nav-lens-${lens}`).textContent!.replace(/\D/g, "");
}

const titles = () => screen.queryAllByTestId("nav-story-title").map((t) => t.textContent);

beforeEach(() => {
  appState.raw = "{}";
  resetSessionProjection();
  resetPrunedAgentSessionsForTests();
  useAgentStateStore.setState({ bySession: {}, activityBySession: {}, exitBySession: {} });
  resetManagedSessionStoreForTests();
  useViewerStore.setState({
    workspaces: {},
    aiStatusBySession: {},
    navLens: {},
    statusNavExpandedPaths: { [TEAM]: ["/ctx/26-inbox", "/ctx/27-nav"] },
  });
  agentOn("AID-4", "26-inbox", "paused");
  agentOn("LN-1", "27-nav", "thinking");
  agentOn("LN-3", "27-nav", "failed");
});

describe("the lens menu counts what each lens holds", () => {
  it("counts and groups questions and self-reported blocks as Needs you, separately from the blocked story stage", () => {
    agentOn("AID-3", "26-inbox", "thinking");
    useManagedSessionStore.getState().addPendingAsk({
      correlationId: "question",
      sessionId: "sess-AID-3",
      question: "Which layout should I use?",
    });

    projectSessionStatus("sess-AID-3", "awaiting-answer");
    projectSessionStatus("sess-LN-1", "blocked");
    renderNav();
    expect(screen.getByTestId("left-nav-needs-badge")).toHaveTextContent("3");
    const menu = openMenu();
    expect(countOf(menu, "needs")).toBe("3");
    expect(countOf(menu, "blocked")).toBe("1");
    fireEvent.click(within(menu).getByTestId("left-nav-lens-needs"));
    expect(screen.getByTestId("left-nav-lens-chip")).toHaveTextContent("Needs you3");
    expect(titles()).toEqual(
      expect.arrayContaining([
        "AID-3Rail list",
        "AID-4Dashboards from the inbox",
        "LN-1Story rows show agents",
      ]),
    );
    expect(titles()).toHaveLength(3);

    act(() => useViewerStore.getState().setNavLens(TEAM, { lens: "everything", arrange: "stage" }));
    const needs = screen.getByTestId("nav-stage-group-needs");
    const stageBlocked = screen.getByTestId("nav-stage-group-blocked");
    expect(needs).toHaveTextContent("Needs you3");
    expect(stageBlocked).toHaveTextContent("Blocked1");
    // All three live needs belong between the Needs-you and Blocked-stage headings.
    for (const title of [
      "AID-3Rail list",
      "AID-4Dashboards from the inbox",
      "LN-1Story rows show agents",
    ]) {
      const row = screen.getAllByTestId("nav-story-title").find((el) => el.textContent === title)!;
      expect(needs.compareDocumentPosition(row) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(
        row.compareDocumentPosition(stageBlocked) & Node.DOCUMENT_POSITION_FOLLOWING,
      ).toBeTruthy();
    }
    act(() => useViewerStore.getState().setNavLens(TEAM, { lens: "blocked" }));
    expect(titles()).toEqual(["LN-3One icon family"]);
  });

  it("in the Team scope: by mark for Needs you / Agents on it, by role for the stages", () => {
    renderNav();
    const menu = openMenu();
    expect(countOf(menu, "needs")).toBe("1");
    expect(countOf(menu, "agents")).toBe("3");
    expect(countOf(menu, "active")).toBe("2");
    expect(countOf(menu, "review")).toBe("1");
    expect(countOf(menu, "blocked")).toBe("1");
    expect(countOf(menu, "done")).toBe("1");
  });

  it("in an agent's context: only what that agent gets counts", () => {
    renderNav(BARB);
    const menu = openMenu();
    expect(countOf(menu, "needs")).toBe("1");
    expect(countOf(menu, "agents")).toBe("1");
    expect(countOf(menu, "active")).toBe("1");
    expect(countOf(menu, "blocked")).toBe("0");
  });
});

describe("a lens is a chip in the field", () => {
  it("the amber badge picks Needs you: only that story stays, framed by its epic; files drop", () => {
    renderNav();
    expect(screen.getByTestId("left-nav-needs-badge")).toHaveTextContent("1");
    fireEvent.click(screen.getByTestId("left-nav-needs-badge"));
    expect(screen.getByTestId("left-nav-lens-chip")).toHaveTextContent("Needs you");
    expect(titles()).toEqual(["AID-4Dashboards from the inbox"]);
    expect(screen.getByText("26-inbox")).toBeInTheDocument();
    expect(screen.queryByText("27-nav")).toBeNull();
    expect(screen.queryByText("flow")).toBeNull();
  });

  it("combines with typed text, and an empty result names the lens to clear", async () => {
    renderNav();
    fireEvent.click(screen.getByTestId("left-nav-needs-badge"));
    fireEvent.change(screen.getByTestId("left-nav-search-input"), {
      target: { value: "billing" },
    });
    expect(await screen.findByTestId("left-nav-empty")).toHaveTextContent(
      'No story that needs you matches "billing".',
    );
    fireEvent.click(screen.getByRole("button", { name: /Clear .Needs you./ }));
    expect(screen.queryByTestId("left-nav-lens-chip")).toBeNull();
  });

  it("Esc clears the text first, then the lens", async () => {
    renderNav();
    fireEvent.click(screen.getByTestId("left-nav-needs-badge"));
    const input = screen.getByTestId("left-nav-search-input");
    fireEvent.change(input, { target: { value: "dash" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(input).toHaveValue("");
    expect(screen.getByTestId("left-nav-lens-chip")).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByTestId("left-nav-lens-chip")).toBeNull();
  });

  it("the lens is the team's: it survives a remount", () => {
    const { unmount } = renderNav();
    fireEvent.click(screen.getByTestId("left-nav-needs-badge"));
    unmount();
    renderNav();
    expect(screen.getByTestId("left-nav-lens-chip")).toHaveTextContent("Needs you");
  });

  it("Stories only drops every file but keeps the epics as the frame", () => {
    renderNav();
    act(() => useViewerStore.getState().setNavLens(TEAM, { storiesOnly: true }));
    expect(screen.queryByText("flow")).toBeNull();
    expect(titles()).toHaveLength(5);
    expect(screen.getByText("27-nav")).toBeInTheDocument();
  });
});

describe("arranged by stage", () => {
  it("is a flat list grouped by stage, Needs you first and Done collapsed", () => {
    renderNav();
    act(() => useViewerStore.getState().setNavLens(TEAM, { arrange: "stage" }));
    expect(screen.getByTestId("left-nav-stage-chip")).toBeInTheDocument();
    const groups = screen
      .getAllByTestId(/^nav-stage-group-/)
      .map((g) => g.getAttribute("data-testid")!.replace("nav-stage-group-", ""));
    expect(groups).toEqual(["needs", "active", "review", "blocked", "done"]);
    // Needs you wins over its stage: AID-4 is listed once, under Needs you.
    expect(titles()).toEqual([
      "AID-4Dashboards from the inbox",
      "LN-1Story rows show agents",
      "AID-3Rail list",
      "LN-3One icon family",
    ]);
    fireEvent.click(screen.getByTestId("nav-stage-group-done"));
    expect(titles()).toContain("AID-1Inbox read model");
  });

  it("in an agent's context lists only what that agent gets", () => {
    renderNav(BARB);
    act(() => useViewerStore.getState().setNavLens(TEAM, { arrange: "stage" }));
    expect(titles()).toEqual(["AID-4Dashboards from the inbox", "AID-3Rail list"]);
  });
});

describe("Agents on it counts only stories an agent is still on", () => {
  /** A story workspace whose one plain shell (no agent status) has exited. */
  function exitedShellOn(
    id: string,
    dir: string,
    exit: { code: number | null; signal: string | null },
  ) {
    const session = `shell-${id}`;
    useViewerStore.setState((s) => ({
      workspaces: {
        [TEAM]: [
          ...(s.workspaces[TEAM] ?? []),
          {
            id,
            kind: "story",
            cwd: `/wt/${id}`,
            status: "ready",
            storyId: id,
            storyFilePath: `/ctx/${dir}/${id}.md`,
          },
        ],
      } as never,
    }));
    seedTerminalTabs(TEAM, id, [{ session, name: "zsh" }]);
    useAgentStateStore.setState((s) => ({
      exitBySession: {
        ...s.exitBySession,
        [session]: {
          ...exit,
          requested: false,
          outcome: exit.signal ? "crashed" : "done",
          at: 1,
        } as never,
      },
    }));
  }

  afterEach(() => useAgentStateStore.setState({ exitBySession: {} }));

  it("an exited shell, clean or killed, is not an agent on its story", () => {
    exitedShellOn("AID-3", "26-inbox", { code: 0, signal: null });
    exitedShellOn("AID-1", "26-inbox", { code: null, signal: "SIGKILL" });
    renderNav();
    expect(countOf(openMenu(), "agents")).toBe("3");
    fireEvent.click(screen.getByTestId("left-nav-lens-agents"));
    expect(titles()).toEqual([
      "AID-4Dashboards from the inbox",
      "LN-1Story rows show agents",
      "LN-3One icon family",
    ]);
  });
});

describe("the filter field", () => {
  it("a click on the field outside its buttons focuses the input and keeps the lens", () => {
    renderNav();
    fireEvent.click(screen.getByTestId("left-nav-needs-badge"));
    const field = screen.getByTestId("left-nav-filter-field");
    const chip = screen.getByTestId("left-nav-lens-chip");
    const targets = [field.querySelector("svg")!, within(chip).getByText("Needs you"), chip, field];
    for (const target of targets) {
      fireEvent.mouseDown(target);
      fireEvent.click(target);
      expect(useViewerStore.getState().navLens[TEAM]?.lens).toBe("needs");
      expect(screen.getByTestId("left-nav-lens-chip")).toBeInTheDocument();
      expect(screen.getByTestId("left-nav-search-input")).toHaveFocus();
      act(() => screen.getByTestId("left-nav-search-input").blur());
    }
  });

  it("the Show stories options say which lens is selected", () => {
    renderNav();
    act(() => useViewerStore.getState().setNavLens(TEAM, { lens: "agents" }));
    const menu = openMenu();
    for (const { lens } of NAV_LENSES) {
      const item = within(menu).getByTestId(`left-nav-lens-${lens}`);
      expect(item).toHaveAttribute("role", "menuitemradio");
      expect(item).toHaveAttribute("aria-checked", String(lens === "agents"));
    }
  });
});

describe("the lens menu inside the field", () => {
  it("picking a lens from the menu still applies it", () => {
    renderNav();
    const menu = openMenu();
    const item = within(menu).getByTestId("left-nav-lens-agents");
    fireEvent.mouseDown(item);
    fireEvent.click(item);
    expect(useViewerStore.getState().navLens[TEAM]?.lens).toBe("agents");
  });
});

it("selects Private in the one Source section, preserves View, and clears only with its chip or menu", async () => {
  const { useLocalProfileStore } = await import("@/shared/stores/local-profile-store");
  useLocalProfileStore.setState({
    status: {
      active: { principal: "42", displayName: "Me", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
  renderNav();
  const menu = openMenu();
  expect(within(menu).getByRole("menuitemradio", { name: /main · your clone/ })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  fireEvent.click(within(menu).getByRole("menuitemradio", { name: "Private" }));
  expect(screen.getByTestId("left-nav-source-chip")).toHaveTextContent("Private");
  expect(useViewerStore.getState().navLens[TEAM].source).toEqual({ kind: "private" });
  expect(
    await sharedUiState.readSharedUiState(JSON.stringify(["ui:file-nav", "team:acme/platform"])),
  ).toMatchObject({ source: { kind: "private" }, scope: "private" });
  fireEvent.keyDown(screen.getByRole("textbox"), { key: "Escape" });
  expect(useViewerStore.getState().navLens[TEAM].source).toEqual({ kind: "private" });
  fireEvent.click(screen.getByRole("button", { name: "Clear Private" }));
  expect(useViewerStore.getState().navLens[TEAM].source).toEqual({ kind: "clone" });
  expect(screen.queryByTestId("left-nav-source-chip")).toBeNull();
  expect(
    await sharedUiState.readSharedUiState(JSON.stringify(["ui:file-nav", "team:acme/platform"])),
  ).toMatchObject({
    source: { kind: "clone" },
    scope: "team",
  });
});

it("mirrors a native worktree selection through the shared lens", async () => {
  const { useWorktreeRosterStore } = await import("@/features/viewer/stores/worktree-roster-store");
  useWorktreeRosterStore.setState({
    rows: [
      {
        kind: "context",
        orgSlug: "acme",
        team: "platform",
        id: "cw_sprint",
        name: "Sprint",
        branch: "sprint",
        path: "/worktree",
        liveCliSession: false,
      } as any,
    ],
  });
  renderNav();
  const menu = openMenu();
  fireEvent.click(within(menu).getByRole("menuitemradio", { name: /Sprint/ }));
  expect(useViewerStore.getState().navLens[TEAM].source).toEqual({
    kind: "worktree",
    id: "cw_sprint",
  });
  expect(
    await sharedUiState.readSharedUiState(JSON.stringify(["ui:file-nav", "team:acme/platform"])),
  ).toMatchObject({ source: { kind: "worktree", id: "cw_sprint" }, scope: "team" });
  useWorktreeRosterStore.setState({ rows: [] });
});
