// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";
import type { FileEntry } from "@/tauri/commands";
import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";
import type { PipelineGraph } from "@/shared/lib/pipeline-graph/pipeline-graph";
import type { BlockedDep } from "@/shared/lib/pipeline-graph/resolve-nav-readiness";
import { isInstalledNavPath } from "@/shared/lib/pipeline-graph/composite-to-repo-scan";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";
import { storyMetadataTitle } from "@/shared/lib/pipeline-graph/story-display-title";
import { compareNavNames, orderSubEpics, type NodeCounts } from "./nav-ordering";
import { normalizePinPath, type NavPinMeta, type PinnedNavItem } from "./nav-pins";
import { STAGE_GROUPS } from "@/shared/lib/nav-lens";

/** A stage group's key: Needs you, a taxonomy role, or no stage (off-taxonomy). */
export type StageGroupKey = (typeof STAGE_GROUPS)[number]["key"];

// ---------------------------------------------------------------------------
// FlatNavItem — the discriminated union the virtualized renderer switches on.
// ---------------------------------------------------------------------------

export type FlatNavItem =
  | {
      type: "folder-header";
      node: DerivedPipelineTree;
      depth: number;
      expanded: boolean;
      total: number;
      done: number;
    }
  | { type: "story"; story: StoryRef; depth: number; isReady: boolean; blockedBy: BlockedDep[] }
  // SLW-NC-1: `installed` marks a source-repo doc (browse-only — the doc context
  // menu delete/rename/create-canvas is suppressed). Phase 3: `sourceRepo` (the
  // `owner/repo` slug) drives the provenance color tint, and `shadows` drives the
  // shadowed hint badge (the winner covers a team-workspace version). Both come
  // from the per-`DocRef` markers the flat merge stamps.
  | {
      type: "doc";
      filePath: string;
      label: string;
      depth: number;
      installed: boolean;
      sourceRepo?: string;
      shadows?: boolean;
      // SW-XS-19: the team that owns this doc (`DocRef.sourceTeam`); a parent
      // team here drives the inherited marker naming it.
      sourceTeam?: string | null;
      // SW-XS-20: the parent team whose file this own doc overrides.
      overrides?: string;
      // SLW-MD-1: `false` ⇒ render the amber invalid-metadata badge (AC-11/16).
      metadataValid?: boolean;
      metadataInvalidReason?: string | null;
    }
  | { type: "generic-dir"; entry: FileEntry; depth: number; expanded: boolean }
  | { type: "generic-file"; entry: FileEntry; depth: number }
  // SW-VE-1: an expanded generic dir whose children have not resolved yet
  // (skeleton row) vs. resolved-empty / errored (muted "Empty folder" row).
  // `parentPath` is the owning dir — it makes the virtualizer key unique and
  // keys the row to its folder. Both carry `depth` for the indent read.
  | { type: "generic-loading"; depth: number; parentPath: string }
  | { type: "generic-empty"; depth: number; parentPath: string }
  // SLW-NC-7: the "Pinned" section, emitted ahead of the tree by
  // `prependPinnedSection`. The header is hidden when there are no pins; the rows
  // are omitted when the section is collapsed (the header still shows a count).
  // `depth` is always 0 (top-of-nav section) — declared so the virtualized
  // renderer's `item.depth` indent read stays total over the union.
  | { type: "pinned-header"; depth: number; count: number; collapsed: boolean }
  | { type: "pinned-row"; depth: number; pin: PinnedNavItem }
  // Arranged by stage: a group's header (its stories follow unless it is collapsed).
  | {
      type: "stage-group";
      depth: number;
      key: StageGroupKey;
      label: string;
      count: number;
      collapsed: boolean;
    }
  // A line of words between rows: a group's heading (with its count) or a closing note.
  | {
      type: "section-note";
      depth: number;
      key: string;
      tone: "heading" | "note";
      trailingAction?: ReactNode;
      text: string;
      count?: number;
    }
  | {
      type: "main-doc";
      filePath: string;
      label: string;
      depth: number;
      installed: boolean;
      sourceRepo?: string;
      shadows?: boolean;
      // Never set on a mainDoc — declared only to share the `DocRow` render path
      // with `doc` items.
      sourceTeam?: string | null;
    };

