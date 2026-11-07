// SPDX-License-Identifier: MIT
import { readFile } from "@/tauri/commands";
import {
  isPipelineFileName,
  PIPELINE_EXTENSIONS,
} from "@/shared/lib/pipeline-graph/pipeline-file-filter";
import { stripPipelineExtension } from "@/shared/lib/pipeline-graph/pipeline-utils";
import { basename } from "@/shared/lib/basename";
import type { WorktreeConfig } from "@/shared/types/worktree-config";

const DEFAULT_BRANCH_PREFIX = "feat/";

/**
 * Heuristic to auto-detect HTML content from file content.
 * Avoids threading an extension parameter through every call site.
 */
function isHtmlContent(content: string): boolean {
  const t = content.trimStart().toLowerCase();
  // Check for <!doctype (not bare `<!` which matches markdown `<!-- comment -->`)
  return (
    t.startsWith("<!doctype") ||
    t.startsWith("<html") ||
    t.startsWith("<head") ||
    t.startsWith("<meta")
  );
}

/**
 * Extract a `<meta name="cs:{field}" content="...">` value from HTML content.
 * Handles both attribute orders (name-first and content-first).
 * Returns the content value or `null` if not found.
 */
function extractHtmlMetaField(content: string, field: string): string | null {
  // name-first: <meta name="cs:field" content="value">
  const nf = new RegExp(`<meta\\s+name=["']cs:${field}["']\\s+content=["']([^"']*)["']`, "i");
  const nfMatch = content.match(nf);
  if (nfMatch) return nfMatch[1];

  // content-first: <meta content="value" name="cs:field">
  const cf = new RegExp(`<meta\\s+content=["']([^"']*)["']\\s+name=["']cs:${field}["']`, "i");
  const cfMatch = content.match(cf);
  if (cfMatch) return cfMatch[1];

  return null;
}

/**
 * Derive the bare story id from an absolute story file path.
 * `/repo/stories/30-10-foo.md` → `30-10-foo`.
 *
 * The full stem is used as the workspace id (uniqueness guard for stories
 * that share a numeric prefix) — the workspace bar renders a shorter
 * `30-10`-style display label via `storyDisplayLabel`.
 */
export function storyIdFromPath(storyFilePath: string): string {
  const base = storyFilePath.split("/").pop() ?? storyFilePath;
  return stripPipelineExtension(base);
}

/**
 * R-18 Phase B (AC #28): defence-in-depth path canonicaliser. All
 * `byAbsPath` keys and `Workspace.storyFilePath` values flow through
 * this — two paths are considered equal iff their normalised forms are
 * byte-identical. Pure string transform: NFC + `//` collapse + trailing
 * slash drop. No filesystem I/O (no `realpath`) — keeps the helper
 * synchronous and safe to call inside React render paths.
 *
 * Idempotent: `normalize(normalize(x)) === normalize(x)`.
 */
export function normalizeStoryFilePath(path: string): string {
  let p = path.normalize("NFC");
  p = p.replace(/\/+/g, "/");
  if (p.length > 1 && p.endsWith("/")) p = p.slice(0, -1);
  return p;
}

/**
 * Render the compact workspace-bar label for a story id. Collapses
 * both numeric (`30-10-story-foo` → `30-10`) and alpha-numeric
 * (`R-12-capture-switch-serialization` → `R-12`, `NB-7-epic-cleanup`
 * → `NB-7`) prefixes. Ids that match neither shape fall back to the
 * raw id (so non-conforming files like `misc-story` still render
 * legibly). R-18 Phase B (AC #25) widened the regex from `\d+-\d+`
 * to also handle alpha-prefixed series (R-, F-, NB-, P-, etc.).
 */
export function storyDisplayLabel(storyId: string): string {
  const match = storyId.match(/^([A-Za-z]+-\d+|\d+-\d+)(?:-|$)/);
  return match ? match[1] : storyId;
}

/**
 * SW-EW-25: render a clean tile label for an EPIC workspace from its directory
 * path. Takes the basename and prettifies it from kebab/snake-case to Title
 * Case (`epic-workspaces` → `Epic Workspaces`). The leading word's first letter
 * is upper-cased; the rest of each word is left as-is so acronyms (`api`-vs-`API`)
 * aren't mangled. Returns `""` for an empty/segment-less input (caller falls
 * back to the id). Pure string transform — safe in a render path.
 */
