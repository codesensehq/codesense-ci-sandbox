// SPDX-License-Identifier: MIT
import { resolveViewerLinkPath } from "@/shared/lib/viewer-file-type";

/** SW-SN-9: the shared, framework-free link model for scratch notes.
 *
 *  A note can link to another note, a pipeline story doc, or a repo file via an
 *  internal `cs:` URL scheme. This module is the SINGLE source of truth for
 *  parsing, resolving, and constructing those links — the DRY seam SW-SN-10's
 *  backlink scan imports (AC-6) and the editor's activation dispatcher (Task 3)
 *  dispatches on. It holds NO tiptap/React types so it stays pure and unit-
 *  testable (the tiptap Link config lives in `cs-link.ts`).
 *
 *  Href grammar: `cs:<kind>:<target>`
 *   - `cs:note:<id>`    — another note, by SW-SN-1 immutable id (rename-proof).
 *   - `cs:story:<id>`   — a pipeline doc, by canonical `storyId` (move/rename-proof).
 *   - `cs:file:<rel>`   — a repo file, by path relative to `teamContentPath`.
 */

export type CsLinkKind = "note" | "story" | "file";

export interface CsLink {
  kind: CsLinkKind;
  /** Note id, storyId, or content-root-relative file path (may carry `#hash`). */
  target: string;
}

export const CS_SCHEME = "cs:";

const CS_KINDS: readonly CsLinkKind[] = ["note", "story", "file"];

// A `cs:` href rides inside a markdown link `[label](href)`, so the target must
// not contain characters that break markdown link URLs — spaces and parentheses
// (a file can legitimately be named "My Doc (v2).md"). `#` is also encoded so a
// literal `#` in a filename isn't mistaken for a fragment delimiter (v1 authors
// no fragments). Percent-encode those (and `%` itself, FIRST, so decoding is
// unambiguous) on the way out; reverse on the way in. `/` and `:` stay literal —
// valid in a markdown URL, and the resolver relies on them.
function encodeCsTarget(target: string): string {
  return target
    .replace(/%/g, "%25")
    .replace(/ /g, "%20")
    .replace(/\(/g, "%28")
    .replace(/\)/g, "%29")
    .replace(/#/g, "%23");
}
function decodeCsTarget(target: string): string {
  return target
    .replace(/%20/g, " ")
    .replace(/%28/gi, "(")
    .replace(/%29/gi, ")")
    .replace(/%23/gi, "#")
    .replace(/%25/g, "%");
}

/** True when `absPath` is `root` or a descendant of it — the shared path-boundary
 *  test used by `toContentRelative` and the picker's file-candidate guard so a
 *  sibling sharing a prefix (`/x/content` vs `/x/content-2`) is never treated as
 *  inside the root. `root` may carry a trailing slash. */
export function isUnderContentRoot(absPath: string, root: string): boolean {
  const r = root.replace(/\/+$/, "");
  return r !== "" && (absPath === r || absPath.startsWith(`${r}/`));
}

/** A minimal projection of a react-flow pipeline node's `data` — the only two
 *  fields link resolution needs. Callers pass `nodes.map((n) => n.data)` so this
 *  module never imports `@xyflow/react`. */
export interface CsLinkStoryNode {
  storyId?: string;
  storyFile?: string;
}

export interface CsLinkResolveContext {
  /** The warm pipeline tree's node data (story identity → file path). */
  nodes: readonly CsLinkStoryNode[];
  /** Content root, for resolving `cs:file` relative targets to absolute paths. */
  teamContentPath: string;
}

export interface CsResolved {
  /** Absolute file path for a `story`/`file` link. */
  filePath?: string;
  /** Note id for a `note` link (existence is validated by the caller). */
  noteId?: string;
  /** Fragment (without `#`) carried by a `cs:file` target, for scroll-to. */
  hash?: string;
}

/** Serialize a link to its `cs:` href form (target percent-encoded so it's a
 *  valid markdown link URL). */
export function formatCsHref(link: CsLink): string {
  return `${CS_SCHEME}${link.kind}:${encodeCsTarget(link.target)}`;
}

/** Parse a `cs:` href into a `CsLink` (target percent-decoded), or `null` when
 *  it is not a well-formed internal link. Splits on the FIRST colon after the
 *  scheme+kind so a target may itself contain slashes and a `#fragment` (file
 *  paths never contain `:`, and note ids / storyIds are colon-free). */
export function parseCsHref(href: string): CsLink | null {
  if (!href.startsWith(CS_SCHEME)) return null;
  const rest = href.slice(CS_SCHEME.length);
  const sep = rest.indexOf(":");
  if (sep < 0) return null;
  const kind = rest.slice(0, sep);
  const target = rest.slice(sep + 1);
  if (!CS_KINDS.includes(kind as CsLinkKind) || !target) return null;
  return { kind: kind as CsLinkKind, target: decodeCsTarget(target) };
}

/** True for any `cs:` href this module understands — the allow-test the tiptap
 *  Link `isAllowedUri` uses so the scheme survives sanitization (R1). */
export function isCsHref(href: string): boolean {
  return parseCsHref(href) !== null;
}

const CS_LINK_MD_RE = /\]\((cs:(?:note|story|file):[^)\s]+)\)/g;

