// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { renderRow, type RowInteractionModel } from "./rows";
import type { FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";
import { buildStoryRef } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import type { StatusState } from "@/tauri/commands";
import type { StoryRef } from "@/shared/lib/pipeline-graph/types";

// SW-FX-81: a nav StoryRow must render its title text beside the status ring.
// The shared `StatusSelectorPopover` trigger wraps only the 14px status icon, so
// it must HUG that icon — a full-width trigger (the 273cd6d42 canvas relayout)
// takes the whole row's flex-basis and starves the adjacent `flex-1` title
// button to zero width, blanking every story row. These are the regression guard.

const TAXONOMY: StatusState[] = [
  { id: "todo", label: "To Do", color: "#8899aa", icon: "Circle", order: 0, role: "not_started" },
];

afterEach(() => {
  useStatusTaxonomyStore.getState().resetForTest();
});

function storyItem(fileName: string, over: Partial<StoryRef> = {}): FlatNavItem {
  return {
    type: "story",
    story: {
      ...buildStoryRef(fileName, { absolutePath: `/repo/${fileName}`, status: "todo" }),
      ...over,
    },
    depth: 0,
    isReady: false,
    blockedBy: [],
  };
}

function rowModel(over: Partial<RowInteractionModel>): RowInteractionModel {
  return {
    teamKey: "acme:team",
    selectedPath: null,
    onFileSelect: vi.fn(),
    onContextMenu: vi.fn(),
    toggleExpand: vi.fn(),
    toggleGenericExpand: vi.fn(),
    statusPopoverOpenMap: new Map(),
    setStatusPopoverOpen: vi.fn(),
    activeTeam: null,
    onTogglePinnedSection: vi.fn(),
    onPinnedSelect: vi.fn(),
    onPinnedUnpin: vi.fn(),
    onPinnedContextMenu: vi.fn(),
    ...over,
  };
}

describe("StoryRow — SW-FX-81 title renders beside the status ring", () => {
  it("renders the filename-stem title text (R-14 nav label)", () => {
    useStatusTaxonomyStore.getState().setStates(TAXONOMY);
    render(renderRow(storyItem("SW-FX-81-left-nav.md"), rowModel({ onStatusChange: vi.fn() })));

    // Title is the .md-stripped stem — the R-14 nav label.
    expect(screen.getByText("SW-FX-81-left-nav")).toBeInTheDocument();
  });

  it("keeps the status trigger hugging its icon while the title button stays flex-1 (not starved)", () => {
    useStatusTaxonomyStore.getState().setStates(TAXONOMY);
    render(renderRow(storyItem("SW-FX-81-left-nav.md"), rowModel({ onStatusChange: vi.fn() })));

    // The shared trigger must NOT be full-width: a `w-full` icon-wrapper takes
    // the row's flex-basis and collapses the title. It must hug (inline-flex).
    const trigger = screen.getByRole("button", { name: "Change status" });
    expect(trigger.className).not.toContain("w-full");
    expect(trigger.className).toContain("inline-flex");

    // The title button remains the growing, shrink-safe child (flex-1 + min-w-0)
    // so the stem gets the width and only `truncate`s when genuinely too narrow.
    const titleButton = screen.getByText("SW-FX-81-left-nav").closest("button")!;
    expect(titleButton.className).toContain("flex-1");
    expect(titleButton.className).toContain("min-w-0");
  });
});

// SW-LN-B42: a story with a metadata title reads "<id> <title>" (the id in muted ink), not its stem.
describe("StoryRow — metadata title (SW-LN-B42)", () => {
  it("shows the frontmatter title after the id, keeping the file name in the tooltip", () => {
    useStatusTaxonomyStore.getState().setStates(TAXONOMY);
    render(
      renderRow(
        storyItem("TR-1-api-health-endpoint.md", {
          storyId: "TR-1",
          openTags: { title: ["API health endpoint: returns build info"] },
        }),
        rowModel({}),
      ),
    );
    const label = screen.getByTestId("nav-story-title");
    expect(label).toHaveTextContent("TR-1API health endpoint: returns build info");
    expect(screen.getByText("TR-1")).toHaveClass("text-muted-foreground");
    expect(screen.queryByText("TR-1-api-health-endpoint")).toBeNull();
    expect(label.closest("button")!.getAttribute("title")).toContain("TR-1-api-health-endpoint.md");
  });
});
