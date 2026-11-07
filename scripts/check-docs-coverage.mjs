#!/usr/bin/env node
// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync, spawnSync } from "node:child_process";
import ts from "typescript";
import { buildTree } from "./build-codesense-docs.mjs";
import { formatHit, loadConfig, scan, scanText } from "./check-docs-terms.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const PREFIX = "getting-started/content/";
const ALLOWLIST = "scripts/docs-coverage-allowlist.json";
const ARC = "arc-i-dashboard-primitives/";
const identity = (entry) =>
  JSON.stringify([entry.kind, entry.name, entry.context ?? "", entry.page]);
const sorted = (entries) => entries.sort((a, b) => identity(a).localeCompare(identity(b)));
const label = (entry) => `${entry.kind} ${entry.name}${entry.context ? ` (${entry.context})` : ""}`;

function visibleText(text, markdown) {
  const code = [];
  const protectCode = (content) => {
    const index = code.push(content) - 1;
    return `\uE000${index}\uE001`;
  };
  if (markdown) {
    text = text.replace(
      /(^|\n)( {0,3})(`{3,}|~{3,})[^\n]*\n([\s\S]*?)\n {0,3}\3[ \t]*(?=\n|$)/g,
      (_match, line, _indent, _fence, content) => `${line}${protectCode(content)}`,
    );
    text = text.replace(/(`+)([^\n]*?)\1/g, (_match, _ticks, content) => protectCode(content));
  }
  text = text
    .replace(/<!--\s*model-fields:start\s*-->[\s\S]*?<!--\s*model-fields:end\s*-->/gi, " ")
    .replace(/<!--[\s\S]*?-->/g, " ")
    .replace(/<(script|style)\b[^>]*>[\s\S]*?<\/\1\s*>/gi, " ")
    .replace(/<[^>]*>/g, " ");
  if (markdown)
    text = text
      .replace(/^\s{0,3}\[[^\]]+\]:[^\n]*$/gm, " ")
      .replace(/(!?\[[^\]]*\])\((?:[^()\n]|\([^()\n]*\))*\)/g, "$1");
  const entities = { lt: "<", gt: ">", amp: "&", quot: '"', apos: "'", nbsp: " " };
  text = text.replace(/&(#x[\da-f]+|#\d+|lt|gt|amp|quot|apos|nbsp);/gi, (match, value) => {
    if (!value.startsWith("#")) return entities[value.toLowerCase()] ?? match;
    const code =
      value[1].toLowerCase() === "x" ? parseInt(value.slice(2), 16) : Number(value.slice(1));
    return code <= 0x10ffff ? String.fromCodePoint(code) : match;
  });
  return text.replace(/\uE000(\d+)\uE001/g, (_match, index) => code[Number(index)]);
}

/** Only authored teaching content from the actual shipped tree can cover names. */
export function authoredPages(files) {
  return [...files]
    .filter(
      ([path]) =>
        path.startsWith(PREFIX) &&
        /\.(html|md|mdx)$/.test(path) &&
        !path
          .slice(PREFIX.length)
          .split("/")
          .some((part) =>
            ["generated", "_shared", "examples", "diagrams", ".claude"].includes(part),
          ),
    )
    .map(([path, bytes]) => ({ path, text: visibleText(bytes.toString(), /\.mdx?$/.test(path)) }));
}

function callable(signature) {
  const source = ts.createSourceFile(
    "member.ts",
    `interface Member { ${signature} }`,
    ts.ScriptTarget.Latest,
    true,
  );
  const member = source.statements[0]?.members?.[0];
  const functionType = (node) =>
    node &&
    (ts.isFunctionTypeNode(node) ||
      (ts.isParenthesizedTypeNode(node) && functionType(node.type)) ||
      (ts.isUnionTypeNode(node) && node.types.some(functionType)));
  return (
    member &&
    (ts.isMethodSignature(member) || (ts.isPropertySignature(member) && functionType(member.type)))
  );
}

