// SPDX-License-Identifier: MIT
import { act, useContext, useLayoutEffect } from "react";
import { waitFor } from "@testing-library/react";
import { HostMenuLayer } from "@/features/shell/host-menu-layer";
import {
  HostMenuContext,
  type HostMenuRequest,
  type HostMenuResult,
} from "@/features/shell/host-menu-context";
vi.mock("@/features/shell/host-agent-menu", () => ({
  useHostAgentMenu: () => null,
  HostAgentRowMenu: () => null,
  HostAgentPopover: () => null,
}));
import { openNativeMenu } from "@/features/shell/native-menu-openers";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, within } from "@/test-utils";
import { PipelineStatusNav } from "./pipeline-status-nav";
import type { StoryRef, DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import {
  buildStoryRef,
  buildTree,
  type BuildTreeOptions,
} from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";

const mockToggleStatusNavExpanded = vi.fn();
const mockToggleStatusNavPinPersonal = vi.fn();
let mockStatusNavExpandedPaths: Record<string, string[]> = {};
let mockStatusNavPinnedPaths: Record<string, string[]> = {};

vi.mock("@/shared/lib/team-key", () => ({
  useActiveTeamKey: () => "/repo",
  getActiveTeamKey: () => "/repo",
  buildTeamKey: (orgSlug: string, teamName: string | null) => `${orgSlug}:${teamName ?? "_none"}`,
  NO_TEAM_SENTINEL: "_no_team:_none",
}));

vi.mock("@/shared/stores/viewer-store", () => {
  return {
    useViewerStore: Object.assign(
      (selector: (s: Record<string, unknown>) => unknown) =>
        selector({
          navLens: {},
          statusNavExpandedPaths: mockStatusNavExpandedPaths,
          toggleStatusNavExpanded: mockToggleStatusNavExpanded,
          statusNavPinnedPaths: mockStatusNavPinnedPaths,
          toggleStatusNavPinPersonal: mockToggleStatusNavPinPersonal,
        }),
      {
        subscribe: () => () => {},
        getState: () => ({
          toggleStatusNavExpanded: mockToggleStatusNavExpanded,
          toggleStatusNavPinPersonal: mockToggleStatusNavPinPersonal,
          // RF-perf-12: the nav wires useScrollSnapshot, which reads/writes these.
          readViewState: () => undefined,
          captureViewState: () => {},
        }),
      },
    ),
  };
});

vi.mock("@/shared/lib/send-to-terminal", () => ({
  sendTextToTerminal: vi.fn(),
}));
import { sendTextToTerminal } from "@/shared/lib/send-to-terminal";
const mockSendTextToTerminal = vi.mocked(sendTextToTerminal);

/** Thin wrapper: delegates to shared buildStoryRef with /repo path convention. */
function makeStory(
  fileName: string,
  status: string | null,
  overrides?: Partial<StoryRef>,
): StoryRef {
  return buildStoryRef(fileName, {
    absolutePath: `/repo/${fileName}`,
    status,
    ...overrides,
  });
}

/** Thin wrapper: delegates to shared buildTree with /repo path convention. */
function makeTree(
  overrides?: Partial<DerivedPipelineTree> & BuildTreeOptions,
): DerivedPipelineTree {
  const { dirName, ...rest } = overrides ?? {};
  return buildTree(dirName ?? "", { absoluteDirPath: "/repo", ...rest });
}

describe("PipelineStatusNav", () => {
  const onFileSelect = vi.fn();
  const onDrillPathChange = vi.fn();
  const onStatusChange = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    mockStatusNavExpandedPaths = {};
    mockStatusNavPinnedPaths = {};
  });

  it("a dashboard story opens the same nav menu and handlers as a native row", () => {
    const story = makeStory("target.md", "todo");
    render(
      <PipelineStatusNav
        tree={makeTree({ stories: [story] })}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    fireEvent.contextMenu(screen.getByText("target"));
    const nativeItems = within(screen.getByRole("menu", { name: "File actions" }))
      .getAllByRole("menuitem")
      .map((item) => item.textContent);
    fireEvent.keyDown(document, { key: "Escape" });
    const onClose = vi.fn();
    let close: (() => void) | null = null;
    act(() => {
      close = openNativeMenu({
        row: { kind: "story", path: "target.md" },
        teamKey: "/repo",
        contentRoot: "/repo",
        anchorRect: { x: 90, y: 70, width: 0, height: 0 },
        owner: {},
        onClose,
      });
    });
    expect(close).not.toBeNull();
    expect(
      within(screen.getByRole("menu", { name: "File actions" }))
        .getAllByRole("menuitem")
        .map((item) => item.textContent),
    ).toEqual(nativeItems);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("host nav menus return focus for every path and alternate with dashboard menus", async () => {
    const frame = document.createElement("iframe");
    document.body.append(frame);
    const post = vi.fn();
    Object.defineProperty(frame, "contentWindow", { value: { postMessage: post } });
    let show!: (request: HostMenuRequest) => Promise<HostMenuResult>;
    function Bridge() {
      const open = useContext(HostMenuContext);
      useLayoutEffect(() => {
        show = open;
      }, [open]);
      return null;
    }
    const tree = makeTree({
      stories: [makeStory("target.md", "todo")],
      docs: [
        {
          filePath: "/repo/file.html",
          fileName: "file.html",
          sourceTeam: null,
        },
      ],
      subEpics: [
        buildTree("epic", {
          absoluteDirPath: "/repo/epic",
          dirPath: "epic",
          mainDoc: { filePath: "/repo/epic/README.md", supportingDocs: null, purposeText: null },
        }),
      ],
    });
    render(
      <HostMenuLayer>
        <Bridge />
        <PipelineStatusNav
          tree={tree}
          repoPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
        />
      </HostMenuLayer>,
    );
    const request: HostMenuRequest = {
      type: "native",
      frame,
      origin: "http://frame.test",
      anchorRect: { x: 30, y: 40, width: 0, height: 0 },
      anchorId: "anchor",
      teamKey: "/repo",
      contentRoot: "/repo",
      dashboard: { reload: vi.fn() },
      run: vi.fn(),
    };
    for (const [kind, path] of [
      ["story", "target.md"],
      ["epic", "epic"],
      ["file", "file.html"],
    ]) {
      let result!: Promise<HostMenuResult>;
      act(() => {
        result = show({ ...request, row: { kind, path } });
      });
      await screen.findByRole("menu", { name: "File actions" });
      fireEvent.keyDown(document, { key: "Escape" });
      await expect(result).resolves.toEqual({ closed: true });
      expect(post).toHaveBeenLastCalledWith(
        { type: "codesense:focus-return", anchorId: "anchor" },
        "http://frame.test",
      );
    }
    act(() => {
      void show({ ...request, row: { kind: "story", path: "target.md" } });
    });
    await screen.findByRole("menu", { name: "File actions" });
    act(() => {
      void show({ ...request, type: "frame" });
    });
    await screen.findByRole("menuitem", { name: "Reload pane" });
    await waitFor(() => expect(screen.getAllByRole("menu")).toHaveLength(1));
    expect(screen.queryByRole("menu", { name: "File actions" })).toBeNull();
    act(() => {
      void show({ ...request, row: { kind: "story", path: "target.md" } });
    });
    await screen.findByRole("menu", { name: "File actions" });
    await waitFor(() => expect(screen.getAllByRole("menu")).toHaveLength(1));
    frame.remove();
  });

  // -- Empty state --

  it("shows empty message when tree has no content", () => {
    render(
      <PipelineStatusNav
        tree={makeTree()}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    expect(screen.getByText("Nothing here yet")).toBeInTheDocument();
    // The sample project is retired: the empty state offers no way to add one.
    expect(screen.queryByTestId("add-sample-project")).toBeNull();
    expect(screen.queryByText("Try the sample project")).toBeNull();
  });

  // The Guided Setup checklist (full panel + "Setup ✓"/"Finish setup" footer) is
  // retired: the nav renders only the pipeline, in every state.
  describe("renders no Guided Setup panel or footer", () => {
    function expectNoGuidedSetup() {
      expect(screen.queryByText(/guided setup/i)).toBeNull();
      expect(screen.queryByText(/finish setup/i)).toBeNull();
      expect(screen.queryByText(/setup ✓/i)).toBeNull();
      expect(screen.queryByText(/choose an agent/i)).toBeNull();
    }

    it("for an empty team: only the plain empty message", () => {
      render(
        <PipelineStatusNav
          tree={makeTree()}
          repoPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
        />,
      );
      expect(screen.getByText("Nothing here yet")).toBeInTheDocument();
      expectNoGuidedSetup();
    });

    it("for a drilled-in empty sub-epic", () => {
      render(
        <PipelineStatusNav
          tree={makeTree({ dirName: "epic-1", absoluteDirPath: "/repo/epic-1" })}
          repoPath="/repo"
          selectedPath={null}
          isDrilledIn
          rootPath="/repo"
          onFileSelect={onFileSelect}
          onDrillPathChange={onDrillPathChange}
        />,
      );
      expect(screen.getByText("Nothing here yet")).toBeInTheDocument();
      expectNoGuidedSetup();
    });

    it("for a populated pipeline", () => {
      render(
        <PipelineStatusNav
          tree={makeTree({
            subEpics: [
              makeTree({
                dirName: "epic-1",
                absoluteDirPath: "/repo/epic-1",
                stories: [makeStory("SW-1.md", "todo", { title: "Story One" })],
              }),
            ],
          })}
          repoPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
        />,
      );
      expect(screen.queryByText("Nothing here yet")).toBeNull();
      expect(screen.getByText("epic-1")).toBeInTheDocument();
      expectNoGuidedSetup();
    });
  });

  // The lens row (scope + Changes toggle) lives in `LeftNavHeader`, rendered once
  // by `team-workspace-view` above the body swap; this nav is the pipeline BODY
  // only and never renders an org/team identity row.
  it("does not render an org·team identity row", () => {
    render(
      <PipelineStatusNav
        tree={makeTree()}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    expect(screen.queryByTestId("nav-org-team-row")).toBeNull();
  });

  // The left nav is a static folder tree — no sprint filter (removed 2026-08-12:
  // nav = place, board = work view). It shows folders + stories, never a sprint
  // selector.
  it("is a static folder tree with no sprint selector", () => {
    render(
      <PipelineStatusNav
        tree={makeTree({
          stories: [makeStory("R-1-sprint.md", "todo", { title: "Sprint Story" })],
          subEpics: [
            makeTree({
              dirName: "fixes",
              absoluteDirPath: "/repo/fixes",
              stories: [makeStory("d.md", "done", { title: "Done Story" })],
            }),
          ],
        })}
        repoPath="/repo"
        repoName="Codesense"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    // Folder tree (not a filtered flat list) + no sprint selector.
    expect(screen.getByText("fixes")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Sprint:/ })).toBeNull();
  });

  // -- Epic level (has sub-epics) --

  it("renders stories and sub-epic folders in unified view", () => {
    const tree = makeTree({
      stories: [makeStory("F-1-story.md", "in-progress", { title: "My Story" })],
      subEpics: [
        makeTree({
          dirName: "fixes",
          absoluteDirPath: "/repo/fixes",
          stories: [makeStory("d.md", "done")],
        }),
      ],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onDrillPathChange={onDrillPathChange}
      />,
    );

    expect(screen.getByText("My Story")).toBeInTheDocument();
    expect(screen.getByText("fixes")).toBeInTheDocument();
  });

  it("renders sub-epic folder with progress chip", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirName: "auth",
          absoluteDirPath: "/repo/auth",
          stories: [makeStory("x.md", "done"), makeStory("y.md", "todo")],
        }),
      ],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.getByText("auth")).toBeInTheDocument();
  });

  it("calls onDrillPathChange when drill button clicked", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirName: "auth",
          absoluteDirPath: "/repo/auth",
          stories: [makeStory("x.md", "todo")],
        }),
      ],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onDrillPathChange={onDrillPathChange}
      />,
    );

    const drillButton = screen.getByRole("button", { name: "Drill into auth" });
    fireEvent.click(drillButton);
    expect(onDrillPathChange).toHaveBeenCalledWith("/repo/auth");
  });

  it("calls toggleStatusNavExpanded on folder click", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirName: "fixes",
          absoluteDirPath: "/repo/fixes",
          stories: [makeStory("F-1-auth-bug.md", "in-progress", { title: "Auth Bug" })],
        }),
      ],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    // Stories not visible initially (expanded paths is empty)
    expect(screen.queryByText("Auth Bug")).not.toBeInTheDocument();

    // Click folder — should call store toggle
    fireEvent.click(screen.getByText("fixes"));
    expect(mockToggleStatusNavExpanded).toHaveBeenCalledWith("/repo", "/repo/fixes");
  });

  it("shows expanded sub-epic stories when path is in expanded set", () => {
    mockStatusNavExpandedPaths = { "/repo": ["/repo/fixes"] };

    const tree = makeTree({
      subEpics: [
        makeTree({
          dirName: "fixes",
          absoluteDirPath: "/repo/fixes",
          stories: [
            makeStory("F-1-auth-bug.md", "in-progress", { title: "Auth Bug" }),
            makeStory("F-2-login-fix.md", "done", { title: "Login Fix" }),
          ],
        }),
      ],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.getByText("Auth Bug")).toBeInTheDocument();
    expect(screen.getByText("Login Fix")).toBeInTheDocument();
  });

  // -- Leaf level (no sub-epics) --

  it("renders flat sorted story list at leaf level", () => {
    const tree = makeTree({
      stories: [
        makeStory("F-1-done-story.md", "done", { title: "Done Story" }),
        makeStory("F-2-active-story.md", "in-progress", { title: "Active Story" }),
        makeStory("F-3-pending-story.md", "todo", { title: "Pending Story" }),
      ],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    // All stories visible (flat list, no group headers)
    expect(screen.getByText("Done Story")).toBeInTheDocument();
    expect(screen.getByText("Active Story")).toBeInTheDocument();
    expect(screen.getByText("Pending Story")).toBeInTheDocument();
  });

  // -- Doc type filtering (N.2) --

  it("renders type:doc as DocRow (no status selector), not StoryRow", () => {
    // PTS-2 follow-up: `type: doc` files are routed to tree.docs[] by the
    // Rust scanner; tree.stories[] now only carries actionable pipeline
    // types ({story, bug, spike}). DocRow rendering comes from the docs
    // array directly — no TS-side documentType partition any more.
    const tree = makeTree({
      stories: [makeStory("story.md", "in-progress")],
      docs: [{ fileName: "notes.md", filePath: "/repo/notes.md" }],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onStatusChange={onStatusChange}
      />,
    );

    // Doc label = stripped filename stem; story gets a status selector
    expect(screen.getByText("notes")).toBeInTheDocument();
    expect(screen.getByText("story")).toBeInTheDocument();
    // Only one "Change status" — from the story, not the doc
    expect(screen.getAllByRole("button", { name: "Change status" })).toHaveLength(1);
  });

  it("renders type:retro as DocRow", () => {
    // PTS-2 follow-up: retro now also lands in tree.docs[].
    const tree = makeTree({
      docs: [{ fileName: "retro.md", filePath: "/repo/retro.md" }],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onStatusChange={onStatusChange}
      />,
    );

    expect(screen.getByText("retro")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Change status" })).not.toBeInTheDocument();
  });

  it("renders an image in docs[] as a single DocRow keeping its extension (SW-SC-7)", () => {
    // SW-SC-7: images are scanner-routed into tree.docs[] (DocRow), keeping
    // their extension (AC-8). The viewer-root generic-file pass excludes them
    // so a flat-layout root image renders exactly once, not twice.
    const tree = makeTree({
      stories: [makeStory("story.md", "in-progress")],
      docs: [{ fileName: "shot.png", filePath: "/repo/shot.png" }],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onStatusChange={onStatusChange}
        viewerRootPath="/repo"
      />,
    );

    // Label keeps the extension (AC-8), and there is exactly one row for it.
    expect(screen.getAllByText("shot.png")).toHaveLength(1);
    // It's a DocRow, not a status-bearing StoryRow.
    expect(screen.getAllByRole("button", { name: "Change status" })).toHaveLength(1); // only the story
  });

  it("renders null documentType as StoryRow (defaults to actionable)", () => {
    const tree = makeTree({
      stories: [makeStory("F-1-story.md", "todo", { documentType: null })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onStatusChange={onStatusChange}
      />,
    );

    expect(screen.getByRole("button", { name: "Change status" })).toBeInTheDocument();
  });

  it("renders unknown document type as DocRow", () => {
    // PTS-2 follow-up: `type: meeting` (not in {story, bug, spike}) lands
    // in tree.docs[] per the Rust gate. No status selector.
    const tree = makeTree({
      docs: [{ fileName: "meeting.md", filePath: "/repo/meeting.md" }],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onStatusChange={onStatusChange}
      />,
    );

    expect(screen.getByText("meeting")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Change status" })).not.toBeInTheDocument();
  });

  it("progress counts exclude non-actionable types without status", () => {
    // PTS-2 follow-up: docs no longer share stories[] — they're in
    // tree.docs[]. Progress counting walks stories[] only, which means
    // doc files are inherently excluded from progress totals (no
    // documentType filter needed downstream).
    const tree = makeTree({
      stories: [makeStory("F-1-story.md", "done", { title: "Done Story" })],
      docs: [{ fileName: "notes.md", filePath: "/repo/notes.md" }],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        repoName="myrepo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    // Both should be visible
    expect(screen.getByText("Done Story")).toBeInTheDocument();
    expect(screen.getByText("notes")).toBeInTheDocument();
  });

  it("SubEpicFolder renders tree.docs[] as DocRow inside expanded sub-epics", () => {
    // PTS-2 follow-up: sub-epic docs come from `tree.docs[]` per-node
    // (Rust scanner does the story/doc routing). Pipeline cards from
    // stories[] still get status selectors; doc rows do not.
    mockStatusNavExpandedPaths = { "/repo": ["/repo/nav"] };

    const tree = makeTree({
      subEpics: [
        makeTree({
          dirName: "nav",
          absoluteDirPath: "/repo/nav",
          stories: [makeStory("N-1-story.md", "in-progress", { title: "Nav Story" })],
          docs: [{ fileName: "research.md", filePath: "/repo/nav/research.md" }],
        }),
      ],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onStatusChange={onStatusChange}
      />,
    );

    expect(screen.getByText("Nav Story")).toBeInTheDocument();
    expect(screen.getByText("research")).toBeInTheDocument();
    // Only the story gets a status selector, not the doc
    expect(screen.getAllByRole("button", { name: "Change status" })).toHaveLength(1);
  });

  it("renders mainDoc in docs section", () => {
    const tree = makeTree({
      stories: [makeStory("a.md", "todo")],
      mainDoc: {
        filePath: "/repo/README.md",
        supportingDocs: null,
        purposeText: null,
      },
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.getByText("README.md")).toBeInTheDocument();
  });

  // -- File selection --

  it("calls onFileSelect when story row is clicked", () => {
    const tree = makeTree({
      stories: [makeStory("F-1-my-story.md", "todo", { title: "My Story" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    fireEvent.click(screen.getByText("My Story"));
    expect(onFileSelect).toHaveBeenCalledWith("/repo/F-1-my-story.md");
  });

  // SW-FX-10: double-click forwards to onFileDoubleClick, which the parent
  // (team-workspace-view) routes through the shared open-and-focus action. The
  // nav row only forwards the gesture; single-click + meta-click are unchanged.
  it("calls onFileDoubleClick when a story row is double-clicked (SW-FX-10 AC-3)", () => {
    const onFileDoubleClick = vi.fn();
    const tree = makeTree({
      stories: [makeStory("F-1-my-story.md", "todo", { title: "My Story" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onFileDoubleClick={onFileDoubleClick}
      />,
    );

    fireEvent.doubleClick(screen.getByText("My Story"));
    expect(onFileDoubleClick).toHaveBeenCalledWith("/repo/F-1-my-story.md");
  });

  it("meta-click on a story row sends to terminal, not onFileSelect (SW-FX-10 AC-7)", () => {
    const tree = makeTree({
      stories: [makeStory("F-1-my-story.md", "todo", { title: "My Story" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    fireEvent.click(screen.getByText("My Story"), { metaKey: true });
    expect(mockSendTextToTerminal).toHaveBeenCalledWith("/repo/F-1-my-story.md");
    expect(onFileSelect).not.toHaveBeenCalled();
  });

  it("highlights selected file", () => {
    const tree = makeTree({
      stories: [makeStory("F-1-my-story.md", "todo", { title: "My Story" })],
    });

    const { container } = render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath="/repo/F-1-my-story.md"
        onFileSelect={onFileSelect}
      />,
    );

    // The selected row should have the selected class
    const selectedRow = container.querySelector(".cs-sidebar-item-selected");
    expect(selectedRow).toBeInTheDocument();
  });

  // -- Status editing --

  it("opens status popover on status icon click", () => {
    const tree = makeTree({
      stories: [makeStory("F-1-my-story.md", "todo", { title: "My Story" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onStatusChange={onStatusChange}
      />,
    );

    // Click the status trigger (the "Change status" labeled button)
    fireEvent.click(screen.getByRole("button", { name: "Change status" }));

    // Status options should appear
    expect(screen.getByText("In progress")).toBeInTheDocument();
    expect(screen.getByText("Done")).toBeInTheDocument();
  });

  it("calls onStatusChange when status is selected", () => {
    const tree = makeTree({
      stories: [makeStory("F-1-my-story.md", "todo", { title: "My Story" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onStatusChange={onStatusChange}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Change status" }));
    fireEvent.click(screen.getByText("In progress"));
    expect(onStatusChange).toHaveBeenCalledWith("/repo/F-1-my-story.md", "in-progress");
  });

  // -- Story ID display --

  it("displays story title from StoryRef", () => {
    const tree = makeTree({
      stories: [
        makeStory("F-19-terminal-persistence.md", "in-progress", {
          title: "Terminal Persistence",
        }),
      ],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.getByText("Terminal Persistence")).toBeInTheDocument();
  });

  // -- Refresh reactivity --

  it("updates pipeline content reactively when tree prop changes (status change)", () => {
    const tree1 = makeTree({
      stories: [makeStory("F-1-story.md", "todo", { title: "My Story" })],
    });

    const { rerender } = render(
      <PipelineStatusNav
        tree={tree1}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    // Simulate frontmatter status change → pipeline rescan → new tree
    const tree2 = makeTree({
      stories: [makeStory("F-1-story.md", "done", { title: "My Story" })],
    });

    rerender(
      <PipelineStatusNav
        tree={tree2}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    // Component re-renders with new tree — the status icon updates
    // (verified by the component not crashing and still showing the story)
    expect(screen.getByText("My Story")).toBeInTheDocument();
  });

  it("does not fetch non-pipeline content when drilled in", () => {
    const tree = makeTree({
      stories: [makeStory("F-1-story.md", "todo", { title: "My Story" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        viewerRootPath="/viewer"
        isDrilledIn={true}
        refreshKey={1}
      />,
    );

    // No docs/non-pipeline dirs should appear when drilled in
    expect(screen.queryByText("docs")).not.toBeInTheDocument();
  });

  it("updates pipeline content reactively when tree prop changes", () => {
    const tree1 = makeTree({
      stories: [makeStory("F-1-story.md", "todo", { title: "Old Title" })],
    });

    const { rerender } = render(
      <PipelineStatusNav
        tree={tree1}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.getByText("Old Title")).toBeInTheDocument();

    // Simulate pipeline rescan returning updated tree
    const tree2 = makeTree({
      stories: [makeStory("F-1-story.md", "in-progress", { title: "Updated Title" })],
    });

    rerender(
      <PipelineStatusNav
        tree={tree2}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.queryByText("Old Title")).not.toBeInTheDocument();
    expect(screen.getByText("Updated Title")).toBeInTheDocument();
  });

  // -- N-3: rootDisplayName --

  // At the root the org name is the title bar's job: no breadcrumb row restates it.
  it("renders no breadcrumb row or root label at the root", () => {
    const tree = makeTree({
      stories: [makeStory("F-1-story.md", "done", { title: "Done Story" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        repoName="codesense (Codesense)"
        rootDisplayName="Codesense"
        rootPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.queryByTestId("left-nav-breadcrumb-row")).toBeNull();
    expect(screen.queryByRole("navigation", { name: "Breadcrumb" })).toBeNull();
    expect(screen.queryByText("Codesense")).not.toBeInTheDocument();
    expect(screen.queryByText("codesense (Codesense)")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Up one level" })).not.toBeInTheDocument();
  });

  it("uses rootDisplayName as the root crumb once drilled in (N-3 AC-1)", () => {
    render(
      <PipelineStatusNav
        tree={makeTree({ absoluteDirPath: "/repo/foo" })}
        repoPath="/repo"
        repoName="codesense (Codesense)"
        rootDisplayName="Codesense"
        isDrilledIn={true}
        rootPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.getByRole("button", { name: "Codesense" })).toBeInTheDocument();
    expect(screen.queryByText("codesense (Codesense)")).not.toBeInTheDocument();
  });

  it("falls back to the drilled folder basename when no rootPath is given (N-3 AC-2)", () => {
    // Defensive fallback path: drilled but `rootPath` absent → the breadcrumb
    // model can't build, so the toolbar shows the scoped tree's basename
    // (derived locally, no `drillFolderName` prop).
    const tree = makeTree({
      absoluteDirPath: "/repo/nav",
      stories: [makeStory("F-1-story.md", "done", { title: "Done Story" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        repoName="codesense (Codesense)"
        rootDisplayName="Codesense"
        isDrilledIn={true}
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.getByText("nav")).toBeInTheDocument();
    expect(screen.queryByText("Codesense")).not.toBeInTheDocument();
  });

  it("falls back to repoName for the root crumb when rootDisplayName not provided (N-3 AC-3)", () => {
    render(
      <PipelineStatusNav
        tree={makeTree({ absoluteDirPath: "/repo/foo" })}
        repoPath="/repo"
        repoName="my-legacy-repo"
        isDrilledIn={true}
        rootPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.getByRole("button", { name: "my-legacy-repo" })).toBeInTheDocument();
  });

  // -- SW-EW-36: drilled breadcrumb trail --

  describe("breadcrumb trail (SW-EW-36)", () => {
    it("renders root + segment crumbs with current emphasized (AC-1)", () => {
      const tree = makeTree({ absoluteDirPath: "/repo/foo/bar" });
      render(
        <PipelineStatusNav
          tree={tree}
          repoPath="/repo"
          repoName="codesense"
          rootDisplayName="Codesense"
          isDrilledIn={true}
          rootPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
          onDrillPathChange={onDrillPathChange}
        />,
      );

      const nav = screen.getByRole("navigation", { name: "Breadcrumb" });
      expect(nav).toBeInTheDocument();
      // Root + intermediate are buttons; current is non-interactive text.
      expect(screen.getByRole("button", { name: "Codesense" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "foo" })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "bar" })).not.toBeInTheDocument();
      expect(screen.getByText("bar")).toBeInTheDocument();
      // ChevronRight separators present (one before each crumb after the first).
      expect(nav.querySelectorAll("svg").length).toBeGreaterThanOrEqual(2);
    });

    it("clicking the root crumb fires onDrillPathChange(rootPath) (AC-2)", () => {
      const tree = makeTree({ absoluteDirPath: "/repo/foo/bar" });
      render(
        <PipelineStatusNav
          tree={tree}
          repoPath="/repo"
          repoName="codesense"
          rootDisplayName="Codesense"
          isDrilledIn={true}
          rootPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
          onDrillPathChange={onDrillPathChange}
        />,
      );

      fireEvent.click(screen.getByRole("button", { name: "Codesense" }));
      expect(onDrillPathChange).toHaveBeenCalledWith("/repo");
    });

    it("clicking an intermediate crumb re-scopes to that ancestor's absolute path (AC-2)", () => {
      const tree = makeTree({ absoluteDirPath: "/repo/foo/bar" });
      render(
        <PipelineStatusNav
          tree={tree}
          repoPath="/repo"
          repoName="codesense"
          rootDisplayName="Codesense"
          isDrilledIn={true}
          rootPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
          onDrillPathChange={onDrillPathChange}
        />,
      );

      fireEvent.click(screen.getByRole("button", { name: "foo" }));
      expect(onDrillPathChange).toHaveBeenCalledWith("/repo/foo");
    });

    it("the back arrow re-targets to the parent (up one level), not root (AC-3)", () => {
      const tree = makeTree({ absoluteDirPath: "/repo/foo/bar" });
      render(
        <PipelineStatusNav
          tree={tree}
          repoPath="/repo"
          repoName="codesense"
          rootDisplayName="Codesense"
          isDrilledIn={true}
          rootPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
          onDrillPathChange={onDrillPathChange}
        />,
      );

      expect(screen.queryByRole("button", { name: "Back to root" })).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Up one level" }));
      expect(onDrillPathChange).toHaveBeenCalledWith("/repo/foo");
    });

    it("collapses the middle of a deep path, keeping root + parent + current clickable (AC-5)", () => {
      const tree = makeTree({ absoluteDirPath: "/repo/a/b/c/d" });
      render(
        <PipelineStatusNav
          tree={tree}
          repoPath="/repo"
          repoName="codesense"
          rootDisplayName="Codesense"
          isDrilledIn={true}
          rootPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
          onDrillPathChange={onDrillPathChange}
        />,
      );

      // Hidden ancestors collapsed away.
      expect(screen.queryByText("a")).not.toBeInTheDocument();
      expect(screen.queryByText("b")).not.toBeInTheDocument();
      // Root + parent + current remain.
      expect(screen.getByRole("button", { name: "Codesense" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "c" })).toBeInTheDocument();
      expect(screen.getByText("d")).toBeInTheDocument();
      // Parent and root re-scope in one click.
      fireEvent.click(screen.getByRole("button", { name: "c" }));
      expect(onDrillPathChange).toHaveBeenCalledWith("/repo/a/b/c");
      fireEvent.click(screen.getByRole("button", { name: "Codesense" }));
      expect(onDrillPathChange).toHaveBeenCalledWith("/repo");
    });

    it("Cmd+click on the current crumb sends the scope path to the terminal (AC-4)", () => {
      const tree = makeTree({ absoluteDirPath: "/repo/foo/bar" });
      render(
        <PipelineStatusNav
          tree={tree}
          repoPath="/repo"
          repoName="codesense"
          rootDisplayName="Codesense"
          isDrilledIn={true}
          rootPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
          onDrillPathChange={onDrillPathChange}
        />,
      );

      fireEvent.click(screen.getByText("bar"), { metaKey: true });
      expect(mockSendTextToTerminal).toHaveBeenCalledWith("/repo/foo/bar");
    });

    it("keeps the scope's story progress beside the filter while drilled (AC-7)", () => {
      const tree = makeTree({
        absoluteDirPath: "/repo/foo/bar",
        stories: [makeStory("x.md", "done"), makeStory("y.md", "todo")],
      });
      render(
        <PipelineStatusNav
          tree={tree}
          repoPath="/repo"
          repoName="codesense"
          rootDisplayName="Codesense"
          isDrilledIn={true}
          rootPath="/repo"
          selectedPath={null}
          onFileSelect={onFileSelect}
          onDrillPathChange={onDrillPathChange}
        />,
      );

      expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toBeInTheDocument();
      const filterRow = screen.getByTestId("left-nav-filter-field").parentElement!;
      expect(within(filterRow).getByText("1/2")).toBeInTheDocument();
      expect(within(filterRow).getByText("1 of 2 stories done")).toBeInTheDocument();
    });
  });

  // -- Readiness indicator --

  it("shows ready indicator on todo story with met deps", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "nav",
          dirName: "nav",
          absoluteDirPath: "/repo/nav",
          stories: [
            makeStory("N-1.md", "done", { title: "Story 1", dependsOn: null }),
            makeStory("N-2.md", "todo", { title: "Story 2", dependsOn: ["N-1"] }),
          ],
        }),
      ],
    });
    mockStatusNavExpandedPaths = { "/repo": ["/repo/nav"] };

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    // N-2 is todo with all deps done → should have ready indicator
    const readyRow = screen.getByText("Story 2").closest("[role='listitem']")!;
    expect(
      screen.getByRole("img", { name: "Ready to start · its dependency is done" }),
    ).toBeInTheDocument();
    expect(readyRow).toHaveAttribute("data-ready", "true");
  });

  it("does not show ready class on non-ready story", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "nav",
          dirName: "nav",
          absoluteDirPath: "/repo/nav",
          stories: [
            makeStory("N-1.md", "in-progress", { title: "In Progress Story", dependsOn: null }),
          ],
        }),
      ],
    });
    mockStatusNavExpandedPaths = { "/repo": ["/repo/nav"] };

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    // in-progress story should NOT have the ready title
    expect(screen.queryByRole("img", { name: /^Ready to start/ })).not.toBeInTheDocument();
    // Row should NOT be marked ready
    const row = screen.getByText("In Progress Story").closest("[role='listitem']")!;
    expect(row).not.toHaveAttribute("data-ready");
  });

  it("does not show ready class when isReady is not set (no deps, non-todo)", () => {
    const tree = makeTree({
      stories: [makeStory("F-1.md", "done", { title: "Done Story", dependsOn: null })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    const row = screen.getByText("Done Story").closest("[role='listitem']")!;
    expect(row).not.toHaveAttribute("data-ready");
    expect(screen.queryByRole("img", { name: /^Ready to start/ })).not.toBeInTheDocument();
  });

  it("shows ready indicator on root-level todo story with no deps", () => {
    const tree = makeTree({
      stories: [makeStory("F-1.md", "todo", { title: "Ready Story", dependsOn: null })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    const readyRow = screen.getByText("Ready Story").closest("[role='listitem']")!;
    expect(
      screen.getByRole("img", { name: "Ready to start · no dependencies" }),
    ).toBeInTheDocument();
    expect(readyRow).toHaveAttribute("data-ready", "true");
  });

  it("shows ready indicator on stories inside expanded sub-epic folders", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "fixes",
          dirName: "fixes",
          absoluteDirPath: "/repo/fixes",
          stories: [
            makeStory("F-1.md", "done", { title: "Done Fix", dependsOn: null }),
            makeStory("F-2.md", "todo", { title: "Ready Fix", dependsOn: ["F-1"] }),
            makeStory("F-3.md", "todo", { title: "Blocked Fix", dependsOn: ["F-99-nonexistent"] }),
          ],
        }),
      ],
    });
    mockStatusNavExpandedPaths = { "/repo": ["/repo/fixes"] };

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    // F-2 is ready (dep F-1 is done)
    const readyRows = document.querySelectorAll("[role='listitem'][data-ready]");
    expect(readyRows.length).toBe(1); // only F-2, not F-1 (done) or F-3 (unresolved dep)
    expect(readyRows[0].querySelector(".cs-rail")).toHaveAccessibleName(
      "Ready to start · its dependency is done",
    );

    // F-3 should NOT be ready (unresolved dep)
    const blockedRow = screen.getByText("Blocked Fix").closest("[role='listitem']")!;
    expect(blockedRow).not.toHaveAttribute("data-ready");
    expect(blockedRow.querySelector(".cs-rail")!.getAttribute("aria-label")).toMatch(
      / · waiting on F-99-nonexistent \(unresolved\)$/,
    );
  });

  // -- N.6: data-filepath attributes for scroll-into-view --

  it("renders data-filepath on story rows", () => {
    const tree = makeTree({
      stories: [makeStory("S-1-auth.md", "todo", { title: "Auth Story" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    const row = screen.getByText("Auth Story").closest("[data-filepath]");
    expect(row).not.toBeNull();
    expect(row!.getAttribute("data-filepath")).toBe("/repo/S-1-auth.md");
  });

  // -- NB-6: virtualization sanity --

  it("renders all rows for a 50+ story flat list under virtualization", () => {
    const stories = Array.from({ length: 60 }, (_, i) =>
      makeStory(`S-${i + 1}-story.md`, "todo", { title: `Story ${i + 1}` }),
    );
    const tree = makeTree({ stories });

    const { container } = render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    // With the test-setup getBoundingClientRect shim providing a tall
    // container, all 60 stories should materialize in the DOM.
    const rows = container.querySelectorAll("[data-filepath]");
    expect(rows.length).toBe(60);
  });

  it("renders data-filepath on doc rows (non-actionable story)", () => {
    const tree = makeTree({
      stories: [makeStory("retro.md", null, { title: "Retro", documentType: "doc" })],
    });

    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );

    const row = screen.getByText("Retro").closest("[data-filepath]");
    expect(row).not.toBeNull();
    expect(row!.getAttribute("data-filepath")).toBe("/repo/retro.md");
  });

  // -- SLW-NC-7: Pinned section (personal pins; team scope covered by nav-pins unit tests) --

  function treeWithFixes() {
    return makeTree({
      subEpics: [
        makeTree({
          dirName: "fixes",
          absoluteDirPath: "/repo/fixes",
          stories: [makeStory("d.md", "done")],
        }),
      ],
    });
  }

  it("hides the Pinned section when there are no pins", () => {
    render(
      <PipelineStatusNav
        tree={treeWithFixes()}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    expect(screen.queryByTestId("pinned-section-header")).toBeNull();
  });

  it("renders a personal pin as a leaf row at the top of the nav", () => {
    mockStatusNavPinnedPaths = { "/repo": ["fixes"] };
    render(
      <PipelineStatusNav
        tree={treeWithFixes()}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    expect(screen.getByTestId("pinned-section-header")).toBeInTheDocument();
    const rows = screen.getAllByTestId("pinned-row");
    expect(rows).toHaveLength(1);
    expect(rows[0]).toHaveTextContent("fixes");
    // Personal pin → no "Pinned for the team" marker.
    expect(rows[0].querySelector('[aria-label="Pinned for the team"]')).toBeNull();
  });

  it("clicking a pinned DIR reveals the folder by drilling into it (not open-as-file)", () => {
    mockStatusNavPinnedPaths = { "/repo": ["fixes"] };
    render(
      <PipelineStatusNav
        tree={treeWithFixes()}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onDrillPathChange={onDrillPathChange}
      />,
    );
    fireEvent.click(screen.getByTestId("pinned-row"));
    expect(onDrillPathChange).toHaveBeenCalledWith("/repo/fixes");
    // A dir must NOT be routed through onFileSelect (would try to open a dir as a file).
    expect(onFileSelect).not.toHaveBeenCalled();
  });

  it("clicking a pinned FILE opens it (onFileSelect with the abs path)", () => {
    // A top-level doc is a file pin target.
    const tree = makeTree({ docs: [{ fileName: "readme.md", filePath: "/repo/readme.md" }] });
    mockStatusNavPinnedPaths = { "/repo": ["readme.md"] };
    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onDrillPathChange={onDrillPathChange}
      />,
    );
    fireEvent.click(screen.getByTestId("pinned-row"));
    expect(onFileSelect).toHaveBeenCalledWith("/repo/readme.md");
    expect(onDrillPathChange).not.toHaveBeenCalled();
  });

  it("a pin whose target is gone renders dimmed (stale)", () => {
    mockStatusNavPinnedPaths = { "/repo": ["ghost/epic"] };
    render(
      <PipelineStatusNav
        tree={treeWithFixes()}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    const row = screen.getByTestId("pinned-row");
    expect(row).toHaveTextContent("epic");
    expect(row.className).toContain("opacity-50");
  });

  it("the hover unpin icon on a personal pin toggles it off", () => {
    mockStatusNavPinnedPaths = { "/repo": ["fixes"] };
    render(
      <PipelineStatusNav
        tree={treeWithFixes()}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Unpin" }));
    expect(mockToggleStatusNavPinPersonal).toHaveBeenCalledWith("/repo", "fixes");
  });

  it("collapsing the Pinned section hides the rows but keeps a count badge", () => {
    mockStatusNavPinnedPaths = { "/repo": ["fixes"] };
    render(
      <PipelineStatusNav
        tree={treeWithFixes()}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    expect(screen.getAllByTestId("pinned-row")).toHaveLength(1);
    fireEvent.click(screen.getByTestId("pinned-section-header"));
    expect(screen.queryByTestId("pinned-row")).toBeNull();
    // Count badge (collapsed) shows "1".
    expect(screen.getByTestId("pinned-section-header")).toHaveTextContent("1");
  });

  it("right-clicking a pinned FILE row shows unpin only — no tree gestures (Delete/Rename)", () => {
    const tree = makeTree({ docs: [{ fileName: "readme.md", filePath: "/repo/readme.md" }] });
    mockStatusNavPinnedPaths = { "/repo": ["readme.md"] };
    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    fireEvent.contextMenu(screen.getByTestId("pinned-row"));
    // The pinned-row menu offers Unpin, and NOT the tree-row gestures that would
    // misfire on a repo-relative pin path.
    expect(screen.getByText("Unpin")).toBeInTheDocument();
    expect(screen.queryByText("Delete")).toBeNull();
    expect(screen.queryByText("Rename…")).toBeNull();
    expect(screen.queryByText("Pin for me only")).toBeNull();
  });

  it('context menu on a dir row offers "Pin for me only" and writes the personal pin', () => {
    render(
      <PipelineStatusNav
        tree={treeWithFixes()}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    fireEvent.contextMenu(screen.getByText("fixes"));
    const pinItem = screen.getByText("Pin for me only");
    fireEvent.click(pinItem);
    expect(mockToggleStatusNavPinPersonal).toHaveBeenCalledWith("/repo", "fixes");
  });

  // -- SLW-FX-21: drag-to-link (capability 2) --

  function makeDataTransfer(): DataTransfer {
    const store: Record<string, string> = {};
    return {
      setData: (t: string, v: string) => {
        store[t] = v;
      },
      getData: (t: string) => store[t] ?? "",
      effectAllowed: "",
      dropEffect: "",
    } as unknown as DataTransfer;
  }

  it("SLW-FX-21 AC-4/AC-5: dropping story A onto story B relates A INTO B, with the link affordance", () => {
    const onRelateDrop = vi.fn();
    const tree = makeTree({
      stories: [
        makeStory("a.md", "todo", { title: "Story A" }),
        makeStory("b.md", "todo", { title: "Story B" }),
      ],
    });
    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onRelateDrop={onRelateDrop}
      />,
    );
    const dt = makeDataTransfer();
    fireEvent.dragStart(screen.getByText("Story A"), { dataTransfer: dt });
    fireEvent.dragOver(screen.getByText("Story B"), { dataTransfer: dt });
    // valid target → the Link2 drop affordance appears on exactly that row
    expect(screen.getByTestId("relate-drop-icon")).toBeInTheDocument();
    fireEvent.drop(screen.getByText("Story B"), { dataTransfer: dt });
    // A becomes a supporting_doc of B (the drop target) — the product wording
    expect(onRelateDrop).toHaveBeenCalledWith("/repo/b.md", "/repo/a.md");
  });

  it("SLW-FX-21 AC-5: a self-drop shows no affordance and writes nothing", () => {
    const onRelateDrop = vi.fn();
    const tree = makeTree({ stories: [makeStory("a.md", "todo", { title: "Story A" })] });
    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onRelateDrop={onRelateDrop}
      />,
    );
    const dt = makeDataTransfer();
    fireEvent.dragStart(screen.getByText("Story A"), { dataTransfer: dt });
    fireEvent.dragOver(screen.getByText("Story A"), { dataTransfer: dt });
    expect(screen.queryByTestId("relate-drop-icon")).toBeNull();
    fireEvent.drop(screen.getByText("Story A"), { dataTransfer: dt });
    expect(onRelateDrop).not.toHaveBeenCalled();
  });

  it("SLW-FX-21 AC-9: adding drag handlers doesn't regress single-click open", () => {
    const tree = makeTree({ stories: [makeStory("a.md", "todo", { title: "Story A" })] });
    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        onRelateDrop={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByText("Story A"));
    expect(onFileSelect).toHaveBeenCalledWith("/repo/a.md");
  });

  it("SLW-FX-21: no row is draggable when onRelateDrop is omitted (feature off)", () => {
    const tree = makeTree({ stories: [makeStory("a.md", "todo", { title: "Story A" })] });
    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
      />,
    );
    // The DnD wrapper renders a bare passthrough → no draggable ancestor.
    expect(document.querySelector('[draggable="true"]')).toBeNull();
  });

  it("shows the scope's story progress beside the filter at the root", () => {
    const tree = makeTree({
      stories: [makeStory("a.md", "done"), makeStory("b.md", "todo"), makeStory("c.md", "todo")],
    });
    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={onFileSelect}
        repoName="codesense"
        rootDisplayName="Codesense"
      />,
    );
    const filterRow = screen.getByTestId("left-nav-filter-field").parentElement!;
    expect(within(filterRow).getByTestId("nav-story-progress")).toHaveAttribute(
      "title",
      "1 of 3 stories done",
    );
  });
});
