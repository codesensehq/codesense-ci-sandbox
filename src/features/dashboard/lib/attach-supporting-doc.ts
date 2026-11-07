// SPDX-License-Identifier: MIT
/**
 * SW-SC-2: attach a canvas (or any file) to a story as a supporting doc by
 * upserting its relative path into the story's `supporting_docs:` frontmatter.
 *
 * Pure string surgery — deliberately NOT a YAML round-trip — so the story's
 * frontmatter (key order, comments, the AI-owned body) is preserved and the
 * diff is minimal: a single appended list item. Idempotent: a path already
 * listed is a no-op.
 */

import { extractFrontmatterBlock } from "@/shared/lib/worktree";

/** Strip a leading `./` and surrounding quotes from a frontmatter list value. */
function normalizeRef(ref: string): string {
  let r = ref.trim().replace(/^['"]|['"]$/g, "");
  if (r.startsWith("./")) r = r.slice(2);
  return r;
}

/**
 * The `supporting_docs:` ref for `canvasPath` relative to the story at
 * `storyPath` — `./design.excalidraw` for a sibling, `./sub/x` / `../x` for
 * other directories. Both inputs are absolute POSIX-style paths.
 */
export function relativeSupportingDocPath(storyPath: string, canvasPath: string): string {
  const slash = storyPath.lastIndexOf("/");
  const storyDir = slash >= 0 ? storyPath.slice(0, slash) : "";
  const from = storyDir.split("/").filter(Boolean);
  const to = canvasPath.split("/").filter(Boolean);
  let i = 0;
  while (i < from.length && i < to.length && from[i] === to[i]) i++;
  const ups = from.slice(i).map(() => "..");
  const downs = to.slice(i);
  const rel = [...ups, ...downs].join("/");
  return ups.length === 0 ? `./${rel}` : rel;
}

type Frontmatter = { open: number; close: number } | null;

/** The file's dominant line ending — preserved across the upsert so we never
 *  mix `\n` into a CRLF file (or vice-versa). */
function detectEol(text: string): string {
  return text.includes("\r\n") ? "\r\n" : "\n";
}

/** A `---` fence line, tolerating trailing whitespace — matches the
 *  tolerance of the shared `extractFrontmatterBlock` (`^---\s*`) so the two
 *  detectors never disagree (a padded fence offered by `frontmatterType` must
 *  also be found here, or `upsertSupportingDoc` would prepend a duplicate
 *  block and orphan the real frontmatter). */
const FENCE_RE = /^---\s*$/;

/** Locate the leading `--- … ---` frontmatter block (line indices into a
 *  `/\r?\n/`-split array), or null if absent. CRLF- and trailing-whitespace-
 *  aware. */
function findFrontmatter(text: string): Frontmatter {
  // Split on either EOL and compare bare lines so a CRLF file (`---\r\n…`)
  // is detected exactly like an LF one.
  const lines = text.split(/\r?\n/);
  if (!FENCE_RE.test(lines[0])) return null;
  for (let i = 1; i < lines.length; i++) {
    if (FENCE_RE.test(lines[i])) return { open: 0, close: i };
  }
  return null;
}

/**
 * Add `relPath` to the story text's `supporting_docs:` list. Handles: no
 * frontmatter (creates one), no `supporting_docs:` key (adds a block list),
 * existing block list (`- item` lines), and existing flow list (`[a, b]`).
 * Returns the new text and whether anything changed (idempotent).
 */
export function upsertSupportingDoc(
  text: string,
  relPath: string,
): { text: string; changed: boolean } {
  const target = normalizeRef(relPath);
  const eol = detectEol(text);
  const fm = findFrontmatter(text);

  if (!fm) {
    // No frontmatter — create a minimal one carrying just the attachment.
    const block = ["---", "supporting_docs:", `  - ${relPath}`, "---", "", ""].join(eol);
    return { text: block + text, changed: true };
  }

  // Surgery on bare (EOL-stripped) lines; rejoin with the file's own EOL so a
  // CRLF story stays CRLF and a new line never injects a lone `\n`.
  const lines = text.split(/\r?\n/);
  const keyIdx = lines.findIndex(
    (l, i) => i > fm.open && i < fm.close && /^supporting_docs:/.test(l),
  );

  if (keyIdx === -1) {
    // Key absent — insert a block list just before the closing fence.
    lines.splice(fm.close, 0, "supporting_docs:", `  - ${relPath}`);
    return { text: lines.join(eol), changed: true };
  }

  const keyLine = lines[keyIdx];
  const inline = keyLine.slice("supporting_docs:".length).trim();

  // Flow list on the same line: `supporting_docs: [a, b]`. Slice to the `]`
  // so a trailing inline comment (`[a] # note`) doesn't leak into the body or
  // produce invalid YAML on rewrite (the comment is dropped on rewrite).
  if (inline.startsWith("[")) {
    const close = inline.indexOf("]");
    const body = (close >= 0 ? inline.slice(1, close) : inline.slice(1)).trim();
    const items = body.length ? body.split(",").map((s) => s.trim()) : [];
    if (items.some((it) => normalizeRef(it) === target)) return { text, changed: false };
    items.push(relPath);
    lines[keyIdx] = `supporting_docs: [${items.join(", ")}]`;
    return { text: lines.join(eol), changed: true };
  }

  // Block list: scan the following `  - item` lines within the frontmatter.
  let lastItem = keyIdx;
  for (let i = keyIdx + 1; i < fm.close; i++) {
    if (/^\s*-\s+/.test(lines[i])) {
      if (normalizeRef(lines[i].replace(/^\s*-\s+/, "")) === target) {
        return { text, changed: false }; // already attached
      }
      lastItem = i;
    } else if (lines[i].trim() === "") {
      continue; // tolerate blank lines inside the list
    } else {
      break; // next key — list ended
    }
  }
  // `inline` non-empty + not a list (e.g. `supporting_docs: null` / scalar):
  // fall through to block-list insertion, which YAML treats as the list.
  lines.splice(lastItem + 1, 0, `  - ${relPath}`);
  return { text: lines.join(eol), changed: true };
}

/**
 * SLW-FX-21: remove `relPath` from the story text's `supporting_docs:` list —
 * the unlink direction {@link upsertSupportingDoc} never needed. Mirrors its
 * `findFrontmatter`/`normalizeRef`/`detectEol` rigor and its flow-vs-block
 * branches. When the last item is removed, the whole `supporting_docs:` key is
 * dropped (no empty `supporting_docs: []` residue). Returns the new text and
 * whether anything changed — idempotent: a ref not present is a byte-for-byte
 * no-op (`{ changed: false }`, original `text` returned verbatim).
 */
export function removeSupportingDoc(
  text: string,
  relPath: string,
): { text: string; changed: boolean } {
  const target = normalizeRef(relPath);
  const eol = detectEol(text);
  const fm = findFrontmatter(text);
  if (!fm) return { text, changed: false };

  const lines = text.split(/\r?\n/);
  const keyIdx = lines.findIndex(
    (l, i) => i > fm.open && i < fm.close && /^supporting_docs:/.test(l),
  );
  if (keyIdx === -1) return { text, changed: false };

  const inline = lines[keyIdx].slice("supporting_docs:".length).trim();

  // Flow list on the same line: `supporting_docs: [a, b]`. Slice to the `]` so a
  // trailing inline comment doesn't leak (dropped on rewrite, matching upsert).
  if (inline.startsWith("[")) {
    const close = inline.indexOf("]");
    const body = (close >= 0 ? inline.slice(1, close) : inline.slice(1)).trim();
    const items = body.length ? body.split(",").map((s) => s.trim()) : [];
    const kept = items.filter((it) => normalizeRef(it) !== target);
    if (kept.length === items.length) return { text, changed: false }; // ref absent
    // Last item gone → drop the key line entirely; else rewrite the flow list.
    if (kept.length === 0) lines.splice(keyIdx, 1);
    else lines[keyIdx] = `supporting_docs: [${kept.join(", ")}]`;
    return { text: lines.join(eol), changed: true };
  }

  // Block list: scan the following `  - item` lines within the frontmatter,
  // tolerating blank lines exactly like the upsert scan.
  let matchIdx = -1;
  let itemCount = 0;
  for (let i = keyIdx + 1; i < fm.close; i++) {
    if (/^\s*-\s+/.test(lines[i])) {
      itemCount++;
      if (matchIdx === -1 && normalizeRef(lines[i].replace(/^\s*-\s+/, "")) === target) {
        matchIdx = i;
      }
    } else if (lines[i].trim() === "") {
      continue; // tolerate blank lines inside the list
    } else {
      break; // next key — list ended
    }
  }
  if (matchIdx === -1) return { text, changed: false }; // ref absent

  // Removing the only item drops the `- item` line AND the now-empty key line.
  // Splice the higher index (the item) first so `keyIdx` stays valid.
  lines.splice(matchIdx, 1);
  if (itemCount === 1) lines.splice(keyIdx, 1);
  return { text: lines.join(eol), changed: true };
}

/** The disk I/O the {@link attachSupportingDoc} composition needs, injected so
 *  the read → upsert → write sequence stays pure-testable with fakes. Production
 *  callers pass `{ readFile, writeContentFile }` from `@/tauri/commands`. */
export interface AttachDocIo {
  readFile: (path: string) => Promise<string>;
  writeContentFile: (path: string, content: string) => Promise<void>;
}

/**
 * Record `docPath` into `nodeMdPath`'s `supporting_docs:` frontmatter and write
 * it back — the read → {@link upsertSupportingDoc} → write sequence shared by the
 * canvas attach (SW-SC-12) and the sync-view attach-to-node (SW-GS-11). Idempotent:
 * a doc already listed returns `{ changed: false }` and performs NO write (so the
 * watcher-suppressed writer never fires needlessly). `nodeMdPath` is the node's own
 * `.md` (story file or epic README); both paths are absolute POSIX-style.
 */
export async function attachSupportingDoc(
  nodeMdPath: string,
  docPath: string,
  io: AttachDocIo,
): Promise<{ changed: boolean }> {
  const rel = relativeSupportingDocPath(nodeMdPath, docPath);
  const { text, changed } = upsertSupportingDoc(await io.readFile(nodeMdPath), rel);
  if (changed) await io.writeContentFile(nodeMdPath, text);
  return { changed };
}

/**
 * SLW-FX-21: the unlink sibling of {@link attachSupportingDoc} — read →
 * {@link removeSupportingDoc} → write. Idempotent: a doc not listed returns
 * `{ changed: false }` and performs NO write. `nodeMdPath` is the node's own
 * `.md`; both paths are absolute POSIX-style. Shares the injected
 * {@link AttachDocIo} so the sequence stays pure-testable with fakes.
 */
export async function detachSupportingDoc(
  nodeMdPath: string,
  docPath: string,
  io: AttachDocIo,
): Promise<{ changed: boolean }> {
  const rel = relativeSupportingDocPath(nodeMdPath, docPath);
  const { text, changed } = removeSupportingDoc(await io.readFile(nodeMdPath), rel);
  if (changed) await io.writeContentFile(nodeMdPath, text);
  return { changed };
}

/**
 * Extract the `type:` value (lowercased) from a doc's leading YAML
 * frontmatter, or null. Reuses the shared `extractFrontmatterBlock` and
 * strips inline comments from unquoted values, matching the canonical
 * `parse*FromFrontmatter` family in `shared/lib/worktree/resolve.ts`.
 */
export function frontmatterType(text: string): string | null {
  const block = extractFrontmatterBlock(text);
  if (!block) return null;
  const m = block.match(/^type:\s*(.+?)\s*$/im);
  if (!m) return null;
  let raw = m[1].trim();
  // Strip a YAML inline comment from unquoted values (`type: story # x`);
  // leave quoted values intact (`type: "a # b"`).
  if (!/^["']/.test(raw))
    raw = raw
      .replace(/\s+#.*$/, "")
      .replace(/#\s*$/, "")
      .trim();
  raw = raw.replace(/^"([^"]*)"$/, "$1").replace(/^'([^']*)'$/, "$1");
  return raw.length > 0 ? raw.toLowerCase() : null;
}
