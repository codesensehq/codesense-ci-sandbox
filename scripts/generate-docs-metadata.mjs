#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// The manifest uses JSON syntax (a YAML subset) to need no build dependency.
// Metadata is applied to bundle bytes only; authored pages remain untouched.
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { isMain } from "./lib/is-main.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const MANIFEST_PATH = join(
  ROOT,
  "codesense-docs/getting-started/content/_shared/topics.yml",
);
const FIELDS = ["topic", "task", "tools", "audience"];
export function loadManifest() {
  return JSON.parse(readFileSync(MANIFEST_PATH, "utf8"));
}

// Manifest globs: * is one path segment, ** crosses directories.
function matches(glob, path) {
  const pattern = glob
    .split(/(\*\*|\*)/)
    .map((part) =>
      part === "**" ? ".*" : part === "*" ? "[^/]*" : part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"),
    )
    .join("");
  return new RegExp(`^${pattern}$`).test(path);
}

function isMetadataPage(path) {
  return (
    /\.(html|md)$/i.test(path) &&
    // Golden runnable pane copies are executable source artifacts, kept byte-exact.
    !path.startsWith("examples/panes/") &&
    !path.split("/").some((p) => ["_shared", "assets", "templates", "data"].includes(p))
  );
}

export function pageMetadata(path, manifest = loadManifest()) {
  let meta = { ...manifest.defaults };
  for (const rule of manifest.rules) {
    if (matches(rule.glob, path))
      meta = {
        ...meta,
        ...Object.fromEntries(
          [...FIELDS, "type"].filter((k) => rule[k] !== undefined).map((k) => [k, rule[k]]),
        ),
      };
  }
  return meta;
}

const esc = (s) => s.replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;");
function withMetadata(path, bytes, manifest = loadManifest()) {
  if (!isMetadataPage(path)) return bytes;
  let text = bytes.toString("utf8");
  const meta = pageMetadata(path, manifest);
  if (path.endsWith(".html")) {
    const existing = /<meta\b[^>]*name=["']cs:type["'][^>]*content=["']([^"']*)["'][^>]*>/i.exec(
      text,
    );
    const runnable = existing && !["content", "doc"].includes(existing[1]);
    const kind = runnable ? existing[1] : meta.type;
    if (existing && !runnable)
      text = text.replace(existing[0], `<meta name="cs:type" content="${esc(kind)}" />`);
    const tags = FIELDS.map((k) => `<meta name="cs:${k}" content="${esc(meta[k].join(", "))}" />`);
    if (!existing && kind) tags.unshift(`<meta name="cs:type" content="${esc(kind)}" />`);
    text = text.replace(/<\/head>/i, `${tags.join("\n")}\n</head>`);
  } else {
    // Do not override runnable kinds (story, dashboard, skill) or their metadata.
    const kind = /(?:^|\n)type:\s*([^\n]+)/.exec(
      text.startsWith("---\n") ? text.slice(0, text.indexOf("\n---", 4)) : "",
    )?.[1];
    const values = [
      ...(!kind ? [`type: ${path.endsWith("/SKILL.md") ? "skill" : meta.type}`] : []),
      ...FIELDS.map((k) => `${k}: ${JSON.stringify(meta[k])}`),
    ].join("\n");
    text = text.startsWith("---\n")
      ? text.replace(/^---\n/, `---\n${values}\n`)
      : `---\n${values}\n---\n${text}`;
  }
  return Buffer.from(text);
}

export function applyMetadata(files, manifest = loadManifest()) {
  const prefix = "getting-started/content/";
  for (const [path, bytes] of files) {
    if (path.startsWith(prefix))
      files.set(path, withMetadata(path.slice(prefix.length), bytes, manifest));
  }
}

export function validateMetadata(files, manifest = loadManifest()) {
  if (manifest.version !== 1) throw new Error("topics.yml requires version 1");
  const tools = new Set(
    JSON.parse(
      readFileSync(
        join(ROOT, "codesense-docs/getting-started/content/generated/mcp-tools.json"),
        "utf8",
      ),
    ).tools.map((t) => t.name),
  );
  const pages = [...files.keys()]
    .filter((p) => p.startsWith("getting-started/content/"))
    .map((p) => p.slice("getting-started/content/".length))
    .filter(isMetadataPage);
  const validate = (meta, label, required) => {
    for (const key of FIELDS)
      if ((required || meta[key] !== undefined) && !Array.isArray(meta[key]))
        throw new Error(`${label}: ${key} must be a list`);
    for (const topic of meta.topic ?? [])
      if (!manifest.topics.includes(topic)) throw new Error(`${label}: unknown topic ${topic}`);
    for (const tool of meta.tools ?? [])
      if (!tools.has(tool)) throw new Error(`${label}: unknown tool ${tool}`);
    for (const audience of meta.audience ?? [])
      if (!["agents", "humans"].includes(audience))
        throw new Error(`${label}: unknown audience ${audience}`);
  };
  validate(manifest.defaults, "defaults", true);
  for (const rule of manifest.rules) {
    if (!pages.some((p) => matches(rule.glob, p)))
      throw new Error(`Unresolved metadata rule: ${rule.glob}`);
    validate(rule, rule.glob, false);
  }
  for (const path of pages) {
    if (!manifest.rules.some((r) => matches(r.glob, path)))
      throw new Error(`Metadata coverage missing: ${path}`);
    validate(pageMetadata(path, manifest), path, true);
  }
  return pages.length;
}

export function startHere(guide) {
  const section = guide.match(/^## Start here: by task\n([\s\S]*?)(?=^## |$(?![\s\S]))/m)?.[1];
  if (!section) throw new Error("Missing Start here: by task section");
  return (
    `# Start here: by task\n${section}`
      .replace(/\]\(\.\.\//g, "](")
      .replace(/\]\(README\.md#/g, "](concepts/README.md#")
      .trimEnd() + "\n"
  );
}

export function checkMirror(check = true) {
  const content = join(ROOT, "codesense-docs/getting-started/content");
  const path = join(content, "START-HERE.md");
  const fresh = startHere(readFileSync(join(content, "concepts/README.md"), "utf8"));
  if (check) {
    if (!existsSync(path) || readFileSync(path, "utf8") !== fresh)
      throw new Error("START-HERE.md is stale; run pnpm gen:docs");
  } else writeFileSync(path, fresh);
}

if (isMain(import.meta.url)) {
  checkMirror(process.argv.includes("--check"));
  import("./build-codesense-docs.mjs").then(({ buildTree }) => {
    console.log(
      `generate-docs-metadata: ${validateMetadata(buildTree())} pages covered (bundle only)`,
    );
  });
}