export function prettifyDirLeaf(dirPath: string): string {
  return basename(dirPath)
    .split(/[-_]+/)
    .filter(Boolean)
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(" ");
}

/**
 * The canonical short identity label for a workspace tile: an EPIC
 * (integration) workspace has no `storyId`, so it shows its prettified epic dir
 * leaf; a story workspace shows its `storyDisplayLabel`. Shared by the tile
 * chrome and the story-context HUD (SW-EW-28) so the two can't drift. Pure —
 * safe in a render path. Structurally typed (not the full `Workspace`) to avoid
 * an import cycle through the viewer store.
 */
export function workspaceDisplayLabel(ws: {
  contextWorktreeName?: string;
  kind?: string;
  id: string;
  storyId?: string;
  epicDirPath?: string;
  cwd?: string;
}): string {
  // SW-GW-1: a generic workspace has no backing doc/story/epic — label it with the
  // basename of its chosen dir (the raw folder name, least-surprising for a
  // user-picked path). For a segment-less cwd (e.g. root "/"), show the cwd itself
  // rather than leaking the raw `generic:/…` id; fall back to the id only if empty.
  if (ws.kind === "context") return ws.contextWorktreeName ?? ws.id;
  if (ws.kind === "generic") {
    return basename(ws.cwd ?? "") || ws.cwd || ws.id;
  }
  return ws.kind === "epic"
    ? prettifyDirLeaf(ws.epicDirPath ?? "") || storyDisplayLabel(ws.id)
    : storyDisplayLabel(ws.storyId || ws.id);
}

/**
 * SW-EW-35: extract an epic's compact CODE from its doc title — the trailing
 * `(CODE)` parenthetical (e.g. `Sub-Epic: Epic Planning Workspace (SW-EW)` →
 * `SW-EW`). The code mirrors the prefix of the epic's child story ids
 * (`SW-EW-35`), so an epic tab stays as compact as a story tab instead of
 * showing the verbose H1. Returns `null` when the title is absent or has no
 * trailing parenthetical (caller falls back to the prettified dir leaf). Pure —
 * safe in a render path.
 */
export function epicCodeFromTitle(title: string | null | undefined): string | null {
  if (!title) return null;
  const match = title.match(/\(([^()]+)\)\s*$/);
  const code = match?.[1].trim();
  return code ? code : null;
}

/**
 * Parse the first H1 line out of a story file's content. Used to populate
 * the workspace-tab hover-title per AC-3. Returns `null` when no H1 is
 * present (caller falls back to the story id). Strips a leading YAML
 * frontmatter block first so a `# yaml-comment` line inside frontmatter
 * doesn't masquerade as the title.
 */