export function collectCoverage(files) {
  const read = (name) => {
    const path = `${PREFIX}generated/${name}.json`;
    if (!files.has(path))
      throw new Error(`Missing inventory: codesense-docs/${path}; run pnpm gen:docs`);
    return JSON.parse(files.get(path).toString());
  };
  const inventory = [];
  const add = (kind, name, source, page, context) =>
    inventory.push({
      kind,
      name,
      source: `generated/${source}.json`,
      page,
      ...(context ? { context } : {}),
    });
  const mcp = read("mcp-tools");
  for (const tool of mcp.tools) add("mcp-tool", tool.name, "mcp-tools", tool.docs);
  const sdk = read("page-sdk");
  for (const name of new Set(
    sdk.sdk.filter((member) => callable(member.signature)).map((member) => member.name),
  ))
    add("sdk-method", name, "page-sdk", ARC + "calling-codesense.html");
  for (const verb of sdk.verbs) add("verb", verb.name, "page-sdk", ARC + "calling-codesense.html");
  for (const cap of read("capabilities").capabilities)
    add("capability", cap.name, "capabilities", ARC + "pane-shapes.html");
  for (const slot of read("slots").slots) add("slot", slot.id, "slots", ARC + "pane-shapes.html");
  const hub = read("live-hub");
  for (const topic of [...hub.topics, ...hub.families])
    add("live-hub", topic.name, "live-hub", ARC + "reacting-to-state.html");
  const pane = read("pane-declaration.schema");
  for (const [context, model] of [
    ["PaneManifest", pane],
    ["PaneManifestEntry", pane.$defs.PaneManifestEntry],
    ["DashboardCommand", pane.$defs.DashboardCommand],
  ])
    for (const key of Object.keys(model.properties))
      add(
        "declaration-key",
        key,
        "pane-declaration.schema",
        ARC + (context === "DashboardCommand" ? "commands-and-menus.html" : "pane-shapes.html"),
        context,
      );
  const yamlPages = {
    "agent-yaml": "arc-e-agents/agent-yaml.html",
    "routine-yml": "arc-g-routines/routine-yml.html",
    "role-yaml": "arc-f-inboxes-and-triage/roles.html",
  };
  for (const schema of read("schemas").schemas) {
    if (!yamlPages[schema.slug]) continue;
    for (const field of schema.fields)
      if (!/(\[\]|\.\*)$/.test(field.name))
        add(schema.slug, field.name, "schemas", yamlPages[schema.slug]);
  }
  const pages = authoredPages(files);
  const gaps = mcp.coverage.documentationGaps.map(({ tool, docs }) => ({
    kind: "mcp-tool",
    name: tool,
    page: docs,
    source: "generated/mcp-tools.json",
  }));
  for (const item of inventory) {
    if (item.kind === "mcp-tool") continue;
    const escaped = item.name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const mention = new RegExp(`(?<![\\w$])${escaped}(?![\\w$])`);
    if (!pages.some(({ text }) => mention.test(text))) gaps.push(item);
  }
  return { inventory: sorted(inventory), gaps: sorted(gaps), authoredPageCount: pages.length };
}

/** Who owns an exception: a tracker id such as DOC-14, or an issue URL. */
const OWNER = /^(?:[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)*-\d+|https:\/\/\S+)$/;

/** Compare identities, never counts: a removed exception cannot fund a new one. */
export function evaluateAllowlist(gaps, allowlist, baseline) {
  const diagnostics = [];
  const entries = allowlist.entries;
  if (allowlist.version !== 1 || !Array.isArray(entries))
    throw new Error(`${ALLOWLIST}: expected version 1 and entries array`);
  const known = new Set(baseline.map(identity));
  const allowed = new Map();
  for (const entry of entries) {
    const key = identity(entry);
    if (
      !["kind", "name", "page", "reason"].every(
        (field) => typeof entry[field] === "string" && entry[field].trim(),
      ) ||
      !OWNER.test(entry.story ?? "")
    )
      diagnostics.push(
        `INVALID EXCEPTION ${label(entry)} — require kind, name, page, nonempty reason and an owning story id (AREA-<number>) or issue URL`,
      );
    if (allowed.has(key))
      diagnostics.push(`DUPLICATE EXCEPTION ${label(entry)} — remove duplicate ${ALLOWLIST} entry`);
    allowed.set(key, entry);
    if (!known.has(key))
      diagnostics.push(
        `NEW EXCEPTION ${label(entry)} — document the capability; ${ALLOWLIST} may only shrink`,
      );
  }
  const unresolved = new Set(gaps.map(identity));
  let resolved = 0;
  for (const entry of entries) {
    if (unresolved.has(identity(entry))) continue;
    resolved++;
    diagnostics.push(`RESOLVED ${label(entry)} — remove ${ALLOWLIST} entry`);
  }
  const retainedByStory = {};
  let newGaps = 0;
  for (const gap of gaps) {
    const entry = allowed.get(identity(gap));
    if (entry) retainedByStory[entry.story] = (retainedByStory[entry.story] ?? 0) + 1;
    else {
      newGaps++;
      diagnostics.push(
        `UNDOCUMENTED ${label(gap)} — update codesense-docs/${PREFIX}${gap.page} (inventory: ${gap.source})`,
      );
    }
  }
  return {
    ok: diagnostics.length === 0,
    diagnostics: diagnostics.sort(),
    newGaps,
    resolved,
    retainedByStory,
  };
}

