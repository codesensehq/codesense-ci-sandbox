// SPDX-License-Identifier: MIT
export const HTML_FRAME_ALLOW = "clipboard-write";

/**
 * Extension-based file-type routing for the viewer.
 *
 * Single config surface: add an entry to VIEWER_EXTENSIONS, a component,
 * and a switch case in renderViewer() to support a new file type.
 */

/** Viewer file types supported by the renderer routing layer. */
export type ViewerFileType = "markdown" | "html" | "excalidraw" | "image" | "code";

/**
 * Map of lowercase file extensions (without leading dot) to their viewer
 * file type.  This is the single source of truth — file tree filtering,
 * tab-name stripping, quick-open indexing, and renderViewer() all derive
 * from this map.
 *
 * Keep in sync with VIEWABLE_EXTENSIONS in src-tauri/src/commands/file_index.rs.
 *
 * Adding a new viewer-supported file type (e.g. image, PDF, code) — three steps:
 *   1. Add the extension(s) to `VIEWER_EXTENSIONS` and a tag to the
 *      `ViewerFileType` union (e.g. `png: "image"` + `| "image"`).
 *   2. Build a component whose props are `CommonViewerProps` (or that
 *      type intersected with format-specific extras, the way
 *      `MarkdownViewer` extends with `MarkdownExtras`). For text formats,
 *      read the file via Tauri `readFile` (markdown's pattern); for
 *      URL-served formats, use `buildViewerUrl(port, teamContentPath, …)`.
 *      The server already serves arbitrary bytes from `~/.codesense/users/`
 *      under `validate_context_path` — no Rust-side viewer changes needed.
 *   3. Add a `case` branch to `renderViewer`'s switch in
 *      `team-workspace-view.tsx`. The `never` exhaustiveness guard will fail
 *      the TS build if step 3 is forgotten.
 */
export const VIEWER_EXTENSIONS = {
  md: "markdown",
  // SW-VE-2: `.markdown` is a real (if uncommon) markdown extension — keep it on
  // the prose renderer. Before VE-2 it reached markdown via the `?? "markdown"`
  // fallback; the fallback now returns `"code"`, so `.markdown` must be listed
  // explicitly or it would regress to a plain-text code view.
  markdown: "markdown",
  html: "html",
  htm: "html",
  excalidraw: "excalidraw",
  // SW-SC-7: images are URL-served via `buildViewerUrl` (the html-viewer
  // pattern), not read through `readFile`. They are *viewable* but NOT
  // *navigable* — deliberately absent from `VIEWER_LINK_RE` below and the
  // HTML-iframe link interceptor (`viewer.rs`).
  png: "image",
  jpg: "image",
  jpeg: "image",
  gif: "image",
  webp: "image",
  svg: "image",
  // SW-VE-2: code / config / data files render read-only + syntax-highlighted
  // in the CM6 `CodeEditor` (the `code` render case in renderViewer). This is
  // the explicit named set; an UNKNOWN text extension still routes to `code`
  // via the getViewerFileType catch-all below (the CodeFileViewer content
  // sniff is the final text-vs-binary authority). Extensions the CM6 language
  // detector highlights (`language-detection.ts`) + the common config/data
  // long tail. Keep in sync with VIEWABLE_EXTENSIONS in file_index.rs.
  yaml: "code",
  yml: "code",
  toml: "code",
  json: "code",
  xml: "code",
  ini: "code",
  env: "code",
  csv: "code",
  sh: "code",
  py: "code",
  rs: "code",
  ts: "code",
  js: "code",
} as const satisfies Record<string, ViewerFileType>;

/**
 * Common prop contract for every viewer-supported file type. New file
 * types (image, PDF, code) should accept this shape directly, or extend
 * it with format-specific extras using a TypeScript intersection (see
 * `MarkdownExtras` in `markdown-viewer.tsx` for the pattern).
 *
 * `teamContentPath` is required — it is the post-PTS-2 per-team content
 * root used for both URL-served file resolution (`buildViewerUrl`) and
 * internal-link nav (`resolveViewerLinkPath`). The pre-PTS-2 split prop
 * `contextRepoRoot` (SF-1) was retired because it converged with
 * `teamContentPath` and keeping both perpetuated drift bugs.
 */
