// SPDX-License-Identifier: MIT
import { PIPELINE_EXTENSIONS } from "./pipeline-file-filter";
import type { DerivedPipelineTree } from "./types";
import { getViewerFileType } from "@/shared/lib/viewer-file-type";
import {
  firstStatusId,
  isKnownStatusId,
  roleOf,
  type StatusIndex,
} from "@/shared/stores/status-taxonomy-store";

/**
 * SW-GT-3: "work is actively underway" is now a ROLE, not a fixed id set — a
 * story whose state's role is `active` or `review` (in-progress + review/qa in
 * the default taxonomy; `blocked` is deliberately NOT active). Single source of
 * truth for the epic progress strip's `active` count (`derived-tree-to-react-flow`)
 * and the epic chrome status rollup (`rollupEpicStatus`), keyed off role so it
 * survives an arbitrary state set.
 */
export function isActiveStatus(status: string): boolean {
  const role = roleOf(status);
  return role === "active" || role === "review";
}

/** SW-GT-3: role-keyed status predicates — the single seam every consumer keys
 *  off instead of a literal id, so control flow survives a fully-replaced state
 *  set. Each takes a status id and reads the live snapshot; an off-taxonomy id
 *  has no role → every predicate is `false` (safe default). */
export function isDoneStatus(status: string, byId?: StatusIndex): boolean {
  return roleOf(status, byId) === "done";
}
export function isBlockedStatus(status: string, byId?: StatusIndex): boolean {
  return roleOf(status, byId) === "blocked";
}
export function isNotStartedStatus(status: string, byId?: StatusIndex): boolean {
  return roleOf(status, byId) === "not_started";
}

/**
 * Resolve a file reference against a group's file map.
 * Priority: exact match → unambiguous stem match → null.
 * When two files share the same stem (e.g. `foo.md` + `foo.html`),
 * returns `null` — the ref is ambiguous and must include the extension.
 */
export function resolveFileInGroup(
  files: ReadonlyMap<string, unknown>,
  filePortion: string,
): string | null {
  if (files.has(filePortion)) return filePortion;
  const stem = stripPipelineExtension(filePortion);
  let match: string | null = null;
  for (const key of files.keys()) {
    if (stripPipelineExtension(key) === stem) {
      if (match !== null) return null; // ambiguous — two files share stem
      match = key;
    }
  }
  return match;
}

/**
 * Strip any recognized pipeline extension (case-insensitive) from a filename
 * or filename-shaped reference. Returns the stem.
 */
export function stripPipelineExtension(name: string): string {
  const dotIdx = name.lastIndexOf(".");
  if (dotIdx < 0) return name;
  const ext = name.slice(dotIdx + 1).toLowerCase();
  if (PIPELINE_EXTENSIONS.has(ext)) return name.slice(0, dotIdx);
  return name;
}

/**
 * Abbreviate a story file stem to its short label form.
 * `"W-5-source-repo"` → `"W.5"`, `"no-prefix"` → `"no-prefix"`.
 */
export function abbreviateStem(stem: string): string {
  const m = stem.match(/^([A-Za-z]+)-(\d+)/);
  return m ? `${m[1]}.${m[2]}` : stem;
}

/**
 * Normalize a raw frontmatter `status` string to a valid status id in the LIVE
 * taxonomy. A recognized id passes through; null / empty / off-taxonomy values
 * fall back to the taxonomy-derived default — the first state by `order` (AC-7) —
 * NOT a hardcoded `"todo"`, so a team that replaces the defaults still gets a
 * sensible default. Reads the snapshot via `getState()` (pure, no hook).
 */
export function normalizeStatus(raw: string | null, byId?: StatusIndex): string {
  // Trim so a padded scalar (`" todo "`) resolves the same known id the board's
  // drift check does (it trims too) — otherwise the two paths disagree and a
  // padded-but-valid status silently falls through to the default.
  const trimmed = raw?.trim() ?? "";
  if (trimmed !== "" && isKnownStatusId(trimmed, byId)) return trimmed;
  return firstStatusId(byId) ?? "";
}

// ---------------------------------------------------------------------------
// Document type helpers (shared by pipeline-status-nav + derived-tree-to-react-flow)
// ---------------------------------------------------------------------------

export const KNOWN_DOCUMENT_TYPES: ReadonlySet<string> = new Set([
  "story",
  "bug",
  "spike",
  "doc",
  "retro",
  // SW-SC-2: `.excalidraw` supporting docs (DocRefs) get a synthesized
  // `canvas` type via `docTypeFromFileName` — keep it "known" so it isn't
  // re-normalized to "doc" or reported as an unknown type.
  "canvas",
  // SW-SC-7: images are first-class supporting docs (synthesized `image` type
  // via `docTypeFromFileName`). Kept "known" for the same reason as `canvas`.
  // Non-actionable — `ACTIONABLE_DOCUMENT_TYPES` is unchanged, so an image
  // never renders as a pipeline node.
  "image",
]);

