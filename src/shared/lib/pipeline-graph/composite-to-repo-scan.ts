// SPDX-License-Identifier: MIT
import type { RepoScanResult } from "./merge-pipeline-trees";
import type { DerivedPipelineTree, DocRef } from "./types";

// ---------------------------------------------------------------------------
// SLW-NC-1 — source-repo content as a second nav tree source. SW-XS-15: it is
// the repo-tier files of the selected agent's want set (`wantSetToRepoScan`);
// the install-tier composite overlay is retired.
//
// The want set is a FLAT file list (one per logical name). The nav renders a
// `DerivedPipelineTree` (nested dirs + files). This adapter bridges the two: it
// nests the flat list by logical path segments into dir nodes (`subEpics`) whose
// files are plain `docs` — NEVER `stories`, so source-repo content is browsed in
// the nav but never folded into the pipeline graph (AC-4).
// ---------------------------------------------------------------------------

/** Scheme prefix that marks an installed dir node's synthetic `absoluteDirPath`
 *  (`installed://<repoSlug>/<logicalDirPath>`). A real context `absoluteDirPath`
 *  is an on-disk *absolute* filesystem path (leading `/`, or a drive letter),
 *  so it can never start with this scheme — even a context directory whose name
 *  contains odd characters is safe. This cleanly discriminates installed nodes
 *  from context nodes; see `isInstalledNavPath`, used by the drill guard to keep
 *  installed groups browse-only. */
const INSTALLED_PATH_SENTINEL = "installed://";

/** True when `absPath` is a source-repo dir node's synthetic path (carries the
 *  `INSTALLED_PATH_SENTINEL` scheme). Source dirs are browse-only — drilling one
 *  stamps a bogus drill/scope state, so the nav's drill handler ignores targets
 *  for which this returns true; it also flags them for the provenance tint
 *  (Phase 3). A real context filesystem path never starts with the scheme, so
 *  this cannot block a legitimate context drill. After the flat merge, every
 *  SOURCE-ONLY dir keeps this scheme (a merged context dir keeps its real path),
 *  so this still cleanly discriminates source dirs from context dirs. */
export function isInstalledNavPath(absPath: string): boolean {
  return absPath.startsWith(INSTALLED_PATH_SENTINEL);
}

/**
 * SLW-NC-1 — parse the source-repo slug out of an installed dir node's synthetic
 * `absoluteDirPath` (`installed://<owner>/<repo>[/<sub>...]`). Returns `null` for
 * any non-installed path. Phase 3 (flat unified merge) tints **every** source
 * dir by this slug (there is no `owner/repo` group root anymore). Kept
 * co-located with the path constructor (`newDir`) + `isInstalledNavPath` so the
 * `installed://` scheme is owned by exactly one module.
 */
export function installedRepoSlugFromNavPath(absPath: string): string | null {
  if (!absPath.startsWith(INSTALLED_PATH_SENTINEL)) return null;
  const rest = absPath.slice(INSTALLED_PATH_SENTINEL.length).replace(/\/+$/, "");
  const segments = rest.split("/").filter((s) => s.length > 0);
  // A well-formed installed path always has at least `owner/repo`.
  if (segments.length < 2) return null;
  return `${segments[0]}/${segments[1]}`;
}

interface MutableDir {
  /** Logical path relative to the repo root ("" at the root). */
  dirPath: string;
  dirName: string;
  absoluteDirPath: string;
  docs: DocRef[];
  /** Child dirs keyed by their leaf name, insertion-ordered. */
  subDirs: Map<string, MutableDir>;
}

function newDir(repoSlug: string, dirPath: string, dirName: string): MutableDir {
  return {
    dirPath,
    dirName,
    // Synthetic, repo-namespaced key — unique + stable for nav expand/collapse
    // state. Installed dirs have no single on-disk directory (files in one
    // logical dir may win from different layers), so this is a logical key, not
    // a filesystem path. The `installed://` scheme (INSTALLED_PATH_SENTINEL)
    // guarantees it can never collide with a real (absolute) context
    // `absoluteDirPath` and lets `isInstalledNavPath` flag it for the drill guard.
    absoluteDirPath: `${INSTALLED_PATH_SENTINEL}${repoSlug}/${dirPath}`,
    docs: [],
    subDirs: new Map(),
  };
}

