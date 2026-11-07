#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// The reference is an export of served_tool_router(), including schemars schemas.
// --export queues one dashboard-publish build and derives the default view; --queued-export is the unqueued inner path.
// Normal generation/checks use sealed exports and never invoke Cargo.
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { runBuildJob } from "./lib/build-queue.mjs";
import Ajv2020 from "ajv/dist/2020.js";
import {
  REFRESH_COMMAND,
  allowStaleExport as switchedOn,
  diskGate,
  exportsUntouchedSinceMain,
  inheritedNotice,
} from "./lib/sealed-export.mjs";
export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const CONTENT = "codesense-docs/getting-started/content";
export const OUTPUT = `${CONTENT}/generated/mcp-tools.json`;
export const CATALOG = `${CONTENT}/_shared/examples.yml`;
const SEAL = `${CONTENT}/generated/mcp-tools.inputs.json`;
const FULL_PROFILE = "dashboard-publish";
const PROFILES = ["default", FULL_PROFILE];
const payloadPath = (profile) => `${CONTENT}/generated/mcp-tools.${profile}.json`;
const hash = (data) => createHash("sha256").update(data).digest("hex");
const json = (path, root = ROOT) => JSON.parse(readFileSync(resolve(root, path), "utf8"));
const SOURCE_REF = /("sourceRef":\s*"handler\.rs:)\d+"/g;
const serialize = (data) => JSON.stringify(data, null, 2) + "\n";
export const META = {
  // ---- Arc A: First touch / onboarding ----
  codesense_overview: { arc: "A", write: false, starter: true, core: true },
  update_codesense_docs: { arc: "A", write: true, starter: false, core: true },
  describe_setup: { arc: "A", write: false, core: true },
  who_am_i: { arc: "A", write: false, core: true },
  // SW-PB-10: beta feedback to the CodeSense team (beta builds only).
  feedback_send: { arc: "A", write: true },
  health_ping: { arc: "A", write: false },

  // ---- Arc B: Model / Agents & Accounts ----
  agent_create: { arc: "B", write: true },
  agent_delete: { arc: "B", write: true },
  agent_get: { arc: "B", write: false },
  agent_list: { arc: "B", write: false, core: true },
  agent_preview: { arc: "B", write: false },
  agent_restore: { arc: "B", write: false },
  agent_unarchive: { arc: "B", write: true },
  agent_update: { arc: "B", write: true },
  avatar_add: { arc: "B", write: true },
  avatar_list: { arc: "B", write: false },
  avatar_share: { arc: "B", write: true },
  cancel_remote_agent_task: { arc: "B", write: false },
  enroll_private_machine_account: { arc: "B", write: false },
  get_active_agent_account: { arc: "B", write: false },
  list_agent_accounts: { arc: "B", write: false },
  list_agent_updates: { arc: "B", write: false },
  model_rates: { arc: "B", write: false },
  post_agent_message: { arc: "B", write: true },
  read_remote_agent_task: { arc: "B", write: false },
  resolve_agent_account: { arc: "B", write: false },
  set_active_agent_account: { arc: "B", write: true },
  set_model_rate: { arc: "B", write: true },
  start_remote_agent_task: { arc: "B", write: true },
  update_workspace_base: { arc: "C", write: true },

  // ---- Arc C: Workspaces & Worktrees ----
  workspace_group_list: { arc: "C", write: false },
  workspace_group_create: { arc: "C", write: true },
  workspace_group_rename: { arc: "C", write: true },
  workspace_group_set_members: { arc: "C", write: true },
  workspace_group_add: { arc: "C", write: true },
  workspace_group_remove: { arc: "C", write: true },
  workspace_group_delete: { arc: "C", write: true },
  hydrate_story: { arc: "C", write: true },
  hydrate_worktree: { arc: "C", write: false },
  list_hydrated: { arc: "C", write: false },
  list_sessions: { arc: "C", write: false },
  list_worktrees: { arc: "C", write: false },
  open_remote_tab: { arc: "C", write: true },
  list_move_intents: { arc: "C", write: false },
  move_workspace: { arc: "C", write: true },
  open_terminal_tab: { arc: "C", write: true },
  open_workspace: { arc: "C", write: true, core: true },
  prepare_remote_workspace: { arc: "C", write: true },
  preview_reverse_sync_worktree: { arc: "C", write: false },
  read_terminal_tab: { arc: "C", write: false },
  read_worktree_config: { arc: "C", write: false },
  remote_workspace_fetch: { arc: "C", write: true },
  rename_terminal_tab: { arc: "C", write: true },
  rename_workspace: { arc: "C", write: true },
  reverse_sync_worktree: { arc: "C", write: true },
  run_in_terminal_tab: { arc: "C", write: true },
  send_to_session: { arc: "C", write: true },
  write_worktree_config: { arc: "C", write: true },
  wv_open_t2_window: { arc: "C", write: false },

  // ---- Arc D: Terminals & Machines ----
  capture_machine_passphrase: { arc: "D", write: false },
  connect_machine: { arc: "D", write: false },
  disconnect_machine: { arc: "D", write: false },
  end_unowned_terminal_session: { arc: "D", write: false },
  install_terminal_server: { arc: "D", write: false },
  list_machines: { arc: "D", write: false },
  list_ssh_sources: { arc: "D", write: false },
  prepare_machine_terminal_server_replacement: { arc: "D", write: false },
  remove_machine: { arc: "D", write: true },
  restart_machine_terminal_server: { arc: "D", write: false },
  restart_terminal_server: { arc: "D", write: false },
  retry_machine: { arc: "D", write: false },
  save_machine: { arc: "D", write: true },
  start_terminal_server: { arc: "D", write: false },
  stop_terminal_server: { arc: "D", write: false },
  set_machine_auto_connect: { arc: "D", write: true },
  set_machine_hidden: { arc: "D", write: true },
  set_machine_remote_mcp_control: { arc: "D", write: true },
  terminal_server_status: { arc: "D", write: false },
  test_machine_connection: { arc: "D", write: false },
  unshare_machine: { arc: "D", write: true },

  // ---- Arc E: Agents as Processes ----
  report_artifact: { arc: "E", write: true },
  report_step_result: { arc: "E", write: true },
  signal_done: { arc: "E", write: true },
  signal_progress: { arc: "E", write: true },
  wv_report_cycle: { arc: "E", write: true },
  wv_report_handoff: { arc: "E", write: true },
  wv_spawn_worker: { arc: "E", write: true },

  // ---- Arc F: Inboxes & Messaging ----
  accept_role_message: { arc: "F", write: true },
  ack_role_message: { arc: "F", write: true },
  ask_user: { arc: "F", write: true },
  claim_role_messages: { arc: "F", write: true },
  get_human_status: { arc: "F", write: false },
  inbox_list: { arc: "F", write: false, core: true },
  inbox_post: { arc: "F", write: true },
  mark_unread: { arc: "F", write: true },
  react: { arc: "F", write: true },
  read_messages: { arc: "F", write: false },
  register_role: { arc: "F", write: true },
  release_role: { arc: "F", write: true },
  reply: { arc: "F", write: true },
  role_state: { arc: "F", write: false },
  send_message: { arc: "F", write: true, core: true },
  send_role_message: { arc: "F", write: true },
  // SW-WC-2: workspace chat engine's agent-facing pair.
  workspace_chat_send: { arc: "F", write: true, core: true },
  workspace_chat_read: { arc: "F", write: false, core: true },

  // ---- Arc G: Routines (scheduled agents) ----
  routine_cancel: { arc: "G", write: true },
  routine_context: { arc: "G", write: false },
  routine_create: { arc: "G", write: true },
  routine_done: { arc: "G", write: true },
  schedule_wakeup: { arc: "G", write: true },
  list_wakeups: { arc: "G", write: false },
  cancel_wakeup: { arc: "G", write: true },
  routine_delete: { arc: "G", write: true },
  routine_get: { arc: "G", write: false },
  routine_list: { arc: "G", write: false },
  routine_publish_result: { arc: "G", write: true },
  routine_result: { arc: "G", write: true },
  routine_run: { arc: "G", write: false },
  routine_arm_on: { arc: "G", write: true },
  routine_handover_release: { arc: "G", write: true },
  routine_refresh_on_machine: { arc: "G", write: true },
  routine_run_now: { arc: "G", write: false },
  routine_runs: { arc: "G", write: false },
  routine_set_enabled: { arc: "G", write: true },
  routine_update: { arc: "G", write: true },

  // ---- Arc H: Pipelines & SDLC (Weft) ----
  list_runs: { arc: "H", write: false },
  list_run_events: { arc: "H", write: false },
  read_run: { arc: "H", write: false },
  read_run_evidence: { arc: "H", write: false },
  read_run_work_product: { arc: "H", write: false },
  wv_audit_prune: { arc: "H", write: true },
  wv_audit_query: { arc: "H", write: false },
  wv_check_t2_window: { arc: "H", write: false },
  wv_claim_queue: { arc: "H", write: true },
  wv_classify_action: { arc: "H", write: true },
  wv_close_t2_window: { arc: "H", write: false },
  wv_close_task: { arc: "H", write: true },
  wv_coord_read: { arc: "H", write: false },
  wv_dispatch_state: { arc: "H", write: false },
  wv_enqueue: { arc: "H", write: true },
  wv_escalate: { arc: "H", write: false },
  wv_handoff_state: { arc: "H", write: false },
  wv_list_overdue_harvests: { arc: "H", write: false },
  wv_mark_dispatched: { arc: "H", write: true },
  wv_pause_t2_windows: { arc: "H", write: true },
  wv_peer_read_file: { arc: "H", write: false },
  wv_post_assertion: { arc: "H", write: true },
  wv_queue_list: { arc: "H", write: false },
  wv_read_posture: { arc: "H", write: false },
  wv_read_recent: { arc: "H", write: false },
  wv_resolve_queue: { arc: "H", write: true },
  wv_resume_t2_windows: { arc: "H", write: false },
  wv_retire_decision: { arc: "H", write: false },
  wv_scope_state: { arc: "H", write: false },
  wv_set_posture: { arc: "H", write: true },
  wv_settle_approval: { arc: "H", write: false },
  wv_upsert_attention: { arc: "H", write: true },
  wv_upsert_watermark: { arc: "H", write: true },
  wv_verify_done: { arc: "H", write: false },
  wv_wait: { arc: "H", write: false },
  wv_watch_list: { arc: "H", write: false },
  wv_write_approval: { arc: "H", write: true },
  wv_write_decision: { arc: "H", write: true },
  wv_write_dispatch: { arc: "H", write: true },

  // ---- Arc I: Dashboards & UI primitives ----
  apply_canvas: { arc: "I", write: true },
  get_live_data_contract: { arc: "I", write: false },
  get_viewer_state: { arc: "I", write: false },
  pane_list: { arc: "I", write: false },
  pane_open: { arc: "I", write: true },
  pane_write: { arc: "I", write: true },
  read_rendered_document: { arc: "I", write: false },
  render_canvas: { arc: "I", write: true },
  validate_live_data_doc: { arc: "I", write: false },
  set_viewer_state: { arc: "I", write: true },
  reorder_workspace: { arc: "I", write: true },
  command_list: { arc: "I", write: false },
  command_run: { arc: "I", write: true },
  get_story_format: { arc: "I", write: false },

  // ---- Arc J: MCP meta ----
  codesense_call_tool: { arc: "J", write: false, starter: true },
  codesense_get_tool_schema: { arc: "J", write: false, starter: true },
  codesense_search_tools: { arc: "J", write: false, starter: true },

  // ---- Arc K: Sync & agent-driven git ----
  attach_context_repository: { arc: "K", write: false },
  cancel_context_operation: { arc: "K", write: true },
  context_operation_status: { arc: "K", write: false },
  context_repo_history: { arc: "K", write: false },
  context_repo_status: { arc: "K", write: false },
  copy_between_contexts: { arc: "K", write: true },
  detach_context_repository: { arc: "K", write: true },
  get_sync_status: { arc: "K", write: false },
  forget_missing_workspace: { arc: "K", write: true },
  list_context_attachments: { arc: "K", write: false },
  list_context_content: { arc: "K", write: false },
  read_content_file: { arc: "K", write: false },
  create_content_file: { arc: "K", write: true },
  move_between_contexts: { arc: "K", write: true },
  move_content_file: { arc: "K", write: true },
  query_context: { arc: "K", write: false },
  refresh_frontmatter_cache: { arc: "K", write: true },
  remove_from_context: { arc: "K", write: true },
  remove_source_repo: { arc: "K", write: true },
  report_context_sync: { arc: "K", write: true },
  route_file_to_context: { arc: "K", write: true },
  sense_sync_adopt: { arc: "K", write: true },
  sense_sync_list_layers: { arc: "K", write: false },
  sense_sync_move_layer: { arc: "K", write: true },
  sense_sync_read_layer_file: { arc: "K", write: false },
  sense_sync_read_lockfile: { arc: "K", write: false },
  unroute_file_from_context: { arc: "K", write: true },

  // ---- Arc L: Factory / Setup & Content ----
  add_source_repo: { arc: "L", write: true },
  archive_team: { arc: "L", write: false },
  attach_source_repo_path: { arc: "L", write: false },
  commit_registry_edit_local: { arc: "L", write: true },
  create_local_organization: { arc: "L", write: true },
  create_team: { arc: "L", write: true },
  delete_content_file: { arc: "L", write: true },
  delete_team: { arc: "L", write: true },
  get_publish_set: { arc: "L", write: false },
  list_orgs_teams: { arc: "L", write: false, core: true },
  list_source_repos: { arc: "L", write: false },
  records_collections: { arc: "L", write: false },
  records_list: { arc: "L", write: false },
  records_read: { arc: "L", write: false },
  records_search: { arc: "L", write: false },
  records_history: { arc: "L", write: false },
  records_read_at: { arc: "L", write: false },
  records_create: { arc: "L", write: true },
  records_write: { arc: "L", write: true },
  records_set_fields: { arc: "L", write: true },
  records_delete: { arc: "L", write: true },
  records_restore: { arc: "L", write: true },
  notes_create: { arc: "L", write: true },
  notes_history: { arc: "L", write: false },
  notes_list: { arc: "L", write: false },
  notes_read: { arc: "L", write: false },
  notes_read_at: { arc: "L", write: false },
  notes_restore: { arc: "L", write: true },
  notes_search: { arc: "L", write: false },
  notes_write: { arc: "L", write: true },
  read_epic_repos: { arc: "L", write: false },
  rename_team: { arc: "L", write: true },
  save_source_repo_attachment: { arc: "L", write: true },
  set_active_team: { arc: "L", write: true },
  set_epic_repos: { arc: "L", write: true },
  set_team_parent: { arc: "L", write: true },
  set_team_starter_page: { arc: "L", write: true },
  validate_story: { arc: "L", write: false },
  list_out_of_date_terminal_servers: { arc: "D", write: false },
  update_terminal_servers: { arc: "D", write: true },
  resolve_machine_identity: { arc: "D", write: false },
  session_usage: { arc: "E", write: false },
  session_read: { arc: "E", write: false },
  why_line: { arc: "E", write: false },
  refresh_history: { arc: "E", write: true },
  story_changes: { arc: "E", write: false },
  story_totals: { arc: "E", write: false },
  task_start: { arc: "E", write: true },
  task_update: { arc: "E", write: true },
  task_list: { arc: "E", write: false },
  task_read: { arc: "E", write: false },
  roles_list: { arc: "F", write: false },
  role_save: { arc: "F", write: true },
  close_terminal_tab: { arc: "C", write: true },
  close_workspace: { arc: "C", write: true },
  set_private_items_use: { arc: "L", write: true },
  dismiss_agent_mark: { arc: "E", write: true },
  create_dashboard: { arc: "I", write: true },
  publish_dashboard: { arc: "I", write: true },
  unpublish_dashboard: { arc: "I", write: true },
  share_dashboard: { arc: "I", write: true },
};

export function readCatalog(root = ROOT) {
  // JSON is a YAML subset, deliberately kept dependency-free for build.rs.
  const catalog = json(CATALOG, root);
  if (catalog.schema !== 1) throw new Error("examples.yml: unsupported schema version");
  return catalog;
}
function walk(root, path) {
  return readdirSync(resolve(root, path), { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? walk(root, `${path}/${entry.name}`) : [`${path}/${entry.name}`],
  );
}
export function referenceInputs(root = ROOT) {
  // MCP helpers own served prose and registry assembly. Nested schema types
  // outside mcp/ must derive or implement JsonSchema (or use a schema macro),
  // so seal those candidate files too, including additions/removals. This
  // avoids requiring two Cargo exports for an unrelated backend edit.
  const rust = [...walk(root, "src-tauri/src"), ...walk(root, "src-tauri/crates")].filter((p) =>
    p.endsWith(".rs"),
  );
  const candidates = rust.filter(
    (path) =>
      path.startsWith("src-tauri/src/mcp/") ||
      /\bJsonSchema\b|schema_for!|json_schema!/.test(readFileSync(resolve(root, path), "utf8")),
  );
  const paths = [
    "Cargo.lock",
    "src-tauri/Cargo.toml",
    "src-tauri/build.rs",
    CATALOG,
    "src-tauri/crates/codesense-pty-core/src/agent_cli/codex.rs",
    "src-tauri/crates/codesense-pty-core/src/agent_cli/claude.rs",
    ...candidates,
    ...walk(root, "src-tauri/crates").filter((p) => p.endsWith("Cargo.toml")),
  ];
  return Object.fromEntries(
    [...new Set(paths)].sort().map((path) => [path, hash(readFileSync(resolve(root, path)))]),
  );
}
export function sealReference(root = ROOT) {
  writeFileSync(
    resolve(root, SEAL),
    serialize({
      schema: 1,
      profiles: PROFILES,
      inputs: referenceInputs(root),
      payloads: Object.fromEntries(
        PROFILES.map((p) => [p, hash(readFileSync(resolve(root, payloadPath(p))))]),
      ),
    }),
  );
}
// Every non-Rust file buildManifest reads: the catalog, each family's docs page, and the
// generator code (META lives in this script).
const nonRustManifestInputs = (root) => [
  CATALOG,
  ...new Set(readCatalog(root).families.map((f) => `${CONTENT}/${f.docs.split("#")[0]}`)),
  "scripts/generate-mcp-reference.mjs",
  "scripts/lib/sealed-export.mjs",
];
function sealDrift(root) {
  const currentInputs = referenceInputs(root);
  if (!existsSync(resolve(root, SEAL)))
    return {
      addedInputs: Object.keys(currentInputs),
      changedInputs: [],
      removedInputs: [],
      changedPayloads: [],
      profileDrift: false,
      missingSeal: true,
    };

  const seal = json(SEAL, root);
  const previousInputs = seal.inputs ?? {};
  const addedInputs = Object.keys(currentInputs).filter((path) => !(path in previousInputs));
  const removedInputs = Object.keys(previousInputs).filter((path) => !(path in currentInputs));
  const changedInputs = Object.keys(currentInputs).filter(
    (path) => path in previousInputs && previousInputs[path] !== currentInputs[path],
  );
  const changedPayloads = PROFILES.filter(
    (profile) =>
      !existsSync(resolve(root, payloadPath(profile))) ||
      seal.payloads?.[profile] !== hash(readFileSync(resolve(root, payloadPath(profile)))),
  );
  const profileDrift = JSON.stringify(seal.profiles) !== JSON.stringify(PROFILES);
  if (
    !addedInputs.length &&
    !changedInputs.length &&
    !removedInputs.length &&
    !changedPayloads.length &&
    !profileDrift
  )
    return null;
  return { addedInputs, changedInputs, removedInputs, changedPayloads, profileDrift };
}

function staleSealMessage(drift) {
  const details = [];
  const changes = [
    ["changed inputs", drift.changedInputs],
    ["added inputs", drift.addedInputs],
    ["removed inputs", drift.removedInputs],
    ["changed payloads", drift.changedPayloads.map((profile) => payloadPath(profile))],
  ];
  let shown = 0;
  for (const [label, paths] of changes) {
    const visible = paths.slice(0, Math.max(0, 10 - shown));
    if (visible.length) details.push(`${label}: ${visible.join(", ")}`);
    shown += visible.length;
  }
  const total = changes.reduce((count, [, paths]) => count + paths.length, 0);
  if (total > shown) details.push(`and ${total - shown} more input/export changes`);
  if (drift.profileDrift) details.push("export profiles changed");
  const reason = drift.missingSeal
    ? "MCP registry has no sealed export"
    : "Stale MCP registry export";
  return `${reason} (${details.join("; ")}); refresh during the next docs wave with ${REFRESH_COMMAND}`;
}

export function validateInputReferences(tool) {
  const properties = tool.inputSchema.properties ?? {};
  // Only explicit single-input markers are compared. Output objects, nested
  // fields and calls to other tools describe different contracts.
  const description = tool.description ?? "";
  const input = description
    .split(/\n\s*\n/)
    .map((paragraph) => {
      const returned = paragraph.search(/\bReturns\b/);
      if (returned < 0) return paragraph;
      const tail = paragraph.slice(returned);
      const resumed = tail.search(/\b(?:Args|Inputs|Optional|Required)\b/);
      return paragraph.slice(0, returned) + (resumed < 0 ? "" : tail.slice(resumed));
    })
    .join("\n");
  const markers = [...input.matchAll(/\{\s*([a-zA-Z_]\w*)\s*\}/g)].map((m) => m[1]);
  for (const key of Object.keys(properties)) {
    const alias = key.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
    if (alias !== key && !Object.hasOwn(properties, alias) && markers.includes(alias))
      throw new Error(`${tool.name}: input {${alias}} must use schema field {${key}}`);
  }
}
// The manifest the export knows: pending tools and their examples dropped, so it still equals the
// committed one unless the PR changed something else.
const withoutPending = (families, pending) =>
  families
    .map((f) => ({
      ...f,
      tools: f.tools.filter((t) => !pending.includes(t)),
      calls: f.calls.filter((c) => !pending.includes(c.tool)),
    }))
    .filter((f) => f.tools.length);
const handlerFn = (root, name) =>
  new RegExp(`\\b(?:async\\s+)?fn\\s+${name}\\s*\\(`).test(
    readFileSync(resolve(root, "src-tauri/src/mcp/handler.rs"), "utf8"),
  );
// `pending` (PR gate only) is an array collecting catalog tools the sealed export lacks: story PRs
// never re-export, the wave does. Omitted (strict), such a tool is fatal.
export function buildManifest(
  root = ROOT,
  metadata = META,
  catalog = readCatalog(root),
  pending = null,
) {
  const baseline = json(payloadPath("default"), root).tools;
  const full = json(payloadPath("dashboard-publish"), root).tools;
  const baseNames = new Set(baseline.map((t) => t.name));
  const fullNames = new Set(full.map((t) => t.name));
  for (const tool of baseline) {
    if (!fullNames.has(tool.name)) throw new Error(`publish profile lost ${tool.name}`);
    const other = full.find((t) => t.name === tool.name);
    if (JSON.stringify(tool) !== JSON.stringify(other))
      throw new Error(`profile contract differs for ${tool.name}`);
  }
  const families = new Map();
  const catalogNames = new Set();
  const ajv = new Ajv2020({ strict: false, allErrors: true, validateFormats: false });
  for (const f of catalog.families) {
    if (!f.id || !f.title || !f.searchTerms?.length || !f.calls?.length)
      throw new Error(`${f.id}: missing family metadata/example`);
    const [page, anchor] = f.docs.split("#");
    if (
      !page ||
      page.startsWith("/") ||
      page.split("/").includes("..") ||
      !existsSync(resolve(root, CONTENT, page))
    )
      throw new Error(`${f.id}: unresolved Docs pointer ${f.docs}`);
    const html = readFileSync(resolve(root, CONTENT, page), "utf8");
    if (anchor && !html.includes(`id="${anchor}"`) && !html.includes(`id='${anchor}'`))
      throw new Error(`${f.id}: unresolved Docs anchor ${f.docs}`);
    for (const name of f.tools) {
      if (catalogNames.has(name)) throw new Error(`${name}: duplicate family`);
      catalogNames.add(name);
      if (!fullNames.has(name)) {
        if (!pending) throw new Error(`${name}: catalog tool not exported`);
        // Pending means implemented (META entry and handler fn) but not yet exported.
        if (!metadata[name] || !handlerFn(root, name))
          throw new Error(`${name}: unknown catalog tool (no META entry or handler fn)`);
        pending.push(name);
        continue;
      }
      families.set(name, f);
    }
    for (const call of f.calls) {
      if (!f.tools.includes(call.tool)) throw new Error(`${f.id}: example tool outside family`);
      if (pending?.includes(call.tool)) continue;
      const tool = full.find((t) => t.name === call.tool);
      const validate = ajv.compile(tool.inputSchema);
      if (!validate(call.arguments))
        throw new Error(`${call.tool}: invalid family example ${ajv.errorsText(validate.errors)}`);
      // serde allows unknown fields on many structs: examples still must name
      // real canonical inputs rather than an ignored typo or accepted alias.
      for (const key of Object.keys(call.arguments))
        if (!Object.hasOwn(tool.inputSchema.properties ?? {}, key))
          throw new Error(`${call.tool}: unknown example input ${key}`);
    }
  }
  const handler = readFileSync(resolve(root, "src-tauri/src/mcp/handler.rs"), "utf8");
  const tools = full
    .map((raw) => {
      const meta = metadata[raw.name];
      if (!meta)
        throw new Error(`${raw.name}: missing META entry in scripts/generate-mcp-reference.mjs`);
      const family = families.get(raw.name);
      if (!family) throw new Error(`${raw.name}: missing family in examples.yml`);
      if (
        !raw.description?.endsWith(`Docs: ${family.docs}`) ||
        raw.description.split("\n").filter((line) => line.startsWith("Docs: ")).length !== 1
      )
        throw new Error(`${raw.name}: missing final Docs pointer`);
      validateInputReferences(raw);
      const match = new RegExp(`\\b(?:async\\s+)?fn\\s+${raw.name}\\s*\\(`).exec(handler);
      if (!match) throw new Error(`${raw.name}: source location missing`);
      return {
        name: raw.name,
        arc: meta.arc,
        description: raw.description,
        write: meta.write,
        starter: meta.starter ?? false,
        core: meta.core ?? false,
        family: family.id,
        docs: family.docs,
        namedOnPage: new RegExp(`\\b${raw.name}\\b`).test(
          readFileSync(resolve(root, CONTENT, family.docs.split("#")[0]), "utf8"),
        ),
        feature: baseNames.has(raw.name) ? null : "dashboard-publish",
        params: raw.inputSchema,
        returns: raw.outputSchema ?? null,
        outputSchemaAvailable: raw.outputSchema != null,
        examples: family.calls.filter((call) => !pending?.includes(call.tool)),
        sourceRef: `handler.rs:${handler.slice(0, match.index).split("\n").length}`,
      };
    })
    .sort((a, b) => a.name.localeCompare(b.name));
  return {
    schema: 1,
    generatedAt: "deterministic",
    commit: "deterministic",
    families: pending?.length ? withoutPending(catalog.families, pending) : catalog.families,
    coverage: {
      familyCovered: tools.length,
      namedOnPage: tools.filter((tool) => tool.namedOnPage).length,
      documentationGaps: tools
        .filter((tool) => !tool.namedOnPage)
        .map((tool) => ({ tool: tool.name, docs: tool.docs })),
    },
    tools,
  };
}
export function generateReference(
  root = ROOT,
  {
    check = false,
    buildScript = false,
    allowStaleExport = false,
    warn = console.warn,
    stdout = console.log,
  } = {},
) {
  const drift = sealDrift(root);
  // Tolerated input-only drift (e.g. a handler.rs edit) shifts every sourceRef
  // line number; the committed manifest is then compared without them.
  let tolerant = false;
  if (drift) {
    const message = staleSealMessage(drift);
    if (buildScript) {
      stdout("cargo:warning=MCP REFERENCE EXPORT NEEDED");
      stdout(
        `cargo:warning=${message}; node scripts/generate-mcp-reference.mjs --export, then pnpm gen:docs`,
      );
      if (!check) return null;
    }
    const inputDriftOnly =
      !drift.missingSeal &&
      (drift.changedInputs.length || drift.addedInputs.length || drift.removedInputs.length) &&
      !drift.changedPayloads.length &&
      !drift.profileDrift;
    // Input-only drift needs the exports untouched. Inherited drift also needs every non-Rust
    // manifest input untouched: the manifest is f(payloads, catalog/docs/META, sourceRefs), the
    // payloads are main's, the sourceRef lines are normalised, so the PR cannot have changed it.
    // (Rust inputs, handler.rs included, may differ.) Anything else runs the strict check.
    let untouched = false;
    if (check && allowStaleExport) {
      try {
        const exports = [SEAL, OUTPUT, ...PROFILES.map(payloadPath)];
        untouched = exportsUntouchedSinceMain(
          root,
          inputDriftOnly ? exports : [...exports, ...nonRustManifestInputs(root)],
        );
      } catch {}
    }
    if (!untouched) throw new Error(message);
    tolerant = true;
    if (inputDriftOnly) warn(`NOTICE: ${message}`);
    else {
      warn(inheritedNotice(root, message));
      return null;
    }
  }
  const pending = allowStaleExport ? [] : null;
  const doc = buildManifest(root, META, readCatalog(root), pending);
  for (const name of pending ?? []) warn(`NOTICE: pending wave export: ${name}`);
  const fresh = serialize(doc);
  const file = resolve(root, OUTPUT);
  if (check) {
    const existing = existsSync(file) ? readFileSync(file, "utf8") : "";
    const comparable = (text) => (tolerant ? text.replace(SOURCE_REF, '$1"') : text);
    if (comparable(existing) !== comparable(fresh)) {
      let culprit = "mcp-tools.json";
      try {
        const old = JSON.parse(existing);
        culprit =
          doc.tools.find(
            (t) =>
              comparable(JSON.stringify(t)) !==
              comparable(JSON.stringify(old.tools.find((p) => p.name === t.name))),
          )?.name ?? culprit;
      } catch {}
      throw new Error(`generate-mcp-reference: ${culprit}: stale ${OUTPUT}; run pnpm gen:docs`);
    }
  } else {
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, fresh);
  }
  return doc;
}
export function queuedExport() {
  mkdirSync(resolve(ROOT, `${CONTENT}/generated`), { recursive: true });
  const inputsBefore = referenceInputs();
  // Cargo builds into the worktree's shared `target/`; only the gated list is ours to remove.
  const scratch = mkdtempSync(join(tmpdir(), "mcp-reference-"));
  const gatedPath = join(scratch, "gated-tools.json");
  try {
    diskGate(ROOT);
    const result = spawnSync(
      "cargo",
      [
        "test",
        "--manifest-path",
        "src-tauri/Cargo.toml",
        "--lib",
        "-j4",
        "--features",
        FULL_PROFILE,
        "--",
        "mcp::handler::",
        "mcp::reference::",
        "mcp::identity_tools::",
        "mcp::feedback_tool::",
      ],
      {
        cwd: ROOT,
        stdio: "inherit",
        env: {
          ...process.env,
          MCP_REFERENCE_EXPORT: "1",
          MCP_REFERENCE_OUTPUT: resolve(ROOT, payloadPath(FULL_PROFILE)),
          MCP_REFERENCE_GATED_OUTPUT: gatedPath,
        },
      },
    );
    if (result.status !== 0) throw new Error(`MCP export failed (${result.status})`);
    // One feature set per export: the default view is the publish view without the tools
    // that `dashboard-publish` gates.
    const gated = new Set(JSON.parse(readFileSync(gatedPath, "utf8")));
    const full = json(payloadPath(FULL_PROFILE));
    writeFileSync(
      resolve(ROOT, payloadPath("default")),
      serialize({ ...full, tools: full.tools.filter((t) => !gated.has(t.name)) }),
    );
    if (JSON.stringify(inputsBefore) !== JSON.stringify(referenceInputs())) {
      throw new Error("MCP source inputs changed during export; retry the export before sealing");
    }
    sealReference();
    generateReference();
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}
export function exportReference() {
  const result = runBuildJob(
    "MCP registry export",
    [process.execPath, fileURLToPath(import.meta.url), "--queued-export"],
    { cwd: ROOT, stdio: "inherit" },
  );
  if (result.status !== 0) throw new Error(`MCP registry export failed (${result.status})`);
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.includes("--queued-export")) queuedExport();
    else if (process.argv.includes("--export")) exportReference();
    // The explicit exporter bootstraps stale fixtures while Cargo compiles the
    // registry. build.rs never starts another Cargo process or queues work.
    else if (process.env.MCP_REFERENCE_EXPORT !== "1")
      generateReference(ROOT, {
        check: process.argv.includes("--check"),
        buildScript: process.argv.includes("--build-script"),
        allowStaleExport: switchedOn(),
      });
  } catch (e) {
    console.error(e.message);
    process.exitCode = 1;
  }
}