export type CommonViewerProps = {
  /** The owning centre host, including a private document opened in several teams. */
  hostTeamKey?: string;
  filePath: string | null;
  refreshKey?: number;
  teamContentPath: string;
  /** Open a file in the host. `keepLayout`: an open the user did not ask for, which keeps
   *  full-screen CLI on. */
  onNavigate?: (
    filePath: string,
    options?: { openInNewTab?: boolean; hash?: string; keepLayout?: boolean },
  ) => void;
};

/**
 * The lowercased final extension of a path (without the leading dot), or `null`
 * when the path has none. The single extraction rule shared by every predicate
 * below so the "derive the extension the SAME way" invariant the file relies on
 * is enforced by construction, not by three hand-copied `lastIndexOf(".")`
 * blocks that could drift. ({@link stripViewerExtension} keeps its own inline
 * form because it needs the dot INDEX to slice the stem, not just the ext.)
 */
export function extensionOf(filePath: string): string | null {
  const dot = filePath.lastIndexOf(".");
  if (dot === -1) return null;
  return filePath.slice(dot + 1).toLowerCase();
}

/**
 * Determine the viewer file type for a given file path.
 * Extracts the extension, lowercases it, and looks it up in
 * VIEWER_EXTENSIONS.
 *
 * SW-VE-2: the fallback is `"code"`, not `"markdown"` — an unknown text
 * extension (`.conf`, `.properties`) and an extensionless file (a script,
 * `LICENSE`) render as read-only plain/highlighted code, never as markdown
 * prose. Only `.md`/`.markdown` reach the markdown viewer. The final
 * text-vs-binary decision for the catch-all is made by CodeFileViewer's
 * content sniff (a listed binary is gated to a placeholder by
 * {@link isPreviewableFilePath} before it ever reaches this dispatch).
 */
export function getViewerFileType(filePath: string): ViewerFileType {
  const ext = extensionOf(filePath);
  if (ext === null) return "code";
  return (VIEWER_EXTENSIONS as Record<string, ViewerFileType>)[ext] ?? "code";
}

/**
 * Returns true when the given extension (without leading dot) belongs
 * to a viewer-supported file type.  Used by the file tree filter.
 * Case-insensitive — lowercases before lookup.
 */
export function isViewerFile(extension: string | null): boolean {
  if (extension == null) return false;
  return extension.toLowerCase() in VIEWER_EXTENSIONS;
}

/**
 * SW-VE-1: path-based counterpart to {@link isViewerFile} — true when the file
 * at `filePath` has a viewer-supported extension. Derives the extension the
 * SAME way {@link getViewerFileType} does, so the "can this render?" decision
 * never drifts from the dispatch. The context viewer uses this to route a
 * listed-but-unviewable file (yaml/toml/binary/…) to a "no preview" placeholder
 * instead of the markdown fallback rendering it as prose. VE-2 widens
 * `VIEWER_EXTENSIONS` with code types, so those files become viewable here for
 * free and flow into the new `code` render case.
 */
export function isViewerFilePath(filePath: string): boolean {
  return isViewerFile(extensionOf(filePath));
}

/**
 * SW-SC-7: true for derived render sidecars (`<name>.excalidraw.png`) that are
 * agent-only artifacts and must never surface in any user-facing listing.
 *
 * Making `png` viewable (SW-SC-7) is exactly what would otherwise un-hide
 * SW-SC-2's derived canvas render, so every viewable-file listing surface
 * (file-tree filters, quick-open) filters these out. Matched on the full file
 * NAME, not the extension — the extension is just `png`.
 *
 * Mirror of `is_derived_render_artifact`, defined in
 * `src-tauri/src/commands/files.rs` and reused by `file_index.rs` /
 * `pipeline_scanner.rs`.
 */
export function isDerivedRenderArtifact(fileName: string): boolean {
  return /\.excalidraw\.png$/i.test(fileName);
}

/**
 * SW-SC-2: derived PNG render path for a canvas — `design.excalidraw` →
 * `design.excalidraw.png`. Suffix-appended (not extension-swapped) so the
 * association is unambiguous and {@link isDerivedRenderArtifact} can filter it.
 *
 * Lives in this engine-free leaf (not `excalidraw-canvas.tsx`, which statically
 * imports the heavy Excalidraw chunk) so the SW-SC-14 render bridge can derive
 * the sidecar path without pulling that chunk into the eager app bundle.
 */
export function canvasPngPath(excalidrawPath: string): string {
  return `${excalidrawPath}.png`;
}

