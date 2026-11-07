#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// SW-ODC-6: walks the Rust validator sources + the committed inventory and
// emits `codesense-docs/getting-started/content/generated/schemas.json`, the
// manifest the schema-browser.html dashboard fetches at runtime.
//
// Each entry documents one YAML / frontmatter / JSON file CodeSense parses:
//
//   {
//     "schema": 1,
//     "generatedAt": "deterministic",
//     "schemas": [
//       {
//         "slug": "agent-yaml",
//         "name": "agent.yaml",
//         "kind": "yaml" | "frontmatter" | "json",
//         "icon": "user",                   // Lucide name
//         "description": "…",
//         "path": "~/.codesense/.../<id>/agent.yaml",
//         "sourceRef": "model.rs:20-288",
//         "sourceFile": "src-tauri/src/agents/model.rs",   // absolute within repo
//         "recordCount": null,              // populated by the dashboard from live context
//         "validator": { "bridgeVerb": "validateYaml", "kind": "agent" },
//         "fields": [
//           {
//             "name": "id",
//             "type": "string",
//             "required": true,
//             "default": null,
//             "allowed": null,
//             "pattern": "[a-z0-9][a-z0-9-]{0,63}",
//             "description": "…",
//             "sourceRef": "model.rs:291"
//           }
//         ],
//         "example": "id: my-claude\n…"
//       }
//     ]
//   }
//
// Agent, routine and role field rows come from the Rust-exported serde schemas.
// Other catalogs retain their existing editorial definitions below.
// Model source hashes are checked before this module builds its manifest.
//
// Modes:
//   write (default) — regenerate schemas.json.
//   --check          — exit non-zero if the committed JSON differs from a fresh
//                      render. Error names (i) the drifted schema + field,
//                      (ii) the file to edit, (iii) `npm run gen:docs`.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { listInputs } from "./lib/generator-inputs.mjs";
import { modelSchemas } from "./generate-model-schemas.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CONTENT = join(ROOT, "codesense-docs", "getting-started", "content");
const OUTPUT = join(CONTENT, "generated", "schemas.json");

// ---------------------------------------------------------------------------
// Rust source files that back each schema. Paths are relative to the repo
// root so error messages are copy-pasteable.
// ---------------------------------------------------------------------------
const SOURCES = {
  agent: "src-tauri/src/agents/model.rs",
  routine: "src-tauri/src/routines/model.rs",
  org: "src-tauri/src/domain/org.rs",
  teams: "src-tauri/src/domain/team.rs",
  worktree: "src-tauri/src/install/worktree_index.rs",
  "story-frontmatter": "src-tauri/src/install/frontmatter_perf.rs",
  "epic-frontmatter": "src-tauri/src/install/frontmatter_perf.rs",
  bundle: "src-tauri/src/commands/dashboard_launch.rs",
};

// ---------------------------------------------------------------------------
// Catalog metadata and examples. Agent, routine and role fields are generated
// from the Rust-exported serde schemas; the remaining catalogs are editorial.
// ---------------------------------------------------------------------------

