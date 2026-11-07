#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Walks `codesense-docs/getting-started/content/examples/**` and emits
// `codesense-docs/getting-started/content/generated/recipes.json`: the
// build-time manifest the recipes-gallery dashboard fetches at runtime
// (SW-ODC-7). One entry per shipped recipe (agent / routine / dashboard /
// skill) plus a small set of scaffold cards for plausible-but-not-yet-shipped
// routines, so the gallery is honest about what works today.
//
// Shape:
//
//   {
//     "schema": 1,
//     "generatedAt": "deterministic",
//     "recipes": [
//       {
//         "id": "agent-codesense-setup",
//         "kind": "agent" | "routine" | "dashboard" | "skill",
//         "name": "codesense-setup",
//         "title": "codesense-setup",
//         "summary": "…",
//         "path": "examples/agents/codesense-setup/agent.yaml",
//         "lang": "yaml" | "html" | "markdown",
//         "role": "setup" | "builder" | …,
//         "surface": ["local" | "remote"],
//         "useCase": ["setup" | "build" | "review" | "coordinate" | "ship"],
//         "ships": true | false,
//         "trigger": "cron" | "inbox_idle" | "webhook" | null,
//         "source": "…"  // verbatim file contents (bytes-identical)
//       }
//     ]
//   }
//
// The dashboard uses `source` to populate its detail sheet without a second
// fetch, so new recipes land by dropping files and re-running `pnpm run gen:docs`.
//
// Modes:
//   write (default) — regenerate `generated/recipes.json` from `examples/`.
//   --check          — exit non-zero if the committed JSON differs from a fresh
//                      render. Error message names (i) the drifted entity,
//                      (ii) the file to edit, (iii) `npm run gen:docs`.

import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { listInputs } from "./lib/generator-inputs.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CONTENT = join(ROOT, "codesense-docs", "getting-started", "content");
const EXAMPLES = join(CONTENT, "examples");
const PANES = join(ROOT, "codesense-docs", "getting-started", "panes");
const OUTPUT = join(CONTENT, "generated", "recipes.json");

// ---------------------------------------------------------------------------
// Classification tables
// Hand-authored because the files themselves don't carry use-case / role /
// surface metadata today. Keyed by recipe id — if a file lands on disk and
// the generator finds no entry here, it falls back to safe defaults (role
// null, surface ["local"], useCase ["build"]) and prints a warning so a
// developer notices and adds the right row.
// ---------------------------------------------------------------------------

/** @type {Record<string, { role?: string | null, surface: string[], useCase: string[], ships: boolean, trigger?: string | null }>} */
const META = {
  // ---- agents ----
  "agent-codesense-setup": {
    role: "setup",
    surface: ["local"],
    useCase: ["setup"],
    ships: true,
  },
  "agent-machine-setup": {
    role: "setup",
    surface: ["local", "remote"],
    useCase: ["setup"],
    ships: true,
  },
  "agent-manager": {
    role: "triager",
    surface: ["local", "remote"],
    useCase: ["coordinate"],
    ships: true,
  },

  // ---- routines (real) ----
  "routine-manager-ensure": {
    role: "triager",
    surface: ["local", "remote"],
    useCase: ["coordinate"],
    ships: true,
    trigger: "cron",
  },
  "routine-manager-wake": {
    role: "triager",
    surface: ["local", "remote"],
    useCase: ["coordinate"],
    ships: true,
    trigger: "inbox_idle",
  },

  // ---- dashboards (all real) ----
  "dashboard-agent-dag": {
    role: "builder",
    surface: ["local", "remote"],
    useCase: ["build", "coordinate"],
    ships: true,
  },
  "dashboard-agent-inbox-console": {
    role: "triager",
    surface: ["local", "remote"],
    useCase: ["coordinate"],
    ships: true,
  },
  "dashboard-live-list-with-command": {
    role: "builder",
    surface: ["local"],
    useCase: ["build"],
    ships: true,
  },
  "dashboard-kickoff-console": {
    role: "analyst",
    surface: ["local"],
    useCase: ["setup", "coordinate"],
    ships: true,
  },
  "dashboard-pipeline-graph": {
    role: "reviewer",
    surface: ["local"],
    useCase: ["review", "ship"],
    ships: true,
  },
  "dashboard-routines-console": {
    role: "analyst",
    surface: ["local", "remote"],
    useCase: ["coordinate"],
    ships: true,
  },
  "dashboard-sprint-analysis": {
    role: "analyst",
    surface: ["local"],
    useCase: ["coordinate", "review"],
    ships: true,
  },
  "dashboard-setup-status": {
    role: "setup",
    surface: ["local"],
    useCase: ["setup"],
    ships: true,
  },
  "dashboard-story-board": {
    role: "builder",
    surface: ["local"],
    useCase: ["build", "coordinate"],
    ships: true,
  },

  // ---- skills (all real) ----
  "skill-agent-design": {
    role: "analyst",
    surface: ["local"],
    useCase: ["setup", "coordinate"],
    ships: true,
  },
  "skill-add-machine-user": {
    role: "setup",
    surface: ["remote"],
    useCase: ["setup"],
    ships: true,
  },
  "skill-agent": {
    role: null,
    surface: ["local"],
    useCase: ["build"],
    ships: true,
  },
  "skill-codesense-setup": {
    role: "setup",
    surface: ["local"],
    useCase: ["setup"],
    ships: true,
  },
  "skill-context-sync": {
    role: "builder",
    surface: ["local", "remote"],
    useCase: ["build", "ship"],
    ships: true,
  },
  "skill-manage-team": {
    role: "triager",
    surface: ["local", "remote"],
    useCase: ["coordinate"],
    ships: true,
  },
  "skill-routine": {
    role: null,
    surface: ["local"],
    useCase: ["build", "coordinate"],
    ships: true,
  },
  "skill-setup-machine": {
    role: "setup",
    surface: ["local", "remote"],
    useCase: ["setup"],
    ships: true,
  },
  "skill-worker-exit-contract": {
    role: "builder",
    surface: ["local", "remote"],
    useCase: ["build", "ship"],
    ships: true,
  },
};