export function parseStoryTitle(content: string): string | null {
  if (isHtmlContent(content)) {
    // Try <title> first, then <h1>
    const titleMatch = content.match(/<title>([^<]*)<\/title>/i);
    if (titleMatch) {
      const t = titleMatch[1].trim();
      if (t.length > 0) return t;
    }
    const h1Match = content.match(/<h1[^>]*>([^<]*)<\/h1>/i);
    if (h1Match) {
      const t = h1Match[1].trim();
      if (t.length > 0) return t;
    }
    return null;
  }
  const body = content.replace(/^---\s*\r?\n[\s\S]*?\r?\n---\s*(?:\r?\n|$)/, "");
  const match = body.match(/^#\s+(.+?)\s*$/m);
  if (!match) return null;
  const trimmed = match[1].trim();
  return trimmed.length > 0 ? trimmed : null;
}

/**
 * Resolve the branch name for a story workspace. Reads the story file's
 * YAML frontmatter for an explicit `branch_name:` field; falls back to
 * the `{branch_prefix}{story-id}` convention (default `feat/{story-id}`).
 *
 * Failures (file missing, frontmatter absent, parse error) all collapse
 * to the convention fallback — branch naming is a hint, not a hard
 * constraint, and we'd rather give the user a workspace than block on
 * a missing field.
 */
export async function resolveBranchName(
  storyFilePath: string,
  config?: WorktreeConfig | null,
): Promise<string> {
  const { branchName } = await resolveWorktreeParams(storyFilePath, config);
  return branchName;
}

/**
 * Read a story file once and resolve both branch name and start point.
 *
 * Start-point resolution (highest priority wins):
 * 1. `depends_on` in frontmatter → resolve the first dependency's branch name
 * 2. The worktree config's `default_start_point`
 * 3. `null` → the backend starts a new branch from the repo's default branch
 *    (`fetch_and_create_worktree` with no start point), so no branch name is assumed
 *
 * (Remote-branch detection is handled by the Rust command, not here.)
 */
export async function resolveWorktreeParams(
  storyFilePath: string,
  config?: WorktreeConfig | null,
  /** @internal Guard against circular depends_on chains. */
  _depth = 0,
  /** Pre-read content to avoid redundant readFile (M.2 DRY). */
  preReadContent?: string,
): Promise<{ branchName: string; startPoint: string | null }> {
  const storyId = storyIdFromPath(storyFilePath);
  const branchPrefix = config?.branch_prefix ?? DEFAULT_BRANCH_PREFIX;
  const fallbackBranch = `${branchPrefix}${storyId}`;
  // `||` so an empty configured value also leaves the base to the backend.
  const defaultStart = config?.default_start_point || null;

  let content: string;
  if (preReadContent !== undefined) {
    content = preReadContent;
  } else {
    try {
      content = await readFile(storyFilePath);
    } catch {
      return { branchName: fallbackBranch, startPoint: defaultStart };
    }
  }

  const branchName = parseBranchNameFromFrontmatter(content) ?? fallbackBranch;

  // Resolve start point from depends_on (max depth 3 to prevent circular chains).
  // SW-FX-3: BMad's cross-epic convention writes path-style deps
  // (`../SIBLING-EPIC/STORY-ID`), so we no longer reject `..` or `/` in the
  // depId. The OS normalizes the `..`-relative path on open and Tauri's
  // allowlist bounds the read — that read sandbox is the bounds check, not a
  // TS-side string guard. The `_depth < 3` cap is the cycle bound, unchanged.
  const depId = parseDependsOnFromFrontmatter(content);
  if (depId && _depth < 3) {
    const dir = storyFilePath.substring(0, storyFilePath.lastIndexOf("/"));
    // Strip any pipeline extension from depId (handles "W-5-foo.html" refs).
    const depStem = stripPipelineExtension(depId);
    // Self-reference skip (SW-FX-3 AC-5): a path-qualified self-ref
    // (`../<own-epic>/<self>`) resolves back to this same story. Identity is
    // the `..`-resolved, extension-stripped path — matching codesense's
    // stem-as-story-id convention — so a self-ref is caught regardless of
    // which extension the probe loop would try (a `.md` story can't sneak
    // back in via a same-stem `.html` twin). Compare the normalized forms:
    // depStem still carries a literal `..`, the parent path is canonical.
    // Without this skip the unwound recursion resolves the story's start
    // point to its own (not-yet-created) branch and the F-29 guard blocks
    // the story on itself.
    const selfRef =
      normalizePath(`${dir}/${depStem}`) === stripPipelineExtension(normalizePath(storyFilePath));
    if (!selfRef) {
      // Probe each extension via readFile — if the file doesn't exist, readFile
      // throws and we try the next. Pre-read content avoids the recursive call's
      // internal catch masking file-not-found.
      for (const ext of PIPELINE_EXTENSIONS) {
        try {
          const depPath = `${dir}/${depStem}.${ext}`;
          const depContent = await readFile(depPath);
          const { branchName: depBranch } = await resolveWorktreeParams(
            depPath,
            config,
            _depth + 1,
            depContent,
          );
          return { branchName, startPoint: depBranch };
        } catch {
          // File doesn't exist — try next extension
        }
      }
    }
  }

  return { branchName, startPoint: defaultStart };
}

/**
 * Extract the raw YAML frontmatter block from content delimited by `---`.
 * Returns the block body (without delimiters) or `null` if no frontmatter.
 * Shared by all lightweight frontmatter field extractors on the TS side.
 */
export function extractFrontmatterBlock(content: string): string | null {
  const match = content.match(/^---\s*\r?\n([\s\S]*?)\r?\n---\s*(?:\r?\n|$)/);
  return match ? match[1] : null;
}

/**
 * Extract a single top-level scalar field from a markdown frontmatter block.
 * Strips matched surrounding quotes (single or double) — leaving inner quotes
 * alone, the same as serde_yaml. Returns `null` when the field is absent or the
 * value is empty/whitespace-only. The `field:` match is exact (a trailing `:`), so
 * `branch` never matches `branch_name`.
 *
 * The whitespace after the colon is matched with `[^\S\r\n]*` (horizontal only,
 * NOT `\s*`) so a whitespace-only value line does not let `(.+?)` reach across the
 * newline and capture the NEXT frontmatter line as the value — it correctly yields
 * `null` (and the precedence chain falls through). The trailing `\s*$` keeps CRLF
 * handling (`$` matches before `\n`, `\r` is consumed).
 */
function scalarFromFrontmatterBlock(block: string, field: string): string | null {
  const lineMatch = block.match(new RegExp(`^${field}:[^\\S\\r\\n]*(.+?)\\s*$`, "im"));
  if (!lineMatch) return null;
  const raw = lineMatch[1].trim();
  const unquoted = raw.replace(/^"([^"]*)"$/, "$1").replace(/^'([^']*)'$/, "$1");
  return unquoted.length > 0 ? unquoted : null;
}