const SCHEMAS = [
  {
    slug: "agent-yaml",
    name: "agent.yaml",
    kind: "yaml",
    icon: "user",
    description:
      "Saved agent definition: runtime, environment, content, role and native permissions. Generated from the serde model; save-time semantic validation still applies.",
    path: "<context repo>/<team>/config/agents/<agent-id>/agent.yaml",
    sourceRef: "agents/model.rs:Agent",
    sourceFile: SOURCES.agent,
    validator: { bridgeVerb: "validateYaml", kind: "agent" },
    fields: [],
    example:
      "id: reviewer\nname: Reviewer\nruntime: codex\nrole: reviewer\nenv:\n  CHANNEL: review\ncontext:\n  - path: docs\nrevision: 1\n",
  },
  {
    slug: "routine-yml",
    name: "routine.yml",
    kind: "yaml",
    icon: "clock",
    description:
      "An agent task or argv command, run manually, on a schedule, on an event or after another routine. Generated from RoutineDef; save-time validation checks valid combinations.",
    path: "<team>/config/agents/<agent-id>/routines/<id>/routine.yml (commands: config/agents/_command/routines/<id>/routine.yml)",
    sourceRef: "routines/model.rs:RoutineDef",
    sourceFile: SOURCES.routine,
    validator: { bridgeVerb: "validateYaml", kind: "routine" },
    fields: [],
    example:
      "id: morning-review\nname: Morning review\nowner: codesense\nmode: fresh\ntask: Review stories and report the result.\nwhere:\n  kind: team_workspace\ntriggers:\n  - schedule:\n      cron: '0 9 * * 1-5'\n      tz: UTC\npolicy:\n  max_runtime: 5m\nrevision: 1\n",
  },
  {
    slug: "role-yaml",
    name: "role config YAML",
    kind: "yaml",
    icon: "users",
    description:
      "Durable role queue display settings and optional default handler. Save through role_save; this does not launch a handler or change queue registrations.",
    path: "<team>/content/roles/<encoded-id>.yaml",
    sourceRef: "queue_roles.rs:RoleConfig",
    sourceFile: "src-tauri/src/queue_roles.rs",
    validator: null,
    fields: [],
    example:
      "id: reviewer\nname: Review\nicon: builtin-chess:knight\ndefaultHandler: acme/dev/reviewer\n",
  },

  {
    slug: "org-yml",
    name: "org.yml",
    kind: "yaml",
    icon: "building",
    description:
      "Organization config. One per context repo at its root. Declares the org slug, display name, tag schema, and credential bindings.",
    path: "<context repo>/org.yml",
    sourceRef: "org.rs:33-78",
    sourceFile: SOURCES.org,
    validator: { bridgeVerb: "validateYaml", kind: "org" },
    fields: [
      {
        name: "slug",
        type: "string",
        required: true,
        default: null,
        description: "URL-normalized org identifier. Must match the directory name.",
      },
      {
        name: "display_name",
        type: "string",
        required: true,
        default: null,
        description: "Human-readable organization name.",
      },
      {
        name: "created_at",
        type: "iso-8601",
        required: false,
        default: "auto",
        description: "Organization creation timestamp. Auto-set on first write.",
      },
      {
        name: "remote_url",
        type: "string",
        required: false,
        default: null,
        description: "Git remote URL (e.g. GitHub SSH origin).",
      },
      {
        name: "config_flags.workspace_root_dir",
        type: "path",
        required: false,
        default: null,
        description:
          "Alternative location for managed source clones. Defaults to ~/.codesense/sources/.",
      },
      {
        name: "tag_schema",
        type: "object",
        required: false,
        default: "{}",
        description: "Team-defined tag registry. Empty dict = unopinionated default.",
      },
      {
        name: "credential_ref",
        type: "object",
        required: false,
        default: null,
        description: "Bound context-repo connection (provider, account id).",
      },
      {
        name: "people",
        type: "array<object>",
        required: false,
        default: "[]",
        description: "Organization members.",
      },
    ],
    example: `slug: acme
display_name: Acme Inc.
remote_url: git@github.com:acme/context.git
tag_schema:
  categories:
    - id: priority
      label: Priority
      tags:
        - id: p0
          label: Critical
          color: red
        - id: p1
          label: High
people: []
`,
  },

  {
    slug: "teams-yml",
    name: "teams.yml",
    kind: "yaml",
    icon: "users",
    description:
      "Team roster for an org. Array of team objects; each team may declare a parent for context inheritance.",
    path: "<context repo>/teams.yml",
    sourceRef: "team.rs; org.rs:80-196",
    sourceFile: SOURCES.teams,
    validator: { bridgeVerb: "validateYaml", kind: "teams" },
    fields: [
      {
        name: "name",
        type: "string",
        required: true,
        default: null,
        description: "Unique team name per org.",
      },
      {
        name: "slug",
        type: "string",
        required: true,
        default: null,
        description: "Directory name under teams/. Kebab-case.",
      },
      {
        name: "display_name",
        type: "string",
        required: true,
        default: null,
        description: "Human-readable label.",
      },
      {
        name: "remote",
        type: "string",
        required: true,
        default: null,
        description: 'Git remote key, e.g. "origin" or "upstream".',
      },
      {
        name: "remote_url",
        type: "string",
        required: false,
        default: null,
        description: "Git clone URL.",
      },
      {
        name: "parent_team",
        type: "string",
        required: false,
        default: null,
        description: "Single-parent chain for inheritance. No cycles, max depth 64.",
      },
      {
        name: "landing_page",
        type: "string",
        required: false,
        default: null,
        description:
          "The team's starter page, relative to its content folder (no `..`, no hidden file or folder), opened whenever the team has nothing open. Set it from a file's menu (Set as team starter page), Settings → Workspace, or the set_team_starter_page MCP tool. Unset: README.md, README.html or index.html there, else nothing.",
      },
      {
        name: "description",
        type: "string",
        required: false,
        default: '""',
        description: "Team description.",
      },
      {
        name: "is_archived",
        type: "boolean",
        required: false,
        default: "false",
        allowed: ["true", "false"],
        description: "Archived teams don't appear in the switcher.",
      },
      {
        name: "settings",
        type: "object",
        required: false,
        default: "{}",
        description: "Team-level overrides.",
      },
    ],
    example: `- name: Backend
  slug: backend
  display_name: Backend Team
  remote: origin
  remote_url: git@github.com:acme/api.git
  description: Core backend services
  parent_team: null
  is_archived: false

- name: Auth
  slug: auth
  display_name: Auth Subsystem
  remote: origin
  parent_team: Backend
`,
  },

  {
    slug: "worktree-yaml",
    name: "worktree.yaml",
    kind: "yaml",
    icon: "git-branch",
    description:
      "Runtime state for a context-repo worktree. Tracks sync status and mtime cache for warm starts. Managed by the sync engine; rarely hand-edited.",
    path: "<context repo>/_codesense/worktree/<owner>/<repo>.yaml",
    sourceRef: "worktree_index.rs",
    sourceFile: SOURCES.worktree,
    validator: { bridgeVerb: "validateYaml", kind: "worktree" },
    fields: [
      {
        name: "id",
        type: "string",
        required: true,
        default: null,
        description: "Worktree identifier, typically team/repo.",
      },
      {
        name: "status",
        type: "enum",
        required: true,
        default: null,
        allowed: ["pending", "syncing", "synced", "failed"],
        description: "SyncStatus. Live state updated by the sync engine.",
      },
      {
        name: "mtime_cache",
        type: "object",
        required: true,
        default: "{}",
        description: "Timestamp -> mtime map for warm-start skips.",
      },
      {
        name: "sync_state",
        type: "object",
        required: false,
        default: "{}",
        description: "Forward/reverse sync state including residual_metrics counters.",
      },
    ],
    example: `id: team/repo
status: synced
mtime_cache: {}
sync_state:
  last_sync: "2026-10-04T10:30:00Z"
  residual_metrics:
    forward: 0
    reverse: 0
`,
  },

  {
    slug: "story-frontmatter",
    name: "story frontmatter",
    kind: "frontmatter",
    icon: "book-open",
    description:
      "YAML frontmatter at the top of any .md file in a pipeline epic directory. type defaults to story when absent; status defaults to none (file renders without a status spine).",
    path: "any .md file in a pipeline epic directory",
    sourceRef: "frontmatter_perf.rs; docs/pipeline-frontmatter.md",
    sourceFile: SOURCES["story-frontmatter"],
    validator: { bridgeVerb: "validateYaml", kind: "story-frontmatter" },
    fields: [
      {
        name: "type",
        type: "enum",
        required: false,
        default: '"story"',
        allowed: ["story", "bug", "spike", "doc", "retro"],
        description: "Document type. Unknown values normalize to doc.",
      },
      {
        name: "status",
        type: "enum",
        required: false,
        default: "none",
        allowed: ["planning", "todo", "in-progress", "blocked", "review", "qa", "done"],
        description:
          "Pipeline status. Actionable types only (story, bug, spike). Invalid values silently ignored.",
      },
      {
        name: "story_id",
        type: "string",
        required: false,
        default: "filename",
        description:
          "Display label on the pipeline node. Falls back to the file's basename minus .md.",
      },
      {
        name: "depends_on",
        type: "array<string>",
        required: false,
        default: "[]",
        description:
          'Story IDs within the epic. Cross-epic refs: "other-epic/S-1". Unresolved refs silently dropped.',
      },
      {
        name: "supporting_docs",
        type: "array<path>",
        required: false,
        default: "[]",
        description:
          "Relative paths - same-directory only; must be non-story types. Actionable stories only.",
      },
      {
        name: "source_repo",
        type: "string",
        required: false,
        default: null,
        description: "Source repository identifier. Observed by the org story resolver.",
      },
    ],
    example: `---
type: story
status: in-progress
story_id: W-5
depends_on: [W-3, W-4]
supporting_docs:
  - ./spike-auth-tokens.md
  - ./bug-login-race.md
source_repo: acme/api
---

# User Auth Flow
`,
  },

  {
    slug: "epic-frontmatter",
    name: "epic frontmatter",
    kind: "frontmatter",
    icon: "book-marked",
    description:
      "Same field set as story frontmatter, but used on the epic overview file (README.md or index.md, case-insensitive). Serves as the epic-level context anchor; can carry supporting_docs regardless of type.",
    path: "README.md or index.md in an epic directory",
    sourceRef: "frontmatter_perf.rs; docs/pipeline-document-types.md",
    sourceFile: SOURCES["epic-frontmatter"],
    validator: { bridgeVerb: "validateYaml", kind: "epic-frontmatter" },
    fields: [
      {
        name: "type",
        type: "enum",
        required: false,
        default: '"story"',
        allowed: ["story", "bug", "spike", "doc", "retro"],
        description:
          "Document type for the epic overview. Non-actionable types (doc, retro) are common.",
      },
      {
        name: "status",
        type: "enum",
        required: false,
        default: "none",
        allowed: ["planning", "todo", "in-progress", "blocked", "review", "qa", "done"],
        description: "Optional epic-level status. Actionable types only.",
      },
      {
        name: "story_id",
        type: "string",
        required: false,
        default: "dirname",
        description: "Display label. Defaults to the epic directory name.",
      },
      {
        name: "depends_on",
        type: "array<string>",
        required: false,
        default: "[]",
        description: "Cross-epic dependencies. Same resolution rules as story frontmatter.",
      },
      {
        name: "supporting_docs",
        type: "array<path>",
        required: false,
        default: "[]",
        description: "Epic overviews can have supporting_docs regardless of type.",
      },
      {
        name: "source_repo",
        type: "string",
        required: false,
        default: null,
        description: "Default source repository for stories in this epic.",
      },
    ],
    example: `---
type: story
supporting_docs:
  - ./retro-sprint-1.md
  - ./research-architecture-options.md
source_repo: acme/api
---

# Epic Overview: Pipeline Viewer

High-level context for the pipeline viewer epic...
`,
  },

  {
    slug: "bundle-json",
    name: "bundle.json",
    kind: "json",
    icon: "package",
    description:
      "Agent launch bundle. Pre-wired agent + startup config a user can one-click launch. Fields inferred from DashboardLaunchState; the full schema is not generated yet.",
    path: "<context repo>/<team>/bundles/<bundle-id>/bundle.json",
    sourceRef: "dashboard_launch.rs",
    sourceFile: SOURCES.bundle,
    validator: { bridgeVerb: "validateYaml", kind: "bundle" },
    fields: [
      {
        name: "id",
        type: "string",
        required: true,
        default: null,
        description: "Bundle identifier. Kebab-case; unique within the team.",
      },
      {
        name: "name",
        type: "string",
        required: true,
        default: null,
        description: "Display name shown in the launch picker.",
      },
      {
        name: "agent_id",
        type: "string",
        required: true,
        default: null,
        description: "Default agent to launch. Must reference an existing agent.yaml id.",
      },
      {
        name: "description",
        type: "string",
        required: false,
        default: '""',
        description: "Short description of what the bundle sets up.",
      },
      {
        name: "launch_params",
        type: "object",
        required: false,
        default: "{}",
        description: "Pre-populated launch parameters merged into the picker form.",
      },
    ],
    example: `{
  "id": "web-dev",
  "name": "Web Dev Loop",
  "description": "Build + review + test for the web app",
  "agent_id": "my-claude",
  "launch_params": {
    "startup_file": "./CLAUDE.md",
    "worktree": "web"
  }
}
`,
  },
];

