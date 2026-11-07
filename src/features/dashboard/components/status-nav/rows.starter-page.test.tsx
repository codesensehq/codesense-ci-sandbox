// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { renderRow, type RowInteractionModel } from "./rows";
import type { FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";

// The team's starter page carries a small house marker in the nav; other rows don't.

const README = "/orgs/acme/web/content/README.md";

function fileItem(path: string): FlatNavItem {
  return {
    type: "generic-file",
    entry: { name: path.split("/").pop()!, path, is_dir: false, extension: "md", size: 1 },
    depth: 0,
  };
}

function docItem(path: string): FlatNavItem {
  return { type: "doc", filePath: path, label: "README", depth: 0 } as FlatNavItem;
}

function rowModel(over: Partial<RowInteractionModel>): RowInteractionModel {
  return {
    teamKey: "acme:web",
    selectedPath: null,
    onFileSelect: vi.fn(),
    onContextMenu: vi.fn(),
    toggleExpand: vi.fn(),
    toggleGenericExpand: vi.fn(),
    statusPopoverOpenMap: new Map(),
    setStatusPopoverOpen: vi.fn(),
    activeTeam: "web",
    onTogglePinnedSection: vi.fn(),
    onPinnedSelect: vi.fn(),
    onPinnedUnpin: vi.fn(),
    onPinnedContextMenu: vi.fn(),
    ...over,
  };
}

describe("renderRow — team starter page marker", () => {
  it("marks the starter page, as a generic file or a doc row", () => {
    const cb = rowModel({ isStarterPage: (p) => p === README });
    const { unmount } = render(renderRow(fileItem(README), cb));
    expect(screen.getByRole("img", { name: "Team starter page" })).toBeInTheDocument();
    unmount();
    render(renderRow(docItem(README), cb));
    expect(screen.getByTestId("starter-page-mark")).toBeInTheDocument();
  });

  it("leaves other rows, and a nav with no starter page, unmarked", () => {
    render(
      renderRow(
        fileItem("/orgs/acme/web/content/notes.md"),
        rowModel({ isStarterPage: (p) => p === README }),
      ),
    );
    render(renderRow(fileItem(README), rowModel({ isStarterPage: undefined })));
    expect(screen.queryByTestId("starter-page-mark")).toBeNull();
  });
});
