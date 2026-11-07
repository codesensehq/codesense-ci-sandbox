#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Walks every HTML doc under `codesense-docs/getting-started/content/` and extracts a
// client-side search index: one entry per `<section data-search>` chunk and per `<h1>/<h2>/<h3>`
// heading. The output `generated/search-index.json` is fetched same-origin by `_shared/nav.js`
// when the user opens the ⌘K palette.
//
// Shape:
//
//   {
//     "schema": 1,
//     "generatedAt": "iso-8601",
//     "entries": [
//       { "title": "...", "crumb": "...", "kind": "section"|"heading",
//         "href": "./arcs/a-first-touch/index.html#anchor", "text": "..." }
//     ]
//   }
//
// Modes:
//   write (default) — regenerate `generated/search-index.json` from HTML sources.
//   --check          — exit non-zero if the committed index differs from a fresh render.
//
// Caps: each entry's `text` field is capped at 2 KB. Entries are deduped by heading-path.

import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CONTENT = join(ROOT, "codesense-docs", "getting-started", "content");
const OUTPUT = join(CONTENT, "generated", "search-index.json");
const TEXT_CAP_BYTES = 2048;

/** Walk every `.html` under `dir`, returning POSIX-style relative paths. */
function walkHtml(dir) {
  const out = [];
  const visit = (current) => {
    if (!existsSync(current)) return;
    for (const name of readdirSync(current).sort()) {
      if (name === ".DS_Store") continue;
      const full = join(current, name);
      if (statSync(full).isDirectory()) visit(full);
      else if (name.endsWith(".html")) out.push(relative(dir, full).split(sep).join("/"));
    }
  };
  visit(dir);
  return out;
}

/** Decode a very small set of HTML entities and strip tags for display text. */
function stripToText(html) {
  return html
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<script[\s\S]*?<\/script>/gi, "")
    .replace(/<style[\s\S]*?<\/style>/gi, "")
    .replace(/<[^>]+>/g, " ")
    .replace(/&nbsp;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/\s+/g, " ")
    .trim();
}

function capText(text) {
  if (Buffer.byteLength(text, "utf8") <= TEXT_CAP_BYTES) return text;
  // Truncate on a word boundary before the cap.
  let cut = text.slice(0, TEXT_CAP_BYTES);
  const space = cut.lastIndexOf(" ");
  if (space > TEXT_CAP_BYTES * 0.6) cut = cut.slice(0, space);
  return `${cut.trimEnd()}…`;
}

function attr(open, name) {
  const match = new RegExp(`\\b${name}\\s*=\\s*"([^"]*)"`, "i").exec(open);
  return match ? match[1] : null;
}

/** Extract page metadata — title + body[data-page] slug — from `<head>` + `<body>`. */
function pageMeta(html) {
  const titleMatch = /<title>([^<]*)<\/title>/i.exec(html);
  const title = titleMatch ? stripToText(titleMatch[1]) : null;
  const bodyOpen = /<body\b([^>]*)>/i.exec(html);
  const page = bodyOpen ? attr(bodyOpen[0], "data-page") : null;
  return { title, page };
}

/** Pull every `<section data-search>` chunk and every `<h1>/<h2>/<h3>` out of `html`. */
function extractChunks(html, pageTitle, pageHref) {
  const entries = [];
  const seenKeys = new Set();

  // 1) sections with `data-search`.
  const sectionRe = /<section\b([^>]*)>([\s\S]*?)<\/section>/gi;
  let s;
  while ((s = sectionRe.exec(html)) !== null) {
    const openAttrs = s[1];
    if (!/\bdata-search\b/i.test(openAttrs)) continue;
    const label = attr(openAttrs, "data-search") || attr(openAttrs, "aria-label") || "";
    const id = attr(openAttrs, "id");
    const inner = s[2];
    const headingMatch = /<h[1-6]\b[^>]*>([\s\S]*?)<\/h[1-6]>/i.exec(inner);
    const heading = headingMatch ? stripToText(headingMatch[1]) : label || pageTitle || "";
    const text = capText(stripToText(inner));
    const href = id ? `${pageHref}#${id}` : pageHref;
    const key = `${href}::${heading}`;
    if (seenKeys.has(key)) continue;
    seenKeys.add(key);
    entries.push({
      title: heading,
      crumb: pageTitle || "",
      kind: "section",
      href,
      text,
    });
  }

  // 2) h1..h3 headings (useful even without an authored data-search chunk).
  const headingRe = /<(h[1-3])\b([^>]*)>([\s\S]*?)<\/\1>/gi;
  let h;
  while ((h = headingRe.exec(html)) !== null) {
    const openAttrs = h[2];
    const id = attr(openAttrs, "id");
    const title = stripToText(h[3]);
    if (!title) continue;
    const href = id ? `${pageHref}#${id}` : pageHref;
    const key = `${href}::${title}`;
    if (seenKeys.has(key)) continue;
    seenKeys.add(key);
    entries.push({
      title,
      crumb: pageTitle || "",
      kind: "heading",
      href,
      text: "",
    });
  }

  return entries;
}

/** Build the full index document from the content tree. */
export function buildIndex() {
  const entries = [];
  for (const rel of walkHtml(CONTENT)) {
    // Skip generated/ and _shared/ templates — they aren't user-facing docs.
    if (rel.startsWith("generated/") || rel.startsWith("_shared/")) continue;
    const html = readFileSync(join(CONTENT, rel), "utf8");
    const { title, page } = pageMeta(html);
    const pageHref = `./${rel}`;
    const pageTitle = title || page || rel;
    entries.push(...extractChunks(html, pageTitle, pageHref));
  }
  // Stable order: by href, then title. Keeps the committed JSON diff-friendly.
  entries.sort((a, b) => (a.href + a.title).localeCompare(b.href + b.title));
  return {
    schema: 1,
    generatedAt: "deterministic",
    entries,
  };
}

function serialize(doc) {
  return `${JSON.stringify(doc, null, 2)}\n`;
}

function main() {
  const check = process.argv.includes("--check");
  const fresh = serialize(buildIndex());
  if (check) {
    const existing = existsSync(OUTPUT) ? readFileSync(OUTPUT, "utf8") : "";
    if (existing !== fresh) {
      console.error(
        [
          "generate-docs-search-index: committed search-index.json is stale.",
          `  entity:    codesense-docs/getting-started/content/generated/search-index.json`,
          `  generator: scripts/generate-docs-search-index.mjs`,
          `  fix:       npm run gen:docs`,
        ].join("\n"),
      );
      process.exit(1);
    }
    console.log("generate-docs-search-index: ok");
    return;
  }
  mkdirSync(dirname(OUTPUT), { recursive: true });
  writeFileSync(OUTPUT, fresh);
  console.log(`generate-docs-search-index: wrote ${OUTPUT}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
