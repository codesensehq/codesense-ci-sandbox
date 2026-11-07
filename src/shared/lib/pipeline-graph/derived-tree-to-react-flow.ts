// SPDX-License-Identifier: MIT
import type { Node, Edge } from "@xyflow/react";
import type {
  DerivedPipelineTree,
  GroupNodeData,
  PipelineStatus,
  ResolvedDep,
  ResolvedSupportingDoc,
  StoryNodeData,
  StoryRef,
} from "./types";
import {
  abbreviateStem,
  resolveFileInGroup,
  stripPipelineExtension,
  normalizeStatus,
  isActiveStatus,
  isBlockedStatus,
  isDoneStatus,
  normalizeDocumentType as normalizeDocumentTypePure,
  resolveScopeRelativePath,
  stripDotSlash,
  scopeRelative as scopeRelativeUtil,
  ACTIONABLE_DOCUMENT_TYPES,
  KNOWN_DOCUMENT_TYPES,
  docTypeFromFileName,
} from "./pipeline-utils";
import { buildGroupIndex, buildGroupStoryIdIndex, type FullGroupIndex } from "./build-group-index";
import { resolveStoryIdRef } from "./story-id-index";
import { resolveReadiness } from "./resolve-nav-readiness";
import { getEpicColor, getEpicBadgeBg } from "./epic-colors";
import { basename } from "../basename";
import { resolveRelativePath } from "../paths/resolve-relative-path";

/**
 * Transform the scanner's `DerivedPipelineTree` into React Flow nodes and
 * edges (story 30.7 AC-20..26; story 30.9 frontmatter-mode branch).
 *
 * Shape:
 * - The scope root is a pseudo-epic: its stories and sub-epics emit at the
 *   top level with no `parentId`. The pseudo-epic itself is NOT emitted.
 * - Sub-epics emit as `type: "group"` React Flow nodes with children
 *   parented to them.
 * - Stories emit as `type: "story"` nodes; `data.storyFile` is always
 *   `storyRef.filePath` (an absolute filesystem path).
 * - Layout coordinates are `{ x: 0, y: 0 }` — `applyNestedDagreLayout`
 *   assigns real positions later.
 *
 * Node ids (AC-22):
 * - Story: scope-relative path including filename, e.g. `"epic-a/a.md"`.
 *   At the scope root, this is just the bare filename.
 * - Group: `"group:<scope-relative-dir-path>"`.
 *
 * Edges — derived from frontmatter `dependsOn` (story 30.9, P.3):
 * - Edges are emitted when any direct-child story has a non-empty
 *   `dependsOn` array. One edge per `(predecessor, story)` pair,
 *   predecessor resolved by bare-stem lookup against sibling stories.
 * - Epics without frontmatter edges render with no edges (solid border).
 *
 * Empty groups (P.12): a group with zero actionable stories AND zero
 * sub-epics is suppressed entirely — no group node is emitted.
 */

type DerivedTreeFlow = {
  nodes: Node[];
  edges: Edge[];
  /** Unknown `type` frontmatter values encountered during this transform. */
  unknownDocumentTypes: string[];
  /** Supporting doc refs that failed to resolve (F.4). */
  unresolvedSupportingDocRefs: string[];
};

// KNOWN_DOCUMENT_TYPES and ACTIONABLE_DOCUMENT_TYPES imported from pipeline-utils.

/** True if `ref` (after `./` stripping) contains a directory separator,
 *  meaning it's a cross-directory reference which is out of scope. */
function isCrossDirectoryRef(stripped: string): boolean {
  return stripped.includes("/");
}

