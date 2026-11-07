// SPDX-License-Identifier: MIT
// Usage: node scripts/spdx-headers.mjs --check | --write | --list
// Adds a one-line `SPDX-License-Identifier: MIT` comment to every first-party source file that git
// tracks under ROOTS (or at the repo root), after a shebang (and a Python coding line, a CSS
// @charset). Vendored, generated, third-party and fixture files are skipped. A file whose header
// already names a licence or a copyright holder is skipped only when LICENCE_HEADER_EXEMPT lists
// it; any other such file fails --check and --write. --check exits 1 listing files without the
// header and prints skip counts by reason; --write adds it; --list prints every file in scope.
// Deterministic: sorted git order, no timestamps, line endings and a BOM preserved.
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

export const SPDX = "SPDX-License-Identifier: MIT";

const STYLES = {
  line: { open: "// ", close: "" },
  block: { open: "/* ", close: " */" },
  hash: { open: "# ", close: "" },
};
const EXT_STYLE = {
  ".rs": "line",
  ".ts": "line",
  ".tsx": "line",
  ".mts": "line",
  ".cts": "line",
  ".js": "line",
  ".jsx": "line",
  ".mjs": "line",
  ".cjs": "line",
  ".css": "block",
  ".sh": "hash",
  ".bash": "hash",
  ".zsh": "hash",
  ".fish": "hash",
  ".py": "hash",
};

// site/ (the website), codesense-docs/ (docs content), design/, patches/ and
// public/ are out of scope; repo-root files (vite.config.ts, eslint.config.js, ...) are in.
export const ROOTS = ["bench/", "scripts/", "src/", "src-emulator/", "src-tauri/", "tests/"];

// Any path segment matching this is vendored, built, generated or a fixture whose bytes a test
// reads.
const SKIP_SEGMENT =
  /^(?:node_modules|target|dist|gen|vendor|vendored|third[_-]party|testdata|fixtures?|test-fixtures|__[\w-]*fixtures__|[\w-]*_fixtures|__snapshots__)$/i;
const SKIP_NAME = /\.min\.(?:js|css)$|\.generated\./;
const GENERATED = /@generated|\bdo not edit\b|^\W*(?:auto-?)?generated (?:by|from)\b/im;
const FOREIGN_LICENCE =
  /copyright|\(c\) \d{4}|licensed under|SPDX-License-Identifier|\b(?:MIT|ISC|BSD|GPL|LGPL|Apache|MPL)\b[^\n]*\blicen[cs]e/i;

// Byte-sensitive first-party files: a leading line would break a test, a guard or a published
// artifact. Each entry says why.
export const BYTE_SENSITIVE = new Map([
  [
    "src-tauri/src/dashboard_publish/toolkit/cs-live.js",
    "line 1 is the toolkit version stamp (payload.rs)",
  ],
  ["src-tauri/src/dashboard_publish/toolkit/cs-embed.js", "line 1 is the cs-embed version stamp"],
  [
    "src-tauri/src/http/routes/dashboard_kit/kit-core.js",
    "a fragment spliced verbatim into kit.js and cs-live.js",
  ],
  [
    "src-tauri/src/http/routes/dashboard_kit/editor.js",
    "build:kit-editor output; line 1 is its digest stamp",
  ],
  [
    "src-tauri/src/http/routes/dashboard_kit/editor.css",
    "build:kit-editor output; line 1 is its digest stamp",
  ],
  ["src/styles/status-mark.css", "copied into kit.css's generated block and editor.css"],
  [
    "src-tauri/src/dashboard_publish/templates/workspaces/app.js",
    "copied into users' own dashboards",
  ],
  [
    "src-tauri/src/dashboard_publish/templates/workspaces/style.css",
    "copied into users' own dashboards",
  ],
]);

// First-party files that keep a header naming another licence, each with why. A file the
// FOREIGN_LICENCE heuristic matches that is not listed here fails the run, so a new file cannot
// opt out by accident.
export const LICENCE_HEADER_EXEMPT = new Map([
  [
    "src-tauri/src/http/routes/dashboard_kit/kit.js",
    "its header carries the Lucide (ISC) notice for the glyphs it inlines",
  ],
]);

export function styleFor(rel) {
  const dot = rel.lastIndexOf(".");
  return dot > rel.lastIndexOf("/") ? (EXT_STYLE[rel.slice(dot)] ?? null) : null;
}

export function inScope(rel) {
  if (!styleFor(rel)) return false;
  if (rel.includes("/") && !ROOTS.some((r) => rel.startsWith(r))) return false;
  if (
    rel
      .split("/")
      .slice(0, -1)
      .some((s) => SKIP_SEGMENT.test(s))
  )
    return false;
  if (SKIP_NAME.test(rel)) return false;
  return !BYTE_SENSITIVE.has(rel);
}