export function readBaseline(root = ROOT) {
  const git = (...args) =>
    execFileSync("git", args, {
      cwd: root,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
  const revision = git("merge-base", "HEAD", "origin/main");
  if (!git("ls-tree", "--name-only", revision, "--", ALLOWLIST))
    throw new Error(`${ALLOWLIST} must be on origin/main before running docs:coverage`);
  const data = JSON.parse(git("show", `${revision}:${ALLOWLIST}`));
  if (data.version !== 1 || !Array.isArray(data.entries))
    throw new Error(`Invalid merge-base ${ALLOWLIST}`);
  return { revision, entries: data.entries };
}

/** Dependency arguments keep composition testable; the CLI always runs every check. */
export function runCoverage({
  root = ROOT,
  build = buildTree,
  baseline = readBaseline,
  run = (script) => spawnSync("pnpm", ["run", script], { cwd: root, stdio: "inherit" }),
  log = console.log,
} = {}) {
  const started = performance.now();
  const diagnostics = [];
  let failedChecks = 0;
  const check = (name, fn) => {
    log(`CHECK ${name}`);
    try {
      return fn();
    } catch (error) {
      failedChecks++;
      diagnostics.push(`FAILED ${name}: ${error.message}`);
    }
  };
  const scripts = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).scripts;
  const genChecks = Object.keys(scripts)
    .filter((name) => /^gen:.*:check$/.test(name))
    .map((name) =>
      name === "gen:docs:check" && scripts["gen:docs:check:pr"] ? "gen:docs:check:pr" : name,
    );
  for (const script of genChecks)
    check(script, () => {
      const result = run(script);
      if (result.error) throw result.error;
      if (result.status !== 0)
        throw new Error(
          `exit ${result.status ?? result.signal ?? "unknown"}; see generator diagnostics above`,
        );
    });
  let files;
  check("shipped tree and example liveness", () => {
    const result = build({
      transformFiles: (tree) => {
        files = tree;
      },
    });
    files = result;
  });
  const config = check("retired terms configuration", () =>
    loadConfig(join(root, "scripts/retired-terms.json")),
  );
  if (config) {
    check("source retired terms", () => {
      const hits = scan({ base: root, config });
      if (hits.length) throw new Error(hits.map(formatHit).join("\n"));
    });
    if (files)
      check("shipped retired terms", () => {
        const hits = [...files].flatMap(([path, bytes]) =>
          scanText({ file: `codesense-docs/${path}`, text: bytes.toString(), config }),
        );
        if (hits.length) throw new Error(hits.map(formatHit).join("\n"));
      });
  }
  let coverage, report;
  if (files) coverage = check("public name coverage", () => collectCoverage(files));
  const base = check("allowlist Git baseline", () => baseline(root));
  if (base) log(`RATCHET merge-base ${base.revision}`);
  if (coverage && base)
    report = check("coverage allowlist", () =>
      evaluateAllowlist(
        coverage.gaps,
        JSON.parse(readFileSync(join(root, ALLOWLIST), "utf8")),
        base.entries,
      ),
    );
  diagnostics.push(...(report?.diagnostics ?? []));
  for (const diagnostic of diagnostics) log(diagnostic);
  const elapsedSeconds = (performance.now() - started) / 1000;
  const owners =
    Object.entries(report?.retainedByStory ?? {})
      .sort()
      .map(([story, count]) => `${story}=${count}`)
      .join(", ") || "none";
  const ok = failedChecks === 0 && report?.ok === true;
  log(
    `docs:coverage: ${ok ? "ok" : "FAILED"}; ${genChecks.length} generator checks; ${coverage?.inventory.length ?? 0} public names; ${coverage?.authoredPageCount ?? 0} authored pages; ${failedChecks} failing checks; ${report?.newGaps ?? 0} new gaps; retained gaps ${owners}; ${report?.resolved ?? 0} resolved entries; ${elapsedSeconds.toFixed(2)}s`,
  );
  return { ok, diagnostics, coverage, report, failedChecks, elapsedSeconds };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 2) throw new Error("Usage: pnpm docs:coverage (no arguments)");
    process.exitCode = runCoverage().ok ? 0 : 1;
  } catch (error) {
    console.error(`docs:coverage: ${error.message}`);
    process.exitCode = 1;
  }
}
