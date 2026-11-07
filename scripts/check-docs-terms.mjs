#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Scans the shipped docs for retired product terms (home base, Publish mode, controller packs,
// …). The word list, with what to write instead and a per-term allowlist of files where the
// term is legitimately historical, is `scripts/retired-terms.json`.
//
// Usage:
//   node scripts/check-docs-terms.mjs [--root <dir>]... [--terms <file>] [--base <dir>]
//
//   --root   a directory to scan, relative to --base. Repeatable. Default: the `roots` in the
//            terms file (`codesense-docs/`, which holds the bundled handbook snapshot).
//   --terms  the terms file. Default: scripts/retired-terms.json.
//   --base   the directory hits and allowlists are relative to. Default: the repo root.
//
// Prints `file:line term` for every hit and exits 1 when there is any; exits 0 when clean and
// 2 on a usage error.

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, extname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULT_TERMS = join(ROOT, "scripts", "retired-terms.json");

/** Reads and validates a terms file. */
export function loadConfig(path = DEFAULT_TERMS) {
  const config = JSON.parse(readFileSync(path, "utf8"));
  if (!Array.isArray(config.terms) || config.terms.length === 0) {
    throw new Error(`${path}: "terms" must be a non-empty array`);
  }
  for (const entry of config.terms) {
    if (typeof entry.term !== "string" || typeof entry.pattern !== "string") {
      throw new Error(`${path}: every term needs a "term" and a "pattern"`);
    }
  }
  return config;
}

/** A glob (`**` any depth, `*` within one segment) as an anchored regular expression. */
function globToRegExp(glob) {
  let out = "";
  for (let i = 0; i < glob.length; i += 1) {
    const ch = glob[i];
    if (ch === "*" && glob[i + 1] === "*") {
      out += ".*";
      i += 1;
      if (glob[i + 1] === "/") i += 1;
    } else if (ch === "*") {
      out += "[^/]*";
    } else {
      out += ch.replace(/[.+?^${}()|[\]\\]/g, "\\$&");
    }
  }
  return new RegExp(`^${out}$`);
}

function compile(config) {
  return config.terms.map((entry) => ({
    term: entry.term,
    regex: new RegExp(entry.pattern, entry.ignoreCase ? "i" : ""),
    allow: (entry.allow ?? []).map(globToRegExp),
  }));
}

/** Every scanned file below `dir`, as POSIX paths relative to `base`, sorted. */
function walk(dir, base, extensions, skipDirs) {
  const out = [];
  const visit = (current) => {
    for (const name of readdirSync(current).sort()) {
      const full = join(current, name);
      if (statSync(full).isDirectory()) {
        if (!skipDirs.has(name)) visit(full);
      } else if (extensions.has(extname(name))) {
        out.push(relative(base, full).split(sep).join("/"));
      }
    }
  };
  if (statSync(dir).isDirectory()) visit(dir);
  else out.push(relative(base, dir).split(sep).join("/"));
  return out;
}

function textHits(file, text, terms) {
  const active = terms.filter((t) => !t.allow.some((glob) => glob.test(file)));
  const hits = [];
  text.split("\n").forEach((line, index) => {
    for (const t of active)
      if (t.regex.test(line)) hits.push({ file, line: index + 1, term: t.term });
  });
  return hits;
}

/** Apply the same retired-term checks to generated bundle bytes. */
export function scanText({ file, text, config }) {
  return textHits(file, text, compile(config));
}

/**
 * Scans `roots` (relative to `base`) for the terms in `config`.
 * Returns `[{ file, line, term }]`, one per term per line.
 */
export function scan({ base = ROOT, roots, config }) {
  const terms = compile(config);
  const extensions = new Set(config.extensions ?? [".html", ".md", ".mdx"]);
  const skipDirs = new Set(config.skipDirs ?? []);
  const hits = [];
  for (const root of roots ?? config.roots) {
    const dir = resolve(base, root);
    if (!existsSync(dir)) throw new Error(`check-docs-terms: no such root ${root}`);
    for (const file of walk(dir, base, extensions, skipDirs)) {
      hits.push(...textHits(file, readFileSync(join(base, file), "utf8"), terms));
    }
  }
  return hits;
}

export const formatHit = (hit) => `${hit.file}:${hit.line} ${hit.term}`;

function parseArgs(argv) {
  const args = { roots: [], terms: DEFAULT_TERMS, base: ROOT };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const value = argv[i + 1];
    if (!["--root", "--terms", "--base"].includes(flag) || value === undefined) {
      throw new Error(`unknown or incomplete argument: ${flag}`);
    }
    if (flag === "--root") args.roots.push(value);
    if (flag === "--terms") args.terms = resolve(value);
    if (flag === "--base") args.base = resolve(value);
    i += 1;
  }
  return args;
}

function main() {
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(`check-docs-terms: ${error.message}`);
    console.error("usage: node scripts/check-docs-terms.mjs [--root <dir>]... [--terms <file>]");
    process.exit(2);
  }
  const config = loadConfig(args.terms);
  const roots = args.roots.length ? args.roots : config.roots;
  const hits = scan({ base: args.base, roots, config });
  for (const hit of hits) console.log(formatHit(hit));
  if (hits.length) {
    console.error(`check-docs-terms: ${hits.length} retired term(s) in ${roots.join(", ")}`);
    process.exit(1);
  }
  console.error(`check-docs-terms: ok (${roots.join(", ")})`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