/**
 * SW-SD-1: coerce arbitrary text (a doc stem or an author-supplied snapshot
 * `name`) into ONE filesystem-safe path segment for a snapshot archive dir.
 *
 * Guarantees a segment with no path separators, no `..` traversal, and no
 * leading dot (a hidden file). This is defense-in-depth + tidy filenames — the
 * hard security boundary is still Rust's `validate_context_path` (it normalizes
 * `..` and checks context-root membership on every `write_content_file`). Kept
 * local to this engine-free leaf; the anchor-oriented `slugify`
 * (`note-headings.ts`) lowercases + collapses to `-`, which would mangle a
 * readable dashboard name, so it is deliberately NOT reused here.
 */
function sanitizeSnapshotSegment(raw: string): string {
  const safe = raw
    .replace(/[/\\]+/g, "-") // path separators → dash (no nesting / escape)
    .replace(/\.{2,}/g, "-") // collapse any `..` run → dash (no traversal)
    .replace(/[^A-Za-z0-9._-]/g, "-") // restrict to a filename-safe charset
    .replace(/^[.-]+/, "") // strip leading dots/dashes (never a hidden file)
    .replace(/-+$/, "") // tidy a trailing dash
    .slice(0, 100); // cap absurd lengths
  return safe || "snapshot";
}

/**
 * SW-SD-1: compact, sortable, filename-safe UTC timestamp for a snapshot
 * filename — e.g. `2026-07-24T18:30:00.123Z` → `20260724T183000123Z`.
 *
 * Pure (takes the `Date`) so callers can inject a fixed instant in tests; the
 * live caller passes `new Date()` at click/verb time (an event handler, never
 * the render path). Lexically sortable === chronologically sortable, so a
 * dashboard's snapshot dir lists as a clean time-series.
 */
export function snapshotTimestamp(date: Date): string {
  return date.toISOString().replace(/[-:.]/g, "");
}

/**
 * SW-SD-1: the ABSOLUTE archive path for a dashboard snapshot —
 * `${teamContentPath}/dashboards/snapshots/${slug}/${ts}.html`.
 *
 * MUST be absolute: `write_content_file` → `validate_context_path_owned`
 * (`src-tauri/src/commands/files.rs`) does a prefix-membership test against the
 * context roots — it does NOT join a relative path to any root, so a relative
 * `dashboards/snapshots/…` string is outside every root and rejected. Both
 * inputs are already absolute in the viewer: `teamContentPath` is the content
 * root and `filePath` sits under it (see `buildViewerUrl`).
 *
 * `slug` = the caller's `name` when provided, else the doc's stem
 * ({@link stripViewerExtension}), sanitized to a single safe segment. `ts` is
 * pre-formatted by {@link snapshotTimestamp} (and re-sanitized defensively so a
 * hand-passed stamp can never inject a separator). Pure and deterministic given
 * `ts` — the whole reason it lives in this testable leaf (Validation review #1).
 */
export function snapshotArchivePath(
  teamContentPath: string,
  filePath: string,
  name: string | undefined,
  ts: string,
): string {
  const root = teamContentPath.endsWith("/") ? teamContentPath.slice(0, -1) : teamContentPath;
  const rawSlug = name ?? stripViewerExtension(filePath.split("/").pop() ?? "");
  const slug = sanitizeSnapshotSegment(rawSlug);
  const stamp = sanitizeSnapshotSegment(ts);
  return `${root}/dashboards/snapshots/${slug}/${stamp}.html`;
}

/**
 * Strip a viewer-recognised extension from a file name.
 * Case-insensitive match.  Only the final extension is removed:
 *   "file.test.html" → "file.test"
 *   "README.MD"      → "README"
 *   "no-ext"         → "no-ext"
 *
 * SW-SC-7: image extensions are kept (`screenshot.png` → `screenshot.png`).
 * Unlike md/html docs, the format is salient for an image and there is no
 * doc-style naming collision to disambiguate by stripping.
 *
 * SW-VE-2: `code`-type extensions are likewise KEPT (`config.yaml` →
 * `config.yaml`) — the extension is salient for a config/code file and
 * stripping it would collide `config.yaml` with `config.json` in tab/nav
 * labels. Only doc types (markdown/html) strip.
 */
export function stripViewerExtension(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  if (dot === -1) return fileName;
  const ext = fileName.slice(dot + 1).toLowerCase();
  const type = (VIEWER_EXTENSIONS as Record<string, ViewerFileType>)[ext];
  if (type === undefined || type === "image" || type === "code") return fileName;
  return fileName.slice(0, dot);
}

