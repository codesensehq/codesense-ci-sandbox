// SPDX-License-Identifier: MIT
import type { FileEntry } from "@/tauri/commands";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import type { PipelineGraph } from "@/shared/lib/pipeline-graph/pipeline-graph";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";
import type { NavChange } from "@/features/git-sync/lib/nav-changes";

import type { FlatNavItem } from "./flatten-nav-tree";
import { compareNavNames, orderSubEpics, type NodeCounts } from "./nav-ordering";

/** A folder of the Changes view: a scanned folder (epic or sub-folder) or a plain directory
 *  the scanner doesn't own, plus the changed rows directly inside it. */
interface ChangeDir {
  /** Absolute directory path. */
  path: string;
  node: DerivedPipelineTree | null;
  dirs: Map<string, ChangeDir>;
  files: FlatNavItem[];
}

function emptyDir(path: string, node: DerivedPipelineTree | null): ChangeDir {
  return { path, node, dirs: new Map(), files: [] };
}

/** A synthetic listing entry for a changed path the scan doesn't list (a deleted file, or one
 *  in a folder no one has expanded yet). */
function entryFor(path: string, isDir: boolean): FileEntry {
  const name = path.split("/").pop() ?? path;
  const dot = name.lastIndexOf(".");
  return {
    name,
    path,
    is_dir: isDir,
    extension: !isDir && dot > 0 ? name.slice(dot + 1) : null,
    size: null,
  };
}

/** Where each scanned file sits, and the row it renders as. */
function indexTree(
  tree: DerivedPipelineTree,
  graph: PipelineGraph,
): Map<string, { dir: DerivedPipelineTree; item: (depth: number) => FlatNavItem }> {
  const out = new Map<string, { dir: DerivedPipelineTree; item: (depth: number) => FlatNavItem }>();
  const walk = (node: DerivedPipelineTree) => {
    for (const s of node.stories) {
      out.set(s.filePath, {
        dir: node,
        item: (depth) => {
          const { ready, blockedBy } = graph.readiness(s, node.dirPath);
          return { type: "story", story: s, depth, isReady: ready, blockedBy };
        },
      });
    }
    for (const d of node.docs ?? []) {
      out.set(d.filePath, {
        dir: node,
        item: (depth) => ({
          type: "doc",
          filePath: d.filePath,
          label: stripViewerExtension(d.fileName),
          depth,
          installed: d.installedSourceRepo != null,
          sourceRepo: d.installedSourceRepo,
          metadataValid: d.metadataValid,
          metadataInvalidReason: d.metadataInvalidReason,
        }),
      });
    }
    if (node.mainDoc) {
      const mdPath = node.mainDoc.filePath;
      out.set(mdPath, {
        dir: node,
        item: (depth) => ({
          type: "main-doc",
          filePath: mdPath,
          label: mdPath.split("/").pop() ?? "README.md",
          depth,
          installed: false,
        }),
      });
    }
    for (const sub of node.subEpics) walk(sub);
  };
  walk(tree);
  return out;
}

function rowOrder(item: FlatNavItem): number {
  switch (item.type) {
    case "story":
      return 0;
    case "doc":
      return 1;
    case "main-doc":
      return 2;
    default:
      return 3;
  }
}

function rowName(item: FlatNavItem): string {
  switch (item.type) {
    case "story":
      return item.story.fileName;
    case "doc":
    case "main-doc":
      return item.label;
    case "generic-file":
      return item.entry.name;
    default:
      return "";
  }
}

/**
 * The Changes view: only the changed files, inside the folders they live in. Scanned files
 * keep the row they have in Files (a story keeps its rail and agent mark, a doc its drawing);
 * a deleted file, or one the scan doesn't list, is drawn as a plain file. Folders open unless
 * the user collapsed them here, and a chain of plain folders holding one folder each reads as
 * one breadcrumb (`repos › widgets`). Changes outside `tree` (another drill scope) are left out.
 * Pure.
 */
