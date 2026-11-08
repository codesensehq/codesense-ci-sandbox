#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Builds the CodeSense docs context repo that every install ships with (SW-OC-16).
//
// The output is a gzipped tar of a real git repository's `.git` directory: one commit on
// `main` whose tree is generated from this source checkout, without transport metadata. The app
// places it under the CodeSense home on launch and checks the tree out with libgit2, so the
// placed folder is an ordinary local repository; updates select an app-bundled commit offline.
//
// Deterministic: the commit uses a fixed author and date from `codesense-docs/bundle.json`,
// so the same sources always produce the same commit id and the same archive bytes. No git
// binary is needed; objects are written directly.
//
// Usage: node scripts/build-codesense-docs.mjs [output.tar.gz]
//        node scripts/build-codesense-docs.mjs --handbook | --check-handbook | --list-inputs
// Default output: src-tauri/resources/codesense-docs.tar.gz (only rewritten when it changes).
//
// The handbook MDX and the agent releases come from the website source: site/ in this checkout, or
// CODESENSE_SITE_ROOT, an absolute path to a site checked out elsewhere (lib/site-root.mjs). With a
// site outside this checkout only --check-handbook and --list-inputs run, unless
// --allow-external-site is passed: that is for refreshing the handbook snapshot in a checkout
// without site/, from a site checked out beside it (run --handbook there, review and commit the
// snapshot in that checkout).

import { applyMetadata } from "./generate-docs-metadata.mjs";

import { lintPaneExamples } from "./lint-pane-examples.mjs";

import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, posix, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync, gzipSync } from "node:zlib";
import { BUILTIN_PANE_EXAMPLES } from "./lib/builtin-panes.mjs";
import { writeBuildOutput } from "./lib/build-output.mjs";
import { isMain } from "./lib/is-main.mjs";
import { HANDBOOK_DIR, siteInside, siteRoot } from "./lib/site-root.mjs";
import { exampleBytes } from "./generate-builtin-pane-examples.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const TEMPLATE = join(ROOT, "codesense-docs");
/** The website's source, where the handbook MDX and the agent releases live: site/ here, or
 *  $CODESENSE_SITE_ROOT (lib/site-root.mjs). */
const SITE = siteRoot(ROOT);
const HANDBOOK = join(SITE, HANDBOOK_DIR);
/** Handbook sections that read well as plain markdown; `reference/` is generated tables. */
const HANDBOOK_SECTIONS = ["getting-started", "concepts", "guides"];

const bundle = JSON.parse(readFileSync(join(TEMPLATE, "bundle.json"), "utf8"));
const TEAM = bundle.team;
const CONTENT = `${TEAM}/content`;
const PAGE_SDK_REFERENCE = "generated/page-sdk.md";
/** Site reference pages that ship as a generated page, by their site path. */
const GENERATED_REFERENCES = {
  "reference/page-sdk": PAGE_SDK_REFERENCE,
  "reference/story-fields": "generated/story-fields.md",
};
/** The handbook as it ships, committed: generated from the site MDX, which only a private
 *  checkout has, so a build from source ships it as is. Never edit it by hand. */
const HANDBOOK_SNAPSHOT_LABEL = `codesense-docs/${CONTENT}/handbook`;
const HANDBOOK_SNAPSHOT = join(TEMPLATE, CONTENT, "handbook");
const AGENT_RELEASES = join(SITE, "src", "content", "generated", "agent-releases.json");
let agentReleases;

/** A `{releases.<cli>}` expression in MDX prose: the snapshot spells out the site's value. */
function releaseText(name) {
  agentReleases ??= JSON.parse(readFileSync(AGENT_RELEASES, "utf8"));
  if (!Object.hasOwn(agentReleases, name))
    throw new Error(`Unknown {releases.${name}} in a handbook page`);
  return agentReleases[name];
}

// SW-ODC-V bridge-verb allowlist: every verb an ambient right pane (SW-ODC-P) calls
// or dispatches via its `data-action` JSON. Hand-maintained. A pane calls
// `window.codesense.invoke(verb)`, which accepts only the page-bridge verbs
// (`HTML_BRIDGE_ALLOWLIST` in src/features/dashboard/components/html-bridge.ts).
// `<cs-try-this verb="…">` blocks are checked by scripts/check-docs-api-examples.mjs,
// against the generated page verbs and MCP tools. `subscribeContext` and `onTheme` are
// page-SDK methods a pane may call directly.
const BRIDGE_VERB_ALLOWLIST = new Set([
  // Page-bridge verbs.
  "context",
  "worktrees",
  "machines",
  "sessions",
  "routines",
  "roles",
  "openWorkspace",
  "openFile",
  "runSkill",
  "subscribeContext",
  "onTheme",
]);

// SW-ODC-P: `showsOn` keys a pane manifest may use. Mirrors the taxonomy the
// viewer's pane registration (`src/features/shell/right-pane/panes/`) treats
// as a canonical surface. `*` means "any surface"; a `pane:*` form matches any
// pane-hosted surface.
const PANE_SHOWS_ON_KEYS = new Set([
  "*",
  "viewer:markdown",
  "viewer:html",
  "viewer:excalidraw",
  "agents:detail",
  "agents:list",
  "inbox:thread",
  "inbox:list",
  "pipeline:story",
  "pipeline:epic",
  "pipeline:sprint",
  "terminals:detail",
  "routines:detail",
  "routines:run",
  "worktrees:detail",
  "pane:*",
]);

/** Every file below `dir`, as POSIX paths relative to it, sorted. */
function walk(dir, skip = () => false) {
  const out = [];
  const visit = (current) => {
    for (const name of readdirSync(current).sort()) {
      if (name === ".DS_Store") continue;
      const full = join(current, name);
      if (skip(full)) continue;
      if (statSync(full).isDirectory()) visit(full);
      else out.push(relative(dir, full).split(sep).join("/"));
    }
  };
  visit(dir);
  return out;
}

// ---------------------------------------------------------------------------
// Handbook MDX → plain markdown
// ---------------------------------------------------------------------------

function frontmatter(source) {
  const match = /^---\n([\s\S]*?)\n---\n?/.exec(source);
  if (!match) return { fields: {}, body: source };
  const fields = {};
  for (const line of match[1].split("\n")) {
    const field = /^([a-z_]+):\s*"?(.*?)"?\s*$/.exec(line);
    if (field && field[2]) fields[field[1]] = field[2];
  }
  return { fields, body: source.slice(match[0].length) };
}

