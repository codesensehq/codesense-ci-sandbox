#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// SW-ODC-24: UI ↔ MCP cross-reference generator (auditor for invariant I-2:
// every UI action has an MCP tool).
//
// Reads source-of-truth code — Tauri command registrations in
// `src-tauri/src/lib.rs` + the TS wrappers in `src/tauri/commands.ts`, and
// MCP tool registrations (`#[tool(...)]` + the trailing fn ident) in
// `src-tauri/src/mcp/handler.rs` — and emits:
//
//   codesense-docs/getting-started/content/generated/ui-mcp.json
//
// The dashboard `ui-mcp-crossref.html` fetches this at runtime and renders
// three filterable views: paired / UI-only / MCP-only.
//
// Pairing strategy, applied in priority order:
//   1. Exact name match (Tauri command ident === MCP tool fn ident).
//   2. Explicit map in `_shared/ui-mcp-map.yml` — hand-maintained pairs
//      the auto-match can't find (e.g. the Tauri name has a `_for_wave`
//      suffix the MCP mirror drops).
//
// Allowlist (`_shared/allow-unpaired.yml`): intentional one-sided entries —
// UI-only lifecycle/menu commands (`save_app_state`, `rebuild_menu`, …) and
// MCP-only meta-tools (`codesense_overview`, `codesense_search_tools`, …).
// Each has a rationale that renders alongside the entry in the dashboard.
//
// Modes:
//   write (default) — regenerate `generated/ui-mcp.json`.
//   --check         — exit non-zero if the committed JSON differs from a
//                     fresh render. Error names (i) the drifted entity,
//                     (ii) the file(s) to edit, (iii) `npm run gen:docs`.
//
// Shape:
//   {
//     "schema": 1,
//     "generatedAt": "deterministic",
//     "counts": { "paired": N, "uiOnly": N, "mcpOnly": N,
//                 "uiOnlyAcknowledged": N, "mcpOnlyAcknowledged": N },
//     "paired": [
//       { "name": "<pair-key>", "ui": {name, path, file, line},
//         "mcp": {name, file, line}, "via": "exact" | "map" }
//     ],
//     "uiOnly": [
//       { "name": "...", "path": "...", "file": "...", "line": N,
//         "acknowledged": true|false, "rationale": "..." | null }
//     ],
//     "mcpOnly": [
//       { "name": "...", "file": "...", "line": N,
//         "acknowledged": true|false, "rationale": "..." | null }
//     ]
//   }

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { listInputs } from "./lib/generator-inputs.mjs";
import { allowStaleExport } from "./lib/sealed-export.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const LIB_RS = join(ROOT, "src-tauri", "src", "lib.rs");
const HANDLER_RS = join(ROOT, "src-tauri", "src", "mcp", "handler.rs");
const COMMANDS_TS = join(ROOT, "src", "tauri", "commands.ts");
const CONTENT = join(ROOT, "codesense-docs", "getting-started", "content");
const SHARED = join(CONTENT, "_shared");
const MAP_PATH = join(SHARED, "ui-mcp-map.yml");
const ALLOW_PATH = join(SHARED, "allow-unpaired.yml");
const OUTPUT = join(CONTENT, "generated", "ui-mcp.json");

// Relative paths used in the manifest — the dashboard surfaces these as
// "source" links the reader can grep for. Repo-relative, POSIX separators.
const LIB_RS_REL = "src-tauri/src/lib.rs";
const HANDLER_RS_REL = "src-tauri/src/mcp/handler.rs";
const COMMANDS_TS_REL = "src/tauri/commands.ts";

// ---------------------------------------------------------------------------
// Source parsers
// ---------------------------------------------------------------------------

/**
 * Walk `src-tauri/src/lib.rs` and pull every path registered inside the
 * `.invoke_handler(tauri::generate_handler![ ... ])` block. Returns one row
 * per command: `{ name, path, line }`.
 *
 * Deliberately a tiny line-oriented scanner — the block is hand-maintained
 * and single-use, so a syntax-tree parser would be overkill. The scanner
 * strips `// …` trailing comments and tolerates `#[cfg(...)]` attribute
 * lines so platform-gated commands (e.g. macOS-only `export_pdf::…`) still
 * land in the list.
 */