/**
 * Resolve a story's canonical branch from frontmatter, by precedence
 * `branch` → `branch_name` → `dev_branch` (first present + non-empty) — the FE
 * mirror of the backend `CacheEntry::story_branch()` (SW-OR-1b), so an interactive
 * open and a trusted-side open resolve the SAME branch. New stories carry only the
 * unified `branch`; the legacy fields are read only as a fallback for docs authored
 * before the merge. Minimal regex extraction (no YAML parser dependency in the FE).
 * Returns `null` when no frontmatter block exists or all three fields are absent/empty.
 */
export function parseBranchNameFromFrontmatter(content: string): string | null {
  if (isHtmlContent(content)) {
    // Normalize empty/whitespace meta content to null so the precedence chain
    // falls through (an empty `cs:branch` must not shadow a valid legacy field
    // nor be used verbatim as a blank branch) — mirroring the markdown path's
    // empty→null and the backend `story_branch()` blank-skip.
    const meta = (field: string): string | null => {
      const v = extractHtmlMetaField(content, field)?.trim();
      return v ? v : null;
    };
    return meta("branch") ?? meta("branch-name") ?? meta("dev-branch");
  }
  const block = extractFrontmatterBlock(content);
  if (!block) return null;
  return (
    scalarFromFrontmatterBlock(block, "branch") ??
    scalarFromFrontmatterBlock(block, "branch_name") ??
    scalarFromFrontmatterBlock(block, "dev_branch")
  );
}

/**
 * Does a git worktree's live branch ref (`WorktreeInfo.branch`) name the same
 * branch as `want`? Tolerates both the bare form (`feat/x`) and a qualified ref
 * (`refs/heads/feat/x`) by treating a `/{want}` suffix as a match. Single home
 * for the "is this worktree on that branch" comparison shared by
 * `findWorktreeOnDisk`, `setupWorktree`'s orphan pre-flight, and the SW-WB-1
 * branch reconcile — so the three never drift.
 */
export function branchRefMatches(live: string | null | undefined, want: string): boolean {
  return !!live && (live === want || live.endsWith(`/${want}`));
}

/**
 * Extract the `source_repo` slug from YAML frontmatter. Returns the raw
 * `owner/repo` string or `null` when absent/empty. Follows the same
 * lightweight pattern as `parseBranchNameFromFrontmatter` — regex
 * extraction via `extractFrontmatterBlock`, no YAML parser.
 */