function freeze(dir: MutableDir): DerivedPipelineTree {
  return {
    dirPath: dir.dirPath,
    dirName: dir.dirName,
    absoluteDirPath: dir.absoluteDirPath,
    // Installed content is never a pipeline story — see AC-4.
    stories: [],
    docs: dir.docs,
    subEpics: [...dir.subDirs.values()].map(freeze),
    orderError: null,
    mainDoc: null,
  };
}

/** Nest flat `logicalName`-addressed items into a repo scan tree, one doc per item. */
function nestScan<T>(
  repoSlug: string,
  items: readonly T[],
  logicalOf: (item: T) => string,
  docOf: (item: T, fileName: string) => DocRef,
): RepoScanResult {
  const root = newDir(repoSlug, "", "");

  for (const item of items) {
    const segments = logicalOf(item)
      .split("/")
      .filter((s) => s.length > 0);
    if (segments.length === 0) continue;

    const fileName = segments[segments.length - 1];
    const dirSegments = segments.slice(0, -1);

    let cursor = root;
    let cursorPath = "";
    for (const seg of dirSegments) {
      cursorPath = cursorPath === "" ? seg : `${cursorPath}/${seg}`;
      let next = cursor.subDirs.get(seg);
      if (!next) {
        next = newDir(repoSlug, cursorPath, seg);
        cursor.subDirs.set(seg, next);
      }
      cursor = next;
    }

    cursor.docs.push(docOf(item, fileName));
  }

  return { repoSlug, tree: freeze(root) };
}

/**
 * SW-XS-15 — the repo-tier files of an agent's want set (`repos/{o}/{r}/content/`, the real
 * per-repo tier) as a `RepoScanResult`, so the nav shows what the agent gets in that repo
 * beside the team content. The nav puts them under ONE browse-only folder, `group` (e.g.
 * "codesense content"), at their own paths inside it: interleaving them with the team's folders would
 * draw a second `.claude` beside the team's (the team's top-level folders are listed by the
 * nav, not in the scan tree, so the flat merge cannot meet them). Each doc opens the file
 * where it lives (`filePath`); files with no place on this Mac are left out.
 */
export function wantSetToRepoScan(
  repoSlug: string,
  files: ReadonlyArray<{ logical: string; path: string | null }>,
  group?: string,
): RepoScanResult {
  return nestScan(
    repoSlug,
    files.filter((f): f is { logical: string; path: string } => !!f.path),
    (f) => (group ? `${group}/${f.logical}` : f.logical),
    (f, fileName) => ({ fileName, filePath: f.path }),
  );
}

/** Stamp a source-repo doc with its provenance: the repo slug (→ browse-only +
 *  color tint) and whether it shadows a team-workspace file at the same path
 *  (→ the P3-AC-3b shadowed badge). */
function markSourceDoc(doc: DocRef, repoSlug: string, shadows: boolean): DocRef {
  return { ...doc, installedSourceRepo: repoSlug, shadowsTeamFile: shadows };
}

/** Mark an ENTIRE source-only subtree (a dir the team workspace doesn't have)
 *  as source-repo content: every doc gets the provenance stamp; none shadow
 *  anything (no team-workspace counterpart). Dir nodes keep their synthetic
 *  `installed://<slug>/…` path, so `isInstalledNavPath` still flags them for the
 *  color tint + browse-only guard after the flat merge. */
function markSourceSubtree(node: DerivedPipelineTree, repoSlug: string): DerivedPipelineTree {
  return {
    ...node,
    docs: (node.docs ?? []).map((d) => markSourceDoc(d, repoSlug, false)),
    subEpics: node.subEpics.map((s) => markSourceSubtree(s, repoSlug)),
  };
}

/**
 * SLW-NC-1 Phase 3 — deep-merge a source repo's composite tree INTO the context
 * (team-workspace) tree by relative path, **source repo wins** any collision.
 * Produces one flat unified tree: no `owner/repo` wrapper group, source files
 * interleaved with context at their real positions.
 *
 * - **Docs** merge by `fileName`. A source doc at a name the team workspace also
 *   has (as a doc, a story, OR the dir's `mainDoc`) **wins**: the context version
 *   is dropped, the source doc shown and stamped `shadowsTeamFile` (P3-AC-3/3b).
 * - **Sub-dirs** merge by `dirName`: present in both → recurse (the merged dir
 *   keeps the CONTEXT node's real path + affordances, source content overlaid);
 *   source-only → the whole subtree is added, marked source.
 * - Context-only nodes are kept **by reference** — the pure-context parts of the
 *   tree are untouched, so anything derived from them stays identical.
 */
