#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// The one-path rule for listing context files, enforced (SV-PBF-267): the context index
// (`src-tauri/src/context_query/`) is the one place the footprint's files are walked, and
// `list_context_files` is served from it. A new per-component walk is how the app got to
// 19 concurrent folder walks at idle, so none may come back unnoticed.
//
// Rust: fails when code outside `context_query/`, `crates/codesense-fs-core/` and the
// allowlist calls `WalkDir::new`, `WalkBuilder::new` (`ignore`), a `jwalk` walker or
// `collect_viewable_files`; when an allowlisted file holds more than its `max`; and when
// `commands/file_index.rs` calls `read_dir` at all (a hand-rolled recursive walk).
// TypeScript: fails on a `listContextFiles(` call or an `invoke("list_context_files"`
// outside `src/tauri/commands.ts` and the shared per-root cache `content-scan-queue.ts`.
//
// Comments are not code and are skipped; so are test files and `#[cfg(test)]` items.
//
//   node scripts/check-context-walk.mjs [--root <dir>] [--list]
//
// `--root` scans another tree (the check's own tests use it on fixtures); `--list` prints
// every hit, allowed or not.
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, posix, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import {
  testModules,
  testOnlyFile,
  tokenize,
  withoutTestItems,
} from "./check-agent-cli-boundary.mjs";

// Rust that may walk: the index itself and the filesystem core. Paths are relative to `src-tauri/`.
const WALK_HOMES = ["src/context_query/", "crates/codesense-fs-core/"];

// `file_index.rs` serves `list_context_files` from the index: it may not read a directory.
const NO_READ_DIR = ["src/commands/file_index.rs"];

// Production walkers that exist today, each with the most hits it may hold, so a new
// walk in the same file still fails. A walker that should read the index instead names
// the child story that moves it. Keep this list shrinking.
export const ALLOWLIST = [
  {
    path: "src/agents/prepare/publish.rs",
    max: 1,
    reason: "agent-definition publish: walks the agent's own prepared tree, not the footprint",
  },
  {
    path: "src/agents/prepare/want.rs",
    max: 1,
    reason: "agent prepare: walks the agent's own skill/command subtrees",
  },
  {
    path: "src/commands/content_engine.rs",
    max: 2,
    reason: "content engine: .claude skill-tree projection and subtree enumeration for rename/move",
  },
  {
    path: "src/commands/file_surface.rs",
    max: 1,
    reason:
      "move delivery: reads every descendant of the folder being moved, which must be live disk",
  },
  {
    path: "src/commands/route_to_context.rs",
    max: 1,
    reason: "epic hydration: whole epic dir copied to a worktree, must be live disk",
  },
  {
    path: "src/content_engine/layers.rs",
    max: 1,
    reason: "content engine layer resolution walks one layer's subtree",
  },
  {
    path: "src/attribution/history.rs",
    max: 1,
    reason: "ref-lock busy check walks a git dir's refs/ (nested branch names), not team content",
  },
  {
    path: "src/context_repo/inheritance.rs",
    max: 1,
    reason: "parent-chain walk of one inherited subtree (sharing.yml rules apply per file)",
  },
  {
    path: "src/context_repo/scratch_space.rs",
    max: 1,
    reason: "scratch space regular_files: private-space listing, not the viewer's per-root list",
  },
  {
    path: "src/dashboard_publish/inventory.rs",
    max: 1,
    reason: "dashboard publish inventory of one dashboard's files",
  },
  {
    path: "src/install/story_linker.rs",
    max: 1,
    reason: "story linker content scan for one dir; candidate to read the index",
  },
];

// TypeScript that may call the command: the typed wrapper and the shared per-root cache.
const TS_HOMES = ["src/tauri/commands.ts", "src/shared/lib/pipeline-graph/content-scan-queue.ts"];
// Test code and e2e controls drive the command directly.
const TS_EXEMPT = [
  /\.(test|spec)\.tsx?$/,
  /(^|\/)__tests__\//,
  /(^|\/)e2e-[^/]*\.tsx?$/,
  /^src\/app\/mock\//,
];

