// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@/test-utils";

import { PipelineStatusNav } from "./pipeline-status-nav";
import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { useOrgStore } from "@/shared/stores/org-store";

// SW-VE-5 (AC-1/AC-4) + SW-XS-19: per-row scope badges — a source-repo doc
// (the repo tint), an own-team doc (no badge) and a doc inherited from a PARENT
// team (`parent_team` in teams.yml → read-only marker naming that team). The
// owning team rides `DocRef.sourceTeam`; no new command.

vi.mock("@/shared/lib/send-to-terminal", () => ({ sendTextToTerminal: vi.fn() }));

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
        toggleStatusNavExpanded: vi.fn(),
      }),
    {
      subscribe: () => () => {},
      getState: () => ({
        toggleStatusNavExpanded: vi.fn(),
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

const SLUG = "alderco/codesense";
const GLYPH_MAP = new Map([[SLUG, "CD"]]);

beforeEach(() => {
  vi.clearAllMocks();
  // The active team is `default`; `platform` is its parent (inherited → read-only).
  useOrgStore.setState({ activeOrgSlug: "alderco", activeTeamName: "default" });
});

function renderMultiLayer() {
  mockStatusNavExpandedPaths["/repo"] = ["/ctx/layers"];
  const tree = buildTree("root", {
    subEpics: [
      buildTree("layers", {
        dirPath: "layers",
        absoluteDirPath: "/ctx/layers",
        docs: [
          {
            fileName: "own-repo.md",
            filePath: "/ctx/layers/own-repo.md",
            installedSourceRepo: SLUG,
          },
          // SW-XS-20: own files carry no sourceTeam.
          { fileName: "own-team.md", filePath: "/ctx/layers/own-team.md" },
          {
            fileName: "inherited.md",
            filePath: "/ctx/layers/inherited.md",
            sourceTeam: "platform",
          },
        ],
      }),
    ],
  });
  render(
    <PipelineStatusNav
      tree={tree}
      repoPath="/repo"
      selectedPath={null}
      onFileSelect={vi.fn()}
      provenance={{ glyphMap: GLYPH_MAP, isLightTheme: false }}
    />,
  );
}

describe("PipelineStatusNav — SW-VE-5 scope badges (AC-1/AC-4)", () => {
  it("tints a source-repo row with its repo glyph", () => {
    renderMultiLayer();
    const row = screen.getByText("own-repo").closest('[role="listitem"]')!;
    expect(row.textContent).toContain("CD");
    expect(row.querySelector('[data-testid="scope-badge-inherited"]')).toBeNull();
  });

  it("shows no badge on an own-team file", () => {
    renderMultiLayer();
    const row = screen.getByText("own-team").closest('[role="listitem"]')!;
    expect(row.querySelector('[data-testid="scope-badge-inherited"]')).toBeNull();
    expect(row.textContent).not.toContain("CD");
  });

  it("SW-XS-19: a file inherited from a parent team still shows the inherited marker", () => {
    renderMultiLayer();
    const row = screen.getByText("inherited").closest('[role="listitem"]')!;
    const badge = row.querySelector('[data-testid="scope-badge-inherited"]')!;
    expect(badge).not.toBeNull();
    expect(badge.textContent).toContain("platform");
    expect(badge.getAttribute("aria-label")).toBe("Inherited from platform");
  });

  it("does NOT mark own-team files inherited (no false read-only)", () => {
    renderMultiLayer();
    expect(screen.getAllByTestId("scope-badge-inherited")).toHaveLength(1);
  });
});
