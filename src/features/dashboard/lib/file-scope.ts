// SPDX-License-Identifier: MIT
import type { RepoScanResult } from "@/shared/lib/pipeline-graph/merge-pipeline-trees";
import type { DerivedPipelineTree, DocRef } from "@/shared/lib/pipeline-graph/types";

// ---------------------------------------------------------------------------
// SW-VE-5 — the scope model. The single FE source of truth for "which team owns
// a file?". VE-3 (edit chrome) and VE-6/VE-7 (lifecycle / drop) CONSUME this —
// they must never re-derive ownership. SW-XS-20: only FILES have an owner. A
// file inherited from a parent team is not read-only — the nav lists it at its
// real path in the parent, so editing, renaming or deleting it acts on the
// parent's file (every team inheriting from that parent sees the change); the
// scope labels it. Folders are not inherited: a folder is one merged view across
// the team chain, and anything created in it lands in the active team's copy.
//
// It is a pure PROJECTION of a doc's owning team (`DocRef.sourceTeam`, from the
// team chain: self → `parent_team` → …) plus its source-repo slug; it invents no
// new resolver and adds no backend call. SW-XS-19 dropped the retired
// install-tier layers (Team Repo / Team Workspace) from it. Where a row names
// no owning team (a generic row), the resolver returns `null` and the UI
// degrades to no badge — never a guessed one (Trevor decision, AC-1).
// ---------------------------------------------------------------------------

/** The team that owns a file, plus whether that is a parent team. */
export type FileScope = {
  /** `owner/repo` slug when the file is a source-repo file; `null` otherwise. */
  repoSlug: string | null;
  /** The team in the chain that owns this file. `null` when unnamed (the
   *  backend names only a parent team; an own file has no scope at all). */
  sourceTeam: string | null;
  /** True iff the owning team is a PARENT of the active team (not the active
   *  team itself): edits there update that team's file. Fails closed — a named
   *  team with the active team unknown counts as inherited (the backend names
   *  only parent teams). */
  isInherited: boolean;
};

/**
 * Project one file's owning team into a {@link FileScope} (AC-2/AC-3). Pure and
 * total: `undefined` (no team signal) → `null` (degrade). `isInherited` — a
 * parent-owned file — is a simple team-name comparison.
 */
export function resolveFileScope(
  sourceTeam: string | null | undefined,
  sourceRepo: string | null | undefined,
  activeTeam: string | null,
): FileScope | null {
  if (sourceTeam === undefined) return null;
  const isInherited = sourceTeam !== null && sourceTeam !== activeTeam;
  return { repoSlug: sourceRepo ?? null, sourceTeam, isInherited };
}

/** The repo leaf name for a badge label (`owner/repo` → `repo`). */
export function repoNameFromSlug(slug: string): string {
  return slug.split("/").pop() || slug;
}

/**
 * Human label for the scope chrome (AC-2, e.g. "Editing in <scope>"). A scope
 * names its repo (a source-repo file) or its owning team; an inherited scope
 * names the owning parent team. Pure.
 */
export function scopeLabel(scope: FileScope): string {
  if (scope.isInherited) return `Inherited from ${scope.sourceTeam ?? "parent team"}`;
  if (scope.repoSlug) return repoNameFromSlug(scope.repoSlug);
  return scope.sourceTeam ?? "this team";
}

/** Recursively find the owning team of the `DocRef` whose absolute `filePath`
 *  matches, depth-first; callers memoize on `(scans, path)`. A doc naming a
 *  team (or `null`) yields it; the `!== undefined` propagation is the graceful
 *  path for a doc with no team signal — the walk continues rather than claiming
 *  a false match. */
function findDocByPath(node: DerivedPipelineTree, filePath: string): string | null | undefined {
  for (const doc of node.docs ?? []) {
    if (doc.filePath === filePath) return doc.sourceTeam;
  }
  for (const sub of node.subEpics) {
    const hit = findDocByPath(sub, filePath);
    if (hit !== undefined) return hit;
  }
  return undefined;
}

/**
 * Resolve a file's {@link FileScope} from the resident `installedScans` by its
 * absolute path (AC-3, the metadata-chip inherited gate). Reuses the scans
 * already loaded for the nav — no new command, no re-scan. Returns `null`
 * when the path isn't covered by any scan (degrade: no gate, no badge).
 */
export function findScopeForPath(
  scans: RepoScanResult[],
  filePath: string,
  activeTeam: string | null,
): FileScope | null {
  for (const scan of scans) {
    const team = findDocByPath(scan.tree, filePath);
    if (team !== undefined) return resolveFileScope(team, scan.repoSlug, activeTeam);
  }
  return null;
}

/**
 * SW-VE-6: resolve a FILE's {@link FileScope} from a single derived nav tree —
 * the shape the pipeline nav already holds in memory — by absolute path, with
 * the same depth-first `findDocByPath` walk as {@link findScopeForPath}. The
 * context menu, move and the viewer label read it. `repoSlug` is unknown from
 * the tree alone, so it comes back `null`. Returns `null` for an own file (no
 * `sourceTeam`), a folder (folders are not inherited) or an uncovered path.
 */
