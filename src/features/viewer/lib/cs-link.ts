// SPDX-License-Identifier: MIT
import {
  isCsHref,
  parseCsHref,
  resolveCsLink,
  type CsLink,
  type CsLinkStoryNode,
} from "./note-links";

/** SW-SN-9 Task 1: configuration for StarterKit's already-bundled `Link` mark so
 *  an internal `cs:` link survives the note's markdown round-trip and never
 *  auto-opens in a browser.
 *
 *  StarterKit v3 registers `@tiptap/extension-link` unless `link:false`; the
 *  scratch editor keeps it, so we RECONFIGURE it here (adding a second Link
 *  extension would be a duplicate-mark schema error). The load-bearing setting
 *  is `isAllowedUri`/`protocols`: Link's default allow-list is
 *  http/https/ftp/ftps/mailto and it silently strips a `cs:` href on parse —
 *  markdown-it itself passes `cs:` fine (its `validateLink` only blocks
 *  vbscript/javascript/file/data). See R1 + `cs-link.integration.test.ts`.
 *
 *  - `protocols: ["cs"]`  — additive; registers `cs` with the linkifier.
 *  - `isAllowedUri`       — admit `cs:` past the sanitizer, defer to the default
 *                           validator for everything else (http/https/mailto).
 *  - `openOnClick: false` — activation is the editor's click/keyboard dispatcher
 *                           (Task 3), not a browser navigation.
 *  - `autolink/linkOnPaste: false` — never fight `handlePaste` /
 *                           `transformPastedText`; links are authored only via
 *                           the `[[` picker (Task 2) or `setLink`. */
export const csLinkOptions = {
  openOnClick: false,
  autolink: false,
  linkOnPaste: false,
  protocols: ["cs"],
  // AC-7: a stable class so note links are visually identifiable and carry a
  // `:focus-visible` ring (styled in the editor CSS). `rel` hardens external
  // links opened via the click dispatcher.
  HTMLAttributes: { class: "cs-note-link", rel: "noopener noreferrer" },
  isAllowedUri: (uri: string, ctx: { defaultValidate: (u: string) => boolean }): boolean =>
    isCsHref(uri) || ctx.defaultValidate(uri),
};

/** Everything the activation dispatcher needs — injected so the logic stays pure
 *  and unit-testable (the prop-less panel wires the live store/workspace values). */
export interface CsLinkDispatchDeps {
  /** Warm pipeline node data (`nodes.map((n) => n.data)`) for `cs:story` resolution. */
  nodes: readonly CsLinkStoryNode[];
  teamContentPath: string;
  teamKey: string;
  repoPath: string | null;
  /** Whether an id is present in the live notes list (a deleted note → inert). */
  noteExists: (id: string) => boolean;
  setActiveNote: (id: string) => void;
  openFile: (teamKey: string, repoPath: string, filePath: string) => void;
}

/** SW-SN-9 Task 3: activate a parsed `cs:` link. Returns `true` when a real
 *  target was dispatched, `false` when inert — an unresolvable story id, a
 *  deleted note, a non-viewer file, or no open workspace (AC-5's graceful
 *  degradation: no crash, no phantom open). Never leaves the current note. */
export function dispatchCsLink(link: CsLink, deps: CsLinkDispatchDeps): boolean {
  const resolved = resolveCsLink(link, {
    nodes: deps.nodes,
    teamContentPath: deps.teamContentPath,
  });
  if (!resolved) return false;

  if (resolved.noteId !== undefined) {
    if (!deps.noteExists(resolved.noteId)) return false;
    deps.setActiveNote(resolved.noteId);
    return true;
  }

  // `cs:story`/`cs:file` both resolve to a file path; opening needs a workspace.
  if (!resolved.filePath || deps.repoPath === null) return false;
  // v1: a `cs:file` `#hash` (resolved.hash) is parsed but not forwarded — the
  // viewer's scroll-to-hash is component-local state unreachable from this
  // prop-less panel; opening the doc is the whole activation.
  deps.openFile(deps.teamKey, deps.repoPath, resolved.filePath);
  return true;
}

/** Outcome of a link activation — lets the caller give feedback (a broken `cs:`
 *  link is consumed but should toast rather than silently do nothing, AC-5). */
export type NoteLinkClickResult = "activated" | "broken" | "external" | "ignored";

/** Handle a click/activation on ANY link href in a note: an internal `cs:` link
 *  dispatches through {@link dispatchCsLink} (`activated`, or `broken` when
 *  unresolvable); an `http(s)` link routes to the external opener (`external` —
 *  Link's `openOnClick:false` means we own this). Anything else is left to the
 *  editor (`ignored`). Every result except `ignored` consumed the click. */
export function handleNoteLinkHref(
  href: string,
  deps: CsLinkDispatchDeps,
  openExternal: (href: string) => void,
): NoteLinkClickResult {
  const cs = parseCsHref(href);
  if (cs) return dispatchCsLink(cs, deps) ? "activated" : "broken";
  if (/^https?:\/\//i.test(href)) {
    openExternal(href);
    return "external";
  }
  return "ignored";
}
