// SPDX-License-Identifier: MIT
import type { PublishFile } from "@/features/git-sync/lib/types";

/**
 * The source-diff rail's pure model: filtering, the directory tree, the visible
 * row order (which is also the j/k order), and totals. Kept out of the component
 * so the rail's behavior is testable without rendering.
 */

import { detectLanguageId } from "@/shared/components/code-editor/language-detection";

export interface DiffTotals {
  added: number;
  removed: number;
  /** At least one file carried a count (an all-binary set has none). */
  counted: boolean;
}

export function diffTotals(files: readonly PublishFile[]): DiffTotals {
  let added = 0;
  let removed = 0;
  let counted = false;
  for (const f of files) {
    if (f.added != null || f.removed != null) counted = true;
    added += f.added ?? 0;
    removed += f.removed ?? 0;
  }
  return { added, removed, counted };
}

/**
 * Whitespace-separated terms, each a case-insensitive substring of the path
 * (or of the pre-rename path), all of which must match: `viewer row` finds
 * `src/features/viewer/components/file-row.tsx`.
 */
export function filterFiles(files: readonly PublishFile[], query: string): PublishFile[] {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return [...files];
  return files.filter((f) => {
    const hay = `${f.path}\n${f.renamedFrom ?? ""}`.toLowerCase();
    return terms.every((t) => hay.includes(t));
  });
}

export type DiffTreeNode =
  | {
      kind: "dir";
      /** Full directory path (the collapse key). */
      path: string;
      /** Display name — single-child directory chains are compacted (`src/features`). */
      name: string;
      children: DiffTreeNode[];
      fileCount: number;
    }
  | { kind: "file"; file: PublishFile; name: string };

interface MutableDir {
  dirs: Map<string, MutableDir>;
  files: PublishFile[];
}

function basenameOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? path : path.slice(i + 1);
}

/** The changed files as a directory tree: directories first, then files, each
 *  sorted by name; a directory holding only one sub-directory merges with it. */
export function buildFileTree(files: readonly PublishFile[]): DiffTreeNode[] {
  const root: MutableDir = { dirs: new Map(), files: [] };
  for (const file of files) {
    const parts = file.path.split("/");
    let dir = root;
    for (const part of parts.slice(0, -1)) {
      let next = dir.dirs.get(part);
      if (!next) {
        next = { dirs: new Map(), files: [] };
        dir.dirs.set(part, next);
      }
      dir = next;
    }
    dir.files.push(file);
  }

  const toNodes = (dir: MutableDir, prefix: string): DiffTreeNode[] => {
    const dirNodes: DiffTreeNode[] = [...dir.dirs.entries()]
      .sort(([a], [b]) => naturalCompare(a, b))
      .map(([name, child]) => {
        let label = name;
        let path = prefix ? `${prefix}/${name}` : name;
        let node = child;
        while (node.files.length === 0 && node.dirs.size === 1) {
          const [[onlyName, only]] = [...node.dirs.entries()];
          label = `${label}/${onlyName}`;
          path = `${path}/${onlyName}`;
          node = only;
        }
        const children = toNodes(node, path);
        return { kind: "dir", path, name: label, children, fileCount: countFiles(children) };
      });
    const fileNodes: DiffTreeNode[] = [...dir.files]
      .sort((a, b) => naturalCompare(basenameOf(a.path), basenameOf(b.path)))
      .map((file) => ({ kind: "file", file, name: basenameOf(file.path) }));
    return [...dirNodes, ...fileNodes];
  };
  return toNodes(root, "");
}

function countFiles(nodes: DiffTreeNode[]): number {
  return nodes.reduce((n, node) => n + (node.kind === "file" ? 1 : node.fileCount), 0);
}

export type DiffRow =
  | {
      kind: "dir";
      path: string;
      name: string;
      depth: number;
      collapsed: boolean;
      fileCount: number;
    }
  | { kind: "file"; file: PublishFile; name: string; depth: number };

/** The rows a tree renders with `collapsed` directories folded, in order. */
export function treeRows(nodes: DiffTreeNode[], collapsed: ReadonlySet<string>): DiffRow[] {
  const rows: DiffRow[] = [];
  const walk = (list: DiffTreeNode[], depth: number) => {
    for (const node of list) {
      if (node.kind === "file") {
        rows.push({ kind: "file", file: node.file, name: node.name, depth });
        continue;
      }
      const isCollapsed = collapsed.has(node.path);
      rows.push({
        kind: "dir",
        path: node.path,
        name: node.name,
        depth,
        collapsed: isCollapsed,
        fileCount: node.fileCount,
      });
      if (!isCollapsed) walk(node.children, depth + 1);
    }
  };
  walk(nodes, 0);
  return rows;
}

/** Flat rows: every file at depth 0, in the order Git listed them. */
export function flatRows(files: readonly PublishFile[]): DiffRow[] {
  return files.map((file) => ({ kind: "file", file, name: file.path, depth: 0 }));
}

/** The file paths in on-screen order — what j/k walk. */
export function navigationOrder(rows: readonly DiffRow[]): string[] {
  return rows.flatMap((r) => (r.kind === "file" ? [r.file.path] : []));
}

/** The path `delta` steps from `current` in `order`, clamped at the ends; the
 *  first file when nothing (or a hidden file) is selected. */
export function stepFile(
  order: readonly string[],
  current: string | null,
  delta: number,
): string | null {
  if (order.length === 0) return null;
  const i = current == null ? -1 : order.indexOf(current);
  if (i < 0) return order[0];
  return order[Math.max(0, Math.min(order.length - 1, i + delta))];
}

/**
 * The tree earns its indentation once a change set spans several directories
 * and enough files that full paths stop fitting; a handful of files reads
 * better as a flat list of paths.
 */
export function prefersTree(files: readonly PublishFile[]): boolean {
  if (files.length < 6) return false;
  const dirs = new Set(files.map((f) => f.path.slice(0, Math.max(0, f.path.lastIndexOf("/")))));
  return dirs.size >= 2;
}

/** Split a repo path into its directory (with trailing slash) and file name. */
export function splitPath(path: string): { dir: string; name: string } {
  const i = path.lastIndexOf("/");
  return i < 0 ? { dir: "", name: path } : { dir: path.slice(0, i + 1), name: path.slice(i + 1) };
}

/** Numeric-aware name order, with a plain compare breaking its ties
 *  (`file-01` and `file-1` compare equal numerically), so the order never
 *  depends on input order. */
function naturalCompare(a: string, b: string): number {
  return (
    a.localeCompare(b, undefined, { numeric: true }) ||
    a.localeCompare(b) ||
    (a < b ? -1 : a > b ? 1 : 0)
  );
}

/** Whether the diff tool reads `path` as prose (the word-level redline) rather
 *  than a code diff: Markdown, but not MDX — its imports and JSX are code, and
 *  the prose redline would render them as text. (The editor still highlights
 *  MDX as Markdown.) */
export function isProseDiffPath(path: string): boolean {
  return detectLanguageId(path) === "markdown" && !/\.mdx$/i.test(path);
}
