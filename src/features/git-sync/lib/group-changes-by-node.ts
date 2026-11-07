// SPDX-License-Identifier: MIT
/**
 * SW-GS-2 — group the real working-tree change set by the pipeline node that
 * owns each file (story / epic / their supporting docs), recursing
 * epic ⊃ sub-epic ⊃ story ⊃ files. Files owned by no node fall to a flat
 * Documentation group.
 *
 * Membership is sourced from the RESOLVED React-Flow nodes (the
 * `derivedTreeToReactFlow` output) — which already carry `supporting_docs`
 * resolved to ABSOLUTE paths plus the node id — correlated to the raw
 * `DerivedPipelineTree` for the hierarchy. This module re-resolves nothing: it
 * is a pure function over `(set, tree, nodes, orgRoot)`, so it unit-tests with
 * hand-built fixtures.
 *
 * Ownership rules (locked — see story G6):
 *  - A node's own `.md` (story file / epic main doc) → that node (group header).
 *  - A file in a node's resolved `supporting_docs` → that node (option B: a doc
 *    owned by ≥2 nodes appears under EACH owner, flagged `shared`).
 *  - A DELETED file owned by no node is directory-attributed to its enclosing
 *    epic (option A); everything else unowned → Documentation.
 *  - `totalFiles` counts UNIQUE changed paths (a shared file counts once).
 */

import type { Node } from "@xyflow/react";

import { derivedTreeToReactFlow } from "@/shared/lib/pipeline-graph";
import { buildFullGroupIndex } from "@/shared/lib/pipeline-graph/build-group-index";
import type {
  DerivedPipelineTree,
  GroupNodeData,
  StoryNodeData,
  StoryRef,
} from "@/shared/lib/pipeline-graph/types";

import type { PublishFile, PublishFileStatus, PublishSet } from "./types";

/** A single changed file as rendered in the grouped list. `path` is
 *  org-root-relative (good for the filename + relative-dir display); resolve to
 *  absolute via `joinOrgPath(orgRoot, path)` to open it. */
export interface ChangedFile {
  path: string;
  status: PublishFileStatus;
  /** Option B: true when ≥2 nodes own this file (drives the `shared` badge). */
  shared: boolean;
  /** SW-GS-14: pre-rename org-root-relative path; present only for a rename.
   *  Drives the row's R chip + `← old` subtitle and rides the opened tab so the
   *  redline diffs by the old path. */
  renamedFrom?: string;
}

/** The pipeline node a group is built around. `id` is the React-Flow node id
 *  (for direct selection); `filePath` is the node's own doc (absolute), opened
 *  when the header row is clicked. */
export interface NodeRef {
  id: string;
  title: string;
  kind: "story" | "epic";
  /** Absolute path to the node's own `.md` (story file / epic main doc), or
   *  "" when the epic has no main doc. */
  filePath: string;
}

export interface NodeChangeGroup {
  node: NodeRef;
  /** The node's own `.md` change, if it changed. */
  self: ChangedFile | null;
  /** Changed supporting docs (+ dir-attributed deletions for epics) under it. */
  docs: ChangedFile[];
  /** Recursive: epic ⊃ sub-epic / story groups. */
  children: NodeChangeGroup[];
  /** Total changed files in this subtree (`self` + `docs` + Σ children) —
   *  precomputed once here so the view's header badge doesn't recurse per render. */
  count: number;
}

export interface GroupedChanges {
  nodes: NodeChangeGroup[];
  documentation: ChangedFile[];
  /** UNIQUE changed paths (dedup) — drives the "N changed files" summary. */
  totalFiles: number;
}

/** Stable empty result — used to skip grouping work when there are no changes. */
export const EMPTY_GROUPED: GroupedChanges = { nodes: [], documentation: [], totalFiles: 0 };

/** Join an org-root-relative path to the absolute org root (no double slash). */
export function joinOrgPath(orgRoot: string, relPath: string): string {
  return `${orgRoot.replace(/\/+$/, "")}/${relPath.replace(/^\/+/, "")}`;
}