export function findScopeInTree(
  tree: DerivedPipelineTree,
  filePath: string,
  activeTeam: string | null,
): FileScope | null {
  const team = findDocByPath(tree, filePath);
  return team === undefined ? null : resolveFileScope(team, null, activeTeam);
}

/** The `DocRef` at `filePath` in the tree, depth-first, or `null`. */
export function findDocInTree(node: DerivedPipelineTree, filePath: string): DocRef | null {
  const doc = (node.docs ?? []).find((d) => d.filePath === filePath);
  if (doc) return doc;
  for (const sub of node.subEpics) {
    const hit = findDocInTree(sub, filePath);
    if (hit) return hit;
  }
  return null;
}

/** SW-XS-20: whether `filePath` is one of the active team's own files in the
 *  tree — a story, a main doc, or a team doc that is neither from a source repo
 *  nor inherited. The delete confirm adds its extra caution for these. */
export function isOwnTeamFileInTree(
  node: DerivedPipelineTree,
  filePath: string,
  activeTeam: string | null,
): boolean {
  if (node.stories.some((st) => st.filePath === filePath)) return true;
  if (node.mainDoc?.filePath === filePath) return true;
  const doc = (node.docs ?? []).find((d) => d.filePath === filePath);
  if (doc) {
    return (
      doc.installedSourceRepo == null &&
      !resolveFileScope(doc.sourceTeam, null, activeTeam)?.isInherited
    );
  }
  return node.subEpics.some((sub) => isOwnTeamFileInTree(sub, filePath, activeTeam));
}

/** SW-XS-20: the files under the folder at `dirPath` (all the way down) that
 *  come from parent teams, grouped by team, in first-seen order. A folder
 *  rename or delete acts on the active team's files only; the confirm names
 *  these, which it leaves untouched. Empty when the folder isn't in the tree. */
export function inheritedFilesUnder(
  tree: DerivedPipelineTree,
  dirPath: string,
  activeTeam: string | null,
): { team: string; count: number }[] {
  const find = (node: DerivedPipelineTree): DerivedPipelineTree | null => {
    if (node.absoluteDirPath === dirPath) return node;
    for (const sub of node.subEpics) {
      const hit = find(sub);
      if (hit) return hit;
    }
    return null;
  };
  const dir = find(tree);
  const counts = new Map<string, number>();
  const walk = (node: DerivedPipelineTree): void => {
    for (const doc of node.docs ?? []) {
      const scope = resolveFileScope(doc.sourceTeam, null, activeTeam);
      if (scope?.isInherited && scope.sourceTeam) {
        counts.set(scope.sourceTeam, (counts.get(scope.sourceTeam) ?? 0) + 1);
      }
    }
    for (const sub of node.subEpics) walk(sub);
  };
  if (dir) walk(dir);
  return [...counts].map(([team, count]) => ({ team, count }));
}

/** SW-XS-20: what the row at `path` is in the tree — a file (doc, story or main
 *  doc), a folder, or `null` when the tree doesn't hold it (a generic row). The
 *  menu reads this instead of guessing from a dotted name (`.claude` is a folder). */
export function navNodeKind(tree: DerivedPipelineTree, path: string): "file" | "dir" | null {
  const walk = (node: DerivedPipelineTree): "file" | "dir" | null => {
    if (node.absoluteDirPath === path) return "dir";
    if (
      node.mainDoc?.filePath === path ||
      node.stories.some((st) => st.filePath === path) ||
      (node.docs ?? []).some((d) => d.filePath === path)
    ) {
      return "file";
    }
    for (const sub of node.subEpics) {
      const hit = walk(sub);
      if (hit) return hit;
    }
    return null;
  };
  return walk(tree);
}

/** SW-XS-20: whether the folder at `dirPath` holds any of the active team's own
 *  files (all the way down) — a folder only a parent team has holds none, so
 *  there is nothing of the team's to rename, move or delete. */
export function folderHasOwnFiles(
  tree: DerivedPipelineTree,
  dirPath: string,
  activeTeam: string | null,
): boolean {
  const find = (node: DerivedPipelineTree): DerivedPipelineTree | null => {
    if (node.absoluteDirPath === dirPath) return node;
    for (const sub of node.subEpics) {
      const hit = find(sub);
      if (hit) return hit;
    }
    return null;
  };
  const own = (node: DerivedPipelineTree): boolean =>
    node.stories.length > 0 ||
    node.mainDoc != null ||
    (node.docs ?? []).some(
      (d) =>
        d.installedSourceRepo == null &&
        !resolveFileScope(d.sourceTeam, null, activeTeam)?.isInherited,
    ) ||
    node.subEpics.some(own);
  const dir = find(tree);
  return !!dir && own(dir);
}

/** Every directory in the nav `tree` (absolute paths), depth-first — the
 *  Move-to picker's destinations. A move into any of them lands in the active
 *  team's copy of that folder. */
export function collectNavDirs(tree: DerivedPipelineTree): string[] {
  const out: string[] = [];
  const walk = (node: DerivedPipelineTree): void => {
    out.push(node.absoluteDirPath);
    for (const sub of node.subEpics) walk(sub);
  };
  walk(tree);
  return out;
}
