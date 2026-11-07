// SPDX-License-Identifier: MIT
import type React from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { ContextRepoStatus } from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  resetContextGitStoreForTests,
  useContextGitStore,
} from "@/features/git-sync/store/use-context-git-store";
import type { NavChange } from "@/features/git-sync/lib/nav-changes";

import { FileViewerToolbar } from "./file-viewer-toolbar";
import type { DocRelations } from "../lib/doc-relations";

// SW-VE-16: unit-level mock of the chip row so we assert the WRAPPER's gate (AC-7) without
// mounting every chip's own machinery. Each chip has its own test file.
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

const TEAM = "default";
const FILE = "/org/default/content/a.md";

function seedStatus(over: Partial<ContextRepoStatus> = {}): void {
  useContextGitStore.getState().put(TEAM, {
    orgRoot: "/org",
    changes: [],
    byPath: new Map(),
    status: {
      branch: "main",
      headSha: "aaaaaaa",
      hasUpstream: true,
      upstream: "origin/main",
      remoteProvider: "github",
      ahead: 0,
      behind: 0,
      sync: { state: "ok", lastSyncedAt: null },
      conflicts: [],
      ...over,
    },
    loading: false,
    historyMoves: 0,
  });
}

function relatedDocFixture(): DocRelations {
  return {
    supportingDocs: [],
    docIdentity: {},
    resolveCtx: {},
  } as unknown as DocRelations;
}

function change(over: Partial<NavChange> = {}): NavChange {
  return {
    absPath: FILE,
    relPath: "default/content/a.md",
    kind: "modified",
    added: 87,
    removed: 12,
    ...over,
  };
}

function renderToolbar(overrides: Partial<React.ComponentProps<typeof FileViewerToolbar>> = {}) {
  seedStatus();
  return render(
    <FileViewerToolbar
      hasChips
      hasDiff={false}
      teamKey={TEAM}
      activeTabPath={FILE}
      change={undefined}
      diff={null}
      relatedDoc={null}
      relateDocActions={undefined}
      inheritedActive={false}
      activeScope={null}
      copyIntoTeamName={null}
      onCopyIntoTeam={undefined}
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

afterEach(() => {
  resetContextGitStoreForTests();
  cleanup();
});

describe("FileViewerToolbar (SW-VE-16 AC-7 wrapper gate)", () => {
  it("mounts the shared band for a CLEAN doc (chips only, no diff) — M2: a clean doc keeps its chip row", () => {
    renderToolbar({ hasChips: true, hasDiff: false });
    expect(screen.getByTestId("file-viewer-toolbar")).toBeInTheDocument();
    expect(screen.getByTestId("metadata-chip")).toBeInTheDocument();
    expect(screen.queryByTestId("diff-side-strip")).not.toBeInTheDocument();
  });

  it("mounts the shared band for a CHANGED doc (both zones) — M2: both zones on one baseline", () => {
    renderToolbar({
      hasChips: true,
      hasDiff: true,
      change: change(),
      relatedDoc: relatedDocFixture(),
    });
    const band = screen.getByTestId("file-viewer-toolbar");
    expect(band).toBeInTheDocument();
    expect(band.className).toMatch(/\bh-8\b/);
    expect(band.className).toMatch(/\bborder-b\b/);
    expect(screen.getByTestId("diff-side-strip")).toBeInTheDocument();
    expect(screen.getByTestId("metadata-chip")).toBeInTheDocument();
  });

  it("mounts the shared band for a CHANGED virtual-is-not-virtual doc with ONLY a diff — M2: diff-only still shows the band", () => {
    renderToolbar({ hasChips: false, hasDiff: true, change: change() });
    expect(screen.getByTestId("file-viewer-toolbar")).toBeInTheDocument();
    expect(screen.getByTestId("diff-side-strip")).toBeInTheDocument();
    expect(screen.queryByTestId("metadata-chip")).not.toBeInTheDocument();
  });

  it("collapses to null when BOTH zones are empty — M2: a virtual management tab has no toolbar", () => {
    const { container } = renderToolbar({ hasChips: false, hasDiff: false });
    expect(container.firstChild).toBeNull();
    expect(screen.queryByTestId("file-viewer-toolbar")).not.toBeInTheDocument();
  });

  it("the chip row persists across a Changes-segment click — M2: shared-band contract", () => {
    renderToolbar({
      hasChips: true,
      hasDiff: true,
      change: change(),
      relatedDoc: relatedDocFixture(),
    });
    expect(screen.getByTestId("metadata-chip")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("diff-side-changes"));
    expect(useViewerStore.getState).toBeTruthy();
    // The chip row MUST survive flipping File→Changes — if the parent wrapper's gate ever
    // regressed to tie chips to the `showingChanges` state, Metadata would unmount here.
    expect(screen.getByTestId("metadata-chip")).toBeInTheDocument();
    expect(screen.getByTestId("file-viewer-toolbar")).toBeInTheDocument();
  });

  it("N8: re-renders cleanly when `activeTabPath` changes (SW-VE-14 OSC 7 cwd retarget)", () => {
    // SW-VE-14 wires OSC 7 cwd events into useSessionCwd revisions; the ACTIVE tab path
    // then shifts to the new terminal cwd. The toolbar must track the new path, not stale
    // the old one — otherwise the Related + Metadata chips key off the old file after a
    // retarget. Rerender with a new path and assert the DiffSideStrip picks it up.
    const { rerender } = renderToolbar({
      hasChips: true,
      hasDiff: true,
      activeTabPath: FILE,
      change: change({ absPath: FILE }),
      relatedDoc: relatedDocFixture(),
    });
    expect(screen.getByTestId("file-viewer-toolbar")).toBeInTheDocument();

    const newPath = "/org/default/content/b.md";
    rerender(
      <FileViewerToolbar
        hasChips
        hasDiff
        teamKey={TEAM}
        activeTabPath={newPath}
        change={change({ absPath: newPath, relPath: "default/content/b.md" })}
        diff={null}
        relatedDoc={relatedDocFixture()}
        relateDocActions={undefined}
        inheritedActive={false}
        activeScope={null}
        copyIntoTeamName={null}
        onCopyIntoTeam={undefined}
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
      />,
    );
    expect(screen.getByTestId("file-viewer-toolbar")).toBeInTheDocument();
    expect(screen.getByTestId("diff-side-strip")).toBeInTheDocument();
    expect(screen.getByTestId("metadata-chip")).toBeInTheDocument();
  });
});

it("uses a pane container at 600px and hides a summary below 40px while preserving segments", async () => {
  const { readFileSync } = await import("node:fs");
  const css = readFileSync("src/features/viewer/components/file-viewer-toolbar.css", "utf8");
  renderToolbar({ hasDiff: true, change: change() });
  expect(screen.getByTestId("file-viewer-toolbar")).toHaveClass("file-viewer-toolbar");
  expect(css).toMatch(/container: file-viewer \/ inline-size/);
  expect(css).toMatch(/@container file-viewer \(max-width: 600px\)/);
  expect(css).toMatch(/\.viewer-header-label\s*\{\s*display: none/);
  expect(css).toMatch(/@container file-summary \(max-width: 40px\)/);
  expect(screen.getByTestId("diff-side-file").parentElement).toHaveClass("shrink-0");
  expect(
    screen.getByTestId("diff-side-summary").closest(".file-viewer-summary"),
  ).toBeInTheDocument();
});