/** The file path a flat item represents (a leaf file row), or null for folders
 *  and pinned shortcut rows. Used for `selectedPath` lookup and as the nav
 *  drag-source path (SLW-FX-21). */
export function itemFilePath(item: FlatNavItem): string | null {
  switch (item.type) {
    case "story":
      return item.story.filePath;
    case "doc":
    case "main-doc":
      return item.filePath;
    case "generic-file":
      return item.entry.path;
    case "folder-header":
      return null;
    case "generic-dir":
      return null;
    // SW-VE-1: placeholder rows are not reveal targets.
    case "generic-loading":
    case "generic-empty":
      return null;
    // SLW-NC-7: pinned rows are shortcuts, not reveal targets themselves.
    case "pinned-header":
    case "pinned-row":
    case "stage-group":
    case "section-note":
      return null;
  }
}

/**
 * SW-LNS-1 — the live-filter overlay for the left-nav search bar.
 *
 * When present, `flattenNavTree` emits ONLY rows that either (a) match the
 * query directly (`matchedFilePaths` / `matchedDirPaths`) or (b) are an
 * ancestor directory of a match (`ancestorDirPaths`). Ancestors render
 * auto-expanded regardless of `expandedPaths` / `genericExpandedPaths` so the
 * user sees matches without manually opening the tree.
 *
 * The caller (`use-nav-items`) owns the walk that fills these sets — it has
 * access to `viewerRootEntries` and `genericChildrenByPath`, which this
 * helper only reads.
 */
export interface NavSearchFilter {
  /** Absolute file paths that match the query (story, doc, main-doc, generic-file). */
  matchedFilePaths: Set<string>;
  /** Absolute directory paths that match the query directly (shown AND recursed into). */
  matchedDirPaths: Set<string>;
  /** Absolute directory paths that are an ancestor of a matched descendant. */
  ancestorDirPaths: Set<string>;
}

export interface FlattenNavTreeArgs {
  tree: DerivedPipelineTree;
  expandedPaths: Set<string>;
  /** The full-graph readiness seam. Resolves cross-epic deps against the full
   *  team-content index even when `tree` is a drilled subtree, so readiness is
   *  scope-independent (SW-FX-1). Pass each story's owner node dirPath; the
   *  graph maps it into its own key space. */
  graph: PipelineGraph;
  /** Root-level non-pipeline content (generic dirs + non-md/html viewables).
   *  Suppressed when drilled in. PTS-2 follow-up: md/html non-stories now
   *  arrive via `tree.docs[]` at every level — `viewerRootEntries` no
   *  longer carries them. */
  viewerRootEntries: FileEntry[];
  /** Ephemeral generic-dir expansion. */
  genericExpandedPaths: Set<string>;
  /** From useGenericChildrenByPath — keyed by entry.path. */
  genericChildrenByPath: Map<string, FileEntry[]>;
  /** Pre-computed counts per sub-epic absoluteDirPath. Single bottom-up
   *  walk in `computeCountsByAbsPath`, read here in O(1) per folder. */
  countsByAbsPath: Map<string, NodeCounts>;
  isDrilledIn: boolean;
  /** SW-LNS-1: when set, the left-nav search filter overlays the normal walk
   *  (non-matching rows hidden, matched subtrees auto-expanded). */
  search?: NavSearchFilter;
}

/**
 * Walks the pipeline tree (plus root-level viewer entries and the lifted
 * orphan/children maps) and emits a single flat list suitable for a
 * virtualizer. All I/O is pre-fetched by the caller — this helper is pure.
 */
export function flattenNavTree(args: FlattenNavTreeArgs): FlatNavItem[] {
  return walkSubEpic(args.tree, 0, args, false);
}

/**
 * SW-LNS-1 — build the live-filter overlay for the left-nav search bar.
 *
 * Pure. Case-insensitive substring match on each node's displayed label
 * (basename for stories/docs/files, dir name for folders). Walks the same
 * surfaces `flattenNavTree` would emit — the pipeline tree, the root-level
 * viewer entries at depth 0, and every already-loaded generic-children
 * subtree reachable from them — so a result landing in the filter is one the
 * flatten pass can emit without any further I/O.
 *
 * Returns `null` when the trimmed query is empty, so the caller can thread a
 * single value through to {@link FlattenNavTreeArgs.search}.
 */