/**
 * SW-VE-2: true only for the doc types whose frontmatter the Metadata band may
 * edit — markdown and HTML. VE-1 gated the metadata chip on {@link isViewerFilePath}
 * because, back then, that set WAS exactly {markdown, html, image, excalidraw}.
 * VE-2 widens `VIEWER_EXTENSIONS` with `code` types, so `isViewerFilePath` now
 * returns true for `.yaml/.json/.toml/…` — using it for the metadata gate would
 * re-expose the frontmatter editor for config/code files and `apply_metadata_edit`
 * (fail-open) would inject a `---` block and corrupt them. This predicate keeps the
 * metadata editor doc-only, decoupled from the (now broader) previewable set.
 */
export function isFrontmatterEditable(filePath: string): boolean {
  const type = getViewerFileType(filePath);
  return type === "markdown" || type === "html";
}

/**
 * SW-VE-2: extensions that are unambiguously binary and cannot render as text —
 * archives, executables/objects, fonts, audio/video, and binary documents. Used
 * by {@link isPreviewableFilePath} to gate them to the "no preview" placeholder
 * WITHOUT an IPC read (so a multi-MB archive is never pulled over the wire just
 * to be sniffed). Deliberately EXCLUDES the image extensions (they have their own
 * `image` viewer) and `excalidraw` (its own canvas). Not exhaustive — an unknown
 * binary that slips through is still caught post-read by CodeFileViewer's
 * `looksBinary` NUL scan; this set is the cheap up-front short-circuit for the
 * common cases.
 */
export const KNOWN_BINARY_EXTENSIONS: ReadonlySet<string> = new Set([
  // archives
  "zip",
  "tar",
  "gz",
  "tgz",
  "bz2",
  "xz",
  "zst",
  "br",
  "7z",
  "rar",
  // executables / objects / bytecode
  "exe",
  "dll",
  "so",
  "dylib",
  "o",
  "a",
  "bin",
  "wasm",
  "class",
  "pyc",
  // fonts
  "woff",
  "woff2",
  "ttf",
  "otf",
  "eot",
  // audio / video
  "mp4",
  "webm",
  "mov",
  "avi",
  "mkv",
  "mp3",
  "wav",
  "flac",
  "ogg",
  "m4a",
  // binary documents / databases
  "pdf",
  "doc",
  "docx",
  "xls",
  "xlsx",
  "ppt",
  "pptx",
  "sqlite",
  "db",
  // raster images not served by the `image` viewer
  "ico",
  "bmp",
  "tiff",
  "tif",
  "heic",
]);

/** SW-VE-2: true when the path's extension is a known-binary type (see
 *  {@link KNOWN_BINARY_EXTENSIONS}). Path-only, synchronous. */
export function isKnownBinaryPath(filePath: string): boolean {
  const ext = extensionOf(filePath);
  return ext !== null && KNOWN_BINARY_EXTENSIONS.has(ext);
}

/**
 * SW-VE-2: the context-viewer previewability gate. A file is previewable unless
 * its extension is a known-binary type. This is deliberately permissive: known
 * viewer types (md/html/image/excalidraw/code) AND unknown TEXT (which routes to
 * the `code` catch-all) all preview; only known binaries gate to the placeholder
 * up front. CodeFileViewer is the final authority — it still shows a placeholder
 * for a file that only turns out to be binary/too-large after the read. Widens
 * VE-1's `isViewerFilePath` gate so the "listing ≠ rendering" contract holds:
 * a listed text file renders, a listed binary shows a placeholder.
 */
export function isPreviewableFilePath(filePath: string): boolean {
  return !isKnownBinaryPath(filePath);
}

/**
 * SW-VE-8: true when a file may enter the deliberate raw-source editor — any
 * OWN-WRITABLE text file. This is the text-ness half of the edit gate: it is
 * viewer-type markdown / html / code (unknown text already catch-alls to `code`
 * in {@link getViewerFileType}) AND {@link isPreviewableFilePath} (so a known
 * binary that catch-alls to `code` — e.g. `archive.zip` — is excluded, AC-6).
 * It does NOT know about scope: the caller ANDs `!inheritedActive` (VE-5) and
 * `!syncView` (VE-2 R-5) to reach `canEditActive`. Widens VE-3's `code`-only
 * edit gate to the whole text tree without touching the frontmatter-editor
 * gate ({@link isFrontmatterEditable}, which stays markdown/html-only so the
 * metadata band can't corrupt a config/code file).
 */
