// SPDX-License-Identifier: MIT
import type { Node } from "@xyflow/react";

import type { ResolvedSupportingDoc } from "@/shared/lib/pipeline-graph";
import { getViewerFileType } from "@/shared/lib/viewer-file-type";

import {
  extractNoteLinks,
  resolveCsLink,
  type CsLinkResolveContext,
  type CsLinkStoryNode,
  type DocIdentity,
} from "./note-links";

/** SW-SN-10: the pure, framework-free relational model behind the "Related" chip.
 *
 *  It answers "what is connected to the OPEN doc?" by reusing the warm pipeline
 *  tree (for team `supporting_docs`) and SW-SN-9's `resolveCsLink` (for private
 *  note backlinks). It holds no React/tiptap types so it stays unit-testable; the
 *  async note-body scan lives in `use-doc-backlinks.ts`, the UI in
 *  `related-docs-chip.tsx`.
 */

/** The subset of a react-flow node's `data` this module reads. Story nodes carry
 *  `storyFile`; group (epic) nodes carry `mainDocFilePath` — both are the node's
 *  OWN doc path. Both carry `storyId` and `resolvedSupportingDocs`. */
interface NodeDocData {
  storyId?: string;
  storyFile?: string;
  mainDocFilePath?: string;
  resolvedSupportingDocs?: ResolvedSupportingDoc[];
}

export interface DocRelations {
  /** Team `supporting_docs` of the open doc's OWN node (empty when the open path
   *  isn't a node's own doc — e.g. the open file is itself someone's supporting
   *  doc; own-doc-only semantics, SW-SN-10 Decision #2). */
  supportingDocs: ResolvedSupportingDoc[];
  /** Identity to seed a "New note about this" link. A STORY doc uses its stable
   *  `storyId` (`cs:story`, resolvable by SN-9); anything else — including a group
   *  (epic) doc, whose `storyId` SN-9's resolver can't map to a file — falls back
   *  to a `cs:file` path so the seeded link is always activatable. */
  docIdentity: DocIdentity;
  /** The exact context SN-9's `resolveCsLink` uses, so backlink matching is
   *  consistent with how a note's links actually activate. */
  resolveCtx: CsLinkResolveContext;
}

/** True when the active tab is a doc the chip should mount over. Markdown/HTML
 *  only (canvas/image get their own viewers), never in a sync/redline diff view
 *  (that's a review surface, not a reading one), never on an empty tab. */
export function shouldShowRelatedChip(activeTabPath: string, syncView: boolean): boolean {
  if (!activeTabPath || syncView) return false;
  const type = getViewerFileType(activeTabPath);
  return type === "markdown" || type === "html";
}

/** Resolve the open doc's relations from the warm pipeline `nodes`. Own-doc-keyed
 *  (`storyFile`/`mainDocFilePath` === the open path) so an open supporting doc
 *  never surfaces its owner's siblings (SW-SN-10 Decision #2 / the
 *  `storyNodeIdForPath` ownership Gotcha). */
export function computeDocRelations(
  nodes: Node[],
  activeTabPath: string,
  teamContentPath: string,
): DocRelations {
  const own = nodes.find((n) => {
    const d = n.data as NodeDocData;
    return d.storyFile === activeTabPath || d.mainDocFilePath === activeTabPath;
  });
  const ownData = own?.data as NodeDocData | undefined;
  const supportingDocs = ownData?.resolvedSupportingDocs ?? [];

  // Prefer a story's `storyId`; a group node's id is NOT resolvable by SN-9's
  // shared `resolveCsLink` (no `storyFile` on groups), so seed it by path.
  const storyId = own?.type === "story" ? ownData?.storyId : undefined;
  const docIdentity: DocIdentity = storyId
    ? { storyId }
    : { filePath: activeTabPath, teamContentPath };

  // Same projection SN-9's dispatcher uses (`nodes.map((n) => n.data)`), so a
  // note's links resolve here exactly as they do on click.
  const resolveCtx: CsLinkResolveContext = {
    nodes: nodes.map((n) => n.data as CsLinkStoryNode),
    teamContentPath,
  };

  return { supportingDocs, docIdentity, resolveCtx };
}

/** True when a note's markdown body links the open doc — any `cs:` link whose
 *  `resolveCsLink` resolves to `activeTabPath`. A `cs:story` link to this doc's
 *  own node resolves to its `storyFile` (== the open path); a `cs:file` link
 *  resolves to the same absolute path. Owner-of-this-supporting-doc links resolve
 *  ELSEWHERE, so they correctly don't match (own-doc backlinks only). */
export function noteBodyLinksDoc(
  body: string,
  activeTabPath: string,
  ctx: CsLinkResolveContext,
): boolean {
  if (!activeTabPath) return false;
  return extractNoteLinks(body).some(
    (link) => resolveCsLink(link, ctx)?.filePath === activeTabPath,
  );
}