export function parseSourceRepoFromFrontmatter(content: string): string | null {
  if (isHtmlContent(content)) return extractHtmlMetaField(content, "source-repo");
  const block = extractFrontmatterBlock(content);
  if (!block) return null;
  const lineMatch = block.match(/^source_repo:\s*(.+?)\s*$/im);
  if (!lineMatch) return null;
  const raw = lineMatch[1].trim();
  // Strip YAML inline comments (` # ...`) from unquoted values only —
  // quoted values like `"owner/repo # not a comment"` are left intact.
  // Also treat a value that is entirely a comment (`# ...`) as empty.
  const decommented = /^["']/.test(raw) ? raw : raw.replace(/^#.*$/, "").replace(/\s+#.*$/, "");
  const unquoted = decommented.replace(/^"([^"]*)"$/, "$1").replace(/^'([^']*)'$/, "$1");
  return unquoted.length > 0 ? unquoted : null;
}

/**
 * Extract the `story_id` value from YAML frontmatter or HTML meta.
 * Returns the trimmed id string or `null` when absent/empty/null-literal.
 *
 * Code review (2026-05-31) hardening:
 * - HTML branch trims whitespace and returns `null` for empty content (P1).
 * - YAML null literals (`null`, `~`) return `null` rather than the literal
 *   string (P6).
 * - Explicit `if (raw.startsWith("#")) return null;` guard before the
 *   inline-comment strip so the path is unambiguous (P7).
 * - Trailing `#` without preceding whitespace (`R-18#`) is stripped (P8).
 */
export function parseStoryIdFromFrontmatter(content: string): string | null {
  if (isHtmlContent(content)) {
    const raw = extractHtmlMetaField(content, "story-id");
    if (raw === null) return null;
    const trimmed = raw.trim();
    return trimmed.length > 0 ? trimmed : null;
  }
  const block = extractFrontmatterBlock(content);
  if (!block) return null;
  const lineMatch = block.match(/^story_id:\s*(.+?)\s*$/im);
  if (!lineMatch) return null;
  const raw = lineMatch[1].trim();
  // Whole-value comment → null (was: accidental fallthrough via empty-string check).
  if (raw.startsWith("#")) return null;
  // Inline-comment strip for unquoted values. Two separate patterns:
  //   1. `\s+#.*$` — proper YAML inline comment (whitespace before `#`).
  //   2. `#\s*$`  — trailing-only `#` (`R-18#` → `R-18`, round-2 R1 fix).
  // Round-2 R1: using `\s*#` for both was too greedy and ate `#` inside
  // values (`bug#42-feature` lost its tail). Preserve mid-value `#`.
  const decommented = /^["']/.test(raw) ? raw : raw.replace(/\s+#.*$/, "").replace(/#\s*$/, "");
  const unquoted = decommented
    .replace(/^"([^"]*)"$/, "$1")
    .replace(/^'([^']*)'$/, "$1")
    .trim();
  if (unquoted.length === 0) return null;
  // YAML null literals: explicit `null` / `Null` / `NULL` / `~` → null.
  if (/^(null|~)$/i.test(unquoted)) return null;
  return unquoted;
}

/**
 * Extract the first `depends_on` entry from YAML frontmatter. Supports both
 * inline (`depends_on: [a, b]`) and block-style (`depends_on:\n  - a\n  - b`)
 * YAML list syntax. Returns the first non-empty entry or `null`.
 *
 * Note: `depends_on` is also parsed on the Rust side by
 * `extract_story_meta` in `pipeline_scanner.rs` using the `gray_matter`
 * crate for pipeline graph display. This TS-side parser exists separately
 * because the Rust-parsed data lives in the `StoryRef` pipeline type and
 * isn't accessible from the worktree setup path.
 */