export function isEditableTextFile(filePath: string): boolean {
  const type = getViewerFileType(filePath);
  return (
    (type === "markdown" || type === "html" || type === "code") && isPreviewableFilePath(filePath)
  );
}

/**
 * SW-VE-10: true when a file's deliberate editor may offer the rendered
 * live-preview pane — MARKDOWN only. The preview is a markdown-specific
 * projection (`react-markdown` over the live buffer); html / code / plain text
 * fall back to raw-source editing with no preview. Purely a type gate — the
 * caller still ANDs the edit gate (`canEditActive`) and the `active` flag (so a
 * sync-hidden inset does no preview work). Named separately from
 * {@link isEditableTextFile} so intent reads clearly at the call site.
 */
export function isLivePreviewEligible(filePath: string | null | undefined): boolean {
  return !!filePath && getViewerFileType(filePath) === "markdown";
}

/**
 * Regex that matches internal-link-NAVIGABLE file extensions as a path-segment
 * suffix. Used by link rewriting (worker + sync fallback) to decide which
 * relative hrefs should be dispatched to `onNavigate`.
 *
 * This is a deliberately NARROW subset of `VIEWER_EXTENSIONS` — the doc/diagram
 * types you cross-link between (md/html/excalidraw). It is NOT "every viewer
 * type": images (SW-SC-7) and `code`/config files (SW-VE-2) are *viewable* but
 * NOT typical link targets, so they are intentionally excluded here (mirrors the
 * image precedent). `.markdown` is likewise excluded — a guard test
 * (`rehype-blocks.test.ts`) pins that the `md` alternation must not over-match
 * `.markdown`/`.mdx`. Keep the alternation anchored so `.md` never substring-
 * matches a longer extension.
 */
export const VIEWER_LINK_RE = /\.(md|html?|excalidraw)(\/|$)/i;

/**
 * PTS-2: anchor segment for absolute-href resolution against per-team
 * content paths. Pre-PTS-2 this was `/_bmad-output/` (source-side
 * projection); post-PTS-2 it's `/content/` (per-team content dir).
 */
export const CONTENT_DIR_SEGMENT = "/content/";

/**
 * Resolve a relative or absolute href against an owning file path.
 *
 * Used by rehype-blocks (worker), MarkdownViewer (sync fallback), and
 * HtmlViewer (postMessage handler) to normalize internal link targets
 * to absolute file-system paths.
 *
 * Returns `null` when the href (ignoring fragment) does not match a
 * viewer-supported extension.
 *
 * - Strips `#fragment` before matching / resolving.
 * - Absolute hrefs (`/...`) are anchored to the `CONTENT_DIR_SEGMENT`
 *   ("/content/") root inside `owningFilePath`. If the segment isn't
 *   present (defensive: the caller passed a non-content path), fall
 *   back to `teamContentPath` as the root.
 * - An absolute href that already names a path under that root (a full
 *   filesystem path, e.g. a `queryContext()` row's `path`) is kept as is,
 *   never prefixed with the root a second time.
 * - Relative hrefs are joined to the owning file's directory.
 * - `..` and `.` segments are normalized away.
 */
export function resolveViewerLinkPath(
  href: string,
  owningFilePath: string,
  teamContentPath: string,
): string | null {
  const hrefPath = href.split("#")[0];
  if (!VIEWER_LINK_RE.test(hrefPath)) return null;

  let resolved: string;
  if (hrefPath.startsWith("/")) {
    const idx = owningFilePath.indexOf(CONTENT_DIR_SEGMENT);
    // Slice up to (but not including) the `/content/` segment. The
    // absolute hrefPath already begins with `/`, so an href like
    // `/docs/x.md` lands at `{root}/docs/x.md` (mirrors the pre-PTS-2
    // `/_bmad-output/` strip semantics).
    const root = (idx >= 0 ? owningFilePath.substring(0, idx) : teamContentPath).replace(
      /\/+$/,
      "",
    );
    resolved = root && hrefPath.startsWith(`${root}/`) ? hrefPath : root + hrefPath;
  } else {
    const dir = owningFilePath.substring(0, owningFilePath.lastIndexOf("/"));
    resolved = `${dir}/${hrefPath}`;
  }

  const parts = resolved.split("/");
  const normalized: string[] = [];
  for (const part of parts) {
    if (part === "..") normalized.pop();
    else if (part !== "." && part !== "") normalized.push(part);
  }
  return "/" + normalized.join("/");
}