/** Actionable types render as pipeline nodes / StoryRow. Non-actionable types
 *  (doc, retro) render as DocRow / supporting docs only. */
export const ACTIONABLE_DOCUMENT_TYPES: ReadonlySet<string> = new Set(["story", "bug", "spike"]);

/**
 * Normalize a raw `type` frontmatter value to a known document type.
 * Returns `"story"` for null/empty, the lowercase value for known types,
 * and `"doc"` for unrecognized values.
 */
export function normalizeDocumentType(raw: string | null): string {
  if (raw == null) return "story";
  const lower = raw.trim().toLowerCase();
  if (lower === "") return "story";
  if (KNOWN_DOCUMENT_TYPES.has(lower)) return lower;
  return "doc";
}

/** True when the raw `type` frontmatter value normalizes to an actionable
 *  pipeline type (story, bug, spike). */
export function isActionableDocumentType(raw: string | null): boolean {
  return ACTIONABLE_DOCUMENT_TYPES.has(normalizeDocumentType(raw));
}

/**
 * SW-SC-2/SW-SC-7: synthesize a document type for a scanner `DocRef` (which
 * carries no frontmatter / `type:`) from its file extension, so it resolves as
 * a first-class supporting doc with a type-appropriate icon:
 *   images → `image` (SW-SC-7); `.excalidraw` → `canvas` (SW-SC-2);
 *   everything else → `doc` (safe default — future types resolve without code).
 * Reuses the viewer registry (`getViewerFileType`) as the image-extension truth.
 */
export function docTypeFromFileName(fileName: string): string {
  if (getViewerFileType(fileName) === "image") return "image";
  const dotIdx = fileName.lastIndexOf(".");
  const ext = dotIdx >= 0 ? fileName.slice(dotIdx + 1).toLowerCase() : "";
  return ext === "excalidraw" ? "canvas" : "doc";
}

/**
 * Compute a dirPath relative to a scope root. Used by `buildGroupIndex`,
 * `derivedTreeToReactFlow`, and the nav readiness threading in
 * `PipelineStatusNav` — all three must agree on the key format.
 */
export function scopeRelative(dirPath: string, scopeDirPath: string): string {
  if (scopeDirPath.length === 0) return dirPath;
  if (dirPath === scopeDirPath) return "";
  return dirPath.slice(scopeDirPath.length + 1);
}

/**
 * Strip a leading `./` prefix from a dependency or supporting_docs reference.
 * Accepts bare filenames (`bug.md`) and dotted (`./bug.md`).
 */
export function stripDotSlash(ref: string): string {
  return ref.startsWith("./") ? ref.slice(2) : ref;
}

/**
 * Resolve a relative path against a scope-relative base. Handles `..` and `.`
 * segments. Returns `null` when `..` over-pops past the scope root.
 *
 * Extracted from `derived-tree-to-react-flow.ts` for shared use by both
 * the React Flow transform and the nav readiness resolver.
 */
export function resolveScopeRelativePath(base: string, relative: string): string | null {
  const parts = base.split("/").filter(Boolean);
  for (const seg of relative.split("/")) {
    if (seg === "..") {
      if (parts.length === 0) return null;
      parts.pop();
    } else if (seg !== "." && seg !== "") {
      parts.push(seg);
    }
  }
  return parts.join("/");
}

/**
 * Find the subtree at `scopePath` within a full tree. Returns the subtree
 * node, or `null` if the scope path doesn't exist in the tree. Empty scope
 * returns the root tree itself.
 */
export function findScopedSubtree(
  tree: DerivedPipelineTree,
  scopePath: string,
): DerivedPipelineTree | null {
  if (scopePath === "" || scopePath === tree.dirPath) return tree;
  for (const sub of tree.subEpics) {
    if (sub.dirPath === scopePath) return sub;
    if (scopePath.startsWith(sub.dirPath + "/")) {
      return findScopedSubtree(sub, scopePath);
    }
  }
  return null;
}

/**
 * The deepest subtree that still exists along `scopePath`: the scoped subtree
 * itself when present, else its nearest surviving ancestor, else the root.
 * A drill into a folder that has since been removed falls back to this.
 */
export function nearestExistingSubtree(
  tree: DerivedPipelineTree,
  scopePath: string,
): DerivedPipelineTree {
  let current = tree;
  for (;;) {
    const next = current.subEpics.find(
      (sub) => sub.dirPath === scopePath || scopePath.startsWith(sub.dirPath + "/"),
    );
    if (!next) return current;
    if (next.dirPath === scopePath) return next;
    current = next;
  }
}