export function derivedTreeToReactFlow(
  tree: DerivedPipelineTree,
  opts?: {
    isLightTheme?: boolean;
    /**
     * SW-FX-1: the FULL team-content index (scope `""`). When provided, an
     * out-of-scope cross-epic dep badge is resolved against it — epic color,
     * title, and `met` status — so the badge looks the same drilled-in as
     * drilled-out. The dep is still rendered as `outOfScope` (no edge — its node
     * isn't visible here — and clicks route through the drill-to-root handler).
     * Omit it and out-of-scope deps render plain with `met: undefined` (legacy).
     */
    fullGroupIndex?: FullGroupIndex;
  },
): DerivedTreeFlow {
  const isLightTheme = opts?.isLightTheme ?? false;
  const fullGroupIndex = opts?.fullGroupIndex;
  const nodes: Node[] = [];
  // SW-FX-48: id→node index maintained incrementally as nodes are pushed. Every
  // group/story lookup below reads this map instead of rescanning the whole
  // `nodes` array, so the build stays O(n) rather than O(groups × nodes) — the
  // latter reached millions of ops at ~3,000 nodes and helped hang the renderer.
  const nodeById = new Map<string, Node>();
  const edges: Edge[] = [];
  const unknownDocTypesSet = new Set<string>();
  const unresolvedRefsSet = new Set<string>();

  /** Normalize with side-effect: unknown types are tracked for reporting. */
  function normalizeDocumentType(raw: string | null): string {
    const result = normalizeDocumentTypePure(raw);
    if (
      result === "doc" &&
      raw != null &&
      raw.trim() !== "" &&
      !KNOWN_DOCUMENT_TYPES.has(raw.trim().toLowerCase())
    ) {
      unknownDocTypesSet.add(raw.trim());
    }
    return result;
  }

  const scopeDirPath = tree.dirPath; // may be ""

  function scopeRelative(dirPath: string): string {
    return scopeRelativeUtil(dirPath, scopeDirPath);
  }

  function storyNodeId(storyFileName: string, ownerScopeRel: string): string {
    return ownerScopeRel.length === 0 ? storyFileName : `${ownerScopeRel}/${storyFileName}`;
  }

  function groupNodeId(ownerScopeRel: string): string {
    return `group:${ownerScopeRel}`;
  }

  function titleForStory(s: StoryRef): string {
    // R-14: scanner no longer populates `title` — in production this falls
    // through to filename-stripped stem. Test fixtures may still set
    // `title` explicitly (deprecated path); the fallback honours both.
    return s.title ?? stripPipelineExtension(s.fileName);
  }

  /** Resolve frontmatter status to a live-taxonomy id (SW-GT-3). A custom id flows
   *  through unchanged; an off-taxonomy id falls back to the taxonomy default. */
  function statusForStory(s: StoryRef): PipelineStatus {
    return normalizeStatus(s.status);
  }

  /** Construct a React Flow edge with the canonical id/shape. Centralizes
   *  the id format so a future change to the edge payload only needs one
   *  update.
   *
   *  P.2: Extended with optional `opts` for cross-epic edges (tooltip,
   *  persistent label, custom edge type). Intra-epic callers pass no opts
   *  — behavior is unchanged. */
  function makeEdge(
    source: string,
    target: string,
    opts?: {
      tooltip?: string;
      label?: string;
      idSuffix?: string;
      type?: string;
      strokeColor?: string;
    },
  ): Edge {
    const id = opts?.idSuffix ? `${source}->${target}:${opts.idSuffix}` : `${source}->${target}`;
    const edge: Edge = { id, source, target };
    if (opts?.type) edge.type = opts.type;
    if (opts?.strokeColor) {
      edge.style = { ...edge.style, stroke: opts.strokeColor };
      edge.markerEnd = {
        type: "arrowclosed" as const,
        width: 16,
        height: 16,
        color: opts.strokeColor,
      };
    }
    if (opts?.tooltip) {
      edge.data = { ...edge.data, tooltip: opts.tooltip };
    }
    if (opts?.label) {
      edge.label = opts.label;
      edge.labelBgPadding = [4, 2] as [number, number];
      edge.labelBgBorderRadius = 4;
      edge.labelStyle = { fontSize: 11, fontWeight: 500, fill: "var(--popover-foreground)" };
      edge.labelBgStyle = {
        fill: "var(--popover)",
        stroke: "var(--border)",
        strokeWidth: 0.5,
      };
    }
    return edge;
  }

  function titleForGroup(dirName: string): string {
    return dirName;
  }

  // ---------------------------------------------------------------------------
  // P.2: Group index — built via shared buildGroupIndex, then augmented with
  // React Flow–specific groupId and normalized documentType.
  // ---------------------------------------------------------------------------

  type GroupIndexFile = {
    filePath: string;
    documentType: string;
    title: string;
    status: string | null;
    /** SW-SB-9: stable story_id (stories only) for location-independent deps. */
    storyId?: string;
  };

  type GroupIndexEntry = {
    groupId: string;
    dirName: string;
    files: Map<string, GroupIndexFile>;
  };

  // Build the shared raw index (includes root at ""), then augment for
  // React Flow: add groupId per entry, normalize documentType per file.
  // F.32: The root entry (key "") is included so root-level stories are
  // resolvable by resolveCrossEpicRef. No visible group node is emitted
  // for the root (isPseudoScopeRoot guard at line 302 handles that).
  const rawGroupIndex = buildGroupIndex(tree);
  const groupIndex = new Map<string, GroupIndexEntry>();
  for (const [key, entry] of rawGroupIndex) {
    const files = new Map<string, GroupIndexFile>();
    for (const [fileName, file] of entry.files) {
      files.set(fileName, {
        filePath: file.filePath,
        documentType: normalizeDocumentType(file.documentType),
        title: file.title,
        status: file.status,
        ...(file.storyId ? { storyId: file.storyId } : {}),
      });
    }
    groupIndex.set(key, {
      groupId: groupNodeId(key),
      dirName: entry.dirName,
      files,
    });
  }

  // SW-SB-9/SW-FX-70: global `story_id → {scopeRel, fileName}` index for
  // location-independent `depends_on` resolution, built from the group index so
  // it shares the exact story set the edge resolver sees. Poisoned on duplicate
  // ids (via the shared `buildGroupStoryIdIndex` helper) so an ambiguous id
  // resolves to nothing — a collision can never bind to an arbitrary story (AC-5).
  const byStoryId = buildGroupStoryIdIndex(groupIndex);

  // SW-SB-17: a SECOND, GLOBAL story_id index built from the FULL index (when a
  // drilled view supplies one). Lets a bare story_id dep whose target is OUTSIDE
  // the current drill scope resolve to an out-of-scope badge — the parity the
  // `../path` form already gets via `resolveOutOfScopeDep`. Empty when no full
  // index is supplied (behavior then matches SW-SB-9: such a dep simply drops).
  const byStoryIdFull = buildGroupStoryIdIndex(fullGroupIndex ?? new Map());

  type CrossEpicResolution = {
    groupId?: string;
    edgeLabel: string;
    title: string;
    epicColor: string;
    epicBadgeBg: string;
    met: boolean;
    /** Absolute path to the dependency story file. */
    filePath: string;
    /** F.32: For root-level targets (no visible group node), the story node ID to use as edge target. */
    targetNodeId?: string;
  };

  /**
   * Build the cross-epic edge/badge metadata for a target already resolved to a
   * concrete `(scopeRel, fileName)`. Shared back-half of `resolveCrossEpicRef`
   * (path refs) and the SW-SB-9 story_id alias — a story_id is just a
   * directory-independent alias for a path, so both converge here and can never
   * disagree on how a cross-epic edge is drawn. Returns null when the target
   * group/file is unknown or the target is in the owner's own scope (a
   * same-group edge is emitted by the sibling path, not here).
   */
  function buildCrossEpicResolution(
    targetScopeRel: string,
    fileName: string,
    ownerScopeRel: string,
  ): CrossEpicResolution | null {
    if (targetScopeRel === ownerScopeRel) return null;
    const entry = groupIndex.get(targetScopeRel);
    if (!entry) return null;
    const fileEntry = entry.files.get(fileName);
    if (!fileEntry) return null;

    const edgeLabel = abbreviateStem(stripPipelineExtension(fileName));
    const color = getEpicColor(entry.dirName, isLightTheme);
    const common = {
      edgeLabel,
      title: fileEntry.title,
      epicColor: color,
      epicBadgeBg: getEpicBadgeBg(color, isLightTheme),
      met: isDoneStatus(normalizeStatus(fileEntry.status)),
      filePath: fileEntry.filePath,
    };
    // F.32: Root-level targets have no visible group node — hand back a
    // targetNodeId so the emitter can draw a direct story edge instead.
    return targetScopeRel === ""
      ? { groupId: undefined, ...common, targetNodeId: storyNodeId(fileName, "") }
      : { groupId: entry.groupId, ...common };
  }

  function resolveCrossEpicRef(ref: string, ownerScopeRel: string): CrossEpicResolution | null {
    const lastSlash = ref.lastIndexOf("/");
    const dirPortion = ref.substring(0, lastSlash);
    const filePortion = ref.substring(lastSlash + 1);
    if (!filePortion) return null;

    const targetScopeRel = resolveScopeRelativePath(ownerScopeRel, dirPortion);
    if (targetScopeRel === null) return null;

    const entry = groupIndex.get(targetScopeRel);
    if (!entry) return null;

    const fileName = resolveFileInGroup(entry.files, filePortion);
    if (!fileName) {
      unresolvedRefsSet.add(ref);
      return null;
    }

    return buildCrossEpicResolution(targetScopeRel, fileName, ownerScopeRel);
  }

  /**
   * Resolve an out-of-scope cross-epic dep against the FULL index, so a drilled
   * view renders the same badge as the full view — the epic color (so external
   * deps are visually distinct, not mistaken for internal deps), the target's
   * title, and its drill-independent `met` status. Keyed via `node.dirPath`
   * (the full scanner-relative path, preserved even inside a drilled subtree).
   * Returns an empty result when no full index was supplied or the target group
   * isn't found — the badge then renders plain, exactly as before.
   */
  type OutOfScopeBadge = {
    met?: boolean;
    title?: string;
    epicColor?: string;
    epicBadgeBg?: string;
    /** SW-SB-17: absolute path of the target (for click routing), when resolved. */
    filePath?: string;
  };

  /**
   * Build the out-of-scope badge fields for a target already resolved to a
   * full-tree scope path + filename. Shared by the `../path` form
   * (`resolveOutOfScopeDep`) and the SW-SB-17 story_id form — a story_id is just a
   * location-independent alias for that same target, so both converge here and
   * can never disagree on the badge's color/title/met. Empty when no full index
   * was supplied or the target group isn't found (badge renders plain).
   */
  function outOfScopeBadgeForTarget(
    targetFullRel: string | null,
    fileName: string | null,
  ): OutOfScopeBadge {
    if (!fullGroupIndex || targetFullRel === null) return {};
    const entry = fullGroupIndex.get(targetFullRel);
    if (!entry) return {};
    const epicColor = getEpicColor(entry.dirName, isLightTheme);
    const epicBadgeBg = getEpicBadgeBg(epicColor, isLightTheme);
    const file = fileName ? entry.files.get(fileName) : undefined;
    if (!file) return { epicColor, epicBadgeBg };
    return {
      met: isDoneStatus(normalizeStatus(file.status)),
      title: file.title,
      epicColor,
      epicBadgeBg,
      filePath: file.filePath,
    };
  }

  function resolveOutOfScopeDep(
    node: DerivedPipelineTree,
    dirPortion: string,
    filePortion: string,
  ): OutOfScopeBadge {
    if (!fullGroupIndex) return {};
    const targetFullRel = resolveScopeRelativePath(node.dirPath, dirPortion);
    if (targetFullRel === null) return {};
    const entry = fullGroupIndex.get(targetFullRel);
    const fileName = entry ? resolveFileInGroup(entry.files, filePortion) : null;
    return outOfScopeBadgeForTarget(targetFullRel, fileName);
  }

  /**
   * Emit all nodes for a group (or the pseudo scope root).
   * - The pseudo scope root emits its children at the top level (no parentId).
   * - A real group emits itself as a "group" node AND its children parented to it.
   *
   * `depth` tracks nesting depth for depth-alternating background colors.
   * Top-level emitted epics (children of the pseudo scope root) are depth 0.
   * The pseudo scope root itself is invisible, so recursing from it does
   * NOT increment depth.
   */
  function emitGroup(
    node: DerivedPipelineTree,
    parentGroupId: string | undefined,
    depth: number,
  ): void {
    const ownerScopeRel = scopeRelative(node.dirPath);
    const isPseudoScopeRoot = node === tree;
    // Computed once per group and reused for edge dispatch (P.3).
    // Hoisted out of the pseudo-root branch so the edge-dispatch site
    // below shares the same value.
    const hasFrontmatterEdges = epicHasFrontmatterEdges(node);

    if (!isPseudoScopeRoot) {
      // P.12 merge: compute emptiness from actionable stories, not raw count.
      const actionableCount = node.stories.filter((s) =>
        ACTIONABLE_DOCUMENT_TYPES.has(normalizeDocumentType(s.documentType)),
      ).length;
      const hasChildren = actionableCount > 0 || node.subEpics.length > 0;

      // P.11/P.12: skip truly empty epics entirely — no group node emitted.
      if (!hasChildren) return;

      const data: GroupNodeData = {
        title: titleForGroup(node.dirName),
        absoluteDirPath: node.absoluteDirPath,
        dirPath: node.dirPath,
        depth,
      };
      if (node.orderError) {
        data.orderError = node.orderError;
      }
      const groupNode: Node<GroupNodeData> = {
        id: groupNodeId(ownerScopeRel),
        type: "group",
        position: { x: 0, y: 0 },
        data,
      };
      if (parentGroupId) {
        groupNode.parentId = parentGroupId;
      }
      nodes.push(groupNode);
      nodeById.set(groupNode.id, groupNode);
    }

    const myGroupId = isPseudoScopeRoot ? undefined : groupNodeId(ownerScopeRel);

    // ----- F.4: resolve supporting docs and build exclusion set ----------
    // Build a fileName lookup for all scanned docs in this group.
    const fileNameToRef = new Map<string, StoryRef>();
    for (const s of node.stories) {
      fileNameToRef.set(s.fileName, s);
    }
    // SW-SC-2: same-dir lookup for `docs[]` entries (DocRefs — `.excalidraw`
    // canvases and non-pipeline md/html). They carry no frontmatter, so the
    // documentType is synthesized from the extension. A `supporting_docs:`
    // ref to one of these resolves through this map (stories take precedence).
    const docNameToRef = new Map<string, { filePath: string; documentType: string }>();
    for (const d of node.docs ?? []) {
      if (fileNameToRef.has(d.fileName)) continue;
      docNameToRef.set(d.fileName, {
        filePath: d.filePath,
        documentType: docTypeFromFileName(d.fileName),
      });
    }

    // For each story-type doc with supportingDocs, resolve refs.
    // resolvedMap: story fileName → resolved supporting doc metadata array
    // excludedFileNames: non-story docs referenced by any story's supportingDocs
    const resolvedMap = new Map<
      string,
      Array<{ fileName: string; filePath: string; documentType: string }>
    >();
    const excludedFileNames = new Set<string>();

    for (const s of node.stories) {
      if (normalizeDocumentType(s.documentType) !== "story") continue;
      const refs = s.supportingDocs;
      if (!refs || refs.length === 0) continue;

      const resolved: Array<{ fileName: string; filePath: string; documentType: string }> = [];
      const seenInThisStory = new Set<string>();
      for (const ref of refs) {
        const stripped = stripDotSlash(ref);

        // P.2: cross-directory supporting_docs resolution via groupIndex.
        if (isCrossDirectoryRef(stripped)) {
          const lastSlash = stripped.lastIndexOf("/");
          const dirPortion = stripped.substring(0, lastSlash);
          const filePortion = stripped.substring(lastSlash + 1);
          if (!filePortion) {
            unresolvedRefsSet.add(ref);
            continue;
          }
          const targetScopeRel = resolveScopeRelativePath(ownerScopeRel, dirPortion);
          if (targetScopeRel === null) {
            unresolvedRefsSet.add(ref);
            continue;
          }
          const indexEntry = groupIndex.get(targetScopeRel);
          if (!indexEntry) {
            unresolvedRefsSet.add(ref);
            continue;
          }
          const resolvedFileName = resolveFileInGroup(indexEntry.files, filePortion);
          if (!resolvedFileName) {
            unresolvedRefsSet.add(ref);
            continue;
          }
          const crossFile = indexEntry.files.get(resolvedFileName)!;
          if (crossFile.documentType === "story") continue; // story-type — skip
          // Use scope-qualified key to avoid collision with same-named sibling files.
          const dedupKey = `${targetScopeRel}/${resolvedFileName}`;
          if (seenInThisStory.has(dedupKey)) continue;
          seenInThisStory.add(dedupKey);
          resolved.push({
            fileName: resolvedFileName,
            filePath: crossFile.filePath,
            documentType: crossFile.documentType,
          });
          continue;
        }

        const target = fileNameToRef.get(stripped);
        if (!target) {
          // SW-SC-2: fall back to the `docs[]` lookup (`.excalidraw` canvases
          // and non-pipeline md/html). DocRefs are never story-type and have
          // no pipeline node, so no `excludedFileNames` bookkeeping is needed.
          const docTarget = docNameToRef.get(stripped);
          if (!docTarget) {
            unresolvedRefsSet.add(stripped);
            continue;
          }
          if (seenInThisStory.has(stripped)) continue; // deduplicate
          seenInThisStory.add(stripped);
          resolved.push({
            fileName: stripped,
            filePath: docTarget.filePath,
            documentType: docTarget.documentType,
          });
          continue;
        }
        if (seenInThisStory.has(target.fileName)) continue; // deduplicate
        const targetType = normalizeDocumentType(target.documentType);
        if (targetType === "story") continue; // story-type ref — ignore
        seenInThisStory.add(target.fileName);
        resolved.push({
          fileName: target.fileName,
          filePath: target.filePath,
          documentType: targetType,
        });
        excludedFileNames.add(target.fileName);
      }
      if (resolved.length > 0) {
        resolvedMap.set(s.fileName, resolved);
      }
    }
    // ----- end F.4 supporting docs resolution ----------------------------

    // F.4: also exclude non-actionable types (doc, retro) from the exclusion
    // set so edge emitters skip them too — they have no nodes to edge into.
    for (const s of node.stories) {
      if (!ACTIONABLE_DOCUMENT_TYPES.has(normalizeDocumentType(s.documentType))) {
        excludedFileNames.add(s.fileName);
      }
    }

    // ----- P.1: resolve epic-level supporting docs -----------------------
    // Auto-discover: non-actionable docs NOT claimed by any story's
    // supporting_docs. Merge with explicit cross-directory refs from the
    // main doc's frontmatter. Dedup by resolved absolute file path.
    if (!isPseudoScopeRoot && node.mainDoc) {
      const epicDocs: ResolvedSupportingDoc[] = [];
      const seenPaths = new Set<string>();

      // Build a set of filenames claimed by stories (appear in resolvedMap).
      const storyClaimedFileNames = new Set<string>();
      for (const resolved of resolvedMap.values()) {
        for (const d of resolved) storyClaimedFileNames.add(d.fileName);
      }

      // 1. Auto-discovered local docs: non-actionable types NOT claimed by stories.
      for (const s of node.stories) {
        const docType = normalizeDocumentType(s.documentType);
        if (ACTIONABLE_DOCUMENT_TYPES.has(docType)) continue;
        if (storyClaimedFileNames.has(s.fileName)) continue; // AC-5: story claims win
        if (!seenPaths.has(s.filePath)) {
          seenPaths.add(s.filePath);
          epicDocs.push({
            fileName: s.fileName,
            filePath: s.filePath,
            documentType: docType,
          });
        }
      }

      // 2. Explicit refs from main doc's supporting_docs frontmatter.
      const mainDocRefs = node.mainDoc.supportingDocs;
      if (mainDocRefs && mainDocRefs.length > 0) {
        for (const ref of mainDocRefs) {
          const stripped = stripDotSlash(ref);
          if (isCrossDirectoryRef(stripped)) {
            // Cross-directory: resolve to absolute path optimistically.
            const absPath = resolveRelativePath(node.absoluteDirPath, stripped);
            if (!seenPaths.has(absPath)) {
              seenPaths.add(absPath);
              epicDocs.push({
                fileName: basename(stripped),
                filePath: absPath,
                documentType: "doc",
              });
            }
          } else {
            // Same-directory: resolve against fileNameToRef.
            // AC-5: skip if already claimed by a story node.
            if (storyClaimedFileNames.has(stripped)) continue;
            const target = fileNameToRef.get(stripped);
            if (target && !seenPaths.has(target.filePath)) {
              seenPaths.add(target.filePath);
              epicDocs.push({
                fileName: target.fileName,
                filePath: target.filePath,
                documentType: normalizeDocumentType(target.documentType),
              });
            }
          }
        }
      }

      // Wire into the group node we already pushed above.
      const gid = groupNodeId(ownerScopeRel);
      const gNode = nodeById.get(gid) as Node<GroupNodeData> | undefined;
      if (gNode) {
        if (epicDocs.length > 0) gNode.data.resolvedSupportingDocs = epicDocs;
        if (node.mainDoc.purposeText) gNode.data.purposeText = node.mainDoc.purposeText;
        gNode.data.mainDocFilePath = node.mainDoc.filePath;
      }
    }

    // ----- P.11: compute status counts for epic progress strip ------------
    // Recursively count all descendant actionable stories so parent epics
    // that only contain sub-epics still show aggregate progress.
    if (!isPseudoScopeRoot) {
      let done = 0,
        active = 0,
        blocked = 0,
        total = 0;
      function countDescendants(n: DerivedPipelineTree): void {
        for (const s of n.stories) {
          if (!ACTIONABLE_DOCUMENT_TYPES.has(normalizeDocumentType(s.documentType))) continue;
          total++;
          const st = normalizeStatus(s.status);
          if (isDoneStatus(st)) done++;
          else if (isActiveStatus(st)) active++;
          else if (isBlockedStatus(st)) blocked++;
        }
        for (const sub of n.subEpics) countDescendants(sub);
      }
      countDescendants(node);
      if (total > 0) {
        const gid = groupNodeId(ownerScopeRel);
        const gNode = nodeById.get(gid) as Node<GroupNodeData> | undefined;
        if (gNode) {
          gNode.data.statusCounts = {
            total,
            done,
            active,
            blocked,
            remaining: total - done - active - blocked,
          };
        }
      }
    }
    // ----- end P.11 status counts ----------------------------------------

    // ----- end P.1 epic supporting docs ----------------------------------

    // Emit stories (children of this group).
    for (const s of node.stories) {
      // F.4: skip excluded files (supporting docs + non-actionable types).
      if (excludedFileNames.has(s.fileName)) continue;

      const id = storyNodeId(s.fileName, ownerScopeRel);
      const data: StoryNodeData = {
        title: titleForStory(s),
        status: statusForStory(s),
        storyFile: s.filePath,
        documentType: normalizeDocumentType(s.documentType),
      };
      // R-14: scanner no longer populates `storyText`. Test fixtures may
      // still pass it through (deprecated). Node component should consult
      // `useStoryDetails(filePath)` for the canonical lazy fetch.
      if (s.storyText) data.storyText = s.storyText;
      if (s.storyId) data.storyId = s.storyId;
      if (s.sourceRepo) data.sourceRepo = s.sourceRepo;
      // SW-ID-6: carry the assignee login onto the node for its avatar.
      if (s.assignedTo) data.assignedTo = s.assignedTo;
      // B-4: populate isBlocked from status (was never set before).
      if (isBlockedStatus(data.status)) data.isBlocked = true;
      // Scope-independent "ready" flag for the node indicator. Keyed by the
      // full node.dirPath against the FULL index (when supplied), so it matches
      // the nav/board and doesn't flip on drill. Undefined without a full index.
      if (fullGroupIndex) {
        // SW-FX-70: pass the already-built FULL story_id index so a story_id dep
        // resolves the same here as in nav/board — readiness never flips on drill
        // and never disagrees with the edges drawn from the same index.
        data.isReady = resolveReadiness(s, node.dirPath, fullGroupIndex, byStoryIdFull).ready;
      }
      // F.4: attach resolved supporting docs metadata for badge rendering.
      const supportingDocs = resolvedMap.get(s.fileName);
      if (supportingDocs) data.resolvedSupportingDocs = supportingDocs;

      const storyNode: Node<StoryNodeData> = {
        id,
        type: "story",
        position: { x: 0, y: 0 },
        data,
      };
      if (myGroupId) {
        storyNode.parentId = myGroupId;
      }
      nodes.push(storyNode);
      nodeById.set(storyNode.id, storyNode);
    }

    // Recurse into sub-epics. The pseudo scope root is invisible, so
    // recursing from it does NOT increment depth — its direct children
    // are the first real epics and sit at depth 0.
    const childDepth = isPseudoScopeRoot ? depth : depth + 1;
    for (const sub of node.subEpics) {
      emitGroup(sub, myGroupId, childDepth);
    }

    // Emit edges from frontmatter dependsOn (P.3).
    // F.4: pass excludedFileNames so edges to/from excluded supporting docs
    // are not emitted (they have no corresponding node).
    if (hasFrontmatterEdges) {
      emitEdgesForFrontmatter(node, ownerScopeRel, excludedFileNames);
    }
  }

  /**
   * True iff at least one direct-child story has a non-empty `dependsOn`.
   * Read at most once per group during `emitGroup` and used for edge dispatch.
   */
  function epicHasFrontmatterEdges(node: DerivedPipelineTree): boolean {
    for (const s of node.stories) {
      // F.4: only actionable types count — a non-actionable doc with dependsOn
      // should not flip the epic to frontmatter mode since it has no node.
      if (!ACTIONABLE_DOCUMENT_TYPES.has(normalizeDocumentType(s.documentType))) continue;
      if (s.dependsOn && s.dependsOn.length > 0) return true;
    }
    return false;
  }

  /**
   * Emit one edge per `(predecessor, story)` pair derived from each direct-
   * child story's `dependsOn` frontmatter. Intra-epic refs resolve against
   * sibling stories; cross-epic refs (containing `/` after `./` stripping)
   * resolve against the group index (P.2).
   *
   * Defends against three classes of authoring mistake that the YAML parser
   * can't catch:
   *   - Self-references (`a.md` lists `a` in its own `dependsOn`) → drop;
   *     emitting `a->a` produces a dagre self-loop with no useful meaning.
   *   - Duplicate refs (`depends_on: [a, a.md]` after stem normalization)
   *     → emit each `(source, target)` pair at most once. React Flow keys
   *     edges by `id` and warns on duplicates.
   *
   * Edge id and shape use `makeEdge` for a stable reconciliation key.
   */
  function emitEdgesForFrontmatter(
    node: DerivedPipelineTree,
    ownerScopeRel: string,
    excluded: ReadonlySet<string>,
  ): void {
    const stemToFileName = new Map<string, string | null>();
    const fileNameSet = new Set<string>();
    for (const s of node.stories) {
      if (excluded.has(s.fileName)) continue;
      fileNameSet.add(s.fileName);
      const stem = stripPipelineExtension(s.fileName);
      // Poison-marker: if stem already exists, mark as null (ambiguous).
      stemToFileName.set(stem, stemToFileName.has(stem) ? null : s.fileName);
    }
    // O(1) node lookup for patching resolvedDeps/hasExternalDeps. Reads the
    // shared incremental `nodeById` index (SW-FX-48) — every node emitted so
    // far (this group's stories/descendants included) is already indexed, so
    // no per-group rebuild is needed.
    const emitted = new Set<string>();
    // P.8: Group-level cross-epic accumulation — one edge per (source, target) group pair.
    const groupCrossEpicDeps = new Map<
      string, // sourceGroupId
      {
        epicColor: string;
        tooltipLines: string[];
      }
    >();
    for (const story of node.stories) {
      if (excluded.has(story.fileName)) continue;
      const deps = story.dependsOn;
      if (!deps || deps.length === 0) continue;
      const targetId = storyNodeId(story.fileName, ownerScopeRel);
      let hasDroppedScopeRef = false;
      const allResolvedDeps: ResolvedDep[] = [];
      const seenCrossEpicLabels = new Set<string>();

      for (const rawRef of deps) {
        const ref = stripDotSlash(rawRef);

        // SW-SB-9: a bare ref (no `/`) may be a global story_id — a location-
        // independent alias resolved up front, then fed into the SAME sibling /
        // cross-epic machinery below. A poisoned (duplicate) id → undefined.
        const storyIdAlias = ref.includes("/") ? undefined : resolveStoryIdRef(byStoryId, ref);

        // 1. Exact filename match (handles refs with explicit extension, e.g. "foo.html").
        // 2. Stem lookup (handles bare stems). Poisoned (null) = ambiguous → unresolved.
        // 3. SW-SB-9: a same-group story_id resolves as a sibling (intra-epic edge).
        let sourceFileName: string | undefined;
        if (fileNameSet.has(ref)) {
          sourceFileName = ref;
        } else {
          const stripped = stripPipelineExtension(ref);
          const mapped = stemToFileName.get(stripped);
          if (mapped) sourceFileName = mapped; // null (poisoned) → skip
        }
        if (
          !sourceFileName &&
          storyIdAlias &&
          storyIdAlias.scopeRel === ownerScopeRel &&
          fileNameSet.has(storyIdAlias.fileName)
        ) {
          sourceFileName = storyIdAlias.fileName;
        }
        if (sourceFileName) {
          if (sourceFileName === story.fileName) continue;
          const sourceId = storyNodeId(sourceFileName, ownerScopeRel);
          const edge = makeEdge(sourceId, targetId);
          if (emitted.has(edge.id)) continue;
          emitted.add(edge.id);
          edges.push(edge);
          // P.2: collect for footer badge.
          const depStem = stripPipelineExtension(sourceFileName);
          const depLabel = abbreviateStem(depStem);
          const depRef = node.stories.find((s) => s.fileName === sourceFileName);
          allResolvedDeps.push({
            label: depLabel,
            // R-14: scanner no longer populates title; falls back to depStem.
            title: depRef?.title ?? depStem,
            met: isDoneStatus(normalizeStatus(depRef?.status ?? null)),
            filePath: depRef?.filePath,
          });
          continue;
        }

        // Cross-epic resolution: a path ref (contains `/`) OR a cross-group
        // story_id alias (SW-SB-9 — the same-group case was already emitted as a
        // sibling above). Both converge on `buildCrossEpicResolution`.
        let resolved: CrossEpicResolution | null = null;
        if (ref.includes("/")) {
          resolved = resolveCrossEpicRef(ref, ownerScopeRel);
          if (!resolved) {
            const lastSlash = ref.lastIndexOf("/");
            const dirPortion = ref.substring(0, lastSlash);
            const filePortion = ref.substring(lastSlash + 1);
            const targetRel = resolveScopeRelativePath(ownerScopeRel, dirPortion);
            if (targetRel === null || !groupIndex.has(targetRel)) {
              hasDroppedScopeRef = true;
              // F.32: Construct badge from ref string for out-of-scope deps.
              if (filePortion) {
                const stem = stripPipelineExtension(filePortion);
                const resolvedPath = resolveRelativePath(node.absoluteDirPath, ref);
                // Append .md only if the ref didn't already have a pipeline extension.
                const hasExt = filePortion !== stem;
                // Resolve against the full index so the badge matches the
                // drilled-out view: epic color (marks it external), title, and
                // drill-independent met. Stays `outOfScope` for click routing.
                const ext = resolveOutOfScopeDep(node, dirPortion, filePortion);
                allResolvedDeps.push({
                  label: abbreviateStem(stem),
                  title: ext.title ?? stem,
                  epicColor: ext.epicColor,
                  epicBadgeBg: ext.epicBadgeBg,
                  met: ext.met,
                  filePath: hasExt ? resolvedPath : resolvedPath + ".md",
                  outOfScope: true,
                });
              }
            }
            continue;
          }
        } else if (storyIdAlias) {
          resolved = buildCrossEpicResolution(
            storyIdAlias.scopeRel,
            storyIdAlias.fileName,
            ownerScopeRel,
          );
        } else {
          // SW-SB-17: a bare ref may be a story_id whose target lives OUTSIDE the
          // current drill scope — the scoped `storyIdAlias` missed it. Resolve it
          // against the FULL story_id index and render an out-of-scope badge,
          // matching what the equivalent `../path` form does in a drilled view.
          const full = resolveStoryIdRef(byStoryIdFull, ref);
          if (full) {
            hasDroppedScopeRef = true;
            const ext = outOfScopeBadgeForTarget(full.scopeRel, full.fileName);
            allResolvedDeps.push({
              label: abbreviateStem(stripPipelineExtension(full.fileName)),
              title: ext.title ?? stripPipelineExtension(full.fileName),
              epicColor: ext.epicColor,
              epicBadgeBg: ext.epicBadgeBg,
              met: ext.met,
              filePath: ext.filePath,
              outOfScope: true,
            });
          }
          // Not a sibling, not an in-scope story_id, and (if resolved above) an
          // out-of-scope badge — no edge in either case.
          continue;
        }
        if (!resolved) continue;

        // P.8: Per-story dedup guards both badge AND tooltip contribution.
        // Key on groupId::edgeLabel so deps to different source groups with
        // the same abbreviated label (e.g. epic-a/W-5 and epic-b/W-5) are
        // not collapsed.
        const dedupKey = `${resolved.groupId ?? resolved.targetNodeId}::${resolved.edgeLabel}`;
        if (!seenCrossEpicLabels.has(dedupKey)) {
          seenCrossEpicLabels.add(dedupKey);

          // Per-story badge (includes met from P.4).
          allResolvedDeps.push({
            label: resolved.edgeLabel,
            title: resolved.title,
            epicColor: resolved.epicColor,
            epicBadgeBg: resolved.epicBadgeBg,
            met: resolved.met,
            filePath: resolved.filePath,
          });

          // F.32: Root-level target (no group node) — emit direct story edge.
          // ownerScopeRel !== "" guarantees the owning group has a visible node.
          if (!resolved.groupId && resolved.targetNodeId && ownerScopeRel !== "") {
            const ownerGroupId = groupNodeId(ownerScopeRel);
            const directEdge = makeEdge(resolved.targetNodeId, ownerGroupId, {
              tooltip: `${abbreviateStem(stripPipelineExtension(story.fileName))} depends on ${resolved.edgeLabel} — ${resolved.title}`,
              type: "crossEpic",
              strokeColor: resolved.epicColor,
            });
            if (!emitted.has(directEdge.id)) {
              emitted.add(directEdge.id);
              edges.push(directEdge);
            }
          } else if (resolved.groupId) {
            // Group-level tooltip line.
            const depLabel = abbreviateStem(stripPipelineExtension(story.fileName));
            const tooltipLine = `${depLabel} depends on ${resolved.edgeLabel} — ${resolved.title}`;
            let bucket = groupCrossEpicDeps.get(resolved.groupId);
            if (!bucket) {
              bucket = { epicColor: resolved.epicColor, tooltipLines: [] };
              groupCrossEpicDeps.set(resolved.groupId, bucket);
            }
            bucket.tooltipLines.push(tooltipLine);
          }
        }
      }

      // P.2: Set resolved deps and external-dep flag on the story node.
      // Use index lookup instead of linear scan (O(1) vs O(n)).
      if (allResolvedDeps.length > 0 || hasDroppedScopeRef) {
        const storyNode = nodeById.get(targetId);
        if (storyNode) {
          const sd = storyNode.data as StoryNodeData;
          if (allResolvedDeps.length > 0) sd.resolvedDeps = allResolvedDeps;
          if (hasDroppedScopeRef) sd.hasExternalDeps = true;
        }
      }
    }

    // P.8: Emit one group→group edge per source epic.
    const myGroupId = groupNodeId(ownerScopeRel);
    for (const [sourceGroupId, bucket] of groupCrossEpicDeps) {
      const tooltip = bucket.tooltipLines.join("\n");
      const crossEdge = makeEdge(sourceGroupId, myGroupId, {
        tooltip,
        type: "crossEpic",
        strokeColor: bucket.epicColor,
      });
      if (emitted.has(crossEdge.id)) continue;
      emitted.add(crossEdge.id);
      edges.push(crossEdge);
    }
  }

  emitGroup(tree, undefined, 0);

  return {
    nodes,
    edges,
    unknownDocumentTypes: [...unknownDocTypesSet],
    unresolvedSupportingDocRefs: [...unresolvedRefsSet],
  };
}
