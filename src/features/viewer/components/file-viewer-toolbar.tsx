// SPDX-License-Identifier: MIT
import "./file-viewer-toolbar.css";
import type { Ref } from "react";

import { DiffSideStrip } from "@/features/git-sync/components/diff-side-strip";
import type { NavChange } from "@/features/git-sync/lib/nav-changes";
import type { FileScope } from "@/features/dashboard/lib/file-scope";
import type { TabDiff } from "@/shared/stores/viewer-store";

import { ViewerToolbar } from "./viewer-toolbar";
import type { RelateDocActions } from "./related-docs-chip";
import type { DocRelations } from "../lib/doc-relations";

/**
 * SW-VE-16: the single `h-8 border-b` toolbar band that hosts the file-viewer's two zones on
 * one baseline (AC-7).
 *
 * - **Left zone** — `DiffSideStrip`: `File` / `Changes` switch over a changed file's tab, plus
 *   a readable `vs <forge> (origin/<branch>)` + diff summary.
 * - **Right zone** — `ViewerToolbar` chips: inherited-file explainer, Related, Metadata,
 *   Read-only + Edit. Each chip is rendered only when its own data applies; `showMetadataChip`
 *   is true for every viewable non-sync doc so the right zone is present for every real doc
 *   (stable content offset — SW-FX-62 AC-7).
 *
 * `hasChips` / `hasDiff` gate whether either zone has content; when **both** are false (a
 * virtual management tab, or an unchanged file with no toolbar chips in scope) the whole band
 * collapses with `return null` so the content region sits flush with the tab chrome. The
 * two-flag gate is the shared-band contract — don't tighten it to `!hasChips || !hasDiff`:
 * clean non-diff docs still need their chip row, and a changed virtual tab shouldn't exist
 * but if one ever does we'd rather drop both than render half a band.
 */
export interface FileViewerToolbarProps {
  /** Whether the right-zone chips (Related / Metadata / Read-only + inherited explainer) have
   *  any content to render for the open tab. The host computes this from the same
   *  `relatedDoc || showMetadataChip || showReadOnlyChip || inheritedActive` gate the
   *  pre-SW-VE-16 standalone `ViewerToolbar` used. */
  hasChips: boolean;
  /** Whether the left zone (File/Changes + remote label + diff summary) has a diff to show for
   *  the open tab. The host computes this from `tabDiff?.[activeTabPath]` or `changeByPath`;
   *  `null` when the tab's path has no change AND no in-flight diff. */
  hasDiff: boolean;
  // ── Left-zone (DiffSideStrip) wiring ───────────────────────────────────────
  teamKey: string;
  activeTabPath: string;
  /** The nav-change row for the open tab, if any — drives the summary count. */
  change: NavChange | undefined;
  /** The in-flight tab-diff for the open tab, if any — tells DiffSideStrip which segment is
   *  pressed and whether to show a History-selected commit vs. the pristine vs-remote view. */
  diff: TabDiff | null;
  // ── Right-zone (ViewerToolbar) wiring ──────────────────────────────────────
  relatedDoc: DocRelations | null;
  relateDocActions: RelateDocActions | undefined;
  inheritedActive: boolean;
  activeScope: FileScope | null;
  copyIntoTeamName?: string | null;
  onCopyIntoTeam?: () => void;
  showMetadataChip: boolean;
  showReadOnlyChip: boolean;
  canEditActive: boolean;
  editingActive: boolean;
  editDirty: boolean;
  onEditActive: () => void;
  metadataOpen: boolean;
  toggleMetadata: () => void;
  metadataValid?: boolean;
  metadataChipRef: Ref<HTMLButtonElement>;
  regionId: string;
  onOpenFile: (filePath: string) => void;
}

export function FileViewerToolbar({
  hasChips,
  hasDiff,
  teamKey,
  activeTabPath,
  change,
  diff,
  relatedDoc,
  relateDocActions,
  inheritedActive,
  activeScope,
  copyIntoTeamName,
  onCopyIntoTeam,
  showMetadataChip,
  showReadOnlyChip,
  canEditActive,
  editingActive,
  editDirty,
  onEditActive,
  metadataOpen,
  toggleMetadata,
  metadataValid,
  metadataChipRef,
  regionId,
  onOpenFile,
}: FileViewerToolbarProps) {
  if (!hasChips && !hasDiff) return null;
  return (
    <div
      className="file-viewer-toolbar flex h-8 flex-shrink-0 items-center gap-2.5 border-b px-2.5"
      style={{
        borderColor: "var(--cs-tab-border, var(--border))",
        backgroundColor: "var(--ibx-muted, var(--muted))",
      }}
      data-testid="file-viewer-toolbar"
    >
      {hasDiff ? (
        <DiffSideStrip teamKey={teamKey} filePath={activeTabPath} change={change} diff={diff} />
      ) : (
        <div className="flex-1" />
      )}
      {hasChips && (
        <ViewerToolbar
          activeTabPath={activeTabPath}
          relatedDoc={relatedDoc}
          relateDocActions={relateDocActions}
          inheritedActive={inheritedActive}
          activeScope={activeScope}
          copyIntoTeamName={copyIntoTeamName}
          onCopyIntoTeam={onCopyIntoTeam}
          showMetadataChip={showMetadataChip}
          showReadOnlyChip={showReadOnlyChip}
          canEditActive={canEditActive}
          editingActive={editingActive}
          editDirty={editDirty}
          onEditActive={onEditActive}
          metadataOpen={metadataOpen}
          toggleMetadata={toggleMetadata}
          metadataValid={metadataValid}
          metadataChipRef={metadataChipRef}
          regionId={regionId}
          onOpenFile={onOpenFile}
        />
      )}
    </div>
  );
}