export function parseDependsOnFromFrontmatter(content: string): string | null {
  if (isHtmlContent(content)) {
    const raw = extractHtmlMetaField(content, "depends-on");
    if (!raw) return null;
    // CSV split — return only the first entry, consistent with the YAML path.
    const first = raw
      .split(",")
      .map((s) => s.trim())
      .find((s) => s.length > 0);
    return first ?? null;
  }
  const block = extractFrontmatterBlock(content);
  if (!block) return null;

  // Try inline syntax: depends_on: [a, b]
  const inlineMatch = block.match(/^depends_on:\s*\[([^\]]*)\]/im);
  if (inlineMatch) {
    const entries = inlineMatch[1].split(",").map((s) => s.trim().replace(/^["']|["']$/g, ""));
    const first = entries.find((e) => e.length > 0);
    return first ?? null;
  }

  // Try block-style syntax: depends_on:\n  - a\n  - b
  // The next line after "depends_on:" must be an indented `- item` entry.
  const blockMatch = block.match(/^depends_on:\s*\r?\n(\s+-\s*.+)/im);
  if (blockMatch) {
    const itemMatch = blockMatch[1].match(/^\s*-\s*(.+?)\s*$/);
    if (itemMatch) {
      return itemMatch[1].replace(/^["']|["']$/g, "").trim() || null;
    }
  }

  return null;
}

/**
 * Resolve the absolute worktree directory for a (repo, branch) pair.
 * Applies the `worktree_dir` template from the config (default
 * `../{repo}-wt/{slug}` per Rust's `DEFAULT_WORKTREE_DIR`) with
 * `{repo}` = the repo's basename and `{slug}` = the branch name with
 * `/` collapsed to `-` so the result is a valid single-segment dirname.
 *
 * Pure function — no filesystem access. Callers handle existence checks
 * via `listWorktrees`.
 */
export function resolveWorktreePath(
  config: WorktreeConfig | null,
  repoPath: string,
  branchName: string,
): string {
  const template = config?.worktree_dir ?? "../{repo}-wt/{slug}";
  const repoBasename = repoPath.split("/").filter(Boolean).pop() ?? "repo";
  const slug = branchName.replace(/\//g, "-");
  const rendered = template.replace(/\{repo\}/g, repoBasename).replace(/\{slug\}/g, slug);

  // The template can be relative (default) or absolute. Relatives are
  // resolved against the repo path itself — so the default
  // `../{repo}-wt/{slug}` produces a sibling of the repo directory,
  // matching the Rust runner's semantics.
  if (rendered.startsWith("/")) return normalizePath(rendered);
  return normalizePath(`${repoPath}/${rendered}`);
}

/**
 * Minimal path normalizer — collapses `a/b/../c` to `a/c` and strips
 * `./` segments. Pure string manipulation; no filesystem resolution.
 *
 * Used by the SW-FX-3 self-reference identity check in
 * `resolveWorktreeParams`: a path-qualified self-ref dep carries a literal
 * `..` that must be resolved before it can be compared to the parent's
 * already-canonical path. Kept module-private — no external consumer.
 */
function normalizePath(path: string): string {
  const isAbs = path.startsWith("/");
  const parts = path.split("/").filter((p) => p && p !== ".");
  const out: string[] = [];
  for (const p of parts) {
    if (p === "..") {
      if (out.length > 0 && out[out.length - 1] !== "..") out.pop();
      else if (!isAbs) out.push("..");
    } else {
      out.push(p);
    }
  }
  return (isAbs ? "/" : "") + out.join("/");
}

/**
 * Shared heuristic for "is this file path a pipeline story file?". Used
 * by every story-workspace entry point (pipeline-viewer click, file-tree
 * right-click, Cmd+P quick-open) — DO NOT inline the check at call
 * sites. Story 30.10 Task 6.1 designates this as the single source of
 * truth so the three entry points can't drift.
 *
 * v1 heuristic: file must (a) live under `contentRootAbsolute` (absolute path
 * — the sole caller passes `teamContentPath`) and (b) be a recognized pipeline
 * file name per `isPipelineFileName` from the pipeline-file-filter (matches any
 * registered pipeline extension — `.md`, `.html`, `.htm`). Frontmatter-field
 * detection is intentionally deferred — it would require a file read at every
 * check site and isn't worth the IO cost for v1.
 *
 * SW-FX-56: param renamed from `pipelineRootAbsolute` (a stale scan-scope name;
 * the value is the content root, not the override-glob prefix).
 */
export function isStoryFile(filePath: string, contentRootAbsolute: string): boolean {
  if (!filePath || !contentRootAbsolute) return false;
  if (!filePath.startsWith(`${contentRootAbsolute}/`)) return false;
  const fileName = filePath.split("/").pop() ?? "";
  return isPipelineFileName(fileName);
}
