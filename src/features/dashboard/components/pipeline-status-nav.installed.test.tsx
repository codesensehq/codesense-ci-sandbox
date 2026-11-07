// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { PipelineStatusNav } from "./pipeline-status-nav";
import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { sendTextToTerminal } from "@/shared/lib/send-to-terminal";

// SLW-NC-1: installed group rows (synthetic `installed://` absoluteDirPath) are
// browse-only — path-based affordances (cmd+click → terminal cd, context menu)
// must be suppressed, while a real context folder keeps them.

vi.mock("@/shared/lib/send-to-terminal", () => ({ sendTextToTerminal: vi.fn() }));

const mockToggleStatusNavExpanded = vi.fn();
const mockStatusNavExpandedPaths: Record<string, string[]> = {};

vi.mock("@/shared/lib/team-key", () => ({
  useActiveTeamKey: () => "/repo",
  getActiveTeamKey: () => "/repo",
  buildTeamKey: (orgSlug: string, teamName: string | null) => `${orgSlug}:${teamName ?? "_none"}`,
  NO_TEAM_SENTINEL: "_no_team:_none",
}));

vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(
    (selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        navLens: {},
        statusNavExpandedPaths: mockStatusNavExpandedPaths,
        toggleStatusNavExpanded: mockToggleStatusNavExpanded,
      }),
    {
      subscribe: () => () => {},
      getState: () => ({
        toggleStatusNavExpanded: mockToggleStatusNavExpanded,
        readViewState: () => undefined,
        captureViewState: () => {},
      }),
    },
  ),
}));

vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, listDirectory: () => Promise.resolve([]) };
});

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

const mockSend = vi.mocked(sendTextToTerminal);

beforeEach(() => vi.clearAllMocks());

function renderNav() {
  // A real context folder + an installed group (synthetic absoluteDirPath).
  const tree = buildTree("root", {
    subEpics: [
      buildTree("context-epic", { dirPath: "in-progress/context-epic" }),
      buildTree("alderco/codesense", {
        dirPath: "alderco/codesense",
        absoluteDirPath: "installed://alderco/codesense/",
      }),
    ],
  });
  render(
    <PipelineStatusNav tree={tree} repoPath="/repo" selectedPath={null} onFileSelect={vi.fn()} />,
  );
}

describe("PipelineStatusNav — installed groups are browse-only (SLW-NC-1)", () => {
  it("cmd+click on a CONTEXT folder sends its path to the terminal", () => {
    renderNav();
    const btn = screen.getByText("context-epic").closest("button")!;
    fireEvent.click(btn, { metaKey: true });
    expect(mockSend).toHaveBeenCalledWith("/ctx/pipe/in-progress/context-epic");
  });

  it("cmd+click on an INSTALLED group does NOT send a (bogus) path to the terminal", () => {
    renderNav();
    const btn = screen.getByText("alderco/codesense").closest("button")!;
    fireEvent.click(btn, { metaKey: true });
    expect(mockSend).not.toHaveBeenCalled();
  });

  it("right-click on a CONTEXT folder opens the context menu (Copy path)", () => {
    renderNav();
    fireEvent.contextMenu(screen.getByText("context-epic").closest("button")!);
    expect(screen.queryByText("Copy path")).not.toBeNull();
  });

  it("right-click on an INSTALLED group opens NO context menu", () => {
    renderNav();
    fireEvent.contextMenu(screen.getByText("alderco/codesense").closest("button")!);
    expect(screen.queryByText("Copy path")).toBeNull();
  });

  // Docs UNDER an installed group carry a REAL winningSourcePath, so the
  // browse-only guard must reach them too (else delete/rename could mutate the
  // installed source repo).
  function renderNavWithDocs() {
    mockStatusNavExpandedPaths["/repo"] = [
      "/ctx/pipe/in-progress/context-epic",
      "installed://alderco/codesense/",
    ];
    const tree = buildTree("root", {
      subEpics: [
        buildTree("context-epic", {
          dirPath: "in-progress/context-epic",
          docs: [
            { fileName: "ctx-doc.md", filePath: "/ctx/pipe/in-progress/context-epic/ctx-doc.md" },
          ],
        }),
        buildTree("alderco/codesense", {
          dirPath: "alderco/codesense",
          absoluteDirPath: "installed://alderco/codesense/",
          docs: [{ fileName: "inst-doc.md", filePath: "/real/layer/store/inst-doc.md" }],
        }),
      ],
    });
    render(
      <PipelineStatusNav tree={tree} repoPath="/repo" selectedPath={null} onFileSelect={vi.fn()} />,
    );
  }

  it("right-click on a CONTEXT doc opens the context menu (Copy path)", () => {
    renderNavWithDocs();
    fireEvent.contextMenu(screen.getByText("ctx-doc").closest("button")!);
    expect(screen.queryByText("Copy path")).not.toBeNull();
  });

  it("right-click on an INSTALLED doc opens NO context menu (no delete/rename of source files)", () => {
    renderNavWithDocs();
    fireEvent.contextMenu(screen.getByText("inst-doc").closest("button")!);
    expect(screen.queryByText("Copy path")).toBeNull();
    expect(screen.queryByText("Delete")).toBeNull();
  });
});