// ---------------------------------------------------------------------------
// Source presence check for catalog pointers. Generated model fields also have
// source-hash freshness checks in generate-model-schemas.mjs.
// ---------------------------------------------------------------------------

function verifySchema(schema) {
  const abs = join(ROOT, schema.sourceFile);
  if (!existsSync(abs)) {
    return [
      `generate-yaml-schemas: ${schema.slug}: source file ${schema.sourceFile} not found. ` +
        `Update sourceFile in scripts/generate-yaml-schemas.mjs or restore the Rust file.`,
    ];
  }
  return [];
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

export function buildManifest() {
  const warnings = [];
  const models = modelSchemas();
  const schemas = SCHEMAS.map((s) => {
    const w = verifySchema(s);
    warnings.push(...w);
    return {
      slug: s.slug,
      name: s.name,
      kind: s.kind,
      icon: s.icon,
      description: s.description,
      path: s.path,
      sourceRef: s.sourceRef,
      sourceFile: s.sourceFile,
      recordCount: null,
      validator: s.validator,
      fields: (models[s.slug]?.fields ?? s.fields).map((f) => ({
        name: f.name,
        type: f.type,
        required: Boolean(f.required),
        default: f.default ?? null,
        allowed: Array.isArray(f.allowed) ? f.allowed : null,
        pattern: typeof f.pattern === "string" ? f.pattern : null,
        description: f.description,
        sourceRef: typeof f.sourceRef === "string" ? f.sourceRef : null,
      })),
      example: s.example,
    };
  });

  // Stable order: by kind then slug. Matches the dashboard's list grouping
  // (YAML / Frontmatter / JSON) and keeps the committed JSON diff-friendly.
  const kindOrder = { yaml: 0, frontmatter: 1, json: 2 };
  schemas.sort((a, b) => {
    const ka = kindOrder[a.kind] ?? 99;
    const kb = kindOrder[b.kind] ?? 99;
    if (ka !== kb) return ka - kb;
    return a.slug.localeCompare(b.slug);
  });

  return {
    doc: {
      schema: 1,
      generatedAt: "deterministic",
      schemas,
    },
    warnings,
  };
}

function serialize(doc) {
  return `${JSON.stringify(doc, null, 2)}\n`;
}

function updateFactoryTriggerEnum(check) {
  const path = join(CONTENT, "generated", "schemas", "factory-phase.schema.json");
  const existing = readFileSync(path, "utf8");
  const schema = JSON.parse(existing);
  const model = readFileSync(join(ROOT, "src-tauri", "src", "routines", "model.rs"), "utf8");
  const events = [
    ...model.matchAll(/pub\(crate\) const EVENT_[A-Z_]+:\s*&str\s*=\s*"([^"]+)";/g),
  ].map((m) => m[1]);
  schema.properties.trigger.enum = ["manual", "schedule", ...events];
  const fresh = serialize(schema);
  if (check && existing !== fresh)
    throw new Error("factory-phase.schema.json trigger enum is stale; run pnpm gen:docs");
  if (!check && existing !== fresh) writeFileSync(path, fresh);
}