/**
 * Build the URL for the HTTP viewer file-serving endpoint.
 *
 * The root path is base64url-encoded (RFC 4648 §5, no padding) so the
 * directory hierarchy is preserved — relative asset references in HTML
 * resolve naturally via the browser.
 */
// Base64url-encode a root path (RFC 4648 §5, no padding). TextEncoder for
// UTF-8 safety — btoa only handles Latin-1.
function encodeViewerRoot(rootPath: string): string {
  const bytes = new TextEncoder().encode(rootPath);
  const bin = Array.from(bytes, (b) => String.fromCharCode(b)).join("");
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

// Encode each path segment (spaces, #, ?, …) while preserving slashes.
function encodeViewerRelPath(relative: string): string {
  return relative.split("/").map(encodeURIComponent).join("/");
}

export function buildViewerUrl(
  port: number,
  rootPath: string,
  filePath: string,
  params?: { v?: number; origin?: string; theme?: string },
): string {
  // Compute relative path within root.  Strip leading "/" from the
  // fallback so Rust's PathBuf::join doesn't discard the root.
  const root = rootPath.endsWith("/") ? rootPath : `${rootPath}/`;
  let relative = filePath.startsWith(root) ? filePath.slice(root.length) : filePath;
  if (relative.startsWith("/")) relative = relative.slice(1);

  let url = `${params?.origin ?? `http://localhost:${port}`}/viewer/${encodeViewerRoot(rootPath)}/${encodeViewerRelPath(relative)}`;
  const query: string[] = [];
  if (params?.v != null) query.push(`v=${params.v}`);
  appendThemeQuery(query, params?.theme);

  if (query.length) url += `?${query.join("&")}`;
  return url;
}

/**
 * URL for a custom right pane (`<space>/panes/<id>.html`) on the dedicated
 * `/pane` route: the file is served from `panesDir` (so `lib/chart.js` resolves
 * beside it) while the SDK's reads bind to `contentRoot`, the active team's
 * content.
 */
export function buildPaneViewerUrl(
  port: number,
  contentRoot: string,
  panesDir: string,
  fileName: string,
  params?: { v?: number; origin?: string; theme?: string },
): string {
  let url = `${params?.origin ?? `http://localhost:${port}`}/pane/${encodeViewerRoot(contentRoot)}/${encodeViewerRoot(panesDir)}/${encodeViewerRelPath(fileName)}`;
  const query: string[] = [];
  if (params?.v != null) query.push(`v=${params.v}`);
  appendThemeQuery(query, params?.theme);

  if (query.length) url += `?${query.join("&")}`;
  return url;
}

/**
 * URL for an `html` inbox message rendered as a live doc, on the `/message`
 * route: message `messageId` of the `org`/`team` inbox, served with the same
 * shim and proxy capability a dashboard file gets.
 */
export function buildMessageViewerUrl(
  port: number,
  org: string,
  team: string,
  messageId: number,
  params?: { origin?: string; theme?: string },
): string {
  const url = `${params?.origin ?? `http://localhost:${port}`}/message/${encodeViewerRoot(org)}/${encodeViewerRoot(team)}/${messageId}`;

  const query: string[] = [];
  appendThemeQuery(query, params?.theme);
  return query.length ? `${url}?${query.join("&")}` : url;
}

function appendThemeQuery(query: string[], theme?: string): void {
  if (theme) query.push(`theme=${encodeURIComponent(theme)}`);
}

/**
 * SW-GS-3: URL for a file's PUBLISH-BASELINE version (the "before" side of the
 * image/HTML diff toggle), served from the git blob by the `/viewer-baseline`
 * route. `orgRoot` is the context repo's git root; `relPath` is the
 * org_root-relative path.
 */
export function buildBaselineViewerUrl(
  origin: string,
  orgRoot: string,
  relPath: string,
  params?: { v?: number; tree?: string },
): string {
  const rel = relPath.startsWith("/") ? relPath.slice(1) : relPath;
  let url = `${origin}/viewer-baseline/${encodeViewerRoot(orgRoot)}/${encodeViewerRelPath(rel)}`;
  const query: string[] = [];
  if (params?.v != null) query.push(`v=${params.v}`);
  // SW-GS-10 — read the blob from a specific tree OID (the immutable snapshot
  // or the merged tree) instead of the origin/main baseline, for the conflict
  // resolved-vs-ours redline.
  if (params?.tree) query.push(`tree=${encodeURIComponent(params.tree)}`);
  if (query.length) url += `?${query.join("&")}`;
  return url;
}