// Scaffold cards — plausible routines that aren't on disk yet. They render in
// the gallery with a "scaffold" tag; the detail sheet shows a short yaml
// template so a reader can lift it into their own repo verbatim. New scaffolds
// land by appending here; drop a scaffold the moment a real file lands at the
// matching `examples/` path. Like the shipped ones, each sits under the agent
// it runs (`examples/agents/<agent>/routines/<id>/`) and names no `agent:`.
/** @type {Array<{ id: string, name: string, summary: string, path: string, source: string } & NonNullable<(typeof META)[string]>>} */
const SCAFFOLDS = [
  {
    id: "routine-nightly-build-and-triage",
    name: "nightly-build-and-triage",
    summary:
      "Each night, starts a new Builder that builds every open story's worktree and files a short status thread to the builder inbox for morning triage.",
    path: "examples/agents/builder/routines/nightly-build-and-triage/routine.yml",
    role: "builder",
    surface: ["local", "remote"],
    useCase: ["build", "coordinate"],
    ships: false,
    trigger: "cron",
    source:
      "# config/agents/builder/routines/nightly-build-and-triage/routine.yml\n" +
      "id: nightly-build-and-triage\n" +
      "name: Nightly build and triage\n" +
      "owner: codesense\n" +
      "mode: fresh\n" +
      "task: |\n" +
      "  Build every open story's worktree and file a short status thread.\n" +
      "\n" +
      "  1. For each ready story, run bmad-build and capture the result.\n" +
      "  2. Send one status thread to the builder role with send_role_message.\n" +
      "  3. Call signal_done with the counts.\n" +
      "where:\n" +
      "  kind: team_workspace\n" +
      "triggers:\n" +
      "  - schedule:\n" +
      '      cron: "0 2 * * *"\n' +
      "      tz: UTC\n" +
      "policy:\n" +
      "  max_runtime: 90m\n" +
      "runs_on: each_mac\n" +
      "revision: 1\n",
  },
  {
    id: "routine-inbox-idle-reviewer",
    name: "inbox-idle-reviewer",
    summary:
      "When the Reviewer sits idle with unread mail, wakes its running session to claim the oldest ready PR and review it.",
    path: "examples/agents/reviewer/routines/inbox-idle-reviewer/routine.yml",
    role: "reviewer",
    surface: ["local"],
    useCase: ["review"],
    ships: false,
    trigger: "inbox_idle",
    source:
      "# config/agents/reviewer/routines/inbox-idle-reviewer/routine.yml\n" +
      "id: inbox-idle-reviewer\n" +
      "name: Review the oldest ready PR\n" +
      "owner: codesense\n" +
      "mode: wake\n" +
      "task: |\n" +
      "  You have unread mail. Call read_messages with your session id, then\n" +
      "  claim_role_messages for the reviewer role. Review the oldest ready PR\n" +
      "  with bmad-code-review. If nothing is ready, say so in one line and wait.\n" +
      "where:\n" +
      "  kind: team_workspace\n" +
      "  machine: local\n" +
      "triggers:\n" +
      "  - event:\n" +
      "      kind: inbox_idle\n" +
      "policy:\n" +
      "  max_runtime: 30m\n" +
      "runs_on: each_mac\n" +
      "revision: 1\n",
  },
  {
    id: "routine-scout-ci-failures",
    name: "scout-ci-failures",
    summary:
      "Watches GitHub Actions on the sample source repo; files a scout thread when a build goes red.",
    path: "examples/agents/scout/routines/scout-ci-failures/routine.yml",
    role: "scout",
    surface: ["remote"],
    useCase: ["review", "coordinate"],
    ships: false,
    trigger: "webhook",
    source:
      "# config/agents/scout/routines/scout-ci-failures/routine.yml\n" +
      "# Scaffold: webhook triggers are not shipped yet.\n" +
      "id: scout-ci-failures\n" +
      "name: Scout CI failures\n" +
      "owner: codesense\n" +
      "mode: fresh\n" +
      "task: |\n" +
      "  When GitHub Actions reports a failed workflow run, file a scout thread with\n" +
      "  the failing job name and the run URL so a human can look at it fast.\n" +
      "where:\n" +
      "  kind: team_workspace\n" +
      "triggers:\n" +
      "  - webhook:\n" +
      "      from: github-actions\n" +
      "      event: workflow_run.failed\n" +
      "policy:\n" +
      "  max_runtime: 5m\n" +
      "revision: 1\n",
  },
];

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/** Every `examples/agents/<agent>/routines/<id>/` dir, sorted by agent then id. */
function agentRoutineDirs() {
  const out = [];
  for (const agent of listDirs(join(EXAMPLES, "agents"))) {
    for (const name of listDirs(join(EXAMPLES, "agents", agent, "routines"))) {
      out.push({ agent, name });
    }
  }
  return out;
}