export function parseTauriCommands(text) {
  const lines = text.split("\n");
  const commands = [];
  let inBlock = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!inBlock) {
      // `counted(...)` is the invoke-rate wrapper (shared::invoke_stats); its `]))` also ends the block.
      if (/invoke_handler\((?:[\w:]+\()?tauri::generate_handler!\[/.test(line)) inBlock = true;
      continue;
    }
    if (/^\s*\]\)/.test(line)) break;
    const stripped = line.replace(/\/\/.*$/, "").trim();
    if (!stripped) continue;
    // Skip attributes; the command on the next line is captured on its own iteration.
    if (/^#\[/.test(stripped)) continue;
    const m = /^([a-z_][a-zA-Z0-9_:]*)\s*,?\s*$/.exec(stripped);
    if (!m) continue;
    const path = m[1];
    const name = path.split("::").pop();
    commands.push({ name, path, file: LIB_RS_REL, line: i + 1 });
  }
  return commands;
}

/**
 * Walk `src-tauri/src/mcp/handler.rs` and pair every `#[tool(...)]`
 * attribute (possibly spanning multiple lines) with the following
 * `fn <name>(…)` ident. The fn name is the tool name in the shipped
 * MCP schema — `rmcp`'s `#[tool]` macro uses the fn ident unless a
 * `name = "..."` is passed, and we assert via a greppable sweep that
 * nobody does the latter so a future rename shows up as a review signal.
 */