export function headerLine(style) {
  const { open, close } = STYLES[style];
  return `${open}${SPDX}${close}`;
}

const head = (text, n) => text.split("\n", n).join("\n");

export function hasHeader(text) {
  return head(text, 5).includes(SPDX);
}

// Why a file in scope is left alone: null when it takes the header.
export function skipReason(text) {
  if (text.length === 0) return "empty";
  if (text.includes("\0")) return "binary";
  if (GENERATED.test(head(text, 5))) return "generated";
  if (!hasHeader(text) && FOREIGN_LICENCE.test(head(text, 15))) return "existing licence header";
  return null;
}

// Lines that must stay first: a shebang, then a Python coding cookie (PEP 263 wants it on line 1
// or 2) or a CSS @charset (only valid as the first bytes).
function preambleLength(lines, style) {
  let i = 0;
  if (/^#!(?!\[)/.test(lines[0] ?? "")) i = 1; // not a Rust `#![inner]` attribute
  if (style === "hash" && /^#.*coding[:=]/.test(lines[i] ?? "")) i += 1;
  if (style === "block" && /^@charset\b/.test(lines[0] ?? "")) i = 1;
  return i;
}

export function addHeader(text, style) {
  if (hasHeader(text)) return text;
  const bom = text.startsWith("\uFEFF") ? "\uFEFF" : "";
  const body = text.slice(bom.length);
  const eol = body.includes("\r\n") ? "\r\n" : "\n";
  const lines = body.split(eol);
  lines.splice(preambleLength(lines, style), 0, headerLine(style));
  return bom + lines.join(eol);
}

export function trackedFiles(root) {
  return execFileSync("git", ["ls-files", "-z"], { cwd: root, maxBuffer: 1 << 28 })
    .toString("utf8")
    .split("\0")
    .filter(Boolean)
    .sort();
}

export const UNLISTED = "unlisted licence header";

// What the run does to one in-scope file: "ok" (has the header), "add", a skip reason, or
// UNLISTED for a licence or copyright header that LICENCE_HEADER_EXEMPT does not name.
export function actionFor(rel, text) {
  if (hasHeader(text)) return "ok";
  const reason = skipReason(text);
  if (reason === "existing licence header" && !LICENCE_HEADER_EXEMPT.has(rel)) return UNLISTED;
  return reason ?? "add";
}

// Every in-scope file with its action.
export function plan(root, files = trackedFiles(root)) {
  return files.filter(inScope).map((rel) => {
    const text = readFileSync(join(root, rel), "utf8");
    const action = actionFor(rel, text);
    return action === "add" ? { rel, action, text } : { rel, action };
  });
}

export function summary(entries) {
  const counts = new Map();
  for (const e of entries) counts.set(e.action, (counts.get(e.action) ?? 0) + 1);
  return [...counts]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([a, n]) => `${a} ${n}`)
    .join(", ");
}

function reportUnlisted(unlisted) {
  if (!unlisted.length) return;
  console.error(
    `spdx-headers: ${unlisted.length} files have a licence or copyright header that LICENCE_HEADER_EXEMPT does not list:`,
  );
  for (const e of unlisted) console.error(`  ${e.rel}`);
  console.error("add the header, or list the file with its reason in LICENCE_HEADER_EXEMPT");
}

function main(argv) {
  const mode = ["--check", "--write", "--list"].find((m) => argv.includes(m));
  if (!mode) {
    console.error("usage: node scripts/spdx-headers.mjs --check | --write | --list");
    return 2;
  }
  const root = fileURLToPath(new URL("../", import.meta.url));
  const entries = plan(root);
  const missing = entries.filter((e) => e.action === "add");
  const unlisted = entries.filter((e) => e.action === UNLISTED);
  if (mode === "--list") {
    for (const e of entries) console.log(`${e.action}\t${e.rel}`);
    return 0;
  }
  if (mode === "--write") {
    for (const e of missing) writeFileSync(join(root, e.rel), addHeader(e.text, styleFor(e.rel)));
    console.log(`spdx-headers: wrote ${missing.length} (${summary(entries)})`);
    reportUnlisted(unlisted);
    return unlisted.length ? 1 : 0;
  }
  console.log(`spdx-headers: ${entries.length} files in scope (${summary(entries)})`);
  if (missing.length) {
    console.error(`spdx-headers: ${missing.length} first-party files lack "${SPDX}":`);
    for (const e of missing) console.error(`  ${e.rel}`);
    console.error("run: node scripts/spdx-headers.mjs --write");
  }
  reportUnlisted(unlisted);
  return missing.length || unlisted.length ? 1 : 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  process.exitCode = main(process.argv.slice(2));
}