/** Extract every `cs:` link from a note's markdown body, in document order.
 *  Pure string scan (no editor instance) — SW-SN-10's backlink derivation and
 *  any future indexer share this one parser (AC-6). */
export function extractNoteLinks(noteBodyMarkdown: string): CsLink[] {
  const links: CsLink[] = [];
  for (const match of noteBodyMarkdown.matchAll(CS_LINK_MD_RE)) {
    const link = parseCsHref(match[1]);
    if (link) links.push(link);
  }
  return links;
}

/** Resolve a `cs:` link to a concrete target, or `null` when it cannot resolve
 *  (unknown story id, a file path outside the viewer's supported extensions).
 *  A `note` link always resolves to its id here — the caller checks the id
 *  against the live notes list (a deleted note degrades to inert, AC-5). This is
 *  the exact resolution the activation dispatcher (Task 3) and SW-SN-10 reuse. */
export function resolveCsLink(link: CsLink, ctx: CsLinkResolveContext): CsResolved | null {
  switch (link.kind) {
    case "note":
      return { noteId: link.target };
    case "story": {
      const node = ctx.nodes.find((n) => n.storyId === link.target);
      return node?.storyFile ? { filePath: node.storyFile } : null;
    }
    case "file": {
      const [pathPart, hash] = splitHash(link.target);
      // `cs:file` targets are content-root-relative; anchor with a leading "/"
      // so `resolveViewerLinkPath` roots them at `teamContentPath` (it also
      // rejects non-viewer extensions and normalizes `..`). Normalize a trailing
      // slash so the root matches `toContentRelative`'s form.
      const root = ctx.teamContentPath.replace(/\/+$/, "");
      const filePath = resolveViewerLinkPath(`/${pathPart}`, root, root);
      if (!filePath) return null;
      return hash ? { filePath, hash } : { filePath };
    }
  }
}

export interface DocIdentity {
  /** Canonical `storyId` when the doc is a pipeline story (preferred). */
  storyId?: string;
  /** Absolute file path (used when the doc is not a pipeline story). */
  filePath?: string;
  /** Content root, needed to make `filePath` content-root-relative. */
  teamContentPath?: string;
}

/** Build a `cs:` link *to* a doc — the inverse of `resolveCsLink`, seeded by
 *  SW-SN-10's "New note about this". Prefers the stable `storyId`; falls back to
 *  a content-root-relative file link. Returns `null` when neither is available. */
export function buildCsLinkToDoc(doc: DocIdentity): CsLink | null {
  if (doc.storyId) return { kind: "story", target: doc.storyId };
  // A file link needs the content root to make a portable relative target;
  // without it, refuse rather than bake a non-portable host-absolute path.
  if (doc.filePath && doc.teamContentPath) {
    return { kind: "file", target: toContentRelative(doc.filePath, doc.teamContentPath) };
  }
  return null;
}

/** Strip a leading `teamContentPath` (and slash) so a `cs:file` target is a
 *  portable, content-root-relative path. Requires a real path boundary so a
 *  sibling dir sharing a prefix (`/x/content` vs `/x/content-2`) isn't mangled. */
export function toContentRelative(absPath: string, teamContentPath: string): string {
  if (isUnderContentRoot(absPath, teamContentPath)) {
    const root = teamContentPath.replace(/\/+$/, "");
    return absPath.slice(root.length).replace(/^\/+/, "");
  }
  return absPath.replace(/^\/+/, "");
}

function splitHash(target: string): [string, string | undefined] {
  const idx = target.indexOf("#");
  if (idx < 0) return [target, undefined];
  return [target.slice(0, idx), target.slice(idx + 1) || undefined];
}