const RUST_WALK =
  /\bcollect_viewable_files\b|\b(?:walkdir\s*::\s*)?WalkDir\s*::\s*new\b|\b(?:ignore\s*::\s*)?WalkBuilder\s*::\s*new\b|\bjwalk\s*::\s*\w+/g;
const READ_DIR = /\bread_dir\b/g;
const TS_CALL =
  /\blistContextFiles\s*\(|\binvoke\s*(?:<[^>]*>)?\s*\(\s*["'`]list_context_files["'`]/g;

function* filesUnder(dir, ext) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    // Symlinks are not followed: a link cycle would never end.
    if (lstatSync(path).isDirectory()) {
      if (!name.startsWith("target") && name !== "node_modules") yield* filesUnder(path, ext);
    } else if (ext.some((e) => name.endsWith(e))) yield path;
  }
}

/** Every `pattern` match in the kept code of `source`: `{ line, text }`. */
function rustHits(source, tokens, pattern) {
  const starts = [0];
  for (let at = source.indexOf("\n"); at >= 0; at = source.indexOf("\n", at + 1))
    starts.push(at + 1);
  const lineOf = (at) => {
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid] <= at) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  };
  // The kept code alone at its own offsets (strings, comments and test items blanked).
  const blank = (text) => text.replace(/[^\n]/g, " ");
  const parts = [];
  let end = 0;
  for (const t of withoutTestItems(tokens)) {
    parts.push(blank(source.slice(end, t.at)));
    parts.push(t.kind === "string" ? blank(t.text) : t.text);
    end = t.at + t.text.length;
  }
  const code = parts.join("");
  return [...code.matchAll(pattern)].map((m) => ({
    line: lineOf(m.index),
    text: m[0].replace(/\s+/g, " "),
  }));
}