export function parseMcpTools(text) {
  const lines = text.split("\n");
  const tools = [];
  for (let i = 0; i < lines.length; i++) {
    if (!/^    #\[tool\b/.test(lines[i])) continue;
    for (let j = i + 1; j < Math.min(i + 60, lines.length); j++) {
      const m = /^\s*(?:pub\s+)?(?:async\s+)?fn\s+([a-z_][a-z0-9_]*)\s*\(/.exec(lines[j]);
      if (m) {
        tools.push({ name: m[1], file: HANDLER_RS_REL, line: j + 1 });
        break;
      }
    }
  }
  return tools;
}

/**
 * Walk `src/tauri/commands.ts` and record every `invoke<T>("<name>", …)`
 * call site. Not every Tauri command has a TS wrapper (some are called
 * via Rust-side bridges or direct `invoke(...)` elsewhere in the FE), so
 * this is a best-effort annotation attached to the Tauri command row.
 */
export function parseTauriWrappers(text) {
  const lines = text.split("\n");
  const wrappers = new Map();
  for (let i = 0; i < lines.length; i++) {
    const m = /\binvoke(?:<[^>]+>)?\s*\(\s*["']([a-z_][a-z0-9_]*)["']/.exec(lines[i]);
    if (!m) continue;
    const name = m[1];
    if (!wrappers.has(name)) wrappers.set(name, i + 1);
  }
  return wrappers;
}

// ---------------------------------------------------------------------------
// Tiny YAML loader — the two shipped files use a fixed shape we parse by hand.
// Keeps the generator dependency-free, same as every other `scripts/generate-*.mjs`.
// ---------------------------------------------------------------------------

/**
 * Load a config with the shape `top_level: - name: foo\n    rationale: "..."`.
 * Returns `{ ui_only: Map<name, rationale>, mcp_only: Map<name, rationale> }`.
 * Unknown top-level keys are ignored so one file can host both the map and
 * future extensions without the parser growing a schema.
 */
export function parseYamlPairingFile(text) {
  const out = { ui_only: new Map(), mcp_only: new Map() };
  if (!text) return out;
  const lines = text.split("\n");
  let section = null;
  let currentName = null;
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    const line = raw.replace(/\s+$/, "");
    if (!line || /^\s*#/.test(line)) continue;
    // Top-level section header.
    let m = /^([a-z_][a-z0-9_]*):\s*$/.exec(line);
    if (m) {
      section = m[1];
      currentName = null;
      continue;
    }
    if (section !== "ui_only" && section !== "mcp_only") continue;
    // List entry start: `  - name: foo`.
    m = /^\s*-\s*name:\s*(.+)$/.exec(line);
    if (m) {
      currentName = stripScalar(m[1]);
      out[section].set(currentName, null);
      continue;
    }
    // `rationale:` with either a trailing scalar or a folded-block indicator.
    m = /^(\s*)rationale:\s*(.+)$/.exec(line);
    if (m && currentName) {
      const marker = m[2].trim();
      if (marker === ">-" || marker === ">" || marker === "|-" || marker === "|") {
        // Folded block scalar — join indented continuation lines on a space.
        const baseIndent = m[1].length;
        const parts = [];
        for (let j = i + 1; j < lines.length; j++) {
          const next = lines[j];
          if (!next.trim()) continue;
          const indent = next.match(/^\s*/)[0].length;
          if (indent <= baseIndent) break;
          parts.push(next.trim());
          i = j;
        }
        out[section].set(currentName, parts.join(" "));
        continue;
      }
      out[section].set(currentName, stripScalar(marker));
    }
  }
  return out;
}

/** Shape of the `ui-mcp-map.yml`: pairs: - ui: foo\n    mcp: bar\n    note?: ... */
export function parseYamlMapFile(text) {
  const pairs = [];
  if (!text) return pairs;
  let section = null;
  let current = null;
  for (const raw of text.split("\n")) {
    const line = raw.replace(/\s+$/, "");
    if (!line || /^\s*#/.test(line)) continue;
    let m = /^([a-z_][a-z0-9_]*):\s*$/.exec(line);
    if (m) {
      section = m[1];
      current = null;
      continue;
    }
    if (section !== "pairs") continue;
    m = /^\s*-\s*ui:\s*(.+)$/.exec(line);
    if (m) {
      if (current) pairs.push(current);
      current = { ui: stripScalar(m[1]), mcp: null, note: null };
      continue;
    }
    if (!current) continue;
    m = /^\s*mcp:\s*(.+)$/.exec(line);
    if (m) current.mcp = stripScalar(m[1]);
    m = /^\s*note:\s*(.+)$/.exec(line);
    if (m) current.note = stripScalar(m[1]);
  }
  if (current) pairs.push(current);
  // Drop incomplete rows — pairing needs both sides.
  return pairs.filter((p) => p.ui && p.mcp);
}

function stripScalar(raw) {
  let value = raw.trim();
  // Strip a trailing `#` comment on an unquoted scalar.
  if (!value.startsWith("'") && !value.startsWith('"')) {
    const hash = value.indexOf(" #");
    if (hash >= 0) value = value.slice(0, hash).trim();
  }
  if (value.startsWith('"') && value.endsWith('"')) value = value.slice(1, -1);
  else if (value.startsWith("'") && value.endsWith("'")) value = value.slice(1, -1);
  return value;
}

// ---------------------------------------------------------------------------
// Build the manifest
// ---------------------------------------------------------------------------

export function buildCrossref({ libRs, handlerRs, commandsTs, mapYaml, allowYaml }) {
  const tauriCmds = parseTauriCommands(libRs);
  const mcpTools = parseMcpTools(handlerRs);
  const wrappers = parseTauriWrappers(commandsTs);
  const explicitPairs = parseYamlMapFile(mapYaml);
  const allow = parseYamlPairingFile(allowYaml);

  // De-dup by name — the invoke_handler block has no duplicates today, but
  // keep the generator robust against a future cfg-arm variant that lands
  // the same ident twice (we'd keep the first; drift surfaces as a review
  // signal via the committed JSON).
  const tauriByName = new Map();
  for (const cmd of tauriCmds) {
    if (!tauriByName.has(cmd.name)) tauriByName.set(cmd.name, cmd);
  }
  const mcpByName = new Map();
  for (const tool of mcpTools) {
    if (!mcpByName.has(tool.name)) mcpByName.set(tool.name, tool);
  }

  // Pairing. Explicit map wins over exact-name match so a story can retire a
  // name collision without losing visibility.
  const paired = [];
  const consumedTauri = new Set();
  const consumedMcp = new Set();

  // 1. Explicit map first — fail loudly on a mapping that doesn't resolve.
  const explicitWarnings = [];
  for (const pair of explicitPairs) {
    const ui = tauriByName.get(pair.ui);
    const mcp = mcpByName.get(pair.mcp);
    if (!ui || !mcp) {
      explicitWarnings.push(
        `ui-mcp-map.yml: pair { ui: ${pair.ui}, mcp: ${pair.mcp} } ` +
          `does not resolve (ui=${!!ui}, mcp=${!!mcp}).`,
      );
      continue;
    }
    if (consumedTauri.has(ui.name) || consumedMcp.has(mcp.name)) continue;
    consumedTauri.add(ui.name);
    consumedMcp.add(mcp.name);
    paired.push({
      name: pair.ui, // stable key for `#pair/<name>` deep-links
      ui: wrapperAnnotated(ui, wrappers),
      mcp: { name: mcp.name, file: mcp.file, line: mcp.line },
      via: "map",
      note: pair.note || null,
    });
  }

  // 2. Exact-name match.
  for (const cmd of tauriCmds) {
    if (consumedTauri.has(cmd.name)) continue;
    const mcp = mcpByName.get(cmd.name);
    if (!mcp || consumedMcp.has(mcp.name)) continue;
    consumedTauri.add(cmd.name);
    consumedMcp.add(mcp.name);
    paired.push({
      name: cmd.name,
      ui: wrapperAnnotated(cmd, wrappers),
      mcp: { name: mcp.name, file: mcp.file, line: mcp.line },
      via: "exact",
      note: null,
    });
  }

  // 3. Residuals.
  const uiOnly = tauriCmds
    .filter((c) => !consumedTauri.has(c.name))
    .map((c) => {
      const rationale = allow.ui_only.get(c.name) ?? null;
      return {
        ...wrapperAnnotated(c, wrappers),
        acknowledged: allow.ui_only.has(c.name),
        rationale,
      };
    });
  const mcpOnly = mcpTools
    .filter((t) => !consumedMcp.has(t.name))
    .map((t) => {
      const rationale = allow.mcp_only.get(t.name) ?? null;
      return {
        name: t.name,
        file: t.file,
        line: t.line,
        acknowledged: allow.mcp_only.has(t.name),
        rationale,
      };
    });

  // Stable order so committed JSON diffs are small.
  paired.sort((a, b) => a.name.localeCompare(b.name));
  uiOnly.sort((a, b) => a.name.localeCompare(b.name));
  mcpOnly.sort((a, b) => a.name.localeCompare(b.name));

  const doc = {
    schema: 1,
    generatedAt: "deterministic",
    counts: {
      paired: paired.length,
      uiOnly: uiOnly.length,
      mcpOnly: mcpOnly.length,
      uiOnlyAcknowledged: uiOnly.filter((r) => r.acknowledged).length,
      mcpOnlyAcknowledged: mcpOnly.filter((r) => r.acknowledged).length,
    },
    paired,
    uiOnly,
    mcpOnly,
  };

  return { doc, warnings: explicitWarnings };
}

function wrapperAnnotated(cmd, wrappers) {
  const row = { name: cmd.name, path: cmd.path, file: cmd.file, line: cmd.line };
  if (wrappers.has(cmd.name)) {
    row.wrapperFile = COMMANDS_TS_REL;
    row.wrapperLine = wrappers.get(cmd.name);
  }
  return row;
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

function serialize(doc) {
  return `${JSON.stringify(doc, null, 2)}\n`;
}

function readOrEmpty(path) {
  return existsSync(path) ? readFileSync(path, "utf8") : "";
}

// Line numbers shift whenever an unrelated edit lands in lib.rs/handler.rs/commands.ts; they
// are the only thing the PR gate forgives (names, paths, files and pairings still compare).
const withoutLines = (value) =>
  JSON.stringify(value, (key, v) => (key === "line" || key === "wrapperLine" ? undefined : v));

/** "ok" | "notice" (line-only drift tolerated by the PR gate) | "stale". Strict is exact. */
export function crossrefVerdict(existingText, fresh, allowLineDrift) {
  if (existingText === serialize(fresh)) return "ok";
  if (!allowLineDrift) return "stale";
  try {
    return withoutLines(JSON.parse(existingText)) === withoutLines(fresh) ? "notice" : "stale";
  } catch {
    return "stale";
  }
}

export function buildInputs() {
  return listInputs(import.meta.url, [LIB_RS, HANDLER_RS, COMMANDS_TS, MAP_PATH, ALLOW_PATH]);
}

function run() {
  if (process.argv.includes("--list-inputs")) {
    for (const input of buildInputs()) console.log(input);
    return;
  }
  const check = process.argv.includes("--check");
  const { doc, warnings } = buildCrossref({
    libRs: readFileSync(LIB_RS, "utf8"),
    handlerRs: readFileSync(HANDLER_RS, "utf8"),
    commandsTs: readFileSync(COMMANDS_TS, "utf8"),
    mapYaml: readOrEmpty(MAP_PATH),
    allowYaml: readOrEmpty(ALLOW_PATH),
  });
  for (const w of warnings) console.warn(`generate-ui-mcp-crossref: ${w}`);

  const fresh = serialize(doc);
  if (check) {
    const existing = existsSync(OUTPUT) ? readFileSync(OUTPUT, "utf8") : "";
    const verdict = crossrefVerdict(existing, doc, allowStaleExport());
    if (verdict === "notice") {
      console.warn(
        "NOTICE: committed ui-mcp.json differs from a fresh render only in line numbers; regenerate during the next docs wave with pnpm gen:docs",
      );
    } else if (verdict === "stale") {
      // Pinpoint the drifted entity so the error message is actionable.
      // We walk paired / uiOnly / mcpOnly in order and report the first
      // name that changed, is missing, or appeared.
      const culprit = findDriftCulprit(existing, doc);
      const files = describeFilesForCulprit(culprit);
      console.error(
        [
          "generate-ui-mcp-crossref: committed ui-mcp.json is stale.",
          `  entity:    ${culprit.label}`,
          `  files:     ${files}`,
          `  generator: scripts/generate-ui-mcp-crossref.mjs`,
          `  fix:       npm run gen:docs`,
        ].join("\n"),
      );
      process.exit(1);
    }
    console.log("generate-ui-mcp-crossref: ok");
    return;
  }
  mkdirSync(dirname(OUTPUT), { recursive: true });
  if (!existsSync(OUTPUT) || readFileSync(OUTPUT, "utf8") !== fresh) writeFileSync(OUTPUT, fresh);
  console.log(`generate-ui-mcp-crossref: wrote ${OUTPUT}`);
}

/**
 * Compare committed JSON against the fresh doc section by section and
 * return a `{ label, kind }` identifying the first entry that drifted.
 * `kind` is `paired` | `uiOnly` | `mcpOnly` so the error message can
 * point at the right source file.
 */
function findDriftCulprit(existingText, fresh) {
  let existing;
  try {
    existing = existingText ? JSON.parse(existingText) : {};
  } catch {
    return { label: "ui-mcp.json (unparseable; regenerate)", kind: "paired" };
  }
  for (const kind of ["paired", "uiOnly", "mcpOnly"]) {
    const have = new Map((existing[kind] || []).map((r) => [r.name, r]));
    for (const row of fresh[kind] || []) {
      const prev = have.get(row.name);
      if (!prev) return { label: `${row.name} (new ${friendlyKind(kind)})`, kind };
      if (JSON.stringify(prev) !== JSON.stringify(row)) {
        return { label: `${row.name} (${friendlyKind(kind)} changed)`, kind };
      }
    }
    for (const prev of existing[kind] || []) {
      if (!(fresh[kind] || []).some((r) => r.name === prev.name)) {
        return { label: `${prev.name} (removed from ${friendlyKind(kind)})`, kind };
      }
    }
  }
  return { label: "ui-mcp.json (counts drifted)", kind: "paired" };
}

function friendlyKind(kind) {
  if (kind === "paired") return "paired entry";
  if (kind === "uiOnly") return "UI-only command";
  return "MCP-only tool";
}

function describeFilesForCulprit(culprit) {
  switch (culprit.kind) {
    case "paired":
      return `${LIB_RS_REL}, ${HANDLER_RS_REL}, ${COMMANDS_TS_REL}, _shared/ui-mcp-map.yml`;
    case "uiOnly":
      return (
        `${LIB_RS_REL}, _shared/allow-unpaired.yml (add rationale), ` +
        `_shared/ui-mcp-map.yml (if a mirror MCP tool now exists)`
      );
    case "mcpOnly":
      return (
        `${HANDLER_RS_REL}, _shared/allow-unpaired.yml (add rationale), ` +
        `_shared/ui-mcp-map.yml (if a mirror UI command now exists)`
      );
    default:
      return `${LIB_RS_REL}, ${HANDLER_RS_REL}`;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) run();