function mergeTreeSourceWins(
  ctx: DerivedPipelineTree,
  src: DerivedPipelineTree,
  repoSlug: string,
): DerivedPipelineTree {
  const ctxDocs = ctx.docs ?? [];
  const srcDocs = src.docs ?? [];
  const srcDocNames = new Set(srcDocs.map((d) => d.fileName));
  const ctxDocNames = new Set(ctxDocs.map((d) => d.fileName));
  const ctxStoryNames = new Set(ctx.stories.map((s) => s.fileName));
  // The context epic's main doc (README.md / index.md) is a THIRD team-workspace
  // file a source doc can shadow — it renders as its own `main-doc` row, so if
  // the source wins its name we must drop it too (else the row duplicates + the
  // winner would miss its shadow badge).
  const ctxMainDocName = ctx.mainDoc?.filePath.split("/").pop() ?? null;
  const mainDocShadowed = ctxMainDocName != null && srcDocNames.has(ctxMainDocName);

  // Team-workspace docs/stories the source repo does NOT shadow stay as-is;
  // shadowed ones are dropped (their source winner shows instead).
  const keptCtxDocs = ctxDocs.filter((d) => !srcDocNames.has(d.fileName));
  const keptStories = ctx.stories.filter((s) => !srcDocNames.has(s.fileName));
  const mergedSrcDocs = srcDocs.map((d) => {
    const shadows =
      ctxDocNames.has(d.fileName) || ctxStoryNames.has(d.fileName) || ctxMainDocName === d.fileName;
    return markSourceDoc(d, repoSlug, shadows);
  });

  // Sub-dirs: recurse where both have the dir; append source-only subtrees.
  const srcSubByName = new Map(src.subEpics.map((s) => [s.dirName, s]));
  const ctxSubNames = new Set(ctx.subEpics.map((s) => s.dirName));
  const mergedSubs = ctx.subEpics.map((cSub) => {
    const sSub = srcSubByName.get(cSub.dirName);
    return sSub ? mergeTreeSourceWins(cSub, sSub, repoSlug) : cSub; // ctx-only by ref
  });
  for (const sSub of src.subEpics) {
    if (!ctxSubNames.has(sSub.dirName)) mergedSubs.push(markSourceSubtree(sSub, repoSlug));
  }

  return {
    ...ctx,
    mainDoc: mainDocShadowed ? null : ctx.mainDoc,
    stories: keptStories,
    docs: [...keptCtxDocs, ...mergedSrcDocs],
    subEpics: mergedSubs,
  };
}

/**
 * SLW-NC-1 Phase 3 — compose the nav's flatten input as ONE flat unified tree.
 * This is the ONLY place installed items enter a nav tree — `fullPipelineTree`
 * and everything it feeds (graph, readiness, `syncFrontmatter`, tray) stay pure
 * context, so AC-4/AC-5 (P3-AC-5) hold by construction.
 *
 * - **Team-workspace default** (`selectedRepoSlug` empty) OR **drilled scope**:
 *   the context tree is returned **by reference** — no source content, no groups
 *   (P3-AC-1). One source repo at a time; there is no "all repos at once".
 * - **A source repo selected:** its composite scan is deep-merged into the
 *   context tree flat, **source wins** collisions (`mergeTreeSourceWins`,
 *   P3-AC-2/3). A selection with no resolved scan (still loading / empty) also
 *   returns context by reference — never an empty nav.
 */
export function mergeInstalledIntoNav(
  scopedTree: DerivedPipelineTree | null,
  pipelineScope: string,
  installedScans: RepoScanResult[],
  selectedRepoSlug?: string,
): DerivedPipelineTree | null {
  if (!scopedTree) return null;
  // Team-workspace base: no repo selected, or drilled into a context sub-scope.
  if (pipelineScope !== "" || !selectedRepoSlug) return scopedTree;
  const scan = installedScans.find((s) => s.repoSlug === selectedRepoSlug);
  if (!scan) return scopedTree; // selected repo not resolved yet / empty
  return mergeTreeSourceWins(scopedTree, scan.tree, selectedRepoSlug);
}