export function buildNavSearchFilter(args: {
  query: string;
  tree: DerivedPipelineTree;
  viewerRootEntries: FileEntry[];
  genericChildrenByPath: Map<string, FileEntry[]>;
  isDrilledIn: boolean;
  /** The lens: when set, only stories it keeps match, and no other row does (epics stay
   *  as the frame of their matching stories). */
  storyFilter?: (story: StoryRef) => boolean;
  /** Stories only: every row that is not a story drops out. */
  storiesOnly?: boolean;
}): NavSearchFilter | null {
  const q = args.query.trim().toLowerCase();
  const storyFilter = args.storyFilter;
  const storiesOnly = !!args.storiesOnly || !!storyFilter;
  if (!q && !storiesOnly) return null;
  const matchedFilePaths = new Set<string>();
  const matchedDirPaths = new Set<string>();
  const ancestorDirPaths = new Set<string>();

  const matches = (name: string): boolean => name.toLowerCase().includes(q);
  /** A non-story row matches only by name, and only while files are listed at all. */
  const fileMatches = (name: string): boolean => !storiesOnly && matches(name);
  const storyMatches = (s: StoryRef): boolean =>
    (!storyFilter || storyFilter(s)) &&
    (!q || matches(s.fileName) || matches(storyMetadataTitle(s) ?? "") || matches(s.storyId ?? ""));

  const markAncestors = (parents: readonly string[]) => {
    for (const p of parents) ancestorDirPaths.add(p);
  };

  const walkTree = (node: DerivedPipelineTree, parents: string[]) => {
    for (const s of node.stories) {
      if (storyMatches(s)) {
        matchedFilePaths.add(s.filePath);
        markAncestors(parents);
      }
    }
    for (const d of node.docs ?? []) {
      if (fileMatches(d.fileName)) {
        matchedFilePaths.add(d.filePath);
        markAncestors(parents);
      }
    }
    if (node.mainDoc) {
      const mdName = node.mainDoc.filePath.split("/").pop() ?? "";
      if (fileMatches(mdName)) {
        matchedFilePaths.add(node.mainDoc.filePath);
        markAncestors(parents);
      }
    }
    for (const sub of node.subEpics) {
      const subParents = [...parents, sub.absoluteDirPath];
      if (fileMatches(sub.dirName)) {
        matchedDirPaths.add(sub.absoluteDirPath);
        markAncestors(parents);
      }
      walkTree(sub, subParents);
    }
  };
  walkTree(args.tree, []);

  const walkGeneric = (entry: FileEntry, parents: string[]) => {
    const myParents = [...parents, entry.path];
    const kids = args.genericChildrenByPath.get(entry.path);
    if (!kids) return;
    for (const child of kids) {
      if (child.is_dir) {
        if (matches(child.name)) {
          matchedDirPaths.add(child.path);
          markAncestors(myParents);
        }
        walkGeneric(child, myParents);
      } else if (matches(child.name)) {
        matchedFilePaths.add(child.path);
        markAncestors(myParents);
      }
    }
  };
  if (!args.isDrilledIn && !storiesOnly) {
    const subEpicDirPaths = new Set(args.tree.subEpics.map((s) => s.absoluteDirPath));
    for (const entry of args.viewerRootEntries) {
      if (entry.is_dir) {
        if (subEpicDirPaths.has(entry.path)) continue;
        if (matches(entry.name)) matchedDirPaths.add(entry.path);
        walkGeneric(entry, []);
      } else if (matches(entry.name)) {
        matchedFilePaths.add(entry.path);
      }
    }
  }

  return { matchedFilePaths, matchedDirPaths, ancestorDirPaths };
}

