// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { renderRow, type RowInteractionModel } from "./rows";
import type { FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";
import type { FileEntry } from "@/tauri/commands";

// SW-CP-27 AC-3: a merge-marked generic config row shows its indicator in the
// tree; an unmarked row does not. Proven through the real `renderRow` dispatch.

function configEntry(path: string): FileEntry {
  return { name: path.split("/").pop()!, path, is_dir: false, extension: "json", size: 10 };
}

function genericFileItem(path: string): FlatNavItem {
  return { type: "generic-file", entry: configEntry(path), depth: 0 };
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

describe("renderRow — SW-CP-27 merge badge", () => {
  it("renders the merge badge for a marked config row", () => {
    const cb = rowModel({ isMergeMarked: (p) => p === "/repo/.mcp.json" });
    render(renderRow(genericFileItem("/repo/.mcp.json"), cb));
    expect(screen.getByTestId("merge-marked-badge")).toBeInTheDocument();
  });

  it("renders no badge for an unmarked row", () => {
    const cb = rowModel({ isMergeMarked: () => false });
    render(renderRow(genericFileItem("/repo/other.json"), cb));
    expect(screen.queryByTestId("merge-marked-badge")).toBeNull();
  });

  it("renders no badge when no mark predicate is supplied (no active team)", () => {
    const cb = rowModel({ isMergeMarked: undefined });
    render(renderRow(genericFileItem("/repo/config.json"), cb));
    expect(screen.queryByTestId("merge-marked-badge")).toBeNull();
  });
});
