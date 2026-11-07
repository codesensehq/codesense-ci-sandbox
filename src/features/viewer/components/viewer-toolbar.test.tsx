// SPDX-License-Identifier: MIT
import type React from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ViewerToolbar, type ViewerToolbarProps } from "./viewer-toolbar";
import type { DocRelations } from "../lib/doc-relations";

// SW-FX-62: the toolbar's ONLY responsibilities are (a) rendering a normal-flow
// docked band and (b) conditionally including each chip. The chips themselves
// have their own tests (related-docs-chip.test.tsx,
// metadata-chip via team-workspace), so mock them to lightweight markers and
// assert only what the band owns.
vi.mock("./related-docs-chip", () => ({
  RelatedDocsChip: () => <div data-testid="related-chip" />,
}));
vi.mock("./metadata-chip", () => ({
  MetadataChip: () => <div data-testid="metadata-chip" />,
}));
vi.mock("@/features/dashboard/components/scope-badge", () => ({
  InheritedFileExplainer: ({ action }: { action?: React.ReactNode }) => (
    <div data-testid="inherited-explainer">{action}</div>
  ),
}));
const relatedDoc = {
  supportingDocs: [],
  docIdentity: {},
  resolveCtx: {},
} as unknown as DocRelations;

function renderToolbar(overrides: Partial<ViewerToolbarProps> = {}) {
  return render(
    <ViewerToolbar
      activeTabPath="docs/a.md"
      relatedDoc={relatedDoc}
      relateDocActions={undefined}
      inheritedActive={false}
      activeScope={null}
      showMetadataChip
      showReadOnlyChip={false}
      canEditActive={false}
      editingActive={false}
      editDirty={false}
      onEditActive={vi.fn()}
      metadataOpen={false}
      toggleMetadata={vi.fn()}
      metadataValid
      metadataChipRef={{ current: null }}
      regionId="metadata-band-region"
      onOpenFile={vi.fn()}
      {...overrides}
    />,
  );
}

describe("ViewerToolbar (SW-FX-62 — docked chip band)", () => {
  afterEach(cleanup);

  it("AC-1/AC-4: renders a normal-flow docked band, not an absolute overlay", () => {
    renderToolbar();
    const band = screen.getByTestId("viewer-toolbar");
    // Normal-flow: no absolute positioning / z-index escape hatch (the bug).
    expect(band.className).not.toMatch(/\babsolute\b/);
    expect(band.className).not.toMatch(/\bz-20\b/);
    // Docked toolbar idiom, mirroring builtin-panels.tsx:93.
    expect(band.className).toMatch(/\bflex\b/);
    expect(band.className).toMatch(/items-center/);
  });

  it("AC-6: hosts its chips when their data is present", () => {
    renderToolbar();
    expect(screen.getByTestId("related-chip")).toBeInTheDocument();
    expect(screen.getByTestId("metadata-chip")).toBeInTheDocument();
  });

  it("AC-7: with no related/versions, the band still renders with only the metadata chip (stable offset)", () => {
    renderToolbar({ relatedDoc: null, showMetadataChip: true });
    expect(screen.getByTestId("viewer-toolbar")).toBeInTheDocument();
    expect(screen.queryByTestId("related-chip")).not.toBeInTheDocument();
    expect(screen.getByTestId("metadata-chip")).toBeInTheDocument();
  });

  it("SW-VE-2 (AC-4): a code file docks the Read-only chip in place of the Metadata chip", () => {
    renderToolbar({
      relatedDoc: null,
      showMetadataChip: false,
      showReadOnlyChip: true,
    });
    expect(screen.getByLabelText("Read-only")).toBeInTheDocument();
    expect(screen.queryByTestId("metadata-chip")).not.toBeInTheDocument();
  });

  it("SW-VE-16 (AC-8): an own-writable file docks the Read-only badge + distinct Edit button, firing onEditActive", () => {
    const onEditActive = vi.fn();
    renderToolbar({
      relatedDoc: null,
      showMetadataChip: false,
      showReadOnlyChip: true,
      canEditActive: true,
      onEditActive,
    });
    expect(screen.getByLabelText("Read-only")).toBeInTheDocument();
    screen.getByRole("button", { name: "Edit" }).click();
    expect(onEditActive).toHaveBeenCalledTimes(1);
  });

  it("SW-VE-3: while editing, the chip reads 'Editing'", () => {
    renderToolbar({
      relatedDoc: null,
      showMetadataChip: false,
      showReadOnlyChip: true,
      canEditActive: true,
      editingActive: true,
      editDirty: true,
    });
    expect(screen.getByLabelText("Editing")).toBeInTheDocument();
  });

  it("SW-XS-20: an inherited doc keeps its chips and adds the explainer with Copy into", () => {
    const onCopyIntoTeam = vi.fn();
    renderToolbar({
      inheritedActive: true,
      activeScope: { repoSlug: null, sourceTeam: "platform", isInherited: true },
      copyIntoTeamName: "default",
      onCopyIntoTeam,
      showReadOnlyChip: true,
      canEditActive: true,
    });
    expect(screen.getByTestId("inherited-explainer")).toBeInTheDocument();
    expect(screen.getByTestId("metadata-chip")).toBeInTheDocument();
    expect(screen.getByLabelText(/Edit/)).toBeInTheDocument();
    screen.getByRole("button", { name: /Copy into default/ }).click();
    expect(onCopyIntoTeam).toHaveBeenCalled();
  });
});

it("keeps Related, Metadata and Edit in order with the right zone centered", () => {
  renderToolbar({ showReadOnlyChip: true, canEditActive: true });
  const toolbar = screen.getByTestId("viewer-toolbar");
  expect(Array.from(toolbar.children).map((child) => child.getAttribute("data-testid"))).toEqual([
    "related-chip",
    "metadata-chip",
    null,
  ]);
  expect(toolbar).toHaveClass("items-center", "flex-shrink-0");
  expect(screen.getByRole("button", { name: "Edit" })).toHaveTextContent("Edit");
});