function walkSubEpic(
  node: DerivedPipelineTree,
  depth: number,
  args: FlattenNavTreeArgs,
  /** SLW-NC-1: true once we've descended into an installed source-repo group —
   *  propagated to every descendant row so they render browse-only. */
  installed: boolean,
): FlatNavItem[] {
  const items: FlatNavItem[] = [];
  const search = args.search;

  // Sub-epic folders (header, then optional recursive expansion).
  for (const sub of orderSubEpics(node.subEpics)) {
    const dirMatched = search?.matchedDirPaths.has(sub.absoluteDirPath) ?? false;
    const dirAncestor = search?.ancestorDirPaths.has(sub.absoluteDirPath) ?? false;
    if (search && !dirMatched && !dirAncestor) continue;
    const expanded = search ? true : args.expandedPaths.has(sub.absoluteDirPath);
    const { total, done } = args.countsByAbsPath.get(sub.absoluteDirPath) ?? {
      total: 0,
      done: 0,
    };
    const subInstalled = installed || isInstalledNavPath(sub.absoluteDirPath);
    items.push({ type: "folder-header", node: sub, depth, expanded, total, done });
    if (expanded) items.push(...walkSubEpic(sub, depth + 1, args, subInstalled));
  }

  if (depth === 0 && !args.isDrilledIn) {
    const subEpicDirPaths = new Set(node.subEpics.map((s) => s.absoluteDirPath));
    for (const entry of args.viewerRootEntries) {
      if (!entry.is_dir) continue;
      if (subEpicDirPaths.has(entry.path)) continue;
      const dirMatched = search?.matchedDirPaths.has(entry.path) ?? false;
      const dirAncestor = search?.ancestorDirPaths.has(entry.path) ?? false;
      if (search && !dirMatched && !dirAncestor) continue;
      const expanded = search ? true : args.genericExpandedPaths.has(entry.path);
      items.push({ type: "generic-dir", entry, depth, expanded });
      if (expanded) items.push(...walkGenericChildren(entry, depth + 1, args));
    }
  }

  const sortedStories = [...node.stories].sort((a, b) => compareNavNames(a.fileName, b.fileName));
  for (const s of sortedStories) {
    if (search && !search.matchedFilePaths.has(s.filePath)) continue;
    const { ready, blockedBy } = args.graph.readiness(s, node.dirPath);
    items.push({
      type: "story",
      story: s,
      depth,
      isReady: ready,
      blockedBy,
    });
  }

  // PTS-2 follow-up: doc rows from `tree.docs[]` — md/html files in this
  // dir without pipeline-type frontmatter. The Rust scanner is the single
  // source of truth for the story/doc split; this renderer just consumes
  // whichever array the scanner placed each file in. Retired the legacy
  // `useOrphanFilesByPath` hook (was redundant once scanner ownership of
  // md/html became total).
  const sortedDocs = [...(node.docs ?? [])].sort((a, b) => compareNavNames(a.fileName, b.fileName));
  for (const d of sortedDocs) {
    if (search && !search.matchedFilePaths.has(d.filePath)) continue;
    // SLW-NC-1 Phase 3: after the flat merge, a source-repo doc can sit inside a
    // context dir, so browse-only + provenance ride the per-`DocRef` marker, not
    // ancestor propagation. `installed` stays true inside a source-only subtree
    // too (belt-and-suspenders).
    const source = d.installedSourceRepo;
    items.push({
      type: "doc",
      filePath: d.filePath,
      // stripViewerExtension covers md/html/htm (identical to the pipeline
      // strip) plus `.excalidraw` supporting docs.
      label: stripViewerExtension(d.fileName),
      depth,
      installed: installed || source != null,
      sourceRepo: source,
      shadows: d.shadowsTeamFile,
      // SW-XS-19: the owning team, for the inherited marker.
      sourceTeam: d.sourceTeam,
      overrides: d.overridesTeam,
      // SLW-MD-1: plain-doc invalid-metadata badge (AC-16).
      metadataValid: d.metadataValid,
      metadataInvalidReason: d.metadataInvalidReason,
    });
  }

  if (node.mainDoc) {
    const mdPath = node.mainDoc.filePath;
    if (!search || search.matchedFilePaths.has(mdPath)) {
      items.push({
        type: "main-doc",
        filePath: mdPath,
        label: mdPath.split("/").pop() ?? "README.md",
        depth,
        installed,
      });
    }
  }

  if (depth === 0 && !args.isDrilledIn) {
    for (const entry of args.viewerRootEntries.filter((e) => !e.is_dir)) {
      if (search && !search.matchedFilePaths.has(entry.path)) continue;
      items.push({ type: "generic-file", entry, depth });
    }
  }

  return items;
}