function listDirs(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((name) => name !== ".DS_Store")
    .filter((name) => statSync(join(dir, name)).isDirectory())
    .sort();
}

function listHtml(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((name) => name.endsWith(".html"))
    .sort();
}

/** Parse the leading `--- ... ---` block of a markdown or yaml file. */
function parseFrontmatter(text) {
  const match = /^---\n([\s\S]*?)\n---/.exec(text);
  if (!match) return {};
  const fields = {};
  for (const line of match[1].split("\n")) {
    const m = /^([a-zA-Z_][\w-]*):\s*(.*)$/.exec(line);
    if (!m) continue;
    let value = m[2].trim();
    if (value.startsWith('"') && value.endsWith('"')) value = value.slice(1, -1);
    fields[m[1]] = value;
  }
  return fields;
}

/** Parse a tiny subset of yaml: top-level `key: value` lines with scalar values. */
function parseFlatYaml(text) {
  const fields = {};
  for (const raw of text.split("\n")) {
    // Only top-level keys (no leading whitespace, not inside a multi-line block).
    const m = /^([a-zA-Z_][\w-]*):\s*(.*)$/.exec(raw);
    if (!m) continue;
    let value = m[2].trim();
    // Strip trailing comments on quoted-less scalars.
    if (!value.startsWith("'") && !value.startsWith('"')) {
      const hash = value.indexOf(" #");
      if (hash >= 0) value = value.slice(0, hash).trim();
    }
    if (value.startsWith('"') && value.endsWith('"')) value = value.slice(1, -1);
    else if (value.startsWith("'") && value.endsWith("'")) value = value.slice(1, -1);
    fields[m[1]] = value;
  }
  return fields;
}

/** Extract the one-sentence summary from a SKILL.md's `description:` field, trimmed to its first clause. */
function skillSummary(desc) {
  if (!desc) return "";
  // Take only up to the first ". Use" or ". " so cards stay quiet.
  const stop = desc.search(/\.\s+Use when/);
  return (stop > 0 ? desc.slice(0, stop + 1) : desc).trim();
}

/** Extract the `<title>…</title>` text from a dashboard HTML file. */
function dashboardSummary(html) {
  const m = /<title>([^<]+)<\/title>/i.exec(html);
  return m ? m[1].trim() : "";
}

