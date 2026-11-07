// SPDX-License-Identifier: MIT
import type { Ref } from "react";

import { RelatedDocsChip, type RelateDocActions } from "./related-docs-chip";
import { MetadataChip } from "./metadata-chip";
import { ReadOnlyChip } from "./read-only-chip";
import { CopyPlus } from "lucide-react";

import { InheritedFileExplainer } from "@/features/dashboard/components/scope-badge";
import type { FileScope } from "@/features/dashboard/lib/file-scope";
import type { DocRelations } from "../lib/doc-relations";

export interface ViewerToolbarProps {
  /** Path of the open doc — keys the Related chip + feeds Versions' activePath. */
  activeTabPath: string;
  /** Related supporting docs + backlinks for the open doc, or null when none apply. */
  relatedDoc: DocRelations | null;
  /** Write actions (relate/unlink) for the Related chip; undefined when read-only. */
  relateDocActions: RelateDocActions | undefined;
  /** SW-XS-20: true when the open doc comes from a parent team — shows the
   *  explainer (edits update that team's file) beside the usual chips. */
  inheritedActive: boolean;
  /** Resolved scope for the explainer (parent team / layer); null when unknown. */
  activeScope: FileScope | null;
  /** SW-XS-20: the active team, for "Copy into ‹team›" on an inherited doc. */
  copyIntoTeamName?: string | null;
  /** SW-XS-20: copy the open inherited doc into the active team (same path). */
  onCopyIntoTeam?: () => void;
  /** Whether the Metadata chip is offered (true for every viewable, non-sync doc). */
  showMetadataChip: boolean;
  /** SW-VE-2 (AC-4) + SW-VE-8: whether the read-only / "Read-only · Edit" chip
   *  is shown. True for every `code`-type file (read-only is its visible
   *  default) AND for any editable doc (markdown/html) as the Edit switch. For a
   *  code file it's mutually exclusive with the Metadata chip; for an editable
   *  DOC it deliberately COEXISTS with the Metadata chip (the Edit affordance
   *  sits beside it, AC-14) — the two are no longer strictly exclusive. */
  showReadOnlyChip: boolean;
  /** SW-VE-3: the open file is in an own-writable scope, so the read-only chip
   *  becomes the deliberate "Read-only · Edit" mode switch. */
  canEditActive: boolean;
  /** SW-VE-3: edit mode is active for the open file (chip reads "Editing"). */
  editingActive: boolean;
  /** SW-VE-3: the active edit buffer has unsaved changes (chip dirty dot). */
  editDirty: boolean;
  /** SW-VE-3: enter edit mode for the open file (only wired when `canEditActive`). */
  onEditActive: () => void;
  /** Metadata band open state (drives the chip caret + aria-expanded). */
  metadataOpen: boolean;
  /** Toggle the full-width metadata band open/closed. */
  toggleMetadata: () => void;
  /** false when the doc's metadata is invalid → amber dot; true/undefined = valid. */
  metadataValid?: boolean;
  /** Ref to the Metadata chip button (focus returns here when the band closes). */
  metadataChipRef: Ref<HTMLButtonElement>;
  /** id of the metadata band region, for the chip's aria-controls. */
  regionId: string;
  /** Open a file from a chip popover (Related links, Versions switcher). */
  onOpenFile: (filePath: string) => void;
}

/**
 * SW-FX-62: the document viewer's docked toolbar band — the right-zone chip row. Hosts the
 * Related / Versions / Metadata / Read-only chips as a normal-flow row that is a SIBLING
 * ABOVE the content scroll region. Each chip is included only when its data is present;
 * because `showMetadataChip` is true for every viewable non-sync doc, the row is present for
 * every real doc, so the content offset is stable across docs (SW-FX-62 AC-7).
 *
 * SW-VE-16: shares its outer band with the left-zone `DiffSideStrip` (File/Changes toggle +
 * remote label + diff summary) when a diff applies — the parent wraps both in a single
 * `h-8 border-b` flex row so the two zones sit on one baseline (AC-7). This component drops
 * its own outer height/border responsibility and just lays its chips out right-aligned; the
 * wrapper owns the band chrome. The chips themselves are unchanged — SW-FX-79's portaled
 * popovers continue to work.
 */
export function ViewerToolbar({
  activeTabPath,
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
}: ViewerToolbarProps) {
  return (
    <div
      data-testid="viewer-toolbar"
      className="flex items-center justify-end gap-1.5 flex-shrink-0 text-muted-foreground"
    >
      {/* SW-XS-20: a file from a parent team says where edits go, with
          "Copy into ‹team›" right after the line. */}
      {inheritedActive && (
        <InheritedFileExplainer
          scope={activeScope}
          action={
            copyIntoTeamName && onCopyIntoTeam && activeScope?.sourceTeam ? (
              <button
                type="button"
                className="ml-1 inline-flex flex-shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-xs text-foreground hover:bg-[color-mix(in_srgb,var(--foreground)_9%,transparent)] focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring"
                title={`Make ${copyIntoTeamName}'s own copy at the same path. It replaces ${activeScope.sourceTeam}'s version for ${copyIntoTeamName}.`}
                data-testid="copy-into-team"
                onClick={onCopyIntoTeam}
              >
                <CopyPlus size={12} aria-hidden />
                Copy into {copyIntoTeamName}
              </button>
            ) : undefined
          }
        />
      )}
      {relatedDoc && (
        <RelatedDocsChip
          key={activeTabPath}
          activeTabPath={activeTabPath}
          supportingDocs={relatedDoc.supportingDocs}
          docIdentity={relatedDoc.docIdentity}
          resolveCtx={relatedDoc.resolveCtx}
          onOpenFile={onOpenFile}
          docActions={relateDocActions}
          className="relative"
        />
      )}
      {showMetadataChip && (
        <MetadataChip
          ref={metadataChipRef}
          open={metadataOpen}
          onToggle={toggleMetadata}
          metadataValid={metadataValid}
          regionId={regionId}
          className="relative"
        />
      )}
      {/* SW-VE-2 (AC-4): read-only is the visible default for code/config files.
          SW-VE-3: in an own-writable scope it becomes the deliberate
          "Read-only · Edit" mode switch (and "Editing" while active).
          SW-VE-8: also shown for editable docs (markdown/html) as the Edit
          switch — there it sits BESIDE the Metadata chip (AC-14), not instead. */}
      {showReadOnlyChip && (
        <ReadOnlyChip
          className="relative"
          canEdit={canEditActive}
          editing={editingActive}
          dirty={editDirty}
          onEdit={onEditActive}
        />
      )}
    </div>
  );
}