/** SW-GS-8 — the publish key for the node-less Documentation bucket. */
export const DOCS_PUBLISH_KEY = "__documentation__";

/** The absolute paths owned by any pipeline node: the SAME ownership set that drives
 *  grouping (`buildNodeIndex`). */
export function nodeOwnedPaths(nodes: Node[]): Set<string> {
  return new Set(buildNodeIndex(nodes).ownerCountByPath.keys());
}

/** SW-GS-8 / SW-GS-18 — the stable publish key for a node group: the React key
 *  the list row uses AND the key SW-GS-18's pending-publish intent re-matches
 *  against the refreshed set. `node.id` is the React-Flow id; falls back to the
 *  node's own file path when a group has no id. */
export function nodePublishKey(group: NodeChangeGroup): string {
  return group.node.id || group.node.filePath;
}

/** The React-Flow nodes for a team's tree — the membership source `groupChangesByNode`
 *  correlates the change set against. Centralized (SW-GS-18) so the non-obvious
 *  `fullGroupIndex` construction lives in ONE place: the sync list memo
 *  (SW-GS-2/9) and the resume hook (SW-GS-18) both derive nodes through here. */
export function deriveSyncNodes(tree: DerivedPipelineTree): Node[] {
  return derivedTreeToReactFlow(tree, { fullGroupIndex: buildFullGroupIndex(tree) }).nodes;
}

// ---------------------------------------------------------------------------
// Node index — own files, resolved doc paths, and per-path owner counts.
// ---------------------------------------------------------------------------

interface NodeInfo {
  id: string;
  title: string;
  /** Absolute resolved supporting-doc paths. */
  resolvedDocPaths: string[];
}

interface EpicNodeInfo extends NodeInfo {
  /** Absolute epic directory (for deleted-file dir-attribution). */
  absoluteDirPath?: string;
}

interface NodeIndex {
  /** Keyed by the story's own file (absolute `data.storyFile`). */
  storyByFile: Map<string, NodeInfo>;
  /** Keyed by the group node's `data.dirPath` (matches `DerivedPipelineTree.dirPath`). */
  groupByDirPath: Map<string, EpicNodeInfo>;
  /** Every node-owned absolute path → count of distinct owning nodes. */
  ownerCountByPath: Map<string, number>;
}

function isStoryData(n: Node): n is Node<StoryNodeData> {
  return n.type === "story";
}
function isGroupData(n: Node): n is Node<GroupNodeData> {
  return n.type === "group";
}

/** Build the membership index from the resolved React-Flow nodes. */
export function buildNodeIndex(nodes: Node[]): NodeIndex {
  const storyByFile = new Map<string, NodeInfo>();
  const groupByDirPath = new Map<string, EpicNodeInfo>();
  const ownerCountByPath = new Map<string, number>();

  // `ownerCountByPath` counts DISTINCT owning nodes (the `shared` definition),
  // so a single node listing the same path twice (own-file == a resolved doc,
  // or a duplicate doc entry) bumps it only once. Collect each node's owned
  // paths into a Set, then bump once per distinct path.
  for (const n of nodes) {
    const owned = new Set<string>();
    if (isStoryData(n)) {
      const ownFile = n.data.storyFile;
      const docs = (n.data.resolvedSupportingDocs ?? []).map((d) => d.filePath);
      if (ownFile) {
        storyByFile.set(ownFile, { id: n.id, title: n.data.title, resolvedDocPaths: docs });
        owned.add(ownFile);
      }
      for (const d of docs) owned.add(d);
    } else if (isGroupData(n)) {
      const ownFile = n.data.mainDocFilePath;
      const docs = (n.data.resolvedSupportingDocs ?? []).map((d) => d.filePath);
      if (n.data.dirPath != null) {
        groupByDirPath.set(n.data.dirPath, {
          id: n.id,
          title: n.data.title,
          resolvedDocPaths: docs,
          absoluteDirPath: n.data.absoluteDirPath,
        });
      }
      if (ownFile) owned.add(ownFile);
      for (const d of docs) owned.add(d);
    }
    for (const p of owned) ownerCountByPath.set(p, (ownerCountByPath.get(p) ?? 0) + 1);
  }
  return { storyByFile, groupByDirPath, ownerCountByPath };
}