const attr = (tag, name) => new RegExp(`${name}="([^"]*)"`).exec(tag)?.[1];

/** The bold first line a site callout becomes. */
function calloutLabel(tag, attrs) {
  if (tag === "Aside") return attr(attrs, "title") ?? "Note";
  if (tag === "Beta") return "Beta";
  return `Coming: ${attr(attrs, "label") ?? "not built yet"}`;
}

/** Rewrites a site link to the generated markdown file, relative to `fromFile`. */
function rewriteLink(href, fromFile, available) {
  // Other site pages (/download, /roadmap, …) are not in the bundle: link the live site.
  if (/^\/(?!docs\b|handbook\b)/.test(href)) return `https://codesense.ai${href}`;
  const match = /^\/(?:docs|handbook)\/?([^#)]*?)\/?(#[^)]*)?$/.exec(href);
  if (!match) return href;
  const page =
    GENERATED_REFERENCES[match[1]] ?? (match[1] ? `handbook/${match[1]}.md` : "handbook/README.md");
  if (!available.has(page)) return null;
  const rel = posix.relative(posix.dirname(fromFile), page);
  return `${rel}${match[2] ?? ""}`;
}

/** Converts one handbook page. `file` is its output path below the team content dir. */
export function mdxToMarkdown(source, file, available) {
  const { fields, body } = frontmatter(source.replace(/\r\n/g, "\n"));
  const out = [];
  if (fields.title) out.push(`# ${fields.title}`, "");
  if (fields.description) out.push(`_${fields.description}_`, "");
  const lines = body.split("\n");
  let quote = false;
  let fence = false;
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    // Code examples ship verbatim (inside a callout, still quoted).
    const fenceLine = /^\s*(```|~~~)/.test(line);
    if (fence || fenceLine) {
      if (fenceLine) fence = !fence;
      out.push(quote ? `> ${line}` : line);
      continue;
    }
    if (/^import\s.+from\s/.test(line)) continue;
    const oneLine = /^\s*<(Aside|Beta|Coming)\b([^>]*)>(.*)<\/\1>\s*$/.exec(line);
    if (oneLine) {
      out.push(`> **${calloutLabel(oneLine[1], oneLine[2])}**`, ">", `> ${oneLine[3].trim()}`);
      continue;
    }
    const open = /^\s*<(Aside|Beta|Coming)\b([^>]*)>\s*$/.exec(line);
    if (open) {
      const [, tag, attrs] = open;
      out.push(`> **${calloutLabel(tag, attrs)}**`, ">");
      quote = true;
      continue;
    }
    if (/^\s*<\/(Aside|Beta|Coming)>\s*$/.test(line)) {
      quote = false;
      continue;
    }
    // Any other component is a site-only widget (screenshots, generated tables): drop it,
    // including a multi-line element.
    const component = /^\s*<(?!RepoPill\b)([A-Z][A-Za-z]*)\b/.exec(line);
    if (component) {
      const name = component[1];
      let end = i;
      while (
        end < lines.length &&
        !/\/>\s*$/.test(lines[end]) &&
        !new RegExp(`</${name}>\\s*$`).test(lines[end])
      ) {
        end += 1;
      }
      if (end >= lines.length) throw new Error(`Unclosed <${name}> in handbook page ${file}`);
      i = end;
      continue;
    }
    let text = line.replace(/<RepoPill repo="([^"]+)"\s*\/>/g, "`$1`");
    text = text.replace(/\{releases\.(\w+)\}/g, (_, name) => releaseText(name));
    text = text.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (whole, label, href) => {
      const target = rewriteLink(href, file, available);
      return target === null ? label : `[${label}](${target})`;
    });
    out.push(quote ? (text ? `> ${text}` : ">") : text);
  }
  return `${out
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim()}\n`;
}

/** Every bundled handbook page converted from the site MDX, keyed by its path below the
 *  content dir. */
export function convertHandbook() {
  const pages = [["index.mdx", "handbook/README.md"]];
  for (const section of HANDBOOK_SECTIONS) {
    for (const path of walk(join(HANDBOOK, section))) {
      if (path.endsWith(".mdx")) {
        pages.push([`${section}/${path}`, `handbook/${section}/${path.replace(/\.mdx$/, ".md")}`]);
      }
    }
  }
  const available = new Set([
    ...pages.map(([, out]) => out),
    ...Object.values(GENERATED_REFERENCES),
  ]);
  return new Map(
    pages.map(([source, out]) => [
      out,
      mdxToMarkdown(readFileSync(join(HANDBOOK, source), "utf8"), out, available),
    ]),
  );
}

/**
 * Brings the committed handbook snapshot in line with the site MDX and returns the paths that
 * differed (written, or with `check`, only reported). Returns null without a site/ handbook:
 * a build from source has only the snapshot.
 */
export function syncHandbookSnapshot({ check = false } = {}) {
  if (!existsSync(HANDBOOK)) return null;
  const want = convertHandbook();
  const have = existsSync(HANDBOOK_SNAPSHOT)
    ? walk(HANDBOOK_SNAPSHOT).map((path) => `handbook/${path}`)
    : [];
  const changed = [...want].filter(([path, text]) => {
    const file = join(TEMPLATE, CONTENT, path);
    return !existsSync(file) || readFileSync(file, "utf8") !== text;
  });
  const stale = have.filter((path) => !want.has(path));
  if (!check && (changed.length || stale.length)) {
    // Pages are staged outside codesense-docs/ and renamed in one by one, so a walk of the
    // tree never meets a partial or temporary file. A build with site/ never reads the
    // snapshot (buildTree takes the pages from the MDX), and a build without site/ never
    // refreshes it, so no build reads pages while they change.
    const staging = join(ROOT, `.handbook-refresh-${process.pid}-${Date.now()}`);
    try {
      for (const [path, text] of changed) {
        const temp = join(staging, path);
        mkdirSync(dirname(temp), { recursive: true });
        writeFileSync(temp, text);
        const file = join(TEMPLATE, CONTENT, path);
        mkdirSync(dirname(file), { recursive: true });
        renameSync(temp, file);
      }
      for (const path of stale) rmSync(join(TEMPLATE, CONTENT, path), { force: true });
    } finally {
      rmSync(staging, { recursive: true, force: true });
    }
  }
  return [...changed.map(([path]) => path), ...stale].sort();
}

// ---------------------------------------------------------------------------
// The repository tree
// ---------------------------------------------------------------------------

function yaml(lines) {
  return `${lines.join("\n")}\n`;
}

// ---------------------------------------------------------------------------
// SW-ODC build-time lints (shipped HTML docs under getting-started/content/)
// ---------------------------------------------------------------------------

/**
 * Enumerate phase numbers declared in the shipped `phases.yml`. Returns a Set
 * of integers (1..N) in declaration order. The parser is intentionally tiny —
 * it recognises `- phase: 1` style list entries at the first indentation level
 * and ignores everything else. If the file is missing or empty, the Set is
 * empty and the caller (the factory-playbook lint) decides whether that is a
 * failure.
 *
 * Mirrors the per-tool mini-parser elsewhere in this script (see
 * `parsePaneManifest`): no dependency on a full YAML library.
 */
export function phasesInManifest(files) {
  const PHASES = `${CONTENT}/generated/phases.yml`;
  const raw = files.get(PHASES);
  const out = new Set();
  if (!raw) return out;
  const text = raw.toString("utf8");
  for (const line of text.split("\n")) {
    const m = /^\s*-\s*phase\s*:\s*(\d+)\s*$/.exec(line);
    if (m) out.add(Number(m[1]));
  }
  return out;
}

/**
 * SW-ODC-9 build-time check: every phase reference embedded in
 * `getting-started/content/factory-playbook.html` resolves to a phase that is
 * actually declared in `generated/phases.yml` (the single authored source,
 * owned by SW-ODC-11). The dashboard is a renderer; it must never ship a key
 * the manifest doesn't carry.
 *
 * The reference surfaces the lint scans:
 *   - `data-phase="N"` attributes on rail buttons (the mock emits these; the
 *     shipped build renders them from phases.yml at runtime, but any inline
 *     stub for standalone preview must agree).
 *   - `factory-phase-N.json` URL references in fetches / hrefs.
 *   - `state.phaseById[N]` numeric literals (safety belt — the only legal
 *     numbers in that position are the phases we actually declare).
 *
 * The file is also required to live under `getting-started/content/` so the
 * dashboard ships in the docs bundle at all (fail-closed; silent-drop would
 * surface as a 404 on first-launch).
 */
export function lintFactoryPlaybook(files) {
  const path = `${CONTENT}/factory-playbook.html`;
  const raw = files.get(path);
  if (!raw) {
    throw new Error(
      `build-codesense-docs: SW-ODC-9 lint — missing ${path} ` +
        `(the factory-playbook dashboard must ship in the docs bundle).`,
    );
  }
  const html = raw.toString("utf8");
  const phases = phasesInManifest(files);
  if (phases.size === 0) {
    throw new Error(
      `build-codesense-docs: SW-ODC-9 lint — ${CONTENT}/generated/phases.yml ` +
        `is missing or has no phase entries; the dashboard cannot render.`,
    );
  }

  const errors = [];

  // 1 · data-phase="N" attributes.
  const dataPhaseRe = /\bdata-phase\s*=\s*"(\d+)"/g;
  let m;
  while ((m = dataPhaseRe.exec(html)) !== null) {
    const n = Number(m[1]);
    if (!phases.has(n)) {
      errors.push(
        `data-phase="${n}" is not declared in generated/phases.yml ` +
          `(declared: ${[...phases].sort((a, b) => a - b).join(", ")}).`,
      );
    }
  }

  // 2 · generated/factory-phase-N.json URL references.
  const jsonRefRe = /factory-phase-(\d+)\.json/g;
  while ((m = jsonRefRe.exec(html)) !== null) {
    const n = Number(m[1]);
    if (!phases.has(n)) {
      errors.push(
        `factory-phase-${n}.json referenced in factory-playbook.html but not ` +
          `declared in generated/phases.yml.`,
      );
    }
  }

  // 3 · The HTML must reference phases.yml at least once — the whole point of
  //     the dashboard is to render from the single authored source.
  if (html.indexOf("generated/phases.yml") < 0) {
    errors.push(
      `factory-playbook.html does not reference generated/phases.yml. ` +
        `It must render from the single authored source (SW-ODC-11).`,
    );
  }

  // 4 · <data-count> placeholders present — I-11 forbids hard-coded surface
  //     counts in shipped copy.
  if (!/<data-count\b/i.test(html)) {
    errors.push(
      `factory-playbook.html is missing <data-count> placeholders. ` +
        `Use <data-count id="X"> rather than literal counts (I-11).`,
    );
  }

  if (errors.length) {
    throw new Error(
      `build-codesense-docs: SW-ODC-9 factory-playbook lint failed:\n  - ` + errors.join("\n  - "),
    );
  }
}

/**
 * Enforce the training-manual + visual-asset + diagram conventions on every
 * HTML doc under `getting-started/content/`. Called from `buildTree()` once
 * every file is loaded. Throws a readable error naming the first drift and
 * the owning story so a developer can fix it fast.
 *
 * Rules:
 *   - Every file under `getting-started/content/arc-*\/` opens with
 *     `<section data-hero>`.
 *   - Every `<img data-annot>` has a sibling `.annot.json` in the bundle.
 *   - Every annotated screenshot has both `-dark.png` and `-light.png`.
 *   - Zero Mermaid / D2 blocks in shipped HTML.
 *     (The former `.excalidraw.png` sibling-source rule was removed in the
 *     MCP architectural reframe — `render_canvas` returns the PNG inline to
 *     the agent and shipped docs no longer embed `<img>` references to a
 *     per-diagram sidecar.)
 */
export function lintShippedHtml(files) {
  const errors = [];
  const paths = [...files.keys()];
  const prefix = `${CONTENT}/`;

  const hasInBundle = (path) => files.has(path);
  const siblingPath = (owner, relSrc) => {
    const dir = posix.dirname(owner);
    return posix.normalize(posix.join(dir, relSrc));
  };

  for (const path of paths) {
    if (!path.startsWith(prefix) || !path.endsWith(".html")) continue;
    const rel = path.slice(prefix.length);
    if (rel.startsWith("_shared/") || rel.startsWith("generated/")) continue;
    const html = files.get(path).toString("utf8");

    // 1. Arc content pages lead with a hero.
    if (rel.startsWith("arc-")) {
      // Scan only the body: find the first <section …> or <main …> content tag.
      const bodyMatch = /<body\b[^>]*>([\s\S]*?)<\/body>/i.exec(html);
      const body = bodyMatch ? bodyMatch[1] : html;
      const firstSection = /<section\b([^>]*)>/i.exec(body);
      if (!firstSection || !/\bdata-hero\b/i.test(firstSection[1])) {
        errors.push(
          `${path}: arc page must open with a <section data-hero> block ` + `(shipped-HTML lint).`,
        );
      }
    }

    let m;
    // 3. <img data-annot="…"> has a sibling .annot.json in the bundle.
    const annotRe = /<img\b[^>]*\bdata-annot\s*=\s*"([^"]+)"[^>]*>/gi;
    while ((m = annotRe.exec(html)) !== null) {
      const relSrc = m[1];
      const target = siblingPath(path, relSrc);
      if (!hasInBundle(target)) {
        errors.push(
          `${path}: <img data-annot="${relSrc}"> has no sibling annotation sidecar ` +
            `at ${target} (SW-ODC-V lint).`,
        );
      }
    }

    // 4. <img data-annot src="…-dark.png"> has a matching -light.png sibling.
    const imgRe = /<img\b[^>]*>/gi;
    while ((m = imgRe.exec(html)) !== null) {
      const tag = m[0];
      if (!/\bdata-annot\s*=/.test(tag)) continue;
      const src = /\bsrc\s*=\s*"([^"]+)"/i.exec(tag);
      if (!src) continue;
      const url = src[1];
      let pair = null;
      if (/-dark\.png$/.test(url)) pair = url.replace(/-dark\.png$/, "-light.png");
      else if (/-light\.png$/.test(url)) pair = url.replace(/-light\.png$/, "-dark.png");
      else {
        errors.push(
          `${path}: annotated screenshot <img src="${url}"> must end in ` +
            `"-dark.png" or "-light.png" (SW-ODC-V lint).`,
        );
        continue;
      }
      const pairTarget = siblingPath(path, pair);
      if (!hasInBundle(pairTarget)) {
        errors.push(
          `${path}: annotated screenshot ${url} missing its paired theme capture at ` +
            `${pairTarget} (SW-ODC-V lint).`,
        );
      }
    }

    // 5. Zero Mermaid / D2 blocks in shipped HTML.
    //    (The former `.excalidraw.png` sibling-source rule was removed in the
    //    MCP architectural reframe: `render_canvas` returns PNGs inline to the
    //    agent, so shipped docs no longer embed per-diagram `<img>` sidecars.)
    if (/```mermaid\b/.test(html) || /<div\b[^>]*\bclass\s*=\s*"[^"]*\bmermaid\b/i.test(html)) {
      errors.push(
        `${path}: shipped HTML must not contain Mermaid blocks (SW-ODC-D lint; ` +
          `use Excalidraw via apply_canvas).`,
      );
    }
    if (/```d2\b/.test(html) || /<pre\b[^>]*\bdata-d2\b/i.test(html)) {
      errors.push(
        `${path}: shipped HTML must not contain D2 blocks (SW-ODC-D lint; ` +
          `use Excalidraw via apply_canvas).`,
      );
    }
  }

  if (errors.length) {
    throw new Error(
      `build-codesense-docs: shipped-HTML lint failed:\n  - ${errors.join("\n  - ")}`,
    );
  }
}

/**
 * SW-ODC-19 build-time check: every phase page under
 * `getting-started/content/arc-h-pipelines-and-sdlc/` references a phase key
 * that actually lives in `generated/phases.yml`, and renders from that single
 * authored source (same single-source-of-truth rule the factory-playbook
 * dashboard answers to in `lintFactoryPlaybook`).
 *
 * The content pages are a second surface over the same `phases.yml` manifest;
 * SW-ODC-11 owns the data, SW-ODC-9 (dashboard) + SW-ODC-19 (these content
 * pages) consume it. The lint prevents either surface from drifting:
 *
 *   - A phase file (`arc-h-pipelines-and-sdlc/N-*.html`) carries a
 *     `data-phase="N"` on its <body>. The N must be declared in phases.yml.
 *   - The leading number in the filename (`N` in `N-*.html`) must match the
 *     body's data-phase value (so file order and phase number cannot drift).
 *   - Every phase page must reference `generated/phases.yml` (how the client
 *     renders the facets, snippets, worked example, and "what goes wrong").
 *   - If the arc ships at all, `index.html` must be present too — the
 *     overview is the arc's landing door and the only page without a
 *     data-phase number.
 *
 * The arc is optional until SW-ODC-19 lands; the lint only runs if there is
 * at least one file under the arc's directory.
 */
export function lintArcHSdlcContent(files) {
  const prefix = `${CONTENT}/arc-h-pipelines-and-sdlc/`;
  const paths = [...files.keys()].filter((p) => p.startsWith(prefix) && p.endsWith(".html")).sort();
  if (paths.length === 0) return; // arc not shipped yet
  const errors = [];
  const phases = phasesInManifest(files);
  if (phases.size === 0) {
    throw new Error(
      `build-codesense-docs: SW-ODC-19 lint — ${CONTENT}/generated/phases.yml ` +
        `is missing or has no phase entries; the Arc H content pages cannot ` +
        `render.`,
    );
  }

  const indexPath = `${prefix}index.html`;
  if (!files.has(indexPath)) {
    errors.push(
      `${prefix}: SW-ODC-19 arc ships but is missing index.html (the arc overview ` +
        `and 14-phase stepper landing door).`,
    );
  }

  const seenPhases = new Set();
  for (const path of paths) {
    const name = path.slice(prefix.length);
    if (name === "index.html") continue;
    const fname = /^(\d{1,2})-[a-z0-9-]+\.html$/.exec(name);
    if (!fname) continue; // non-phase file (shared helpers etc.) — not our concern here
    const fileNum = Number(fname[1]);
    const html = files.get(path).toString("utf8");

    // 1. Body carries data-phase that matches the filename's leading number.
    const bodyAttr = /<body\b[^>]*\bdata-phase\s*=\s*"(\d+)"/i.exec(html);
    if (!bodyAttr) {
      errors.push(
        `${path}: SW-ODC-19 phase page must declare <body data-phase="N"> so ` +
          `the shared renderer can inject fields from generated/phases.yml.`,
      );
      continue;
    }
    const bodyNum = Number(bodyAttr[1]);
    if (bodyNum !== fileNum) {
      errors.push(
        `${path}: filename declares phase ${fileNum} but body data-phase="${bodyNum}" ` +
          `(the two must agree so the sidenav and renderer stay in step).`,
      );
    }
    if (!phases.has(bodyNum)) {
      errors.push(
        `${path}: data-phase="${bodyNum}" is not declared in generated/phases.yml ` +
          `(declared: ${[...phases].sort((a, b) => a - b).join(", ")}).`,
      );
    }
    seenPhases.add(bodyNum);

    // 2. Page must render from phases.yml. The client-side renderer fetches
    //    ../generated/phases.yml; the page or its shared script must reference
    //    the file.
    const refsManifest =
      html.indexOf("generated/phases.yml") >= 0 || html.indexOf("_phase-page.js") >= 0;
    if (!refsManifest) {
      errors.push(
        `${path}: SW-ODC-19 phase page does not reference generated/phases.yml ` +
          `(directly, or via the shared _phase-page.js runtime). The page must ` +
          `render from the single authored source (SW-ODC-11).`,
      );
    }
  }

  // 3. Every page's `data-phase` must be unique within the arc — two files
  //    pointing at the same phase would ship inconsistent copies of the same
  //    row.
  if (
    seenPhases.size !==
    paths.filter((p) => !p.endsWith("index.html") && /\/\d{1,2}-[a-z0-9-]+\.html$/.test(p)).length
  ) {
    errors.push(
      `${prefix}: SW-ODC-19 arc has duplicate data-phase values across phase ` +
        `pages (each phase number must appear exactly once).`,
    );
  }

  if (errors.length) {
    throw new Error(
      `build-codesense-docs: SW-ODC-19 arc-H lint failed:\n  - ${errors.join("\n  - ")}`,
    );
  }
}

/** `bundle.readme` relative to the team's content root (`teams.yml` `landing_page`). */
export function landingPage(readme) {
  const prefix = `${CONTENT}/`;
  if (!readme.startsWith(prefix)) {
    throw new Error(`build-codesense-docs: bundle.readme ${readme} is not inside ${CONTENT}/`);
  }
  const page = readme.slice(prefix.length);
  if (!page) throw new Error(`build-codesense-docs: bundle.readme ${readme} names no file`);
  return page;
}

/**
 * Resolve the readme path declared in `bundle.json` against the on-disk tree,
 * preferring `.html` over a sibling `.md` when both exist. The declared path
 * can point at either extension — the resolver returns whichever file is live.
 * Throws if neither sibling is present (the bundle would ship with a broken
 * `readme` pointer and the first launch would land on a 404).
 *
 * `fileExists` defaults to the Node fs check; a different predicate (e.g. the
 * build-tree map's `has`) is used in tests.
 */
export function resolveReadme(templateRoot, declaredRelative, fileExists) {
  const exists = fileExists ?? ((rel) => existsSync(join(templateRoot, rel)));
  const htmlVariant = declaredRelative.replace(/\.md$/i, ".html");
  const mdVariant = declaredRelative.replace(/\.html$/i, ".md");
  // Prefer .html where both are present. The HTML landing is the shipped
  // first-launch surface (SW-ODC-2); .md stays supported for pre-ODC bundles.
  if (htmlVariant !== declaredRelative && exists(htmlVariant)) return htmlVariant;
  if (exists(declaredRelative)) return declaredRelative;
  if (mdVariant !== declaredRelative && exists(mdVariant)) return mdVariant;
  throw new Error(
    `build-codesense-docs: bundle.readme ${declaredRelative} not found in ` +
      `the shipped tree (also checked ${htmlVariant} / ${mdVariant}).`,
  );
}

// ---------------------------------------------------------------------------
// SW-ODC-P pane lint — the docs org's ambient right panes
// ---------------------------------------------------------------------------

/** Minimal `pane-manifest.yaml` parser: enough shape for the lint, no deps.
 *  Returns `{ version, panes: [{id, html, showsOn, priority, collapsedByDefault}] }`.
 *  Throws a readable error on the first malformed line. */
export function parsePaneManifest(text) {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const manifest = { version: null, panes: [] };
  let i = 0;
  let current = null;
  // Within an entry: are we filling an inline list for a block-scalar key?
  let inListFor = null;
  const strip = (s) =>
    s
      .trim()
      .replace(/^["'](.*)["']$/, "$1")
      .trim();
  const parseInlineList = (s) => {
    const trimmed = s.trim();
    if (!trimmed.startsWith("[") || !trimmed.endsWith("]")) return null;
    const body = trimmed.slice(1, -1).trim();
    if (!body) return [];
    return body.split(",").map((t) => strip(t));
  };
  for (; i < lines.length; i += 1) {
    const raw = lines[i];
    if (!raw.trim() || raw.trim().startsWith("#")) continue;
    const topMatch = /^(version|panes)\s*:\s*(.*)$/.exec(raw);
    if (topMatch) {
      const key = topMatch[1];
      const value = topMatch[2].trim();
      if (key === "version") {
        const n = Number(value);
        if (!Number.isInteger(n)) throw new Error(`pane-manifest: version must be an integer`);
        manifest.version = n;
      }
      inListFor = null;
      continue;
    }
    const entryStart = /^\s*-\s+id\s*:\s*(.*)$/.exec(raw);
    if (entryStart) {
      current = {
        id: strip(entryStart[1]),
        html: null,
        title: null,
        icon: null,
        showsOn: [],
        priority: null,
        collapsedByDefault: false,
        reactsTo: [],
      };
      manifest.panes.push(current);
      inListFor = null;
      continue;
    }
    if (!current) continue;
    const listItem = /^\s+-\s+(.*)$/.exec(raw);
    if (listItem && inListFor) {
      current[inListFor].push(strip(listItem[1]));
      continue;
    }
    const field = /^\s+([a-zA-Z_]+)\s*:\s*(.*)$/.exec(raw);
    if (!field) continue;
    const [, key, rawValue] = field;
    const value = rawValue.trim();
    inListFor = null;
    switch (key) {
      case "title":
        current.title = strip(value);
        break;
      case "html":
        current.html = strip(value);
        break;
      case "icon":
        current.icon = strip(value);
        break;
      case "priority":
        current.priority = value === "" ? null : Number(value);
        break;
      case "collapsedByDefault":
        current.collapsedByDefault = value === "true";
        break;
      case "showsOn":
      case "reactsTo": {
        const field = key;
        const inline = parseInlineList(value);
        if (inline !== null) {
          current[field] = inline;
        } else if (value === "" || value === null) {
          inListFor = field;
        } else {
          current[field] = [strip(value)];
        }
        break;
      }
      default:
        // Unknown field: ignored (manifest schema may grow).
        break;
    }
  }
  if (manifest.version === null) throw new Error(`pane-manifest: missing \`version\``);
  return manifest;
}

/**
 * Enforce SW-ODC-P invariants on `getting-started/panes/`:
 *   - Every pane HTML file name matches `[a-z0-9][a-z0-9-]*\.html` (the Rust
 *     pane-id contract in `src-tauri/src/panes/mod.rs::is_valid_pane_id`), so
 *     every file in `panes/` is a discoverable pane.
 *   - Every `pane-manifest.yaml` entry's `html` resolves to a shipped file.
 *   - Every manifest `id` matches its `html` filename stem (so the viewer can
 *     merge discovery and manifest by id).
 *   - Every `showsOn` key is a known pane-context taxonomy key.
 *   - Every pane HTML's bridge calls use allowlisted verbs: both direct
 *     `window.codesense.<verb>(…)` reads AND `data-action='{…,"cmd":"<verb>",…}'`
 *     JSON blobs that the pane's own click handler dispatches via `invoke`.
 *   - Priorities are unique within a `showsOn` bucket.
 */
export function lintShippedPanes(files) {
  const prefix = "getting-started/panes/";
  const paneFiles = [...files.keys()].filter((p) => p.startsWith(prefix));
  if (paneFiles.length === 0) return;

  const errors = [];
  const htmlNames = new Set();

  // Verify every pane file is a conforming pane id (except the manifest).
  for (const path of paneFiles) {
    const name = path.slice(prefix.length);
    if (name === "pane-manifest.yaml") continue;
    if (!name.endsWith(".html")) {
      // A non-HTML asset is allowed (pane may load relative `lib/chart.js`),
      // but reject hidden / staging files that would never serve.
      if (name.startsWith(".") || /\.tmp$/.test(name)) {
        errors.push(`${path}: staging/hidden file must not ship under panes/.`);
      }
      continue;
    }
    const stem = name.slice(0, -".html".length);
    if (!/^[a-z0-9][a-z0-9-]*$/.test(stem)) {
      errors.push(
        `${path}: pane file name must match [a-z0-9][a-z0-9-]*\\.html ` +
          `(matches is_valid_pane_id in src-tauri/src/panes/mod.rs).`,
      );
      continue;
    }
    htmlNames.add(name);
  }

  // Parse the manifest, if any.
  const manifestPath = `${prefix}pane-manifest.yaml`;
  const manifest = files.has(manifestPath)
    ? (() => {
        try {
          return parsePaneManifest(files.get(manifestPath).toString("utf8"));
        } catch (e) {
          errors.push(`${manifestPath}: ${e.message}`);
          return null;
        }
      })()
    : null;

  if (manifest) {
    const seenIds = new Set();
    const priorityByBucket = new Map();
    for (const entry of manifest.panes) {
      if (!entry.id || !/^[a-z0-9][a-z0-9-]*$/.test(entry.id)) {
        errors.push(
          `${manifestPath}: entry id ${JSON.stringify(entry.id)} must match [a-z0-9][a-z0-9-]*.`,
        );
        continue;
      }
      if (seenIds.has(entry.id)) {
        errors.push(`${manifestPath}: duplicate pane id ${JSON.stringify(entry.id)}.`);
      }
      seenIds.add(entry.id);
      if (!entry.html) {
        errors.push(`${manifestPath}: entry ${entry.id} is missing \`html\`.`);
        continue;
      }
      if (!htmlNames.has(entry.html)) {
        errors.push(
          `${manifestPath}: entry ${entry.id} points at ${entry.html} ` +
            `which is not shipped under ${prefix}.`,
        );
      }
      if (entry.html !== `${entry.id}.html`) {
        errors.push(
          `${manifestPath}: entry ${entry.id} must point at ${entry.id}.html ` +
            `so the viewer can merge discovery and manifest by id.`,
        );
      }
      for (const key of entry.showsOn) {
        if (!PANE_SHOWS_ON_KEYS.has(key)) {
          errors.push(
            `${manifestPath}: entry ${entry.id} has unknown showsOn key ` +
              `${JSON.stringify(key)} (allowed: ${[...PANE_SHOWS_ON_KEYS].sort().join(", ")}).`,
          );
        }
      }
      for (const bucket of entry.showsOn) {
        const existing = priorityByBucket.get(bucket) ?? new Map();
        if (entry.priority !== null && existing.has(entry.priority)) {
          errors.push(
            `${manifestPath}: entries ${existing.get(entry.priority)} and ${entry.id} ` +
              `share priority ${entry.priority} on bucket ${JSON.stringify(bucket)}.`,
          );
        } else if (entry.priority !== null) {
          existing.set(entry.priority, entry.id);
        }
        priorityByBucket.set(bucket, existing);
      }
    }
    // Every shipped pane should have a manifest entry so the viewer knows its
    // context + priority. (A pane without an entry still works via discovery,
    // but the lint catches an authoring oversight.)
    for (const name of htmlNames) {
      const id = name.slice(0, -".html".length);
      if (!seenIds.has(id)) {
        errors.push(
          `${manifestPath}: no entry for shipped pane ${name}; add one or remove the file.`,
        );
      }
    }
  }

  // Per-pane HTML: every bridge call uses an allowlisted verb.
  for (const path of paneFiles) {
    if (!path.endsWith(".html")) continue;
    const html = files.get(path).toString("utf8");
    // Direct `window.codesense.<verb>(` or `codesense.<verb>(`.
    const direct = /\bcodesense(?:\.kit)?\.([a-zA-Z_][a-zA-Z0-9_]*)\s*\(/g;
    let m;
    while ((m = direct.exec(html)) !== null) {
      const verb = m[1];
      if (verb === "kit" || verb === "invoke") continue;
      if (!BRIDGE_VERB_ALLOWLIST.has(verb)) {
        errors.push(
          `${path}: pane calls window.codesense.${verb}(…) which is not in the ` +
            `bridge-verb allowlist (BRIDGE_VERB_ALLOWLIST in scripts/build-codesense-docs.mjs).`,
        );
      }
    }
    // `data-action='{"kind":"invoke","cmd":"<verb>",…}'` or JS-literal
    // `cmd: '<verb>'` / `cmd: "<verb>"` paired with `kind: 'invoke'`.
    const cmdRe = /["']?cmd["']?\s*:\s*["']([a-zA-Z_][a-zA-Z0-9_]*)["']/g;
    while ((m = cmdRe.exec(html)) !== null) {
      const verb = m[1];
      if (!BRIDGE_VERB_ALLOWLIST.has(verb)) {
        errors.push(
          `${path}: pane dispatches invoke verb ${JSON.stringify(verb)} which is not in ` +
            `the bridge-verb allowlist (BRIDGE_VERB_ALLOWLIST in scripts/build-codesense-docs.mjs).`,
        );
      }
    }
  }

  if (errors.length) {
    throw new Error(`build-codesense-docs: pane lint failed:\n  - ${errors.join("\n  - ")}`);
  }
}

export function lintBuiltinPaneExamples(files, root = ROOT) {
  for (const pane of BUILTIN_PANE_EXAMPLES) {
    const path = pane.example.replace(/^codesense-docs\//, "");
    if (!files.get(path)?.equals(exampleBytes(root, pane)))
      throw new Error(`Stale generated file: ${pane.example}; run pnpm gen:docs`);
  }
}

export function buildTree({ transformFiles } = {}) {
  /** @type {Map<string, Buffer>} */
  const files = new Map();
  const add = (path, data) => {
    if (files.has(path)) throw new Error(`Duplicate docs repo path ${path}`);
    files.set(path, Buffer.isBuffer(data) ? data : Buffer.from(data, "utf8"));
  };

  // With site/, the handbook comes straight from the MDX (a concurrent refresh may be
  // rewriting the snapshot); without it, the committed snapshot ships as is.
  const handbook = existsSync(HANDBOOK) ? convertHandbook() : null;
  const skip = handbook ? (full) => full === HANDBOOK_SNAPSHOT : undefined;
  for (const path of walk(TEMPLATE, skip)) {
    if (path === "bundle.json") continue;
    // SW-ODC-11: the sibling sample-source-repo/ ships as its own tarball
    // (`sample-source-repo.tar.gz`), not inside the docs bundle. Keep it out.
    if (path.startsWith("sample-source-repo/")) continue;
    add(path, readFileSync(join(TEMPLATE, path)));
  }
  for (const [path, text] of handbook ?? []) add(`${CONTENT}/${path}`, text);

  // Preference pass: bundle.readme always points at a file that actually
  // ships. If a `README.html` is present we keep both siblings (the archive
  // tolerates them); if only one is present, that's the one bundle.readme
  // resolves to.
  const resolvedReadme = resolveReadme(TEMPLATE, bundle.readme, (rel) => files.has(rel));
  if (resolvedReadme !== bundle.readme) {
    // The caller declared one but only the sibling exists — surface the
    // mismatch rather than ship with a broken launch pointer.
    throw new Error(
      `build-codesense-docs: bundle.readme is ${bundle.readme} but only ` +
        `${resolvedReadme} is present. Update codesense-docs/bundle.json.`,
    );
  }

  const createdAt = new Date(bundle.committedAt * 1000).toISOString().replace(".000Z", "Z");
  add(
    "org.yml",
    yaml([
      `slug: ${bundle.slug}`,
      `display_name: ${bundle.displayName}`,
      `created_at: ${createdAt}`,
    ]),
  );
  add("people.yml", yaml(["people: []"]));
  // SW-ODC-11: bind the sample source repo to the getting-started team so the shipped
  // dashboards can resolve `source_repo: acme/sample-source-repo` references in the
  // seeded DEMO epic. The clone is placed per-profile by `ensure_sample_repo`; this
  // registration is the team-side half, so it carries the clone origin's host
  // (codesense-docs/sample-source-repo/bundle.json `remoteUrl`): both must key to
  // `example.invalid/acme/sample-source-repo` for the clone to match the binding.
  add(
    "teams.yml",
    yaml([
      "teams:",
      `- name: ${TEAM}`,
      // The team's home page is its front page (the same README the very
      // first launch opens), relative to the team's content root.
      `  landing_page: ${JSON.stringify(landingPage(bundle.readme))}`,
      "  source_repos:",
      "    - owner: acme",
      "      repo: sample-source-repo",
      "      provider: git",
      "      host: example.invalid",
    ]),
  );

  /* SW-ODC-* generator hooks — Wave B/C stories append here.
   * Each generator writes to `${CONTENT}/generated/<name>.json` by the time
   * `src-tauri/build.rs` reaches the archive step; the walk at the top of
   * `buildTree` already picks those files up, so nothing beyond the hook
   * registration needs to run inside this function.
   *
   * Reserved slot order (keep stable; each story appends a line + comment):
   *   - SW-ODC-S  / SW-ODC-1  generated/search-index.json
   *   - SW-ODC-4              generated/mcp-tools.json
   *   - SW-ODC-5              generated/primitives.json
   *   - SW-ODC-6              generated/schemas.json
   *   - SW-ODC-24             generated/ui-mcp.json
   */

  transformFiles?.(files);
  lintPaneExamples(files);
  lintBuiltinPaneExamples(files);
  lintShippedHtml(files);
  lintShippedPanes(files);
  lintFactoryPlaybook(files);
  lintArcHSdlcContent(files);

  applyMetadata(files);
  return files;
}

// ---------------------------------------------------------------------------
// Git objects
// ---------------------------------------------------------------------------

function gitObject(objects, type, payload) {
  const body = Buffer.concat([Buffer.from(`${type} ${payload.length}\0`), payload]);
  const id = createHash("sha1").update(body).digest("hex");
  if (!objects.has(id)) objects.set(id, deflateSync(body));
  return id;
}

/** Writes the tree for `files` (path → bytes) and returns its id. */
function writeTree(objects, files) {
  const root = new Map();
  for (const [path, data] of files) {
    const parts = path.split("/");
    let node = root;
    for (const part of parts.slice(0, -1)) {
      if (!node.has(part)) node.set(part, new Map());
      node = node.get(part);
    }
    node.set(parts.at(-1), data);
  }
  const write = (node) => {
    const entries = [...node].map(([name, value]) =>
      value instanceof Map
        ? { name, key: `${name}/`, mode: "40000", id: write(value) }
        : { name, key: name, mode: "100644", id: gitObject(objects, "blob", value) },
    );
    // Git orders entries byte-wise, comparing a directory as if its name ended in "/".
    entries.sort((a, b) => Buffer.compare(Buffer.from(a.key), Buffer.from(b.key)));
    const payload = Buffer.concat(
      entries.flatMap((entry) => [
        Buffer.from(`${entry.mode} ${entry.name}\0`),
        Buffer.from(entry.id, "hex"),
      ]),
    );
    return gitObject(objects, "tree", payload);
  };
  return write(root);
}

export function buildRepository(files) {
  const objects = new Map();
  const tree = writeTree(objects, files);
  const signature = `${bundle.author} ${bundle.committedAt} +0000`;
  const commit = gitObject(
    objects,
    "commit",
    Buffer.from(
      `tree ${tree}\nauthor ${signature}\ncommitter ${signature}\n\nCodeSense docs\n`,
      "utf8",
    ),
  );
  const config = [
    "[core]",
    "\trepositoryformatversion = 0",
    "\tfilemode = true",
    "\tbare = false",
    "\tlogallrefupdates = true",
    "",
  ].join("\n");
  /** @type {Array<[string, Buffer]>} */
  const entries = [
    [".git/HEAD", Buffer.from(`ref: refs/heads/${bundle.branch}\n`)],
    [".git/config", Buffer.from(config)],
    [`.git/refs/heads/${bundle.branch}`, Buffer.from(`${commit}\n`)],
    ...[...objects]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([id, data]) => [`.git/objects/${id.slice(0, 2)}/${id.slice(2)}`, data]),
  ];
  return { commit, entries };
}

// ---------------------------------------------------------------------------
// Deterministic tar.gz
// ---------------------------------------------------------------------------

function tarHeader(name, size, mtime) {
  const header = Buffer.alloc(512);
  const put = (value, offset, length) => header.write(value, offset, length, "utf8");
  const octal = (value, length) => value.toString(8).padStart(length - 1, "0");
  if (Buffer.byteLength(name) > 100) throw new Error(`tar path too long: ${name}`);
  put(name, 0, 100);
  put(octal(0o644, 8), 100, 8);
  put(octal(0, 8), 108, 8);
  put(octal(0, 8), 116, 8);
  put(octal(size, 12), 124, 12);
  put(octal(mtime, 12), 136, 12);
  put("        ", 148, 8);
  put("0", 156, 1);
  put("ustar\0", 257, 6);
  put("00", 263, 2);
  let sum = 0;
  for (const byte of header) sum += byte;
  put(`${octal(sum, 7)}\0`, 148, 8);
  return header;
}

export function tarGz(entries, mtime) {
  const parts = [];
  for (const [name, data] of entries) {
    parts.push(tarHeader(name, data.length, mtime), data);
    const pad = (512 - (data.length % 512)) % 512;
    if (pad) parts.push(Buffer.alloc(pad));
  }
  parts.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(parts), { level: 9 });
}

/**
 * Every input the archive is built from, for `cargo:rerun-if-changed`. Directories are
 * listed only where everything in them ships (a new handbook page or example must rebuild
 * too); everything else is an exact file.
 */
export function buildInputs() {
  return [
    fileURLToPath(import.meta.url),
    join(ROOT, "scripts/lint-pane-examples.mjs"),
    join(ROOT, "scripts/generate-docs-metadata.mjs"),
    join(ROOT, "scripts/lib/builtin-panes.mjs"),
    join(ROOT, "scripts/lib/build-output.mjs"),
    join(ROOT, "scripts/generate-builtin-pane-examples.mjs"),
    ...BUILTIN_PANE_EXAMPLES.map(({ source }) => join(ROOT, source)),
    // All of it ships (bundle.json is the build's own config).
    TEMPLATE,
    ...(existsSync(HANDBOOK)
      ? [
          join(HANDBOOK, "index.mdx"),
          ...HANDBOOK_SECTIONS.map((section) => join(HANDBOOK, section)),
        ]
      : []),
  ];
}

function main() {
  const external = process.argv.includes("--allow-external-site");
  const args = process.argv.slice(2).filter((a) => a !== "--allow-external-site");
  if (args[0] === "--list-inputs") {
    for (const input of buildInputs()) console.log(input);
    return;
  }
  // A site outside this checkout is read only unless --allow-external-site says the write is the
  // snapshot refresh it is meant for (header): a stray CODESENSE_SITE_ROOT never rewrites the tree.
  if (args[0] !== "--check-handbook" && !external && !siteInside(ROOT, SITE)) {
    console.error(
      `codesense-docs: the site ${SITE} is outside this checkout; only --check-handbook and --list-inputs read it ` +
        "(the snapshot refresh passes --allow-external-site)",
    );
    process.exitCode = 2;
    return;
  }
  if (args[0] === "--handbook" || args[0] === "--check-handbook") {
    const check = args[0] === "--check-handbook";
    const drift = syncHandbookSnapshot({ check });
    if (drift === null) {
      console.log("codesense-docs: no site/ handbook here; the committed snapshot is the source");
    } else if (check && drift.length) {
      console.error(
        `codesense-docs: the handbook snapshot (${HANDBOOK_SNAPSHOT_LABEL}) is stale:\n` +
          `${drift.map((path) => `  ${path}`).join("\n")}\nRun pnpm gen:docs and commit the result.`,
      );
      process.exitCode = 1;
    } else {
      console.log(`codesense-docs: handbook snapshot ${drift.length ? "refreshed" : "up to date"}`);
    }
    return;
  }
  syncHandbookSnapshot();
  const output = resolve(args[0] ?? join(ROOT, "src-tauri", "resources", "codesense-docs.tar.gz"));
  const { commit, entries } = buildRepository(buildTree());
  const archive = tarGz(entries, bundle.committedAt);
  if (existsSync(output) && readFileSync(output).equals(archive)) {
    console.log(`codesense-docs: ${commit} unchanged (${output})`);
    return;
  }
  mkdirSync(dirname(output), { recursive: true });
  writeBuildOutput(output, archive, buildInputs());
  console.log(`codesense-docs: ${commit} → ${output} (${archive.length} bytes)`);
}

if (isMain(import.meta.url)) main();