export function flattenChanges(args: {
  tree: DerivedPipelineTree;
  graph: PipelineGraph;
  changes: readonly NavChange[];
  cloneRoot?: string | null;
  countsByAbsPath: Map<string, NodeCounts>;
  /** Folders the user collapsed in this view (absolute paths). */
  collapsed: ReadonlySet<string>;
}): FlatNavItem[] {
  const { tree, changes } = args;
  const rootPath = tree.absoluteDirPath.replace(/\/+$/, "");
  const index = indexTree(tree, args.graph);
  const nodeByPath = new Map<string, DerivedPipelineTree>();
  const collectNodes = (n: DerivedPipelineTree) => {
    nodeByPath.set(n.absoluteDirPath.replace(/\/+$/, ""), n);
    n.subEpics.forEach(collectNodes);
  };
  collectNodes(tree);

  const root = emptyDir(rootPath, tree);
  const dirAt = (absDir: string): ChangeDir => {
    let cur = root;
    const rel = absDir === rootPath ? "" : absDir.slice(rootPath.length + 1);
    let path = rootPath;
    for (const seg of rel.split("/").filter(Boolean)) {
      path = `${path}/${seg}`;
      let next = cur.dirs.get(path);
      if (!next) cur.dirs.set(path, (next = emptyDir(path, nodeByPath.get(path) ?? null)));
      cur = next;
    }
    return cur;
  };

  for (const c of changes) {
    const pathInTree = args.cloneRoot
      ? `${args.cloneRoot.replace(/\/+$/, "")}/${c.relPath}`
      : c.absPath;
    if (!pathInTree.startsWith(`${rootPath}/`)) continue;
    const known = index.get(pathInTree);
    const parent = pathInTree.slice(0, pathInTree.lastIndexOf("/"));
    const dir = dirAt(known ? known.dir.absoluteDirPath.replace(/\/+$/, "") : parent);
    const item = known?.item(0);
    if (item?.type === "story")
      dir.files.push({ ...item, story: { ...item.story, filePath: c.absPath } });
    else if (item?.type === "doc" || item?.type === "main-doc")
      dir.files.push({ ...item, filePath: c.absPath });
    else dir.files.push({ type: "generic-file", entry: entryFor(c.absPath, false), depth: 0 });
  }

  const items: FlatNavItem[] = [];
  const emit = (dir: ChangeDir, depth: number) => {
    const subs = [...dir.dirs.values()];
    const scanned = orderSubEpics(subs.flatMap((d) => (d.node ? [d.node] : [])));
    const plain = subs.filter((d) => !d.node).sort((a, b) => compareNavNames(a.path, b.path));
    const byNode = new Map(subs.filter((d) => d.node).map((d) => [d.node!, d]));
    for (const node of scanned) {
      const sub = byNode.get(node)!;
      const expanded = !args.collapsed.has(sub.path);
      const { total, done } = args.countsByAbsPath.get(node.absoluteDirPath) ?? {
        total: 0,
        done: 0,
      };
      items.push({ type: "folder-header", node, depth, expanded, total, done });
      if (expanded) emit(sub, depth + 1);
    }
    for (let sub of plain) {
      // A plain folder holding just one plain folder folds into it: `repos › widgets`.
      const names = [sub.path.split("/").pop()!];
      while (sub.files.length === 0 && sub.dirs.size === 1) {
        const only = [...sub.dirs.values()][0];
        if (only.node) break;
        sub = only;
        names.push(sub.path.split("/").pop()!);
      }
      const expanded = !args.collapsed.has(sub.path);
      items.push({
        type: "generic-dir",
        entry: { ...entryFor(sub.path, true), name: names.join(" › ") },
        depth,
        expanded,
      });
      if (expanded) emit(sub, depth + 1);
    }
    const files = [...dir.files].sort(
      (a, b) => rowOrder(a) - rowOrder(b) || compareNavNames(rowName(a), rowName(b)),
    );
    for (const f of files) items.push({ ...f, depth });
  };
  emit(root, 0);
  return items;
}