export function buildInputs() {
  const fixture = join(ROOT, "src-tauri/src/docs_schemas.generated.json");
  return listInputs(import.meta.url, [
    fixture,
    ...Object.keys(JSON.parse(readFileSync(fixture, "utf8")).sources).map((p) => join(ROOT, p)),
    ...Object.values(SOURCES).map((p) => join(ROOT, p)),
    join(CONTENT, "generated/schemas/factory-phase.schema.json"),
  ]);
}

function main() {
  if (process.argv.includes("--list-inputs")) {
    for (const input of buildInputs()) console.log(input);
    return;
  }
  const check = process.argv.includes("--check");
  // Cargo runs this generator before compiling tests. An explicit Rust export
  // must be able to bootstrap or replace the fixture it is about to produce.
  // Check mode always enforces freshness, even in an export environment.
  if (!check && process.env.UPDATE_DOCS_SCHEMAS === "1") {
    console.log(
      "generate-yaml-schemas: deferred until the Rust schema export completes; run pnpm gen:docs afterward",
    );
    return;
  }
  updateFactoryTriggerEnum(check);
  const { doc, warnings } = buildManifest();
  for (const w of warnings) console.warn(w);
  const fresh = serialize(doc);
  if (check) {
    const existing = existsSync(OUTPUT) ? readFileSync(OUTPUT, "utf8") : "";
    if (existing !== fresh) {
      // Name the first drifted schema + field so the error points at the
      // actual culprit, not just "the whole file."
      let culprit = "schemas.json";
      let field = null;
      try {
        const got = existing ? JSON.parse(existing) : { schemas: [] };
        const have = new Map((got.schemas || []).map((s) => [s.slug, s]));
        for (const s of doc.schemas) {
          const prev = have.get(s.slug);
          if (!prev) {
            culprit = `${s.slug} (added)`;
            break;
          }
          if (JSON.stringify(prev) !== JSON.stringify(s)) {
            culprit = s.slug;
            // Find the first drifted field for a sharper pointer.
            const prevFields = new Map((prev.fields || []).map((f) => [f.name, f]));
            for (const f of s.fields) {
              const p = prevFields.get(f.name);
              if (!p || JSON.stringify(p) !== JSON.stringify(f)) {
                field = f.name;
                break;
              }
            }
            if (!field) {
              for (const p of prev.fields || []) {
                if (!s.fields.some((f) => f.name === p.name)) {
                  field = `${p.name} (removed)`;
                  break;
                }
              }
            }
            break;
          }
        }
        if (culprit === "schemas.json") {
          for (const prev of got.schemas || []) {
            if (!doc.schemas.some((s) => s.slug === prev.slug)) {
              culprit = `${prev.slug} (removed)`;
              break;
            }
          }
        }
      } catch {
        /* corrupt JSON — the generic message is still useful. */
      }
      console.error(
        [
          "generate-yaml-schemas: committed schemas.json is stale.",
          `  schema:    ${culprit}${field ? ` · field: ${field}` : ""}`,
          `  file:      codesense-docs/getting-started/content/generated/schemas.json`,
          `  generator: scripts/generate-yaml-schemas.mjs`,
          `  fix:       npm run gen:docs`,
        ].join("\n"),
      );
      process.exit(1);
    }
    console.log("generate-yaml-schemas: ok");
    return;
  }
  mkdirSync(dirname(OUTPUT), { recursive: true });
  if (!existsSync(OUTPUT) || readFileSync(OUTPUT, "utf8") !== fresh) writeFileSync(OUTPUT, fresh);
  console.log(`generate-yaml-schemas: wrote ${OUTPUT}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
