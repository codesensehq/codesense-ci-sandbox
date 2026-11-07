// SPDX-License-Identifier: MIT
import { formatCsHref, toContentRelative, isUnderContentRoot, type CsLink } from "./note-links";
import { VIEWER_LINK_RE } from "@/shared/lib/viewer-file-type";

/** SW-SN-9 Task 2: the pure model behind the `[[` picker. Sources are entirely
 *  in-memory (the store's notes list + the warm pipeline tree), so building and
 *  filtering never touches the filesystem on the typing path (R2). */

export type PickerItemKind = CsLink["kind"];

export interface PickerItem {
  /** Stable de-dupe + React key. */
  key: string;
  /** Primary display text (note title / story title / file name). */
  label: string;
  /** Secondary context (storyId, or the doc's relative path). */
  sublabel?: string;
  kind: PickerItemKind;
  link: CsLink;
}

/** A note as the picker needs it (subset of SW-SN-1 `NoteMeta`). */
export interface PickerNote {
  id: string;
  title: string;
}

/** A pipeline node's `data` as the picker needs it (subset of `StoryNodeData`). */
export interface DocNodeLike {
  title?: string;
  storyId?: string;
  storyFile?: string;
  resolvedSupportingDocs?: Array<{ fileName: string; filePath: string }>;
}

/** Max rows shown — keeps the popup tight and the filter O(n) trivial at
 *  "a few active, dozens total" scale. */
export const PICKER_LIMIT = 20;

/** Build the full, unfiltered candidate list: every note (except the one being
 *  edited), every pipeline story doc (linked by stable `storyId`, de-duped), and
 *  every resolved supporting doc that the viewer can actually open (de-duped by
 *  path, linked by content-relative path). Deterministic order: notes, stories,
 *  files.
 *
 *  Two candidate/resolver-agreement guards: file items are limited to
 *  viewer-supported extensions (an image/canvas doc that `resolveViewerLinkPath`
 *  would reject is never offered), and file items are omitted entirely until
 *  `teamContentPath` is known (else a relative target can't be computed and the
 *  absolute host path would leak into the link). */
export function buildPickerItems(
  notes: readonly PickerNote[],
  nodes: readonly DocNodeLike[],
  teamContentPath: string,
  activeNoteId?: string | null,
): PickerItem[] {
  const items: PickerItem[] = [];

  for (const note of notes) {
    if (note.id === activeNoteId) continue; // no self-link
    items.push({
      key: `note:${note.id}`,
      label: note.title || "Untitled note",
      kind: "note",
      link: { kind: "note", target: note.id },
    });
  }

  const seenStories = new Set<string>();
  for (const node of nodes) {
    if (!node.storyId || !node.storyFile || seenStories.has(node.storyId)) continue;
    seenStories.add(node.storyId);
    items.push({
      key: `story:${node.storyId}`,
      label: node.title || node.storyId,
      sublabel: node.storyId,
      kind: "story",
      link: { kind: "story", target: node.storyId },
    });
  }

  if (teamContentPath) {
    const seenFiles = new Set<string>();
    for (const node of nodes) {
      for (const doc of node.resolvedSupportingDocs ?? []) {
        // Skip already-seen, non-viewer, and `#`-in-name docs. A literal `#` in a
        // path is ambiguous with the fragment delimiter in the flat `cs:file`
        // target, so such a doc can't be linked unambiguously — don't offer it.
        if (
          seenFiles.has(doc.filePath) ||
          !VIEWER_LINK_RE.test(doc.fileName) ||
          doc.filePath.includes("#")
        )
          continue;
        // Only offer docs that actually live under the content root — otherwise
        // `toContentRelative` yields an absolute-ish target that the resolver
        // re-roots into a bogus path (silently "opening" a nonexistent file).
        if (!isUnderContentRoot(doc.filePath, teamContentPath)) continue;
        seenFiles.add(doc.filePath);
        const rel = toContentRelative(doc.filePath, teamContentPath);
        items.push({
          key: `file:${doc.filePath}`,
          label: doc.fileName,
          sublabel: rel,
          kind: "file",
          link: { kind: "file", target: rel },
        });
      }
    }
  }

  return items;
}

/** Case-insensitive substring narrow over label + sublabel, capped at
 *  {@link PICKER_LIMIT}. A blank query returns the first N candidates. */
export function filterPickerItems(items: readonly PickerItem[], query: string): PickerItem[] {
  const q = query.trim().toLowerCase();
  const matches = q
    ? items.filter(
        (it) =>
          it.label.toLowerCase().includes(q) || (it.sublabel?.toLowerCase().includes(q) ?? false),
      )
    : items;
  return matches.slice(0, PICKER_LIMIT);
}

/** The href a selected item inserts — the single place picker → link mark. */
export function pickerItemHref(item: PickerItem): string {
  return formatCsHref(item.link);
}
