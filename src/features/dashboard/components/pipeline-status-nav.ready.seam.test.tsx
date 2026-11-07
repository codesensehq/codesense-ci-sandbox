// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, within } from "@/test-utils";

import { PipelineStatusNav } from "./pipeline-status-nav";
import { buildStoryRef, buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";

// Ready to start, driven through the nav with real stores: the lens, its count, the stage
// arrangement and each row's rail all read the same full-graph readiness, so a not-started
// story whose dependencies are all done is the same story in every one of them.

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

const story = (dir: string, id: string, status: string, dependsOn: string[] | null = null) => ({
  ...buildStoryRef(`${id}.md`, { absolutePath: `/ctx/${dir}/${id}.md`, status, dependsOn }),
  storyId: id,
  openTags: { title: [id.toLowerCase()] },
});

function tree() {
  return buildTree("root", {
    absoluteDirPath: "/ctx",
    subEpics: [
      buildTree("26-inbox", {
        dirPath: "26-inbox",
        absoluteDirPath: "/ctx/26-inbox",
        stories: [
          story("26-inbox", "AID-1", "done"),
          story("26-inbox", "AID-3", "review"),
          // Ready: its one dependency is done.
          story("26-inbox", "AID-5", "todo", ["AID-1"]),
          // Waiting: its dependency is in review.
          story("26-inbox", "AID-6", "todo", ["AID-3"]),
          // Ready: nothing to wait on.
          story("26-inbox", "AID-7", "planning"),
        ],
      }),
      buildTree("27-nav", {
        dirPath: "27-nav",
        absoluteDirPath: "/ctx/27-nav",
        stories: [
          story("27-nav", "LN-1", "in-progress"),
          // Ready across epics: its dependency, by story id, is done.
          story("27-nav", "LN-2", "planning", ["AID-1"]),
          // Waiting on two, one in this epic and one in another.
          story("27-nav", "LN-4", "todo", ["LN-1", "AID-3"]),
          // Underway, so never ready, though its dependency is done.
          story("27-nav", "LN-5", "in-progress", ["AID-1"]),
        ],
      }),
    ],
  });
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
      agentContext={agentContext}
    />,
  );
}

function openMenu() {
  const trigger = screen.getByTestId("left-nav-lens-button");
  fireEvent.pointerDown(trigger, { button: 0, pointerType: "mouse" });
  return screen.getByTestId("left-nav-lens-menu");
}

const countOf = (menu: HTMLElement, lens: string) =>
  within(menu).getByTestId(`left-nav-lens-${lens}`).textContent!.replace(/\D/g, "");

const ids = () =>
  screen.queryAllByTestId("nav-story-title").map((t) => t.textContent!.match(/^[A-Z]+-\d+/)![0]);

const rowOf = (id: string) =>
  screen
    .getAllByTestId("nav-story-title")
    .find((t) => t.textContent!.startsWith(id))!
    .closest<HTMLElement>("[role='listitem']")!;

const railOf = (id: string) => rowOf(id).querySelector<HTMLElement>(".cs-rail")!;

beforeEach(() => {
  useViewerStore.setState({
    workspaces: {},
    aiStatusBySession: {},
    navLens: {},
    statusNavExpandedPaths: { [TEAM]: ["/ctx/26-inbox", "/ctx/27-nav"] },
  });
});

describe("Ready to start", () => {
  it("counts exactly the not-started stories whose dependencies are all done", () => {
    renderNav();
    const menu = openMenu();
    expect(within(menu).getByTestId("left-nav-lens-ready")).toHaveTextContent("Ready to start");
    expect(countOf(menu, "ready")).toBe("3");
    expect(countOf(menu, "not_started")).toBe("5");
  });

  it("lists exactly those stories, framed by their epics", () => {
    renderNav();
    fireEvent.click(within(openMenu()).getByTestId("left-nav-lens-ready"));
    expect(screen.getByTestId("left-nav-lens-chip")).toHaveTextContent("Ready to start3");
    expect(ids()).toEqual(["AID-5", "AID-7", "LN-2"]);
  });

  it("in an agent's context counts and lists only what that agent gets", () => {
    renderNav(BARB);
    expect(countOf(openMenu(), "ready")).toBe("2");
    act(() => useViewerStore.getState().setNavLens(TEAM, { lens: "ready" }));
    expect(ids()).toEqual(["AID-5", "AID-7"]);
  });

  it("arranged by stage, the ready stories lead the Not started group", () => {
    renderNav();
    act(() => useViewerStore.getState().setNavLens(TEAM, { arrange: "stage" }));
    const notStarted = ids().filter((id) =>
      ["AID-5", "AID-6", "AID-7", "LN-2", "LN-4"].includes(id),
    );
    expect(notStarted).toEqual(["AID-5", "AID-7", "LN-2", "AID-6", "LN-4"]);
  });
});

describe("a story row says whether it can start", () => {
  it("a ready story's rail ends in the start arrowhead and names its dependencies", () => {
    renderNav();
    expect(rowOf("AID-5")).toHaveAttribute("data-ready", "true");
    expect(railOf("AID-5")).toHaveAttribute("data-rail-ready", "true");
    expect(railOf("AID-5").querySelector("[data-rail='ready']")).not.toBeNull();
    expect(railOf("AID-5")).toHaveAccessibleName("Ready to start · its dependency is done");
    expect(railOf("AID-7")).toHaveAccessibleName("Ready to start · no dependencies");
  });

  it("a waiting story keeps the plain rail and names what it waits on", () => {
    renderNav();
    expect(rowOf("LN-4")).not.toHaveAttribute("data-ready");
    expect(railOf("LN-4")).not.toHaveAttribute("data-rail-ready");
    expect(railOf("LN-4").querySelector("[data-rail='ready']")).toBeNull();
    expect(railOf("LN-4").getAttribute("aria-label")).toMatch(
      /^.+ · waiting on LN-1 \(in-progress\), AID-3 \(review\)$/,
    );
  });

  it("hovering the rail opens a card: what a waiting story waits on, by stage", async () => {
    renderNav();
    fireEvent.focus(railOf("LN-4").parentElement!.closest("[data-slot='tooltip-trigger']")!);
    const card = await screen.findByRole("tooltip");
    expect(card).toHaveTextContent("Waiting on");
    expect(card).toHaveTextContent("LN-1");
    expect(card).toHaveTextContent("AID-3");
    // Each dependency's stage in the nav's own words, as the menu and stage headers say it.
    expect(card).toHaveTextContent("In progress");
    expect(card).toHaveTextContent("In review");
  });

  it("a story underway never shows the cue, whatever its dependencies", () => {
    renderNav();
    expect(rowOf("LN-5")).not.toHaveAttribute("data-ready");
    expect(rowOf("LN-5")).not.toHaveAttribute("title");
    expect(railOf("LN-5")).not.toHaveAttribute("data-rail-ready");
  });
});