/** Rust test code: under `#[cfg(test)]`, a module declared under it, or a crate's tests/. */
function testPaths(sources) {
  const testFiles = new Set();
  const testDirs = [/^crates\/[^/]+\/(tests|benches|examples)\//];
  for (const [path, { source, tokens }] of sources) {
    for (const { name, path: explicit } of testModules(source, tokens)) {
      const dir = path.slice(0, path.lastIndexOf("/") + 1);
      if (explicit) {
        const file = posix.normalize(dir + explicit);
        testFiles.add(file);
        testDirs.push(file.replace(/(\/mod)?\.rs$/, "/"));
        continue;
      }
      const stem = path.slice(dir.length).replace(/\.rs$/, "");
      const base = ["mod", "lib", "main"].includes(stem) ? dir : `${dir}${stem}/`;
      testFiles.add(`${base}${name}.rs`);
      testFiles.add(`${base}${name}/mod.rs`);
      testDirs.push(`${base}${name}/`);
    }
  }
  return (path) =>
    testFiles.has(path) ||
    testDirs.some((d) => (typeof d === "string" ? path.startsWith(d) : d.test(path)));
}

/** Drops line and block comments from TypeScript, keeping line breaks. */
function withoutTsComments(source) {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "))
    .replace(/(^|[^:"'`\\])\/\/[^\n]*/g, "$1");
}

/**
 * The check over a repo `root`: `{ violations, allowed, over, stale, counts }`. `counts`
 * is keyed by the walker's path, relative to `src-tauri/` for Rust and the repo for TS.
 */
export function check(root, allowlist = ALLOWLIST) {
  const tauri = join(root, "src-tauri");
  const sources = new Map();
  for (const top of ["src", "crates"]) {
    let files;
    try {
      files = [...filesUnder(join(tauri, top), [".rs"])];
    } catch {
      continue;
    }
    for (const file of files) {
      const source = readFileSync(file, "utf8");
      sources.set(relative(tauri, file).split(sep).join("/"), { source, tokens: tokenize(source) });
    }
  }
  const isTest = testPaths(sources);
  const violations = [];
  const allowed = [];
  const counts = new Map();
  for (const [path, { source, tokens }] of sources) {
    if (isTest(path) || testOnlyFile(source, tokens)) continue;
    const noReadDir = NO_READ_DIR.includes(path);
    for (const h of rustHits(source, tokens, READ_DIR)) {
      if (noReadDir)
        violations.push({
          path,
          ...h,
          why: "reads a directory; list_context_files is served from the context index",
        });
    }
    if (WALK_HOMES.some((home) => path.startsWith(home))) continue;
    const found = rustHits(source, tokens, RUST_WALK);
    if (!found.length) continue;
    const entry = allowlist.find((e) => e.path === path);
    counts.set(path, found.length);
    const out = entry && found.length <= entry.max && !noReadDir ? allowed : violations;
    for (const h of found)
      out.push({ path, ...h, why: "walks the filesystem outside the context index" });
  }
  if (!sources.size) throw new Error(`no Rust sources under ${tauri}`);

  let tsFiles = [];
  try {
    tsFiles = [...filesUnder(join(root, "src"), [".ts", ".tsx"])];
  } catch {
    // No frontend under this root (a Rust-only fixture).
  }
  for (const file of tsFiles) {
    const path = relative(root, file).split(sep).join("/");
    if (TS_HOMES.includes(path) || TS_EXEMPT.some((re) => re.test(path))) continue;
    const code = withoutTsComments(readFileSync(file, "utf8"));
    for (const m of code.matchAll(TS_CALL)) {
      const line = code.slice(0, m.index).split("\n").length;
      violations.push({
        path,
        line,
        text: m[0].replace(/\s+/g, " "),
        why: "lists files outside the shared per-root cache (requestContentFileIndex)",
      });
    }
  }

  const held = (e) => counts.get(e.path) ?? 0;
  const over = allowlist.filter((e) => held(e) > e.max);
  const stale = allowlist.filter((e) => held(e) < e.max);
  return { violations, allowed, over, stale, counts };
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const args = process.argv.slice(2);
  const rootAt = args.indexOf("--root");
  const known = new Set(["--root", "--list"]);
  if (
    (rootAt >= 0 && (!args[rootAt + 1] || args[rootAt + 1].startsWith("--"))) ||
    args.some((arg, at) => !known.has(arg) && (rootAt < 0 || at !== rootAt + 1))
  ) {
    console.error("usage: check-context-walk.mjs [--root <dir>] [--list]");
    process.exit(2);
  }
  const root = rootAt >= 0 ? resolve(args[rootAt + 1]) : repo;
  const { violations, allowed, over, stale, counts } = check(root);
  if (args.includes("--list")) {
    for (const h of [...allowed, ...violations]) console.log(`${h.path}:${h.line}: ${h.text}`);
    for (const [path, n] of [...counts].sort()) console.log(`# ${path}: ${n}`);
  }
  for (const h of violations) console.error(`${h.path}:${h.line}: ${h.text} — ${h.why}`);
  for (const e of over) {
    console.error(`${e.path}: allowlisted for ${e.max} walks but holds ${counts.get(e.path)}`);
  }
  for (const e of stale) {
    const held = counts.get(e.path) ?? 0;
    console.error(
      held
        ? `${e.path}: allowlisted for ${e.max} walks but holds ${held}; lower its max`
        : `${e.path}: allowlisted but walks nothing (or is gone); remove its entry`,
    );
  }
  if (violations.length || over.length || stale.length) {
    console.error(
      "context-walk: FAILED — read the context index (context_query / list_context_files via requestContentFileIndex) instead of walking; see scripts/check-context-walk.mjs",
    );
    process.exit(1);
  }
}