/** The file paths of every story in `tree` that is ready to start, read through `graph`. */
export function collectReadyPaths(tree: DerivedPipelineTree, graph: PipelineGraph): Set<string> {
  const ready = new Set<string>();
  const walk = (node: DerivedPipelineTree) => {
    for (const story of node.stories)
      if (graph.readiness(story, node.dirPath).ready) ready.add(story.filePath);
    for (const sub of node.subEpics) walk(sub);
  };
  walk(tree);
  return ready;
}

/**
 * Arranged by stage: every story `keep` admits, flat and grouped — Needs you first, then each
 * stage, Done last. Each group is a header followed by its stories (by name, with the ones
 * ready to start first); Done stays collapsed until `doneOpen`. Empty groups are left out. Pure.
 */
export function flattenByStage(args: {
  tree: DerivedPipelineTree;
  graph: PipelineGraph;
  keep: (story: StoryRef) => boolean;
  groupOf: (story: StoryRef) => StageGroupKey;
  doneOpen: boolean;
}): FlatNavItem[] {
  const byGroup = new Map<StageGroupKey, Array<{ story: StoryRef; dirPath: string }>>();
  const walk = (node: DerivedPipelineTree) => {
    for (const story of node.stories) {
      if (!args.keep(story)) continue;
      const key = args.groupOf(story);
      const list = byGroup.get(key) ?? [];
      list.push({ story, dirPath: node.dirPath });
      byGroup.set(key, list);
    }
    for (const sub of node.subEpics) walk(sub);
  };
  walk(args.tree);
  const items: FlatNavItem[] = [];
  for (const { key, label } of STAGE_GROUPS) {
    const members = byGroup.get(key);
    if (!members?.length) continue;
    const collapsed = key === "done" && !args.doneOpen;
    items.push({ type: "stage-group", depth: 0, key, label, count: members.length, collapsed });
    if (collapsed) continue;
    const rows = members.map(({ story, dirPath }) => ({
      story,
      ...args.graph.readiness(story, dirPath),
    }));
    rows.sort(
      (a, b) =>
        Number(b.ready) - Number(a.ready) || compareNavNames(a.story.fileName, b.story.fileName),
    );
    for (const { story, ready, blockedBy } of rows) {
      items.push({ type: "story", story, depth: 0, isReady: ready, blockedBy });
    }
  }
  return items;
}

/** A path → NavPinMeta accumulator that maps absolute nav paths into the shared
 *  repo-relative key space, dropping out-of-anchor / empty / already-seen keys.
 *  Shared by the two collectors below so they agree on the key space + dedup. */
function pinTargetPutter(
  map: Map<string, NavPinMeta>,
  toRelative: (absPath: string) => string | null,
) {
  return (absPath: string, isDir: boolean, label: string) => {
    const rel = toRelative(absPath);
    if (rel == null) return;
    const key = normalizePinPath(rel);
    if (key && !map.has(key)) map.set(key, { isDir, label });
  };
}

/**
 * SLW-NC-7: build the resolvable-pin map (repo-relative path → {isDir, label})
 * from a flattened nav list, so `partitionPinnedNav` can tell a live pin from a
 * stale one and label it exactly as the tree does. Pinnable rows are dirs
 * (folder-header, generic-dir) and non-story files (doc, main-doc, generic-file);
 * `toRelative` maps each row's absolute nav path into the shared repo-relative key
 * space (returns null to skip a path outside the anchor). Pure.
 *
 * This sees only the CURRENTLY-VISIBLE rows — union it with `collectTreePinTargets`
 * so a pin to a collapsed-away nested node doesn't wrongly read as stale.
 */