// ---------------------------------------------------------------------------
// Grouping.
// ---------------------------------------------------------------------------

interface ChangeEntry {
  path: string; // org-root-relative
  status: PublishFileStatus;
  renamedFrom?: string; // SW-GS-14: pre-rename org-root-relative path (renames only)
}

/** Flatten a PublishSet's buckets + general into one path→change map (keyed by
 *  org-root-relative path). Exported so the sync list can reconcile an open
 *  tab's snapshotted rename info against the live set (SW-GS-14). */
export function flattenChanges(set: PublishSet): Map<string, ChangeEntry> {
  const out = new Map<string, ChangeEntry>();
  const add = (files: PublishFile[]) => {
    for (const f of files) {
      out.set(f.path, {
        path: f.path,
        status: f.status,
        renamedFrom: f.renamedFrom,
      });
    }
  };
  for (const b of set.buckets) add(b.files);
  add(set.general.files);
  return out;
}

/**
 * SW-GS-15 — breadcrumb compaction. Fold runs of single-child `epic` containers
 * that have no own changes into ONE node whose title is the joined path
 * (`_bmad-output / … / seamless-workspaces`), so the deep boilerplate prefix
 * collapses to a single row. Pure + presentation-only: it rewrites `title` and
 * flattens the chain but carries the deepest node's `count`/`self`/`docs`/
 * `children`/`node` **verbatim** — it never recomputes counts or touches
 * ownership. Depth-first so nested chains under a branch point fold too.
 *
 * A node is a fold link only when it has NO own changes (`self == null` and no
 * `docs`), exactly ONE child, and that child is itself an `epic` — so a chain
 * stops at the first branch, own-change, or `story` child, keeping every story's
 * owning epic/sub-epic its own row.
 */
export function compactSingleChildChains(nodes: NodeChangeGroup[]): NodeChangeGroup[] {
  const compact = (node: NodeChangeGroup): NodeChangeGroup => {
    // Depth-first: compact the subtree before folding at this level.
    let current: NodeChangeGroup = { ...node, children: node.children.map(compact) };
    while (
      current.self === null &&
      current.docs.length === 0 &&
      current.children.length === 1 &&
      current.children[0].node.kind === "epic"
    ) {
      const child = current.children[0];
      // Carry the child's subtree + count verbatim; only join the title.
      current = {
        ...child,
        node: { ...child.node, title: `${current.node.title} / ${child.node.title}` },
      };
    }
    return current;
  };
  return nodes.map(compact);
}