// SLW-NC-1 Phase 3 (P3-AC-3b/4): flat-merged source rows carry a color tint;
// source docs that shadow a team-workspace version carry a shadowed hint badge.
describe("PipelineStatusNav — Phase 3 flat-merge provenance (SLW-NC-1 P3-AC-3b/4)", () => {
  const GLYPH_MAP = new Map([["alderco/codesense", "CD"]]);
  const SLUG = "alderco/codesense";

  // Mimics a flat-merged tree: a CONTEXT dir (`_bmad-output`, real path) holding
  // a source doc that shadows (`prd.md`), a source doc that doesn't (`epics.md`),
  // and a context doc (`notes.md`); plus a SOURCE-ONLY dir (`src`, installed://).
  function renderMerged(provenance = { glyphMap: GLYPH_MAP, isLightTheme: false }) {
    mockStatusNavExpandedPaths["/repo"] = ["/ctx/_bmad-output"];
    const tree = buildTree("root", {
      subEpics: [
        buildTree("_bmad-output", {
          dirPath: "_bmad-output",
          absoluteDirPath: "/ctx/_bmad-output",
          docs: [
            {
              fileName: "prd.md",
              filePath: "/src/prd.md",
              installedSourceRepo: SLUG,
              shadowsTeamFile: true,
            },
            {
              fileName: "epics.md",
              filePath: "/src/epics.md",
              installedSourceRepo: SLUG,
              shadowsTeamFile: false,
            },
            { fileName: "notes.md", filePath: "/ctx/_bmad-output/notes.md" },
          ],
        }),
        buildTree("src", { dirPath: "src", absoluteDirPath: "installed://alderco/codesense/src" }),
      ],
    });
    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={vi.fn()}
        provenance={provenance}
      />,
    );
  }

  it("tints a source-only dir with the repo color+short-code, keeping its real name (P3-AC-4)", () => {
    renderMerged();
    const srcBtn = screen.getByText("src").closest("button")!;
    expect(srcBtn.textContent).toContain("CD"); // tint chip
    expect(srcBtn.textContent).toContain("src"); // real folder name kept
  });

  it("tints source docs and flags the shadowing one only (P3-AC-3b/4)", () => {
    renderMerged();
    // SLW-NC-2: a doc row is now a `role=listitem` wrapper (inner open button +
    // sibling tint/badge) — the badge became the layered-detail popover trigger,
    // which can't nest inside the row's open button. Query the row, not the button.
    const prdRow = screen.getByText("prd").closest('[role="listitem"]')!;
    expect(prdRow.textContent).toContain("CD"); // tint
    expect(prdRow.querySelector('[data-testid="shadowed-badge"]')).not.toBeNull(); // shadows
    const epicsRow = screen.getByText("epics").closest('[role="listitem"]')!;
    expect(epicsRow.textContent).toContain("CD"); // source → tint
    expect(epicsRow.querySelector('[data-testid="shadowed-badge"]')).toBeNull(); // no collision
    // Exactly one shadowed badge in the whole nav.
    expect(screen.getAllByTestId("shadowed-badge")).toHaveLength(1);
  });

  it("context rows carry no tint and no shadowed badge (P3-AC-4)", () => {
    renderMerged();
    const notesRow = screen.getByText("notes").closest('[role="listitem"]')!;
    expect(notesRow.textContent).not.toContain("CD");
    expect(notesRow.querySelector('[data-testid="shadowed-badge"]')).toBeNull();
    const bmadBtn = screen.getByText("_bmad-output").closest("button")!;
    expect(bmadBtn.textContent).not.toContain("CD"); // merged context dir — untinted
  });

  it("renders no tint when no glyph map is supplied (graceful fallback)", () => {
    renderMerged({ glyphMap: new Map(), isLightTheme: false });
    expect(screen.queryByText("CD")).toBeNull();
    expect(screen.getByText("src")).toBeInTheDocument(); // rows still render
  });
});