/** Infer the trigger kind from a routine.yml file by eyeballing the `triggers:` block. */
function routineTrigger(text) {
  if (/^\s*triggers:/m.test(text)) {
    if (/-\s*schedule\s*:/.test(text)) return "cron";
    if (/-\s*event\s*:/.test(text) && /inbox_idle/.test(text)) return "inbox_idle";
    if (/-\s*event\s*:/.test(text)) return "event";
    if (/webhook/.test(text)) return "webhook";
  }
  return null;
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

/**
 * Build the manifest. Writes real cards for every example on disk; appends
 * scaffold cards for the plausible routines listed above.
 */
export function buildManifest() {
  const recipes = [];
  const warnings = [];

  function metaFor(id, kind) {
    const m = META[id];
    if (m) return m;
    warnings.push(
      `generate-recipes-manifest: no META entry for ${id}; using defaults. ` +
        `Add an entry in scripts/generate-recipes-manifest.mjs (META table).`,
    );
    return {
      role: null,
      surface: ["local"],
      useCase: kind === "dashboard" ? ["coordinate"] : ["build"],
      ships: true,
    };
  }

  // ---- agents ----
  for (const name of listDirs(join(EXAMPLES, "agents"))) {
    const path = `examples/agents/${name}/agent.yaml`;
    const absolute = join(EXAMPLES, "agents", name, "agent.yaml");
    if (!existsSync(absolute)) continue;
    const source = readFileSync(absolute, "utf8");
    const yaml = parseFlatYaml(source);
    const id = `agent-${name}`;
    const meta = metaFor(id, "agent");
    recipes.push({
      id,
      kind: "agent",
      name,
      title: yaml.name || name,
      summary: yaml.description || "",
      path,
      lang: "yaml",
      role: meta.role ?? null,
      surface: meta.surface,
      useCase: meta.useCase,
      ships: meta.ships,
      trigger: null,
      source,
    });
  }

  // ---- routines ----
  // A routine belongs to the agent it runs (SW-AGR-10): it ships under
  // `examples/agents/<agent>/routines/<id>/routine.yml`, the same layout a
  // team's `config/agents/` uses, and names no `agent:` of its own.
  for (const { agent, name } of agentRoutineDirs()) {
    const path = `examples/agents/${agent}/routines/${name}/routine.yml`;
    const absolute = join(EXAMPLES, "agents", agent, "routines", name, "routine.yml");
    if (!existsSync(absolute)) continue;
    const source = readFileSync(absolute, "utf8");
    const yaml = parseFlatYaml(source);
    const id = `routine-${name}`;
    const meta = metaFor(id, "routine");
    const verb = yaml.mode === "wake" ? "wakes" : "starts a new";
    recipes.push({
      id,
      kind: "routine",
      name,
      title: yaml.name || name,
      summary: yaml.name ? `${yaml.name}: ${verb} ${agent}` : "",
      path,
      lang: "yaml",
      role: meta.role ?? null,
      surface: meta.surface,
      useCase: meta.useCase,
      ships: meta.ships,
      trigger: meta.trigger ?? routineTrigger(source),
      source,
    });
  }

  // ---- dashboards ----
  for (const file of listHtml(join(EXAMPLES, "dashboards"))) {
    const path = `examples/dashboards/${file}`;
    const absolute = join(EXAMPLES, "dashboards", file);
    const source = readFileSync(absolute, "utf8");
    const name = file.replace(/\.html$/, "");
    const id = `dashboard-${name}`;
    const meta = metaFor(id, "dashboard");
    recipes.push({
      id,
      kind: "dashboard",
      name,
      title: dashboardSummary(source) || name,
      summary: dashboardSummary(source),
      path,
      lang: "html",
      role: meta.role ?? null,
      surface: meta.surface,
      useCase: meta.useCase,
      ships: meta.ships,
      trigger: null,
      source,
    });
  }

  // ---- skills ----
  for (const name of listDirs(join(EXAMPLES, "skills"))) {
    const path = `examples/skills/${name}/SKILL.md`;
    const absolute = join(EXAMPLES, "skills", name, "SKILL.md");
    if (!existsSync(absolute)) continue;
    const source = readFileSync(absolute, "utf8");
    const fm = parseFrontmatter(source);
    const id = `skill-${name}`;
    const meta = metaFor(id, "skill");
    recipes.push({
      id,
      kind: "skill",
      name: fm.name || name,
      title: fm.name || name,
      summary: skillSummary(fm.description),
      path,
      lang: "markdown",
      role: meta.role ?? null,
      surface: meta.surface,
      useCase: meta.useCase,
      ships: meta.ships,
      trigger: null,
      source,
    });
  }

  // ---- dashboards shipped under getting-started/panes/ (optional) ----
  // Reserved for future stories: if the shipped viewer adds its own panes/
  // directory, surface them as dashboards too. Keep the walk deterministic
  // and skip silently when the directory is absent.
  if (existsSync(PANES)) {
    for (const file of listHtml(PANES)) {
      const name = file.replace(/\.html$/, "");
      const id = `pane-${name}`;
      if (recipes.some((r) => r.id === id)) continue;
      const source = readFileSync(join(PANES, file), "utf8");
      const meta = metaFor(id, "dashboard");
      recipes.push({
        id,
        kind: "dashboard",
        name,
        title: dashboardSummary(source) || name,
        summary: dashboardSummary(source),
        path: `getting-started/panes/${file}`,
        lang: "html",
        role: meta.role ?? null,
        surface: meta.surface,
        useCase: meta.useCase,
        ships: meta.ships,
        trigger: null,
        source,
      });
    }
  }

  // ---- scaffolds ----
  for (const scaffold of SCAFFOLDS) {
    if (recipes.some((r) => r.id === scaffold.id)) continue; // real file won, drop the scaffold
    recipes.push({
      id: scaffold.id,
      kind: scaffold.id.split("-", 1)[0], // "routine" / "agent" / "skill" / "dashboard"
      name: scaffold.name,
      title: scaffold.name,
      summary: scaffold.summary,
      path: scaffold.path,
      lang: "yaml",
      role: scaffold.role ?? null,
      surface: scaffold.surface,
      useCase: scaffold.useCase,
      ships: scaffold.ships,
      trigger: scaffold.trigger ?? null,
      source: scaffold.source,
    });
  }

  // Stable order: by kind then id. Keeps the committed JSON diff-friendly.
  recipes.sort((a, b) => {
    if (a.kind !== b.kind) return a.kind.localeCompare(b.kind);
    return a.id.localeCompare(b.id);
  });

  return {
    doc: {
      schema: 1,
      generatedAt: "deterministic",
      recipes,
    },
    warnings,
  };
}

function serialize(doc) {
  return `${JSON.stringify(doc, null, 2)}\n`;
}

export function buildInputs() {
  return listInputs(import.meta.url, [EXAMPLES, PANES]);
}

function main() {
  if (process.argv.includes("--list-inputs")) {
    for (const input of buildInputs()) console.log(input);
    return;
  }
  const check = process.argv.includes("--check");
  const { doc, warnings } = buildManifest();
  for (const w of warnings) console.warn(w);
  const fresh = serialize(doc);
  if (check) {
    const existing = existsSync(OUTPUT) ? readFileSync(OUTPUT, "utf8") : "";
    if (existing !== fresh) {
      // Find the drifted entity (first id that differs or is missing) so the
      // error message points at the actual culprit, not just "the whole file."
      let culprit = "recipes.json";
      try {
        const got = existing ? JSON.parse(existing) : { recipes: [] };
        const have = new Map((got.recipes || []).map((r) => [r.id, r]));
        for (const r of doc.recipes) {
          const prev = have.get(r.id);
          if (!prev || JSON.stringify(prev) !== JSON.stringify(r)) {
            culprit = r.id;
            break;
          }
        }
        if (culprit === "recipes.json") {
          for (const prev of got.recipes || []) {
            if (!doc.recipes.some((r) => r.id === prev.id)) {
              culprit = `${prev.id} (removed)`;
              break;
            }
          }
        }
      } catch {
        /* corrupt JSON — the generic message is still useful. */
      }
      console.error(
        [
          "generate-recipes-manifest: committed recipes.json is stale.",
          `  entity:    ${culprit}`,
          `  file:      codesense-docs/getting-started/content/generated/recipes.json`,
          `  generator: scripts/generate-recipes-manifest.mjs`,
          `  fix:       npm run gen:docs`,
        ].join("\n"),
      );
      process.exit(1);
    }
    console.log("generate-recipes-manifest: ok");
    return;
  }
  mkdirSync(dirname(OUTPUT), { recursive: true });
  if (!existsSync(OUTPUT) || readFileSync(OUTPUT, "utf8") !== fresh) writeFileSync(OUTPUT, fresh);
  console.log(`generate-recipes-manifest: wrote ${OUTPUT}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
