// SPDX-License-Identifier: MIT
import {
  findScopeForPath,
  findScopeInTree,
  type FileScope,
} from "@/features/dashboard/lib/file-scope";
import type { RepoScanResult } from "@/shared/lib/pipeline-graph/merge-pipeline-trees";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { isVirtualTabPath } from "@/shared/lib/virtual-viewer-registry";
import { isEditableTextFile, isFrontmatterEditable } from "@/shared/lib/viewer-file-type";

/**
 * SW-VE-5 / SW-XS-20: the open file's scope — from the resident source-repo
 * scans first, then the nav tree. A file inherited from a parent team resolves
 * to that team; its path IS the parent's file, so every write lands there.
 * `null` for an own file.
 */
export function activeFileScope(
  installedScans: RepoScanResult[],
  navTree: DerivedPipelineTree | null,
  filePath: string,
  team: string | null,
): FileScope | null {
  return (
    findScopeForPath(installedScans, filePath, team) ??
    (navTree ? findScopeInTree(navTree, filePath, team) : null)
  );
}

/** What the viewer's doc toolbar offers for the open file. */
export type DocToolbarGates = {
  /** The file comes from a parent team: show the explainer (edits update it). */
  inheritedActive: boolean;
  /** The Metadata chip (frontmatter docs only). */
  showMetadataChip: boolean;
  /** Edit is offered — SW-XS-20: for an inherited file too. */
  canEditActive: boolean;
};

/**
 * The viewer's doc-toolbar gates (SW-VE-2/5/8, SW-XS-20). Never in a diff view;
 * Edit only for real text files (not a virtual management tab). A file from a
 * parent team keeps every affordance and adds the explainer.
 */
export function docToolbarGates(
  filePath: string | null,
  scope: FileScope | null,
  activeDiff: boolean,
): DocToolbarGates {
  const open = !!filePath && !activeDiff;
  return {
    inheritedActive: open && !!scope?.isInherited,
    showMetadataChip: open && isFrontmatterEditable(filePath!),
    canEditActive: open && !isVirtualTabPath(filePath!) && isEditableTextFile(filePath!),
  };
}
