// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { renderRow, type RowInteractionModel } from "./rows";
import type { FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";
import type { FileEntry } from "@/tauri/commands";

// SLW-PT-18: the per-file ⋯ move/copy/remove affordance mounts on real content
// rows via `moveActionSlot`, proven through the real `renderRow` dispatch. The
// slot is a render-prop, so this stays a pure wiring test (no store coupling).

function configEntry(path: string): FileEntry {
  return { name: path.split("/").pop()!, path, is_dir: false, extension: "md", size: 10 };
}

function genericFileItem(path: string): FlatNavItem {
  return { type: "generic-file", entry: configEntry(path), depth: 0 };
}

function docItem(path: string, installed = false, label?: string): FlatNavItem {
  return {
    type: "doc",
    filePath: path,
    label: label ?? path.split("/").pop()!,
    depth: 0,
    installed,
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

const slot = (filePath: string, fileName: string) => (
  <button
    type="button"
    data-testid="move-action-trigger"
    aria-label={`More actions for ${fileName}`}
  >
    ⋯ {filePath}
  </button>
);

describe("renderRow — SLW-PT-18 move-action slot", () => {
  it("mounts the ⋯ action on a generic content file row", () => {
    const cb = rowModel({ moveActionSlot: slot });
    render(renderRow(genericFileItem("/repo/plan.md"), cb));
    expect(screen.getByTestId("move-action-trigger")).toHaveAttribute(
      "aria-label",
      "More actions for plan.md",
    );
  });

  it("mounts the ⋯ action on a real content doc row", () => {
    const cb = rowModel({ moveActionSlot: slot });
    render(renderRow(docItem("/repo/epic.md"), cb));
    expect(screen.getByTestId("move-action-trigger")).toBeInTheDocument();
  });

  it("suppresses the ⋯ action on a browse-only installed doc row", () => {
    const cb = rowModel({ moveActionSlot: slot });
    render(renderRow(docItem("installed://acme/repo/doc.md", true), cb));
    expect(screen.queryByTestId("move-action-trigger")).toBeNull();
  });

  it("renders no ⋯ action when no slot is wired", () => {
    const cb = rowModel({ moveActionSlot: undefined });
    render(renderRow(genericFileItem("/repo/plan.md"), cb));
    expect(screen.queryByTestId("move-action-trigger")).toBeNull();
  });

  // SLW-PT-B5: the nav-tree strips the viewer extension for the display `label`
  // (`movable-doc.md` -> `movable-doc`). The move/copy/remove slot must still
  // receive the REAL on-disk logical name (extension intact) so the backend can
  // key `content/<logicalName>` on the file that actually exists. Passing the
  // stripped label made the move backend join `content/movable-doc` (missing) and
  // soft-reject every `.md` move. Assert through the real renderRow -> DocRow ->
  // slot wiring — the same chain the host feeds context-mobility-host.
  it("hands the slot the extension-bearing name for a doc row whose display label is stripped", () => {
    const cb = rowModel({ moveActionSlot: slot });
    // label is stripped (no `.md`) — exactly what flatten-nav-tree produces.
    render(renderRow(docItem("/repo/movable-doc.md", false, "movable-doc"), cb));
    // The slot (which context-mobility-host turns into logicalName) gets the real filename.
    expect(screen.getByTestId("move-action-trigger")).toHaveAttribute(
      "aria-label",
      "More actions for movable-doc.md",
    );
    // But the row still DISPLAYS the stripped label — no regression to display.
    expect(screen.getByText("movable-doc")).toBeInTheDocument();
  });

  // SLW-PT-B5: the generic-file row strips `.excalidraw` for display but must
  // likewise key the slot on the real on-disk basename.
  it("hands the slot the extension-bearing name for a generic excalidraw row whose display label is stripped", () => {
    const cb = rowModel({ moveActionSlot: slot });
    const entry: FileEntry = {
      name: "diagram.excalidraw",
      path: "/repo/diagram.excalidraw",
      is_dir: false,
      extension: "excalidraw",
      size: 10,
    };
    render(renderRow({ type: "generic-file", entry, depth: 0 }, cb));
    expect(screen.getByTestId("move-action-trigger")).toHaveAttribute(
      "aria-label",
      "More actions for diagram.excalidraw",
    );
    // Display label stays stripped (the `.excalidraw` disambiguator is dropped).
    expect(screen.getByText("diagram")).toBeInTheDocument();
  });
});