export function collectPinTargets(
  items: FlatNavItem[],
  toRelative: (absPath: string) => string | null,
): Map<string, NavPinMeta> {
  const map = new Map<string, NavPinMeta>();
  const put = pinTargetPutter(map, toRelative);
  for (const it of items) {
    switch (it.type) {
      case "folder-header":
        put(it.node.absoluteDirPath, true, it.node.dirName);
        break;
      case "generic-dir":
        put(it.entry.path, true, it.entry.name);
        break;
      case "generic-file":
        put(it.entry.path, false, it.entry.name);
        break;
      case "doc":
      case "main-doc":
        put(it.filePath, false, it.label);
        break;
      // story / pinned-* rows are not pin targets.
    }
  }
  return map;
}

/**
 * SLW-NC-7: build the resolvable-pin map from the FULL pipeline tree — every
 * sub-epic dir and every doc, at every depth, INDEPENDENT of expand/collapse
 * state. This is the wholeness guarantee: a pinned nested epic or doc stays live
 * (not dimmed-stale) when its parent folder is collapsed. Lazily-loaded generic
 * children aren't in the tree, so the caller unions this with `collectPinTargets`
 * (the visible-rows collector) to also cover currently-expanded generic entries.
 * Stories are excluded (not pin targets), matching `collectPinTargets`.
 */
export function collectTreePinTargets(
  tree: DerivedPipelineTree,
  toRelative: (absPath: string) => string | null,
): Map<string, NavPinMeta> {
  const map = new Map<string, NavPinMeta>();
  const put = pinTargetPutter(map, toRelative);
  const walk = (node: DerivedPipelineTree) => {
    for (const d of node.docs ?? []) put(d.filePath, false, stripViewerExtension(d.fileName));
    if (node.mainDoc) {
      put(node.mainDoc.filePath, false, node.mainDoc.filePath.split("/").pop() ?? "README.md");
    }
    for (const sub of node.subEpics) {
      put(sub.absoluteDirPath, true, sub.dirName);
      walk(sub);
    }
  };
  walk(tree);
  return map;
}

/**
 * SLW-NC-7: prepend the "Pinned" section to a flattened nav list (AC-3). Returns
 * the list unchanged when there are no pins (hidden-when-empty). When collapsed,
 * only the header (with its count badge) is emitted; expanded, the header is
 * followed by one `pinned-row` per pin (already ordered/deduped by
 * `partitionPinnedNav`). Pure — the component owns the collapse state + the
 * pin-source reads.
 */
export function prependPinnedSection(
  items: FlatNavItem[],
  pinnedItems: PinnedNavItem[],
  collapsed: boolean,
): FlatNavItem[] {
  if (pinnedItems.length === 0) return items;
  const header: FlatNavItem = {
    type: "pinned-header",
    depth: 0,
    count: pinnedItems.length,
    collapsed,
  };
  const rows: FlatNavItem[] = collapsed
    ? []
    : pinnedItems.map((pin) => ({ type: "pinned-row", depth: 0, pin }));
  return [header, ...rows, ...items];
}

function walkGenericChildren(
  entry: FileEntry,
  depth: number,
  args: FlattenNavTreeArgs,
): FlatNavItem[] {
  const items: FlatNavItem[] = [];
  const search = args.search;
  const children = args.genericChildrenByPath.get(entry.path);
  if (children === undefined) {
    if (!search) items.push({ type: "generic-loading", depth, parentPath: entry.path });
    return items;
  }
  if (children.length === 0) {
    if (!search) items.push({ type: "generic-empty", depth, parentPath: entry.path });
    return items;
  }
  for (const child of children) {
    if (child.is_dir) {
      const dirMatched = search?.matchedDirPaths.has(child.path) ?? false;
      const dirAncestor = search?.ancestorDirPaths.has(child.path) ?? false;
      if (search && !dirMatched && !dirAncestor) continue;
      const expanded = search ? true : args.genericExpandedPaths.has(child.path);
      items.push({ type: "generic-dir", entry: child, depth, expanded });
      if (expanded) items.push(...walkGenericChildren(child, depth + 1, args));
    } else {
      if (search && !search.matchedFilePaths.has(child.path)) continue;
      items.push({ type: "generic-file", entry: child, depth });
    }
  }
  return items;
}