export function groupChangesByNode(
  set: PublishSet,
  tree: DerivedPipelineTree,
  nodes: Node[],
  orgRoot: string,
): GroupedChanges {
  const idx = buildNodeIndex(nodes);
  const changes = flattenChanges(set);

  // Index changes by ABSOLUTE path (to match node file paths) and stamp `shared`.
  const changesByAbs = new Map<string, ChangedFile>();
  for (const c of changes.values()) {
    const abs = joinOrgPath(orgRoot, c.path);
    changesByAbs.set(abs, {
      path: c.path,
      status: c.status,
      shared: (idx.ownerCountByPath.get(abs) ?? 0) >= 2,
      renamedFrom: c.renamedFrom,
    });
  }

  // Set of every node-owned absolute path → identifies Documentation leftovers.
  const ownedPaths = new Set(idx.ownerCountByPath.keys());

  // Pre-attribute deleted, unowned changes to their deepest enclosing epic dir
  // (option A). Built from the group nodes' absolute dir paths.
  const epicDirs: Array<{ dirPath: string; absoluteDirPath: string }> = [];
  for (const [dirPath, info] of idx.groupByDirPath) {
    if (info.absoluteDirPath) {
      // Normalize a trailing slash so the `${dir}/` prefix test below can't
      // become `…//` and silently fail to match.
      epicDirs.push({ dirPath, absoluteDirPath: info.absoluteDirPath.replace(/\/+$/, "") });
    }
  }
  epicDirs.sort((a, b) => b.absoluteDirPath.length - a.absoluteDirPath.length); // deepest first
  const deletedByEpicDirPath = new Map<string, ChangedFile[]>();
  for (const [abs, cf] of changesByAbs) {
    if (ownedPaths.has(abs) || cf.status !== "deleted") continue;
    const owner = epicDirs.find((e) => abs.startsWith(`${e.absoluteDirPath}/`));
    if (owner) {
      const list = deletedByEpicDirPath.get(owner.dirPath) ?? [];
      list.push(cf);
      deletedByEpicDirPath.set(owner.dirPath, list);
    }
  }

  // Resolve changed docs for a node, excluding its own file so a path that is
  // both the node's `.md` and one of its resolved docs renders once (as `self`).
  const pickDocs = (absPaths: string[], exclude?: string): ChangedFile[] =>
    absPaths.filter((p) => p !== exclude).flatMap((p) => changesByAbs.get(p) ?? []);

  const pickSelf = (abs: string | undefined): ChangedFile | null =>
    (abs && changesByAbs.get(abs)) || null;

  const buildStory = (story: StoryRef): NodeChangeGroup | null => {
    const info = idx.storyByFile.get(story.filePath);
    const self = pickSelf(story.filePath);
    const docs = pickDocs(info?.resolvedDocPaths ?? [], story.filePath);
    if (!self && docs.length === 0) return null;
    return {
      node: {
        id: info?.id ?? "",
        title: info?.title ?? story.fileName,
        kind: "story",
        filePath: story.filePath,
      },
      self,
      docs,
      children: [],
      count: (self ? 1 : 0) + docs.length,
    };
  };

  const buildEpic = (epic: DerivedPipelineTree): NodeChangeGroup | null => {
    const info = idx.groupByDirPath.get(epic.dirPath);
    const ownFile = epic.mainDoc?.filePath;
    const self = pickSelf(ownFile);
    const docs = pickDocs(info?.resolvedDocPaths ?? [], ownFile);
    // Option A: dir-attributed deletions surface as struck rows under the epic.
    for (const del of deletedByEpicDirPath.get(epic.dirPath) ?? []) docs.push(del);

    const children: NodeChangeGroup[] = [];
    for (const s of epic.stories) {
      const g = buildStory(s);
      if (g) children.push(g);
    }
    for (const sub of epic.subEpics) {
      const g = buildEpic(sub);
      if (g) children.push(g);
    }
    if (!self && docs.length === 0 && children.length === 0) return null;
    return {
      node: {
        id: info?.id ?? "",
        title: info?.title ?? epic.dirName,
        kind: "epic",
        filePath: ownFile ?? "",
      },
      self,
      docs,
      children,
      count: (self ? 1 : 0) + docs.length + children.reduce((n, c) => n + c.count, 0),
    };
  };

  // Top-level groups: loose root stories + each top-level epic.
  const topNodes: NodeChangeGroup[] = [];
  for (const s of tree.stories) {
    const g = buildStory(s);
    if (g) topNodes.push(g);
  }
  for (const e of tree.subEpics) {
    const g = buildEpic(e);
    if (g) topNodes.push(g);
  }

  // Documentation: changed files owned by no node and not dir-attributed.
  const attributedDeletes = new Set<string>();
  for (const list of deletedByEpicDirPath.values()) {
    for (const cf of list) attributedDeletes.add(joinOrgPath(orgRoot, cf.path));
  }
  const documentation: ChangedFile[] = [];
  for (const [abs, cf] of changesByAbs) {
    if (!ownedPaths.has(abs) && !attributedDeletes.has(abs)) documentation.push(cf);
  }

  // SW-GS-15: fold single-child epic-chain prefixes into breadcrumb rows before
  // handing the tree to the view (memoized upstream — see the `grouped` useMemo).
  return {
    nodes: compactSingleChildChains(topNodes),
    documentation,
    totalFiles: changes.size,
  };
}
