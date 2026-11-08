// SPDX-License-Identifier: MIT
/**
 * Tauri IPC Contract Test
 *
 * Validates that TypeScript command wrappers (commands.ts) stay in sync
 * with Rust handler registrations (lib.rs). Catches drift when someone
 * adds/removes a command on one side without updating the other.
 *
 * This test parses source files directly — no runtime Tauri needed.
 */
import { describe, it, expect } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";

// Parse TypeScript invoke calls from commands.ts
function extractTypeScriptCommands(): string[] {
  const source = [
    "commands.ts",
    "../features/dashboard/lib/verb-dispatch.ts",
    "../shared/stores/local-profile-store.ts",
    "../shared/lib/local-profile-session.ts",
    // SW-PB-9: the beta update wrappers (the commands are registered under the beta cfg).
    "../features/license/api.ts",
  ]
    .map((file) => readFileSync(resolve(__dirname, file), "utf-8"))
    .join("\n");
  // Command names may contain digits (e.g. `wv_e2e_seed_dispatch`), so the char class
  // mirrors the Rust extractor's `\w` — `[a-z0-9_]` — not a digit-free `[a-z_]`.
  const invokePattern = /invoke\s*(?:<[^>]*>)?\s*\(\s*["']([a-z0-9_]+)["']/g;
  const commands: string[] = [];
  let match;
  while ((match = invokePattern.exec(source)) !== null) {
    commands.push(match[1]);
  }
  return [...new Set(commands)].sort();
}

// Parse Rust handler registrations from lib.rs
function extractRustHandlers(): string[] {
  const source = readFileSync(resolve(__dirname, "../../src-tauri/src/lib.rs"), "utf-8");
  // Extract the generate_handler![] block contents. Use a balanced-bracket
  // approach because #[cfg(...)] attributes inside the block contain ']'
  // which breaks simple [^\]]+ patterns.
  const start = source.indexOf("generate_handler![");
  if (start === -1) throw new Error("Could not find generate_handler! block in lib.rs");
  const blockStart = start + "generate_handler![".length;
  let depth = 1;
  let i = blockStart;
  while (i < source.length && depth > 0) {
    if (source[i] === "[") depth++;
    else if (source[i] === "]") depth--;
    i++;
  }
  const handlerBlock = [null, source.slice(blockStart, i - 1)] as const;
  if (!handlerBlock[1]) throw new Error("Could not find generate_handler! block in lib.rs");

  // Match command paths like commands::install::create_symlink,
  // mcp::state::push_viewer_state, or single-segment modules like
  // menu::rebuild_menu (macOS menu, story C.16). Any allowed prefix may nest modules
  // (e.g. live_hub::files::file_surface_prepare_view):
  // `(?:::\w+)*` consumes every module segment greedily and the trailing `::(\w+)`
  // backtracks to capture the final command name at any depth.
  // Single-segment top-level command modules (like `menu`) are allow-listed by
  // name: `renderer_watchdog` hosts the SW-FX-64 `renderer_heartbeat` ack;
  // `mcp_registration` hosts the SW-AN-5 MCP integration-surface commands.
  // `weft` is a top-level module (like `menu`) that hosts `#[tauri::command]`s
  // registered as `weft::<sub>::<cmd>` (SW-WV-F2 `weft::preflight::preflight_wv_dispatch`).
  // `license::commands` hosts the SW-PB-9 beta license commands (registered under the beta cfg).
  const handlerPattern =
    /(?:commands|license::commands|dashboard_publish|verbs::commands|records::commands|mcp|mcp_registration|menu|renderer_watchdog|live_hub|weft)(?:::\w+)*::(\w+)/g;
  const commands: string[] = [];
  let match;
  while ((match = handlerPattern.exec(handlerBlock[1])) !== null) {
    commands.push(match[1]);
  }
  return [...new Set(commands)].sort();
}

// Rust-only commands that intentionally have no TS wrapper.
// These exist in generate_handler![] but have been removed from the TS layer.
const RUST_ONLY_COMMANDS = new Set([
  "revoke_local_machine_access", // Runtime authority revocation; no renderer action currently.
  "watch_for_claude_session", // Dead after terminal unification (story 34.10); Rust removal pending
  "cancel_session_watcher", // Dead after terminal unification (story 34.10); Rust removal pending
  // RF-2: verify_installation removed (deprecated legacy install-health command,
  // deleted with the whole config-lock.yml command/reader surface).
  // Pre-existing on main after the SW-AU-77A rebase: these landed registered in
  // generate_handler![] with no TS wrapper (their own stories own the wrapper if
  // one is intended). Allow-listed here only so this branch's symmetry test is
  // green; not introduced by SW-AU-77A.
  // SW-AU-86B added the `workflowNodeKindCatalog` TS wrapper, so workflow_node_kind_catalog is no
  // longer Rust-only — it left this set and now counts as a both-sides command.
  // SW-OC-25c: the dev/test-gated orchestration_store_spike_probe / status_effect_probe were
  // deleted with the sprint auto-run, so they left this set.
  // SW-IS-F10 (S4): `set_forward_sync_default` + `set_forward_sync_overrides` now have
  // renderer wrappers (`setForwardSyncDefault` / `setForwardSyncOverrides`), driven by the
  // unified Sync settings surface, so they LEFT this Rust-only set — the TS count catches
  // up to Rust for the two forward pushes. → 457 TS / 461 Rust.
  // SW-WV-B5: the eight scope-layer Tauri commands are registered in generate_handler![]
  // but ship with no renderer IPC wrapper yet (the FE scope/watch surface lands in a later
  // WV story) — Rust-only until then, so the symmetry check treats them like the probes.
  "cmd_wv_scope_get",
  "cmd_wv_scope_create",
  "cmd_wv_scope_update_artifact_ptr",
  "cmd_wv_scope_close",
  "cmd_wv_watch_upsert",
  "cmd_wv_watch_close",
  "cmd_wv_watch_mark_quiet",
  "cmd_wv_watch_list",
  // SW-WV-C6: the four decision-harvest-norm Tauri commands are controller-tier and
  // registered in generate_handler![] with no renderer IPC wrapper yet — Rust-only.
  "wv_set_harvest_artifact",
  "wv_approve_harvest",
  "wv_mark_harvest_not_required",
  "wv_list_overdue_harvests",
  // SW-WV-RT1: the signal-done bridge is a Tauri door onto the same signal_done hook
  // registry the MCP verb dispatches — reachable from the UI and the QH harness
  // (core.invoke). It has no renderer IPC wrapper yet (its first caller is the QH
  // harness / a later Board wiring), so it is Rust-only until then.
  "wv_signal_done_bridge",
  // SW-AV-27 removed the lease/controller/gate harness drivers (`wv_e2e_drive_lease`,
  // `wv_e2e_drive_posture`, `wv_e2e_drive_gate`, `wv_e2e_drive_e1`, `wv_e2e_drive_e13`) with
  // the control plane; SW-OC-19 removed the completion-reply drivers
  // (`wv_e2e_e1_reply_state`, `wv_e2e_drive_decoupling_reply`) with the reply-owed ledger.
  // SW-WA-E7: the harness-only Stage-2 red-path driver — an unverified dispatch cannot
  // fold. (SW-AV-27 removed the `wv_e2e_drive_tier_spawn` and `wv_e2e_drive_gate_open`
  // drivers with the tier/gate control plane.) Refused unless CODESENSE_E2E=1 and reached
  // only by the QH weft-e2e harness via core.invoke — no renderer IPC wrapper, so Rust-only.
  "wv_e2e_drive_fold",
  // SW-WA-F4: the harness-only open→run→read round-trip drivers. They drive the REAL
  // F1/F2/F3 MCP verb methods (open_terminal_tab → run_in_terminal_tab →
  // read_terminal_tab) on a fresh handler over the live app so the capstone's Act 3
  // operator round-trip + forged-target red path are provable in the weft-e2e harness.
  // Refused unless CODESENSE_E2E=1 and reached only by that harness via core.invoke —
  // no renderer IPC wrapper, so both are Rust-only. (open+run is one-shot; read is
  // polled because the ls output tees into the PTY ring asynchronously.)
  "wv_e2e_terminal_open_run",
  "wv_e2e_terminal_read",
  // SW-AV-27 removed the harness-only controller arm drivers (`wv_e2e_arm_controller`,
  // `wv_e2e_cold_open_bootstrap`) with the lease/controller control plane.
]);

// TypeScript wrappers that intentionally have no Rust handler YET — the handler
// lands in a paired, later-merging story. The parity check ignores these on both
// sides so a forward-declared FE binding does not redden the branch that ships it.
const TS_ONLY_COMMANDS = new Set<string>([
  // SW-WV-F7: `wv_dispatch_worker` was forward-declared TS-only by SW-WV-F2 (the
  // "Dispatch & release" button's `dispatchWeftWorker` wrapper shipped ahead of its
  // Rust handler). F7 lands `weft::dispatch_bridge::wv_dispatch_worker` and registers
  // it in generate_handler![], so the entry is REMOVED here — the verb is now a full
  // both-sides pair (folds DW-F2-3). No forward-declared TS wrapper remains.
]);

describe("Tauri IPC Contract", () => {
  const tsCommands = extractTypeScriptCommands();
  const rustHandlers = extractRustHandlers();
  const effectiveRustHandlers = rustHandlers.filter((cmd) => !RUST_ONLY_COMMANDS.has(cmd));
  // Forward-declared TS wrappers whose Rust handler ships in a paired later story are
  // excluded from the symmetry check (they are counted in the raw TS count below).
  const effectiveTsCommands = tsCommands.filter((cmd) => !TS_ONLY_COMMANDS.has(cmd));

  it("TypeScript and Rust have the same number of active commands", () => {
    expect(effectiveTsCommands.length).toBe(effectiveRustHandlers.length);
  });

  it("does not register or wrap the retired provider-token command", () => {
    expect(tsCommands).not.toContain("set_provider_token");
    expect(rustHandlers).not.toContain("set_provider_token");
  });

  it("every TypeScript invoke has a matching Rust handler", () => {
    const missingInRust = effectiveTsCommands.filter((cmd) => !rustHandlers.includes(cmd));
    expect(missingInRust).toEqual([]);
  });

  it("every active Rust handler has a matching TypeScript wrapper", () => {
    const missingInTS = effectiveRustHandlers.filter((cmd) => !tsCommands.includes(cmd));
    expect(missingInTS).toEqual([]);
  });

  it("active commands match exactly (sorted comparison)", () => {
    expect(effectiveTsCommands).toEqual(effectiveRustHandlers);
  });

  it("has at least 59 commands registered (sanity check)", () => {
    expect(effectiveTsCommands.length).toBeGreaterThanOrEqual(59);
    expect(effectiveRustHandlers.length).toBeGreaterThanOrEqual(59);
  });

  // Snapshot for tracking additions/removals over time
  it("command registry matches expected count", () => {
    // Update this number when commands are intentionally added or removed
    // R-14: bumped +1 for `get_story_details` (on-demand body extraction).
    // PTS-3: bumped +1 for `resolve_team_content_dir` (per-team content path).
    // PTS-2: bumped +2 for `run_pts_migration_if_needed` and
    // `run_pts_post_migration_resolve` (one-time copy-not-move migration).
    // SS.5: bumped +1 for `is_git_working_tree` (AttachLocalCloneDialog
    // probe; AC-5 non-git rejection).
    // SS-13: bumped +8 for content_engine boundary surface
    // (list_layers, list_layer_contents, read_layer_file,
    // resolve_composite_for_repo, read_lockfile_for_repo,
    // move_file_between_layers, sense_sync_write_to_layer,
    // sense_sync_scan_cwd).
    // SS-4c: +2 for setActiveTeamForWave + triggerStartupWave on both sides.
    // SS-11b: -2 for retiring run_pts_migration_if_needed +
    // run_pts_post_migration_resolve (PTS-2 migration surface); -1 for
    // retiring install_source_repo (legacy install command); -1 for
    // retiring verify_symlink (unused legacy symlink-verify command).
    // SW-WT-9: +1 on both sides for `evaluate_setup_provenance`
    // (team-sync setup provenance gate).
    // SW-WL-3: +2 on both sides for `lazy_attach_workspace` +
    // `lazy_detach_workspace` (surface-bound watcher attach/detach).
    // SW-CP-4: +1 on both sides for `resolve_conflict` (reverse-sync conflict
    // resolution).
    // SW-TS-4: +1 on both sides for `establish_worktree_story_link`
    // (browse→worktree bridge: write link + trigger team-aware sync).
    // SW-TS-5: +1 on both sides for `reconcile_worktrees_on_launch`
    // (launch reconcile across worktree surfaces, §4.4).
    // SW-WT-6: -1 on both sides for `resync_worktree_symlinks` (removed with
    // the `symlink_from_main` schema field — materialization is the sync
    // engine's job, not a per-open replay).
    // SW-SC-1: +1 on both sides for `write_content_file` (general atomic
    // editable-content writer; the Excalidraw canvas save is its first user).
    // SW-SC-2: +1 on both sides for `write_binary_content_file` (binary
    // companion to write_content_file; the canvas PNG render is its first user).
    // SW-SC-5: +4 on both sides for the canvas wireframe-library commands
    // (`list_canvas_libraries`, `list_canvas_library_names`, `add_canvas_library`,
    // `remove_canvas_library`).
    // SW-WT-8: -2 on both sides for the removed validation/rehearsal surface
    // (`validate_worktree`, `validate_worktree_config`) — the unbuilt "Test my
    // setup" UI was their only entry point.
    // SW-TS-3: +1 on both sides for `retry_worktree_sync` (one-click retry of a
    // failed worktree surface).
    // SW-SC-8: net −2 on both sides — those four collapse to
    // `read_canvas_library` + `write_canvas_library` (native panel is the only
    // management surface; one writable team tier).
    // SW-SC-14: +1 on both sides for `resolve_canvas_render` (the frontend→backend
    // reply for the `render_canvas` MCP verb's render bridge).
    // SW-EW-5: +1 on both sides for `ensure_epic_workspace` (registers the epic
    // workspace dir as a content-engine target; synthetic `_epic` slug).
    // SW-EW-6: +2 on both sides for `assemble_epic_worktrees` +
    // `reconcile_epic_worktrees` (integration-worktree assembly + teardown;
    // SW-EW-45 renamed teardown → per-repo reconcile-to-frontmatter teardown).
    // SW-CP-14: +2 on both sides for `delete_content_file` + `move_content_file`
    // (structural content ops: propagate delete + rename across the sync boundary).
    // SW-EW-9: +2 on both sides for `read_epic_repos` + `set_epic_repos`
    // (epic-node repo management; epic doc `repos:` is the source of truth).
    // PT-1: +3 on both sides for `subscribe_terminal` + `unsubscribe_terminal` +
    // `session_is_idle` (sovereign-PTY substrate: viewport attach/detach + the
    // idle/running primitive).
    // PT-5: +1 on both sides for `get_terminal_cwd` (OSC 7 cwd capture readout).
    // SW-CP-18: -1 on both sides — `capture_pending_files` retired (the
    // source→context walk is subsumed by the reverse engine + claim/import).
    // PT-8: +2 on both sides for `attach_warm_pty` (warm-pool handoff /
    // new-terminal entry) + `reconfigure_warm_pool` (pool size + spawn params).
    // PT-6: +4 on both sides for `serialize_sessions` + `list_restorable_sessions`
    // + `read_session_snapshot` + `set_terminal_persistence_enabled` — ALL retired in
    // SW-EW-46 (see the -4 below); the net PT-6 contribution to the count is now 0.
    // SW-GS-2: +1 on both sides for `get_publish_set` (the session-less picker
    // transport, F4). The other five publish/pull sync-engine verbs are MCP-only
    // — no Tauri command, so they do not count here.
    // SW-GS-2a: +1 on both sides — `session_has_live_agent` (true agent-process
    // liveness probe, AC-12). SW-CR-6 retired `ensure_git_sync_provisioned`
    // along with the git-sync agent kind.
    // The launcher, role-binding, guarded injection and env guard are FE-only / a
    // param on the existing `spawn_pty` — no further command count change.
    // SW-GS-6: +1 for `incoming_status` (the out-of-sync behind-count probe).
    // SW-GS-5: +1 for `pull_ff` (the authenticated fast-forward Pull button;
    // distinct from the legacy `pull_context_repo`).
    // SW-AW-4: +1 on both sides — `resolve_ask_user` (the FE→Rust reply leg that
    // unblocks the `ask_user` oneshot; `signal_done` is emit-only, no command).
    // SW-GS-8: +1 on both sides for `publish_node` (node-level commit + push to
    // origin/main; the shared `commit_and_push_paths` core's Tauri door).
    // SW-SN-1: +4 on both sides — plural notes model: -2 (`save_scratch_note`,
    // `load_scratch_note`) +6 (`list_notes`, `read_note`, `create_note`,
    // `write_note`, `update_note_meta`, `delete_note`).
    // SW-SN-7: +2 on both sides — on-demand notes agent: `notes_dir_path` (agent
    // cwd) + `ensure_notes_provisioned` (drops the CLAUDE.md prime).
    // SW-GS-19: +2 on both sides — in-UI PR review: `list_prs_for_review` +
    // `approve_and_merge_pr`. (The PR-sourced diff rides `get_publish_set` /
    // `get_file_redline` via an optional `pr_number`, adding no command.)
    // SW-GS-13: +1 on both sides — sync History panel: `get_sync_history`. (The
    // commit-sourced diff rides `get_publish_set` / `get_file_redline` via an
    // optional `commit_sha`, adding no command.)
    // SW-SN-11: +1 on both sides — `search_notes` (case-insensitive body/content
    // scan backing the ⌘O switcher's body matches; id + matched-line snippet).
    // SLW-FX-3: +1 on both sides — `reconcile_stuck_worktrees` (event-driven
    // in-session self-heal of a worktree stranded `Pending`; stuck-only filter
    // over the launch reconcile so frequent triggers don't storm the healthy set).
    // SW-GS-27: -1 on both sides — removed the orphaned `pull_context_repo`
    // command (destructive auto-pull caller deleted; `pull_ff` is the only pull).
    // SW-GS-23: +1 on both sides — `submit_pty_line` (agent-agnostic reliable line
    // submit: settle → write → confirm-from-output → discrete-CR nudge).
    // SLW-NC-4: +2 on both sides — `get_surface_scope` + `set_surface_designation`
    // (nav-driven per-surface designation read + write over UD-2's writer).
    // SLW-SS-1: +1 on both sides — `list_conflicts` (on-mount enumeration of open
    // reverse-sync conflicts for the resolve surface, over `ConflictRegistry::list()`).
    // SLW-UD-6: +1 on both sides — `get_dir_file_ownership` (per-file owned/
    // source-only/managed read for the nav's expanded-dir legibility).
    // SW-LD-4: -6 on both sides — removed the embedded Atlassian OAuth 3LO
    // commands (start/complete/poll/cancel_jira_auth, get_jira_issue,
    // heartbeat_jira) with the client secret. Intentional drop; Atlassian
    // returns as a BYO-token passthrough in SW-LD-5 (not OAuth).
    // SW-OB-10: +2 on both sides — `switch_github_account` + `remove_github_account`
    // (multi-account active-switch + per-account sign-out; add-account reuses the
    // existing device flow, so no third command).
    // SW-OB-12: +1 on both sides — `recheck_dependencies` (forces a full dep
    // re-resolution + emits `dep-status-changed` for the guidance re-check;
    // the file watcher can't see a PATH-installed `claude`).
    // SW-OB-12 (Claude-agnostic): -3 on both sides — codesense no longer manages
    // the Claude binary or its login. Removed `install_claude`, `check_claude_auth`,
    // `sign_out_claude`. Detection + git's managed install are untouched.
    // SLW-WD-1: +2 on both sides — `source_worktree_changeset` + `source_file_redline`
    // (the source worktree diff data for the WD-2 review overlay). Read-only, no MCP.
    // SW-CN-UX-11: -1 on both sides — the declared Jira connector now uses the
    // generic set_connector_credential command; the provider-specific command was retired.
    // SLW-MD-1: +3 on both sides — `get_metadata_model`, `apply_metadata_edit`,
    // `list_source_repos_for_team` (the in-file frontmatter metadata editor's
    // read-model / write / configured-repos commands).
    // SLW-WD-4: +2 on both sides — `route_file_to_context` + `unroute_file_from_context`
    // (the app-side bridge to WD-3's route/un-route verbs, powering the source-diff
    // overlay's "→ context" affordance; shares one orchestration with the MCP tools).
    // SW-HB-1: +1 on both sides — `detect_team_setup_progress` (Home Base read spine:
    // the four-stage disk-derived team setup progression).
    // SLW-NC-7: +2 on both sides — `set_team_nav_pin` + `get_team_nav_pins` (team-shared
    // nav pins: write/read the `pin` list in the git-shared `designations.yml`).
    // SW-SN-13: +1 on both sides — `reap_empty_scratch_notes` (AC-4: one-shot guarded
    // cleanup of the empty duplicate "Scratch" notes the SN-12 lazy-create minted).
    // SW-FX-52: +1 on both sides — `sessions_have_live_agent` (batched agent liveness:
    // one process-table snapshot for N sessions, replacing N per-session pollers).
    // SLW-WDE-2: +1 on both sides — `write_worktree_file` (in-app editor ⌘S save; a
    // NON-suppressed atomic worktree write, editable counterpart to `write_content_file`).
    // SLW-WDE-3: +2 on both sides — `lazy_attach_worktree_watch` + `detach_worktree_watch`
    // (lazy per-inset `notify` watch rooted at `ws.cwd`, emitting `worktree-file-change`;
    // the observer half of WDE-2's non-suppressed write — external-change reload/dirty-warn).
    // SW-LT-4: +1 on both sides — `promote_content_entry` (promote a materialized
    // install file to another team in the inheritance chain; relocates the source
    // + rewrites the lockfile `source_path` + best-effort forward re-fan).
    // SW-SD-2: +1 on both sides — `analyze` (account/CLI-agnostic headless
    // agent-runner bridge verb: {prompt,data} → configured CLI on the blocking
    // pool with a single-flight latch → analysis text or a typed error).
    // SLW-WD-8: +3 on both sides — `source_commit_graph` (all-refs paginated commit
    // graph), `source_commit_changeset` (flat per-commit diff), and
    // `source_commit_file_redline` (per-commit redline): the worktree commit-history
    // data for the WD-9 graph overlay. Read-only source-git verbs, FE-only consumers.
    // SW-DX-1: +1 on both sides — `get_tag_schema` (read seam returning the
    // org-global team tag registry; extends the SLW-MD metadata surface, no MCP
    // registration — FE-only consumers via the metadata band + DX-2/DX-4).
    // SW-DX-7: +6 on both sides — the dashboard execution loop's trusted-side
    // broker (non-MCP): `dashboard_launch_begin` (SpawnCap slot), `dashboard_launch_bind`
    // (storyId↔sessionId stamp), `dashboard_launch_abort` (free the slot on the FE
    // failure path), `dashboard_resolve_session` (forgery-proof fire target),
    // `preflight_mcp_json` (AC-4 .mcp.json gate), `resolve_story_path` (AC-11
    // storyId→managed path). No new spawn path / MCP verb / agent runtime — the
    // launch reuses the agent-session-launcher.
    // SW-HV-1: +2 on both sides — the Hydration overlay's `read_worktree_hydration`
    // (read-only three-way DTO projected from SW-HV-5's shared
    // `read_worktree_hydration_core`) and `hydrate_epic_into_worktree` (the one
    // write, delegating to SW-SY-5's `run_hydrate`). No new MCP verb.
    // SW-HV-2: +1 on both sides — `dehydrate_epic_from_worktree` (the release half,
    // delegating to SW-SY-6's `run_dehydrate` engine). Still no new MCP verb
    // (`dehydrate_story` shipped in SW-SY-6). → 201/204.
    // SW-HV-6: +4 on both sides — hydration scope policies add
    // `preview_hydration_closure` (read-only closure preview),
    // `get_hydration_policy`, `set_worktree_hydration_policy`, and
    // `set_team_hydration_policy` (config read/write). The write stays the
    // WIDENED `hydrate_epic_into_worktree` (optional `policy?`) — no second write.
    // → 205/208.
    // SW-VE-7: +1 on both sides — `ingest_file_into_context` (drag-drop OS-file
    // ingest: server-side byte copy through the context write core). → 206/209.
    // SW-DX-11: +1 on both sides — `read_workspace_file` (the `readWorkspace`
    // bridge verb: a doc-scoped, managed-confined file read/dir-list). → 207/210.
    // SW-DX-11: +1 on both sides — `run_skill` (the `runSkill` bridge verb: the
    // write-capable, tool-enabled, multi-turn sibling of `analyze`). → 208/211.
    // SW-DX-11: +1 on both sides — `dashboard_register_session` (cap-free register
    // of an app-opened epic/context terminal into the generalized handle
    // registry, so `sendToSession` can address it). → 209/212.
    // SW-DX-17a: +2 on both sides — `dashboard_list_sessions` (enumerate live
    // agent sessions incl. tabless ones) + `dashboard_kill_by_handle` (kill by
    // handle through the one `kill_session` choke). → 211/214.
    // SW-DX-16: +1 on both sides — `resolve_epic_path` (trusted-side `{team, epic}`
    // → the epic's canonical mainDoc, widening the `openWorkspace` epic target to
    // ANY managed epic). → 212/215.
    // SW-FX-64: +1 on both sides — `renderer_heartbeat` (the FE liveness ack for
    // the webview renderer watchdog; hosted in the top-level `renderer_watchdog`
    // module, not `commands::`). → 213/216.
    // SW-DX-18: +1 on both sides — `ensure_branch_workspace` (check out an
    // arbitrary branch into a managed worktree + install layer, for the
    // `openWorkspace` `branch` target). → 214/217.
    // SW-GW-3: +1 on both sides — `dashboard_probe_sessions_alive` (arbitrary-
    // session `try_wait` liveness for the OBSERVE-half `getWorkspaceStatus` verb;
    // `dashboard_list_sessions` is registry-filtered and can't see a manually-
    // opened story's session). → 215/218.
    // RF-1b: -3 on both sides — retired the vestigial `.meta`/sortOrder commands
    // `read_dir_meta` / `write_dir_meta` / `reconcile_meta` (written, never read
    // for ordering). → 212/215.
    // RF-2: -3 on both sides — deleted the dead `config-lock.yml` command surface
    // `read_lockfile` / `write_lockfile_entry` / `remove_lockfile_entry` (no app
    // caller). -1 Rust only — `verify_installation` (deprecated, RUST_ONLY, no TS
    // wrapper). → 209/211.
    // RF-8: -4 on both sides — retired the unwired command surface `get_app_temp_dir`
    // + `get_terminal_cwd` + `walk_user_picked_root` + `cleanup_temp_path` (spec'd,
    // never wired; zero FE/MCP/agent caller). `session_cwd()` mirror kept; `temp_dir()`
    // helper removed with its sole consumer `cleanup_temp_path`. → 205/207.
    // SW-WB-1: +1 on both sides — new `switch_worktree_branch` (migrate-in-place
    // worktree reconcile) with a TS wrapper. → 206/208.
    // SW-SO-2 (③): net-zero swap on both sides — retired `has_worktree_config`,
    // added `probe_worktree_config` (fused status+config read for the open path;
    // `get_worktree_config` survives for its other callers). → 206/208 (unchanged).
    // SW-DX-17d: +3 on both sides — `dashboard_detach_session` /
    // `dashboard_foreground_session` / `dashboard_set_background_limit` (the attach⇄detach
    // verb pair + the user-owned background-agent limit). → 209/211.
    // SLW-WL-1: +1 on both sides — `set_auto_teardown_enabled` (FE→Rust push gating
    // the merge→teardown poll) with a TS wrapper. → 210/212.
    // SLW-WL-2: +1 on both sides — `list_managed_worktrees` (enriched management view
    // for the right-pane Worktrees panel) with a TS wrapper. → 211/213.
    // SLW-WL-5: +1 on both sides — `set_reverse_sync_overrides` (FE→Rust push of the
    // per-worktree reverse-sync override map) with a TS wrapper. → 212/214.
    // SLW-WL-8: +1 on both sides — `reverse_sync_now` (manual one-shot reverse
    // catch-up; thin wrapper over the existing AttachReconcile enqueue) with a TS
    // wrapper. → 213/215.
    // RF-perf-22: +1 on both sides — `get_out_of_date_surfaces` (FE pull that seeds
    // per-window sync chips on mount; read-only wrapper over
    // `hydration::out_of_date_paths`) with a TS wrapper. → 214/216.
    // SR-3: +2 on both sides — `set_worktree_cap` (FE→Rust push of the `maxWorktrees`
    // cap) + `ensure_worktree_capacity` (pre-flight admission gate that auto-reaps
    // reapable worktrees to make room), each with a TS wrapper. → 216/218.
    // SR-7: +1 on both sides — `list_live_sessions` (live-session map is the source of
    // truth for re-attaching terminals after a watchdog reload), with a TS wrapper. → 217/219.
    // SR-4: +1 on both sides — `cleanup_eligible_worktrees` (user-driven one-click bulk
    // cleanup of merged+clean worktrees) with a TS wrapper. → 218/220.
    // SW-BA-4: +1 on both sides — `dashboard_stop_all` (the global Stop-all safety net:
    // halt every unattended background agent through the one `kill_session` choke) with
    // a `dashboardStopAll` TS wrapper. → 220/222.
    // SW-AN-10: +3 on both sides — the user-local launch-profile authoring CRUD and merged
    // view (since retired), each with a TS wrapper. → 223/225.
    // SW-AN-5: +3 on both sides — the MCP integration surface (`mcp_status`,
    // `mcp_wire_assisted`, `mcp_unwire`), each with a TS wrapper. → 226/228.
    // SW-AN-6: +1 on both sides — `list_agent_packs` (the read-only pack→FE bridge: the
    // frontend reads agent identity/profiles from the installed pack registry instead of
    // a hardcoded `"claude"`) with a `listAgentPacks` TS wrapper. → 227/229.
    // SW-AN-7: +1 on both sides — `resolve_launch_profile` (resolve a dashboard-selected
    // launch profile → binary+args+escalation verdict) with a `resolveLaunchProfile` TS
    // wrapper. → 228/230.
    // SW-SB-2: +1 on both sides — `load_sprint_registry` (per-team sprint definition
    // loader: globs `{team}/content/sprints/*.md`, resolves the nullable active sprint)
    // with a `loadSprintRegistry` TS wrapper. → 229/231.
    // SW-GT-1: +1 on both sides — `load_status_taxonomy` (per-team status taxonomy
    // loader: reads `{team}/content/status-taxonomy.yml`, resolves ordered role-keyed
    // states + soft drift) with a `loadStatusTaxonomy` TS wrapper. → 230/232.
    // SW-ID-3: +1 on both sides — `refresh_person_profile` (re-pull a roster member's
    // name + avatar from GitHub, write people.yml local-only, return the refreshed
    // Person) with a `refreshPersonProfile` TS wrapper. → 231/233.
    // SW-AN-14: +3 on both sides — the agent-pack install engine (`install_agent_pack`,
    // `uninstall_agent_pack`, `get_pack_manifest`), each with a TS wrapper
    // (`installAgentPack`/`uninstallAgentPack`/`getPackManifest`). → 234/236.
    // SW-GT-6: +5 on both sides — the phase & transition config editor surface:
    // `load_transitions` / `load_field_schema` (per-team loaders), `save_team_config`
    // (persist an edited taxonomy/transitions/field-schema), `validate_team_run_metadata`
    // (referenced-vs-declared field report), and `preview_prompt` (resolve a prompt
    // against sample values) — each with a TS wrapper. → 239/241.
    // SW-SB-24: +1 on both sides — `save_sprint_registry` (atomic read-modify-write of
    // the relocated `{team}/config/sprints.yaml` ledger, returns the reloaded registry)
    // with a `saveSprintRegistry` TS wrapper. → 240/242.
    // SW-AN-11: +1 on both sides — `install_agent_pack_from_path` (file-picker
    // install: reads the picked path off-thread) with an `installAgentPackFromPath`
    // TS wrapper. → 241/243.
    // SW-AN-18: +1 on both sides — `export_agent_pack` (rebuild an installed team
    // pack into a shareable `.tar.gz` off-thread) with an `exportAgentPack` TS
    // wrapper. → 242/244.
    // SW-AN-16: +1 on both sides — `set_agent_enabled` (user-local enable/disable
    // toggle) with a `setAgentEnabled` TS wrapper. → 243/245.
    // SW-AN-13: net 0 on both sides — REMOVE the dead bytes-in `install_agent_pack` /
    // `installAgentPack` (both the file-picker AND drag-drop install from a path) and
    // ADD `peek_agent_pack_manifest` / `peekAgentPackManifest` (pre-install manifest
    // peek). → 243/245 unchanged.
    // SW-WM-3: +1 on both sides — `reconcile_orphaned_worktree` (parent-gone orphan
    // reconcile) with a `reconcileOrphanedWorktree` TS wrapper. → 244/246.
    // SW-AN-15: +3 on both sides — the pack authoring backend surface under the
    // content model (validate_pack_manifest, probe_binary_on_path,
    // save_pack_manifest) each with a TS wrapper. Export is delegated to
    // SW-AN-18's export_agent_pack; the draft-scratch commands are retired. → 247/249.
    // SW-WM-4: +3 on both sides — `set_worktree_idle_threshold`, `set_worktree_pins`,
    // `reap_worktrees_now` (reap-policy config: idle-threshold + pinned-set FE pushes and
    // on-demand cap enforcement) with matching TS wrappers. → 250/252.
    // SW-AN-12: +1 on both sides — `list_pack_payload_content` (walk the pack-payload
    // content home, incl. dotfiles, for the editor's
    // payload selector) with a `listPackPayloadContent` TS wrapper. → 251/253.
    // SW-SR-2: +1 on both sides — `get_ready_queue` (the trusted ready-queue
    // read-model / SW-SR-3 dispatch source) with a `getReadyQueue` TS wrapper. → 252/254.
    // SW-FX-71: +1 on both sides — `resnapshot_terminal` (read-only replay-buffer
    // snapshot for the show-from-hidden resume rehydrate; no new subscriber/stream)
    // with its TS wrapper. → 253/255.
    // SW-SR-3: +1 on both sides — `dispatch_ready_stories` (the trusted parallel CLI
    // dispatch tick) with a `dispatchReadyStories` TS wrapper. → 254/256.
    // SW-FX-72: +0 — payload widening only, NO new command. `resnapshot_terminal`
    // gains an optional `since` arg and returns `{ data, reset, head }` (contiguous
    // delta vs full-tail reset); `SubscribeResponse` gains `appended_seq`; the live
    // `pty-output-stream-{id}` / global output events gain `seq`. Count stays 254/256.
    // SW-SR-11: +1 on both sides — `dispatch_epic_whole` (the trusted whole-epic
    // single-dispatch: one worker rooted at the hydrated epic dir) with a
    // `dispatchEpicWhole` TS wrapper. → 255/257.
    // SW-SR-5: +4 on both sides — the run-control commands `get_sprint_run_state`,
    // `pause_sprint_run`, `resume_sprint_run`, `stop_sprint_run` with their TS wrappers.
    // (`send_to_sessions` is a trusted primitive, not a webview command → no bump.) → 259/261.
    // SW-CP-24: +2 on both sides — `set_content_merge_mark` + `clear_content_merge_mark`
    // (the merge-mark set/clear seam) with their `setContentMergeMark` /
    // `clearContentMergeMark` TS wrappers. → 261/263.
    // SW-SR-12: +2 on both sides — the run-control-policy commands `get_sprint_run_held` and
    // `approve_sprint_step` (the human-review approval gate) with their TS wrappers. → 263/265.
    // SW-GT-8: +1 on both sides — the `assert_validation` door (IPC command +
    // `assertValidation` TS wrapper; the MCP tool is counted by EXPECTED_MCP_TOOLS,
    // not here). → 264/266.
    // SW-MC-1: +1 on both sides — `clone_and_attach_source_repo` (managed clone +
    // auto-attach), rebased onto main's 264/266. → 265/267.
    // SW-CP-27: +1 on both sides — `list_content_merge_marks` (the merge-mark read
    // seam) with its `listContentMergeMarks` TS wrapper. → 266/268.
    // SW-GT-13: +2 on both sides — `override_status_advance` (reasoned human override)
    // and `sprint_run_gate_state` (query-by-story_id read model over the run/verdict
    // store) with their `overrideStatusAdvance` / `sprintRunGateState` TS wrappers.
    // → 268/270.
    // SW-AN-24: +1 on both sides — `preview_uninstall_agent_pack` (the non-mutating
    // uninstall blast-radius) with its `previewUninstallAgentPack` TS wrapper. → 269/271.
    // SW-CP-28: +2 on both sides — `get_projection_excludes` / `set_projection_excludes`
    // (the team-level projection-exclude read/write seam) with their
    // `getProjectionExcludes` / `setProjectionExcludes` TS wrappers. → 271/273.
    // SW-AN-29: +1 on both sides — `preview_install_agent_pack` (the non-mutating
    // install-time overlap preview) with its `previewInstallAgentPack` TS wrapper. → 272/274.
    // SW-GT-11: +1 on both sides — `resolve_active_step_dag` (the resolved full step-DAG
    // skeleton the gate drawer renders) with its `resolveActiveStepDag` TS wrapper. → 273/275.
    // SW-AN-28: +1 on both sides — `stage_bundled_starter_pack` (materialize the bundled
    // starter to a temp path for onboarding import) with its `stageBundledStarterPack`
    // TS wrapper. → 274/276.
    // SW-GT-21: +1 on both sides — `run_epic_action` (run a generalized named action across an
    // epic's stories via the shared reactor tick) with its `runEpicAction` TS wrapper. → 275/277.
    // SW-CP-29: +2 on both sides — `list_merge_preview_repos` / `preview_merge_marked_file`
    // (the repo-scoped merge-preview selector + read-only compute seam) with their
    // `listMergePreviewRepos` / `previewMergeMarkedFile` TS wrappers. → 277/279.
    // SW-GT-28: +1 on both sides — `resolve_validate_ref_provenance` (team-level validator-slot
    // provenance for the config editor "Edit door", read-only + off-thread) with its
    // `resolveValidateRefProvenance` TS wrapper. → 278/280.
    // SW-CP-30: −3 on both sides — retired `mcp_status` / `mcp_wire_assisted` / `mcp_unwire`
    // (the bespoke MCP-install UI; each launch now brings the bridge in its own session config)
    // with their `mcpStatus` / `mcpWireAssisted` / `mcpUnwire` TS wrappers. → 275/277.
    // Git de-managed: −1 on both sides — retired `install_git` (codesense no longer
    // downloads/pins a managed git binary; it detects the user's system git, mirroring
    // how SW-OB-12 detects `claude`) with its `installGit` TS wrapper. → 274/276.
    // SW-OB-14 git nudge: +1 on both sides — `install_command_line_tools` (triggers
    // Apple's `xcode-select --install`, which provides git) with its
    // `installCommandLineTools` TS wrapper. → 275/277.
    // SW-CN-1 connectors: +4 on both sides — `register_connector` / `list_connectors` /
    // `set_connector_credential` / `connector_test` with their TS wrappers. → 279/281.
    // SW-CN-2 pull: +1 on both sides — `connector_poll_delta` (the deduped delta read
    // seam) with its `connectorPollDelta` TS wrapper. → 280/282.
    // SW-CN-4 credential UX: +1 on both sides — `list_connections` (per-connector
    // Connected status, AC-6) with its `listConnections` TS wrapper. → 281/283.
    // SW-AU-5 watch-state: +1 RUST ONLY — `automation_watch_states` (the watch-state
    // read-model source; its FE consumer AU-3 is not wired yet, so no TS wrapper — see
    // RUST_ONLY_COMMANDS). → 281/284.
    // SW-CN-7 derived: +2 on both sides — `preview_derived_transform` (the derived-op
    // authoring preview) and `get_connector` (load a full definition for the authoring
    // surface) with their `previewDerivedTransform` / `getConnector` TS wrappers. → 283/286.
    // SW-AU-9 run-control intents: +1 on both sides — `is_runner_available` (the FE-facing AD-10
    // capability probe) with its `isRunnerAvailable` TS wrapper. → 284/287.
    // SW-AU-3 pane wires AU-5: +1 TS ONLY — added the `automationWatchStates` wrapper for the now
    // Rust-only `automation_watch_states` and removed it from RUST_ONLY_COMMANDS, so it counts as a
    // normal command on both sides (Rust total unchanged; TS +1). → 285/287.
    // SW-AU-3b automation read-model: +2 on both sides — `list_automations` + `automations_matching`
    // (the by-Automation pivot source + the inverse selector query) with their `listAutomations` /
    // `automationsMatching` TS wrappers. → 287/289.
    // SW-AU-8 create-flow: +4 on both sides — `upsert_automation_into_pack` / `list_pack_workflows` /
    // `validate_automation_target` / `agent_probe_floor_secs` (the create-flow's write, Do-picker
    // enumeration, forward selector validation, and GT-35 cost floor) with their
    // `upsertAutomationIntoPack` / `listPackWorkflows` / `validateAutomationTarget` /
    // `agentProbeFloorSecs` TS wrappers. → 291/293.
    // SW-AU-11 edit-flow: +1 on both sides — `automation_for_edit` (the record-edit pre-fill fetch)
    // with its `automationForEdit` TS wrapper. → 292/294.
    // SW-AU-12 lifecycle: +2 on both sides — `remove_automation_from_pack` (structured entry-delete)
    // and `duplicate_automation_into_pack` (variant + COW fork), with their `removeAutomationFromPack`
    // / `duplicateAutomationIntoPack` TS wrappers. → 294/296.
    // SW-AU-14 multi-automation management: +1 on both sides — `set_sprint_automation_mute` (the
    // per-sprint-instance mute write) with its `setSprintAutomationMute` TS wrapper. → 295/297.
    // SW-AU-32 dispatch dry-run UI: the `dispatchDryRun` wrapper makes `dispatch_dry_run` a normal
    // both-sides command (no longer in RUST_ONLY_COMMANDS). → 296/298.
    // SW-AU-15 workflow management: +5 on both sides — `list_workflow_library` + `workflow_used_by`
    // (library + used-by read-model) and `duplicate_workflow_into_pack` / `remove_workflow_from_pack`
    // / `rename_workflow` (lifecycle), with their `listWorkflowLibrary` / `workflowUsedBy` /
    // `duplicateWorkflowIntoPack` / `removeWorkflowFromPack` / `renameWorkflow` TS wrappers. → 301/303.
    // SW-MC-7 mode conversion & repair: +1 on both sides — `reclone_managed_source_repo` with its
    // `recloneManagedSourceRepo` wrapper (re-clone a Gone managed clone into its recorded path). The
    // two mode-flips add no command (they reuse `save_source_repo_attachment`). → 302/304.
    // SW-MC-3 managed-clone lifecycle: +3 on both sides — `update_managed_clone` /
    // `remove_managed_clone` / `list_managed_clone_health` with their `updateManagedClone` /
    // `removeManagedClone` / `listManagedCloneHealth` TS wrappers. → 305/307.
    // SW-MC-5 workspace reconcile: +2 on both sides — `reconcile_workspace` (executor) and
    // `preview_workspace_reconcile` (read-only dry-run) with their `reconcileWorkspace` /
    // `previewWorkspaceReconcile` wrappers (scan → attach-found / clone-missing / re-home-moved). → 307/309.
    // SLW-PT-6 personal-team front door: +3 on both sides — `create_personal_team` /
    // `stop_using_personal_team_in_teams` / `get_personal_team_status` with their
    // `createPersonalTeam` / `stopUsingPersonalTeamInTeams` / `getPersonalTeamStatus` TS wrappers.
    // → 310/314.
    // SW-AU-39 added the two AU-36 TS wrappers (`workflowForEdit` / `saveWorkflowIntoPack`) and
    // removed them from RUST_ONLY, so the TS count catches up to Rust. → 312/314.
    // SLW-PT-3 provider promote: +2 on both sides — `promote_provider_item` (preview-first,
    // keep-source copy of a provider item into a shared team) + `list_provider_content_items`
    // (the read-only lister behind the card picker), with their `promoteProviderItem` /
    // `listProviderContentItems` TS wrappers. → 314/316.
    // SW-AU-16 history-preserving move: +1 on both sides — `move_pack_entry` (relocate one
    // automation/workflow entry between team packs keeping its id) with its `movePackEntry` TS
    // wrapper. → 315/317.
    // SW-AU-21 knowledge pack: +1 on both sides — `stage_bundled_automation_authoring_pack`
    // (stage the optional automation-authoring bundled pack for install) with its
    // `stageBundledAutomationAuthoringPack` TS wrapper. → 316/318.
    // SW-AU-22 reactive test-fire: +1 on both sides — `test_fire_event` (inject a synthetic reactive
    // event onto the real dispatch entrypoint) with its `testFireEvent` TS wrapper. The Rust command
    // shipped in #979 without a TS wrapper; the wrapper + this count reconcile were added in the
    // SW-AU-38 rebase-cleanup. → 317/319.
    // SW-MC-4 re-home: +2 on both sides — `preview_managed_clone_rehome` (read-only divergence
    // detection) + `rehome_managed_clones` (the scoped move) with their `previewManagedCloneRehome`
    // / `rehomeManagedClones` TS wrappers. → 319/321.
    // SW-AU-40 join layer: +1 on both sides — `list_triggers` (the reused-by grouping over trigger
    // identity, the ⋯ views Triggers list) with its `listTriggers` TS wrapper. → 320/322.
    // SW-AU-41 trigger inspect + test-probe: +1 on both sides — `test_probe` (run a trigger's authored
    // shell probe once, by key, off-thread) with its `testProbe` TS wrapper. → 321/323.
    // SW-AU-49 last-run strip: +1 on both sides — `automation_run_steps` (one run's per-step verdicts,
    // run-scoped) with its `automationRunSteps` TS wrapper. → 322/324.
    // SW-AN-39 personal scratch pack: +1 on both sides — `upsert_automation_into_personal_scratch`
    // (mint-on-first-save into the personal team) with its `upsertAutomationIntoPersonalScratch` TS
    // wrapper. → 323/325.
    // SW-AU-34 background-on-close: +1 on both sides — `set_quit_on_close` (FE→Rust mirror of the
    // quit-on-close preference the window-close handler reads) with its `setQuitOnClose` TS wrapper.
    // → 324/326.
    // SW-AU-42 shared named triggers: +8 on both sides — the `watches:` authoring seam
    // (list_watch_library, watch_used_by, save_watch_into_pack, watch_for_edit, duplicate_watch_into_pack,
    // remove_watch_from_pack, rename_watch, promote_automation_trigger) with their TS wrappers. → 332/334.
    // SW-AN-42 MCP scope authoring: +2 on both sides — a worktree MCP-server read for the picker
    // (since retired) + project_mcp_scope_config, with their TS wrappers. → 334/336.
    // SW-OR-10 headless launch route: +1 on both sides — dashboard_launch_headless with its
    // `launchHeadlessAgent` TS wrapper. → 335/337.
    // NOTE: the Rust total here (337) is intentionally one MORE than `commands/mod.rs`
    // EXPECTED_COMMAND_COUNT (336): that constant excludes one handler this contract test counts. The
    // two are separate denominators with a standing +1 offset — every story bumps both by the same
    // delta, so they stay coherent; the raw numbers differ by design, not by a typo.
    // SW-WG-6 widget grant ledger: +3 on both sides — the load, grant and revoke ledger
    // commands with their TS wrappers. → 338/340.
    // SW-CN-23 auth-code loopback: +1 Rust only — start_auth_code_flow (backend-first,
    // RUST_ONLY_COMMANDS; TS wrapper lands with SW-CN-UX-7). → 338/341.
    // SW-CN-UX-3 catalog read: +1 on both sides — list_data_sources (the FE-facing read of the
    // ONE CN-15 catalog, wrapping the existing enumerate_data_sources) with its `listDataSources`
    // TS wrapper. → 339/342.
    // SW-WG-8 team-chain widget discovery: +1 on both sides — list_team_widgets (the sole
    // chain-walk widget discovery+validation authority) with its `listTeamWidgets` TS wrapper. → 340/343.
    // SW-CN-UX-12 connector-def authoring: +2 on both sides — save_connector_into_pack +
    // list_editable_connectors with their `saveConnectorIntoPack` / `listEditableConnectors` TS
    // wrappers. → 342/345.
    // SW-OR-12 agent-result reads: +2 on both sides — read_run_outcome + read_run_artifact with
    // their `readRunOutcome` / `readRunArtifact` TS wrappers. → 344/347.
    // SW-CN-UX-2 connection management: +3 on both sides — revoke_connector + delete_connector
    // + rename_connector (FE-reachable wrappers over the SW-AU-28 cores; Rust handlers already
    // registered) with their `revokeConnector` / `deleteConnector` / `renameConnector` TS
    // wrappers. → 347/350.
    // SW-CN-UX-14 edit-in-place: −1 on both sides — list_editable_connectors + its
    // `listEditableConnectors` wrapper removed (owning-pack provenance on the list_data_sources
    // catalog read now drives the Edit gate). → 346/349.
    // SW-CN-W2a connector-write outcome: +2 on both sides — describe_write_op (trusted confirm copy)
    // + extract_write_reason (Rust-side blocked-reason extraction) with their `describeWriteOp` /
    // `extractWriteReason` TS wrappers. → 348/351.
    // RF-perf-24 adds NO new command (reverse-sync flush/preview are MCP tools, not IPC commands;
    // the awaited-reap-flush command was reverted — SLW-RS-4 teardown flush already covers removal).
    // SW-CN-W4 distinct write capability: +1 on both sides — mint_connector_write_cap (webview-only
    // mint of the single-use per-write consent cap the proxy requires) with its `mintConnectorWriteCap`
    // TS wrapper. → 349/352.
    // SW-CN-W3 connector-write widget: +1 on both sides — extract_write_url (Rust-side result-URL
    // extraction for the Written outcome's "View ↗") with its `extractWriteUrl` TS wrapper. → 350/353.
    // RF-perf-25: +1 on both sides — set_reverse_sync_default (FE→Rust push of the global
    // reverse-sync default: On-demand/Always/Off) with its `setReverseSyncDefault` TS wrapper. → 351/354.
    // SW-AU-60 status-transition authoring: +1 on both sides — save_status_transition (persist a
    // status-transition automation onto `pipeline.yml`) with its `saveStatusTransition` TS
    // wrapper. → 352/355.
    // SW-CN-UX-13 rejected-connector diagnostics: +1 on both sides — list_rejected_connectors (the
    // negative-space read) with its `listRejectedConnectors` TS wrapper. → 353/356.
    // SW-CN-UX-7 OAuth connect flows: +1 TS only — start_auth_code_flow already existed Rust-side
    // (SW-CN-32, allowlisted as Rust-only); its `startAuthCodeFlow` TS wrapper lands with the
    // 3-legged connect card, so it leaves RUST_ONLY_COMMANDS and the TS count rises to match. → 354/356.
    // SW-CN-UX-21 fix-a-rejected-connector: +1 on both sides — get_rejected_connector_def (raw-read a
    // rejected def from its drop-file for the author-form prefill) with its `getRejectedConnectorDef`
    // TS wrapper. → 355/357.
    // SW-WG-7 capstone pack: +1 on both sides — `stage_bundled_story_review_pack` (stage the
    // bundled Story Review Tray widget dashboard pack for install) with its
    // `stageBundledStoryReviewPack` TS wrapper. → 356/358.
    // SW-FG-4 people-manifest migration: +1 on both sides — its Rust handler bumped the
    // Rust-side `EXPECTED_COMMAND_COUNT` (357→358) but left these two snapshots stale; its
    // TS wrapper lands here too. Reconciled to reality (the TS==effective-Rust contract
    // above already passes). → 357/359.
    // SW-PO-7 legibility badge: +1 on both sides — resolve_pack_provenance (the read-only
    // Managed/Overridden/Hand-authored map, keyed by absolute path) with its `resolvePackProvenance`
    // TS wrapper. → 358/360.
    // Rebase reconcile (SW-PO-7 onto main): SW-FG-2b's `rename_account` handler landed on
    // main with no TS wrapper (contract was red there); this branch adds the `renameAccount`
    // wrapper, so TS rises to 359 and matches effective-Rust (raw 361 − 2 Rust-only). → 359/361.
    // SW-CN-UX-24 active-cancel: +1 on both sides — cancel_auth_code_flow (drop a still-pending
    // auth-code flow on Cancel by state, instead of waiting out the TTL) with its `cancelAuthCodeFlow`
    // TS wrapper. → 360/362.
    // Rebase reconcile (SW-WG-21 onto main): SW-FG-10a's `reconcile_required_providers` handler landed
    // on main with no TS wrapper (contract was red there); this branch adds the `reconcileRequiredProviders`
    // wrapper, so both sides rise by 1 and match again. → 361/363.
    // SW-CN-UX-4 multi-account management: +2 on both sides — revoke_account / set_default_account
    // (thin generic per-account wrappers over the shipped remove_account / switch_active_account) with
    // their `revokeAccount` / `setDefaultAccount` TS wrappers. → 363/365.
    // SW-CN-UX-5: +1 on both sides — `list_accounts` (the per-connector account catalog for the binder's
    // Selector A + the acting-as chip) with its `listAccounts` TS wrapper. → 364/366.
    // SW-CN-UX-11: +1 on both sides — a renderer begins a native credential-write
    // session before its first ordered manual credential mutation. → 364/366
    // (the source-level Rust count includes the invoke-handler macro sentinel).
    // Iteration-7 device cancellation adds the connector-attempt authority command.
    // SW-AU-55: +1 on both sides — one batched durable run-history read.
    // SW-AU-66: +1 on both sides — exact projected transition deletion.
    // SW-CN-UX-26: +1 on both sides — register a widget from a connector binding.
    // SW-CN-UX-28: +1 retry_credential_load command/wrapper.
    // SW-AU-69: +1 on both sides — revision-safe durable transition disablement.
    // Pack override actions: +5 on both sides — preview, reset, adopt, Undo, and recovery discovery.
    // Inherited pack actions: +2 on both sides — preview a witness-bound inherited action and create
    // a child-local customization without mutating its parent.
    // SW-PP-2: +2 on both sides — owned-pack promotion preview and recoverable mutation.
    // SW-FG-10c: +1 on both sides — set the context repository connection binding.
    // SW-FG-10f: +2 on both sides — private pending-create recovery query and cancellation.
    // SW-PO-6: +2 on both sides — plan and migrate flattened pack installs.
    // Repository-local migration status + retry commands.
    // SW-PP-6: +1 on both sides — claim a legacy pack as active-team authored.
    // SW-FG-10g: +1 on both sides — classify the join remote before selecting an account.
    // SW-PP-4: +2 on both sides — external-pack fork preview and apply.
    // Selected pack updates: +3 witnessed preview/apply/downgrade authorization commands.
    // SW-PP-3: +2 preview/add-file-to-owned-pack commands and typed wrappers.
    // SW-OR-17: +1 readRunWorkProduct wrapper/handler.
    // SW-OR-22: +2 raw terminal presence/read wrappers and handlers.
    // SW-OR-27: +3 net on both sides — four handlers added (get_active_agent_account,
    // set_active_agent_account, preview_agent_account_launch, grant_trusted_capability)
    // minus the removed renderer mint_agent_account_launch_binding.
    // SW-AU-77A: +3 on both sides — resolve_subject, enumerate_subjects, query_related_subjects.
    // SW-AU-87: +1 on both sides — get_subject_automation_summary.
    // SW-AU-80A (via rebase): +1 Rust-only — orchestration_store_spike_probe (dev/test-gated, no TS
    // wrapper); added to RUST_ONLY_COMMANDS here so the symmetry test stays green.
    // SW-AU-89: +1 on both sides — automation_audit_gate (the AD-2 render-redacted +
    // authorize-gated projection for one automation's detail surface).
    // SW-EW-44 (reconciled via OC-5 rebase): +1 on both sides — reap_epic_workspace
    // (the full-epic-workspace reap command). SW-EW-44 merged the command on both
    // the TS and Rust sides but left both count guards stale.
    // SW-AU-77E: +2 on both sides — can_declare_team_field + declare_team_field (the
    // metadata-drawer typed custom-field declare gate + write).
    // SW-AU-82A: +1 on both sides — read_run_evidence (the run-evidence aggregate the
    // RunEvidence view opens) with its `readRunEvidence` TS wrapper.
    // SW-AU-74: +1 on both sides — list_accepted_events (the accepted-events lane read:
    // bounded recent rows + per-family bridge availability).
    // SW-AU-85D: +1 on both sides — connector_account_delivery (the per-account delivery +
    // schedule read seam) with its `connectorAccountDelivery` TS wrapper.
    // SW-AU-75B: +1 on both sides — simulate_trigger (the non-dispatching Test Trigger command)
    // with its `simulateTrigger` TS wrapper; +1 TS-only — previewPredicate now wraps the existing
    // preview_predicate Rust command (which therefore leaves RUST_ONLY_COMMANDS); the pre-existing
    // status_effect_probe undercount is corrected in RUST_ONLY_COMMANDS. → 443 TS / 448 Rust.
    // SLW-PT-4: +5 on both sides — personal-provider remote backup (bind/unbind/push/
    // restore_personal_backup + get_personal_backup_status). → 448 TS / 453 Rust.
    // SW-AU-86B: +4 TS wrappers (workflowNodeKindCatalog, validateWorkflowGraph, saveWorkflow,
    // loadWorkflowForBuild) and +3 Rust handlers (save_workflow, validate_workflow_graph,
    // load_workflow_for_build); workflow_node_kind_catalog gains its first TS wrapper and leaves
    // RUST_ONLY_COMMANDS. The SW-AU-79C by-id read wrappers stay forward-declared (AU-90). → 452 TS / 456 Rust.
    // SW-AU-90 (#1320): +1 on both sides — the run-focus overlay's `getRunFocusView` TS wrapper and its
    // `get_run_focus_view` Rust handler (registered in lib.rs); the #1320 merge added the command but did
    // not bump this count. → 453 TS / 457 Rust.
    // SW-AU-78 (#1325): +2 on both sides — the guided primitive automation composer's
    // `preview_automation_composition` + `assign_automation` handlers with their
    // `previewAutomationComposition` / `assignAutomation` TS wrappers; the #1325 merge added the
    // commands but did not bump this count. → 455 TS / 459 Rust.
    // SW-EW-46: -4 on both sides — retired the cross-quit disk-persistence surface
    // (serialize_sessions, list_restorable_sessions, read_session_snapshot,
    // set_terminal_persistence_enabled) as dead write-only I/O; list_live_sessions stays. → 451 TS / 455 Rust.
    // SW-IS-F10: +2 on both sides — set_forward_sync_default (S1) + set_forward_sync_overrides (S2)
    // registered Rust-only, then both gained renderer wrappers (setForwardSyncDefault /
    // setForwardSyncOverrides) in S4, so TS and Rust each rise by two. → 453 TS / 457 Rust.
    // SW-WV-B3: +5 on both sides — the sibling-ledger Tauri commands
    // (wv_upsert_roster, wv_deprovision_roster, wv_list_roster, wv_settle_approval,
    // wv_upsert_watermark) with their wvUpsertRoster / wvDeprovisionRoster /
    // wvListRoster / wvSettleApproval / wvUpsertWatermark TS wrappers. → 458 TS / 462 Rust.
    // SW-WV-D2: +1 on both sides — the watermark advance Tauri command
    // (wv_advance_watermark_cmd) with its wvAdvanceWatermark TS wrapper. → 459 TS / 463 Rust.
    // SW-WV-B5/C6: +12 Rust-only — the eight scope-layer + four harvest-norm Tauri commands
    // are registered in generate_handler![] but have no renderer IPC wrapper yet, so they are
    // added to RUST_ONLY_COMMANDS: `rustHandlers` rises by 12, `tsCommands` does not.
    // SW-WV-E1: +3 on both sides — the endorsement-gate Tauri commands
    // (wv_record_recipient_ack, wv_ratify, wv_revoke) with their wvRecordRecipientAck /
    // wvRatify / wvRevoke TS wrappers. → 462 TS / 478 Rust.
    // SW-WV-A3: +2 on both sides — the worker-lifecycle transition Tauri commands
    // (wv_worker_handoff, wv_worker_deprovision) with their wvWorkerHandoff /
    // wvWorkerDeprovision TS wrappers. → 464 TS / 480 Rust.
    // SW-WV-F1: +2 on both sides — the Weft Board read commands
    // (wv_list_attention_board, wv_get_lease_status) with their listAttentionBoard /
    // getLeaseStatus TS wrappers. → 466 TS / 482 Rust.
    // SW-WV-F2: +2 TS / +1 Rust — the dispatch preflight `preflight_wv_dispatch`
    // (both sides: `preflightWvDispatch` wrapper + `weft::preflight` handler) PLUS the
    // forward-declared `dispatchWeftWorker` TS wrapper (`wv_dispatch_worker`), whose
    // Rust handler is owned by SW-WV-F7 and is allow-listed in TS_ONLY_COMMANDS until
    // F7 lands it (then F7 removes the entry and +1's the Rust count). → 468 TS / 483 Rust.
    // SW-WV-F7: +1 Rust only — the `weft::dispatch_bridge::wv_dispatch_worker` Tauri
    // bridge for the weft-board dispatch surface (the M1 first-click seam) lands its
    // Rust handler, so `wv_dispatch_worker` leaves TS_ONLY_COMMANDS and becomes a full
    // both-sides pair (the `dispatchWeftWorker` wrapper F2 shipped is unchanged, so the
    // TS count holds). → 468 TS / 484 Rust.
    // SW-WV-F3: +3 on both sides — the fleet-view roster commands
    // (wv_roster_for_session, wv_probe_worker, wv_worker_force_close) with their
    // wvRosterForSession / wvProbeWorker / wvWorkerForceClose TS wrappers. → 471 TS / 487 Rust.
    // SW-GS: +1 (TS + Rust) for `changed_path_count` — the cheap closed-pane badge count.
    // → 472 TS / 488 Rust.
    // SW-WV-F4: +1 on both sides — the review-at-altitude read command
    // (wv_read_pending_assertions) with its readWeftPendingAssertions TS wrapper. The
    // wv_revoke change (added optional `note` param) is a signature change, not a new
    // command, so it does not move either count. → 473 TS / 489 Rust.
    // SW-WV-C7: +1 on both sides — the worker exit-contract flavor pack staging command
    // (stage_bundled_worker_exit_contract_pack) with its stageBundledWorkerExitContractPack
    // TS wrapper. → 474 TS / 490 Rust.
    // SW-WV-F5: +2 on both sides — the endorsement-surface commands
    // (wv_list_pending_approvals — the pending T2/T3 read the YOU-OWE surface merges;
    // wv_mark_approval_surfaced — the surfaced-item guard arm, FE half of the E3 window)
    // with their wvListPendingApprovals / wvMarkApprovalSurfaced TS wrappers. The
    // reuse of wv_ratify/wv_revoke/wv_settle_approval/readWeftPendingAssertions (already
    // registered by E1/B3/F4) moves neither count. → 476 TS / 492 Rust.
    // SW-WV-RT1: +1 Rust only — the signal-done bridge (wv_signal_done_bridge), a Tauri
    // door onto the MCP signal_done hook registry, registered in generate_handler![] with
    // no renderer IPC wrapper yet (added to RUST_ONLY_COMMANDS). → 476 TS / 493 Rust.
    // SW-WV-QH1 (#1431): +2 on both sides — the harness-only seed commands
    // (wv_e2e_seed_endorsement, wv_e2e_seed_dispatch) with their wvE2eSeedEndorsement /
    // wvE2eSeedDispatch TS wrappers. Both refuse unless CODESENSE_E2E=1, so they ship
    // inert; the QH1 merge registered them Rust-side but did not bump this count or add
    // the wrappers. → 478 TS / 495 Rust.
    // SW-WA-E3: +1 Rust only — the harness-only lease driver (wv_e2e_drive_lease),
    // registered in generate_handler![] with no renderer IPC wrapper (RUST_ONLY_COMMANDS).
    // → 478 TS / 496 Rust.
    // SW-OC-2: +3 on both sides — the Connections "Add & test" trio (preview_connection,
    // list_writable_packs, add_simple_connection) moved from draft-commands.ts into
    // commands.ts with their wrappers and registered in generate_handler![]. → 481 TS / 499 Rust.
    // SW-CN-45: +1 on both sides — connect_github_token (GitHub manual-token connect
    // path) with its connectGithubToken TS wrapper. → 482 TS / 500 Rust.
    // SW-OC-3: +1 on both sides — remove_source_repo_from_team (Manage-repos detach: the
    // RemoveSourceRepoFromTeam registry edit + attachment-record clear, local clone preserved)
    // with its removeSourceRepoFromTeam wrapper, registered in generate_handler![].
    // → 483 TS / 501 Rust.
    // SW-WA-D4: +1 Rust only — the harness-only posture driver (wv_e2e_drive_posture),
    // registered in generate_handler![] with no renderer IPC wrapper (RUST_ONLY_COMMANDS).
    // → 483 TS / 502 Rust.
    // SW-WA-E6: +1 on both sides — the operator gate-status list read (wv_gate_status),
    // a pure read over the SW-WA-D5 gate store, with its gateStatus TS wrapper, registered
    // in generate_handler![]. → 484 TS / 503 Rust.
    // SW-WA-E2: +1 Rust only — the harness-only closed-gate park driver (wv_e2e_drive_gate),
    // registered in generate_handler![] with no renderer IPC wrapper (RUST_ONLY_COMMANDS).
    // → 484 TS / 504 Rust.
    // SW-WA-E1: +2 Rust only — the harness-only capstone-arc drivers (wv_e2e_drive_e1 +
    // wv_e2e_e1_reply_state), registered in generate_handler![] with no renderer IPC wrapper
    // (both added to RUST_ONLY_COMMANDS). → 484 TS / 506 Rust.
    // SW-WA-E4: +1 Rust only — the harness-only decoupling-reply driver
    // (wv_e2e_drive_decoupling_reply), registered in generate_handler![] with no renderer IPC
    // wrapper (RUST_ONLY_COMMANDS). → 484 TS / 507 Rust.
    // SW-WA-E7: +3 Rust only — the three harness-only Stage-2 red-path drivers
    // (wv_e2e_drive_tier_spawn, wv_e2e_drive_fold, wv_e2e_drive_gate_open), registered in
    // generate_handler![] with no renderer IPC wrapper (RUST_ONLY_COMMANDS). → 484 TS / 510 Rust.
    // SW-WA-F1: +1 on both sides — the open_terminal_tab reply bridge (resolve_open_terminal_tab),
    // the Tauri door the FE tab-open listener answers the MCP verb through, with its
    // resolveOpenTerminalTab TS wrapper, registered in generate_handler![]. → 485 TS / 511 Rust.
    // SW-WA-F4: +2 Rust only — the harness-only open→run→read round-trip drivers
    // (wv_e2e_terminal_open_run, wv_e2e_terminal_read), registered in generate_handler![]
    // with no renderer IPC wrapper (RUST_ONLY_COMMANDS — reached only by the weft-e2e
    // harness via core.invoke, refused unless CODESENSE_E2E=1). → 485 TS / 513 Rust.
    // SW-WA-A8: +1 on both sides — the operator "Arm controller" affordance (wv_arm_controller)
    // with its armController TS wrapper, which enqueues the SAME durable controller fire the A3
    // auto-arm enqueues (a durable run, never a transient lease grab), registered in
    // generate_handler![]. PLUS +1 Rust only — the harness-only arm driver (wv_e2e_arm_controller,
    // RUST_ONLY_COMMANDS) that replays the arm's first act (win lease + write the scope row) so
    // the weft-e2e cold-open scope-row proof runs without the RT1 bridge. → 486 TS / 515 Rust.
    // SW-WA-A9: +1 Rust only — the harness-only agent-independent bootstrap driver
    // (wv_e2e_cold_open_bootstrap, RUST_ONLY_COMMANDS) that drives the REAL perform_bootstrap_arm
    // seam so the weft-e2e proves a cold-open seat WITHOUT the arm door. → 486 TS / 516 Rust.
    // SLW-PT-9: +1 on both sides — `ensure_personal_team_root` (resolve-or-create the standalone
    // Personal repo on demand for a new-personal-file save) with its `ensurePersonalTeamRoot` TS
    // wrapper. The connector + notes save paths route through the same shared seam without adding a
    // command. → 487 TS / 517 Rust.
    // SLW-PT-15: +1 on both sides — `probe_backup_remote` (read-only probe of a re-point /
    // restore target: empty vs the logical collisions with the local personal tree) with its
    // `probeBackupRemote` TS wrapper. → 488 TS / 518 Rust.
    // SLW-PT-12: +1 on both sides — `move_content_between_contexts` (MOVE a content item between
    // any two Contexts incl. Personal, both directions; atomic write-then-remove) with its
    // `moveContentBetweenContexts` TS wrapper. → 489 TS / 519 Rust.
    // SW-WA-E13: +3 Rust only — the harness-only bridge-free end-proof drivers
    // (wv_e2e_drive_e13, wv_e2e_drive_e13_reply, wv_e2e_e13_reply_state), registered in
    // generate_handler![] with no renderer IPC wrapper (all three added to RUST_ONLY_COMMANDS).
    // They drive the WHOLE self-driving loop with NO RT1 seed/done bridge on the happy path
    // (real triage → real A10 waker → controller dispatch → real connector_reply to a loopback
    // stub) and are reached only by the QH weft-e2e harness via core.invoke. → 489 TS / 522 Rust.
    // SLW-PT-14: +3 TS / +3 Rust — `remove_content_from_context` (REMOVE from a Context) plus
    // the "Stop using Personal on this machine" reversal pair (`preview_stop_using_personal`,
    // `stop_using_personal_on_this_machine`). → 492 TS / 525 Rust.
    // Host-owned terminal layout: +12 on both sides — the layout verbs
    // (terminal_layout_snapshot, terminal_open_tab, terminal_close_tab,
    // terminal_place_tab, terminal_rename_tab, terminal_move_tab, terminal_set_active,
    // terminal_set_grid, terminal_set_view_hint, terminal_set_tags,
    // terminal_set_descriptor, terminal_close_workspace) with their TS wrappers.
    // → 504 TS / 537 Rust.
    // −1 on both sides — `detect_team_setup_progress` + its `detectTeamSetupProgress`
    // wrapper, retired with the Guided Setup checklist they fed. → 503 TS / 536 Rust.
    // Default validator gates: +3 on both sides — `resolve_default_validators`,
    // `set_repo_test_command`, `load_default_gates` with their TS wrappers. → 506 TS / 539 Rust.
    // +5 on both sides — create a story from the app (`preview_new_story`,
    // `create_story`, `validate_story_file`) and the bundled sample project's repos
    // (`create_sample_repo`, `remove_sample_repos`). → 511 TS / 544 Rust.
    // +2 on both sides — `read_gate_evidence` (the evidence stamped into a story file) and
    // `story_live_run_id` (the live run that owns a story, for the drawer's Approve) with their
    // `readGateEvidence` / `storyLiveRunId` wrappers. → 513 TS / 546 Rust.
    // +1 on both sides — `story_repo_choices` (a story's epic repos, for fixing a story
    // with no source repo) with its `storyRepoChoices` wrapper. → 514 TS / 547 Rust.
    // +1 on both sides — `list_agent_signals` (installed agents + attention channel for the
    // terminal-tab dot floor) with its `listAgentSignals` wrapper. → 515 TS / 548 Rust.
    // +1 on both sides — `run_gate_for_move` (run the gate a status move crosses) with its
    // `runGateForMove` wrapper. → 516 TS / 549 Rust.
    // +2 on both sides — the terminal server's `terminal_server_status` (an incompatible
    // server, for a view that mounts after it was found) and `restart_terminal_server` with
    // their `terminalServerStatus` / `restartTerminalServer` wrappers. → 518 TS / 551 Rust.
    // +2 on both sides — `sample_project_sessions` (sessions running in the sample project, for
    // the removal confirmation) and `set_pack_automation_on` (turn on a pack automation, which
    // installs off), with their `sampleProjectSessions` / `setPackAutomationOn` wrappers.
    // → 520 TS / 553 Rust.
    // SW-AV-18: +1 on both sides — `cmd_wv_scope_list` (the who-opened-whom tree read) with
    // its `wvScopeList` wrapper. Unlike the eight SW-WV-B5 scope commands (still Rust-only),
    // this one ships its FE wrapper now, so it counts on BOTH sides. → 521 TS / 554 Rust.
    // SW-AV-12: +1 on both sides — the Agent Profiles catalog read (the launchable-template
    // list, DISPLAY only) with its wrapper. → 522 TS / 555 Rust.
    // +2 on both sides — `pack_automation_off_notice` / `dismiss_pack_automation_off_notice` (the
    // one-time notice when pack automations became opt-in) with their `packAutomationOffNotice` /
    // `dismissPackAutomationOffNotice` wrappers. → 524 TS / 557 Rust.
    // SW-LN-35: +2 on both sides — `terminal_shell_info` and `set_terminal_shell_override`
    // (the shell terminals run, and its Settings override) with their `terminalShellInfo` /
    // `setTerminalShellOverride` wrappers. → 526 TS / 559 Rust.
    // SW-AV-23: +1 on both sides — `connector_event_vocabulary` (the per-connector event/filter
    // vocabulary the Trigger editor draws from) with its `connectorEventVocabulary` wrapper. → 527 TS / 560 Rust.
    // SW-LN-B20: +1 on both sides — `set_guest_session` (the frontend declares the local guest)
    // with its `setGuestSession` wrapper. → 528 TS / 561 Rust.
    // SW-LN-B47: +1 on both sides — `get_guest_session` (the frontend recovers the guest from
    // the backend's on-disk marker) with its `getGuestSession` wrapper. → 529 TS / 562 Rust.
    // SW-AV-27: DELETE the lease/controller/gate control plane. −3 on both sides —
    // `wv_get_lease_status`/`wv_arm_controller`/`wv_gate_status` (with their
    // `getLeaseStatus`/`armController`/`gateStatus` wrappers), and −9 Rust-only harness
    // drivers (`wv_e2e_drive_lease`/`_posture`/`_gate`/`_e1`/`_e13`/`_tier_spawn`/
    // `_gate_open`/`wv_e2e_arm_controller`/`wv_e2e_cold_open_bootstrap`). → 526 TS / 550 Rust.
    // +1 on both sides — `sessions_running_processes` (each shell's foreground and background
    // jobs: the close gesture's verdict) with its `sessionsRunningProcesses` wrapper. → 527 TS / 551 Rust.
    // −1 on both sides — `dashboard_detach_session` and its `dashboardDetachSession` wrapper,
    // retired with "Send to background" (Hide keeps a tab in the tray). → 526 TS / 550 Rust.
    // SW-RT-7b: −5 on both sides — the viewer store became a projection of the
    // host-owned terminal layout, retiring `spawn_pty` / `attach_warm_pty` (a terminal
    // opens through `terminal_open_tab`), `kill_pty` (it closes through
    // `terminal_close_tab`), `list_live_sessions` (the layout snapshot restores the
    // tabs) and `resolve_open_terminal_tab` (MCP `open_terminal_tab` opens through the
    // layout, not a webview round trip), with their wrappers. → 521 TS / 545 Rust.
    // SW-FU-B4: +2 on both sides — `adopt_org_clone_cmd` ("Use my clone" on join) and
    // `set_aside_org_target_cmd` (move what's in the team's folder aside). → 523 TS / 547 Rust.
    // +1 on both sides — `terminal_respawn_tab` (a new process in an exited tab, same id and
    // place: Restart and Relaunch on an ended pane) with its `terminalRespawnTab` wrapper.
    // → 524 TS / 548 Rust.
    // Remote Machines: +10 on both sides — the machine registry and SSH connector
    // (`list_machines`, `list_ssh_sources`, `capture_machine_passphrase`,
    // `test_machine_connection`, `install_terminal_server`, `save_machine`,
    // `set_machine_hidden`, `set_machine_auto_connect`, `unshare_machine`,
    // `remove_machine`), with wrappers moved out of `draft-commands.ts`.
    // → 534 TS / 558 Rust.
    // Connection manager: +3 on both sides — connect/disconnect/retry.
    // SW-RM-4d: +2 on both sides — prepare the target-bound one-use replacement
    // reservation, then consume it to restart that exact Machine server.
    // SW-RM-5a-a: +1 on both sides — the backend-issued runtime admission feed.
    // Exact added wrapper/registration: terminal_layout_admission; no removals.
    // +1 on both sides — `reserve_terminal_stream` (a viewport stream id allocated
    // before its subscribe, so the frontend listens first) with its
    // `reserveTerminalStream` wrapper. → 541 TS / 565 Rust.
    // The TS count lagged one wrapper behind commands.ts; +1 on both sides —
    // `read_terminal_text` (a session's emulator terminal as plain text) with its
    // `readTerminalText` wrapper (the Rust count lagged one too). → 543 TS / 567 Rust.
    // Remote workspace terminals (#1729) added one more wrapper and handler
    // without the bump. → 544 TS / 568 Rust.
    // −1 on both sides — `resnapshot_terminal` and its `resnapshotTerminal` wrapper,
    // deleted with the raw output ring. → 543 TS / 567 Rust.
    // Scoped agent MCP over private session connections (#1740) added one more
    // wrapper and handler without the bump. → 544 TS / 568 Rust.
    // Routines tab: +13 on both sides — the `routine_*` commands over the routine
    // service, with their `routine*` wrappers. → 557 TS / 581 Rust.
    // Routine editor: +4 on both sides — save, schedule preview, restore and the
    // authoring agent. → 561 TS / 585 Rust.
    // −4 on both sides — the three standalone-automation preflights and the
    // overlap-skip read, with their wrappers, deleted with the standalone cadence
    // (clock-driven work is a routine). → 557 TS / 581 Rust.
    // +2 on both sides — `add_source_repo_to_team` and `attach_source_repo_clone`
    // (the one add-repo core the forms and MCP share); −1 on both sides —
    // `is_git_working_tree` / `isGitWorkingTree`, now checked inside that core.
    // → 558 TS / 582 Rust.
    // +1 on both sides — `agent_launch_prepare` / `agentLaunchPrepare` (the frontend's
    // interactive launch of a named agent).
    // +1 on both sides — `agent_restore_routines` / `agentRestoreRoutines` (SW-AGR-1: unarchiving
    // an agent offers back the routines its archive archived).
    // +8 on both sides — the Agents tab: `agents_list`, `agent_detail`, `agent_archive`,
    // `agent_unarchive`, `agent_delete`, `agent_copy_to_mine`, `agent_binding_set`,
    // `agent_binding_clear`, with their `agent*` wrappers.
    // +5 on both sides — the agent editor: `agent_save`, `agent_preview`,
    // `agent_restore_revision`, `agent_startup_catalog`, `agent_open_authoring_agent`.
    // −1 on both sides — the Agent Profiles catalog read and its wrapper, retired with the
    // pack-YAML profile model (agents replace it).
    // −4 on both sides — the user launch-profile overlay's three CRUD commands and its MCP
    // picker's server read, with their wrappers; agents replace them.
    // −2 Rust only — the harness-only `wv_e2e_drive_e13_reply` / `wv_e2e_e13_reply_state`
    // connector-reply drivers, retired with connectors. → 482 TS / 505 Rust.
    // +1 on both sides — `ensure_codesense_docs` / `ensureCodesenseDocs` (place and connect
    // the bundled CodeSense docs context repo, SW-OC-16). → 483 TS / 506 Rust.
    // −1 on both sides — `wv_advance_watermark_cmd` / `wvAdvanceWatermark` (the dead triage
    // watermark advance); −2 Rust only — the harness-only `wv_e2e_e1_reply_state` /
    // `wv_e2e_drive_decoupling_reply` completion-reply drivers (SW-OC-19). → 482 TS / 503 Rust.
    // +1 each — read-only remote MCP compatibility catalog; no profile authoring. → 483 TS / 504 Rust.
    // −2 on both sides — `ensure_personal_team_root` / `ensurePersonalTeamRoot` and
    // `list_provider_content_items` / `listProviderContentItems`, retired with the `_personal`
    // org (SLW-PT-19: the scratch space resolves per org). → 481 TS / 502 Rust.
    // +1 on both sides — `local_default_branch` / `localDefaultBranch` (the branch a new
    // agent worktree starts from, named in the launch picker). → 482 TS / 503 Rust.
    // SLW-PT-20: +4 scratch-use commands (get/setScratchTeamUse, listScratchOverlayFiles,
    // setScratchFileUseTeam) with their TS wrappers. → 486 TS / 507 Rust.
    // +1 on both sides: rendered document capture reply. → 487 TS / 508 Rust.
    // −19 on both sides — the commands only the deleted right-pane Agents / Workflows /
    // Triggers panels and the Worktree lens called (workflow library + pack automation
    // notice, triggers list, run steps, step-DAG resolve, worktree cleanup/reconcile/epic
    // reap, reverse-sync-now, Weft probe + scope list). → 468 TS / 489 Rust.
    // SLW-PT-21/23: −5 on both sides — the retired Personal UI commands
    // (`create_personal_team`, `stop_using_personal_team_in_teams`,
    // `get_personal_team_status`, `preview_stop_using_personal`,
    // `stop_using_personal_on_this_machine`) with their wrappers; −1 on both sides —
    // `promote_provider_item` / `promoteProviderItem` (nothing called it); +2 on both
    // sides — the Scratch space tab (`list_scratch_space` / `listScratchSpace`,
    // `create_scratch_file` / `createScratchFile`). → 464 TS / 485 Rust.
    // −1 on both sides — `preflight_mcp_json` / `preflightMcpJson`, retired with every
    // write of CodeSense's own config into a launch folder. → 463 TS / 484 Rust.
    // −2 on both sides — `plan_pack_migration` / `planPackMigration` and
    // `migrate_flattened_pack_installs` / `migrateFlattenedPackInstalls`, deleted with the
    // flat-payload migration. → 461 TS / 482 Rust.
    // −3 on both sides — the bundled starter, automation-authoring and worker-exit-contract
    // staging commands with their `stageBundled*` wrappers: the starter is gone, the skills
    // ship as docs examples, and Weft stages its contract itself. → 458 TS / 479 Rust.
    // +2 on both sides — the avatar packs list and the team default-pack setter.
    // +1 on both sides — `retire_legacy_terminal_server` / `retireLegacyTerminalServer`
    // (stop the shared-account server a removed or switched legacy machine left running).
    // → 461 TS / 482 Rust.
    // SW-WA-G-MSG: +3 on both sides — `list_inbox_messages`, `reply_as_human` and
    // `mark_inbox_message_read` (the Agents panel's Messages list) with their
    // `listInboxMessages` / `replyAsHuman` / `markInboxMessageRead` wrappers.
    // → 464 TS / 485 Rust.
    // SW-BSI-2: +6 on both sides — role-keyed delivery (`register_role_cmd`,
    // `claim_role_messages_cmd`, `accept_role_message_cmd`, `ack_role_message_cmd`,
    // `release_role_cmd`, `send_role_message_cmd`) with their `registerRole` /
    // `claimRoleMessages` / `acceptRoleMessage` / `ackRoleMessage` / `releaseRole` /
    // `sendRoleMessage` wrappers. → 470 TS / 491 Rust.
    // Inbox state: +4 on both sides — the archive command (now batched),
    // `list_agent_updates`, `get_human_status`, `set_human_status` with their
    // its archive wrapper / `listAgentUpdates` / `getHumanStatus` /
    // `setHumanStatus` wrappers. +1 TS and +1 Rust drift absorbed since the
    // SW-BSI-2 merge (upstream wrappers landed on main after the squash — the
    // base shifted from 470/491 to 471/492 before this PR). → 475 TS / 496 Rust.
    // SW-PK-4: −17 on both sides (pack override, provenance, fork and update). → 459 TS / 480 Rust.
    // SW-BSI-3: +1 on both sides — `inbox_attachment_status` with its
    // `inboxAttachmentStatus` wrapper (resolve what the inbox knows about an
    // attached managed path ahead of opening it). → 460 TS / 481 Rust.
    // −40 on both sides — pipeline.d automations fold into Routines: the automation
    // read-model, authoring, mute and audit-gate commands, the workflow and watch libraries and
    // Build canvas, the status-transition authoring trio, trigger simulation, dry-run, test-fire
    // and Run now, the subject automation summary/preview/assign trio, the run-focus graph, the
    // accepted-events lane and the runner-availability probe. → 420 TS / 441 Rust.
    // +1 on both sides — `prepare_remote_story_workspace` / `prepareRemoteStoryWorkspace`
    // (create a story workspace on a machine). → 421 TS / 442 Rust.
    // +1 on both sides — `list_panes` / `listPanes` (custom right panes), and +3 drift
    // absorbed from commands that landed on main without a count update. → 425 TS / 446 Rust.
    // −3 on both sides — every launch names an agent: the launch-profile resolver, the
    // profile-mode headless story launch and the remote MCP profile catalog. → 422 TS / 443 Rust.
    // −6 on both sides — JSON widgets retired: the widget grant ledger trio, team widget
    // discovery, register-from-binding and the bundled review-tray stager. → 416 TS / 437 Rust.
    // SW-BSI-3 Phase 2: +1 on both sides — `trigger_hydrate_from_inbox` with its
    // `triggerHydrateFromInbox` wrapper (the UI's hydrate-attachment button delegates to the
    // shared remote-hydration engine). → 417 TS / 438 Rust.
    // +1 on both sides — `agent_launch_open` / `agentLaunchOpen` (a person launches an agent
    // from the app's launch sheet, through the same targeted open agents use). → 418 TS / 439 Rust.
    // +3 on both sides — unreadable machine files: list, open as text, remove.
    // → 421 TS / 442 Rust.
    // −15 on both sides — the status gate retires (SW-OC-25b): the gate door, gate-for-move,
    // override, gate-state and gate-evidence reads, validate_ref provenance, the default
    // validator trio, the field-schema read, run-metadata validation, prompt preview, and the
    // pipeline document-type / source-repo / sprint-ledger writers. → 406 TS / 427 Rust.
    // −14 on both sides — the pack system is gone: the pack list, install/uninstall with their
    // previews, peek, manifest read and export, the authoring quartet (validate, save, claim,
    // payload listing), the binary probe and the per-user enable toggle. → 392 TS / 413 Rust.
    // SW-OC-25c: −12 on both sides — sprint auto-run and `pipeline.yml` transitions retire: the
    // ready queue, sprint/epic dispatch, run-epic-action, run-state pause/resume/stop, held-step
    // read/approve, story live-run lookup and the transitions read; −2 Rust-only — the dev/test
    // orchestration-store and status-effect probes. → 380 TS / 399 Rust.
    // SW-OC-25d: −4 on both sides — the automation subject provider (resolve_subject,
    // enumerate_subjects, query_related_subjects) and preview_predicate. → 376 TS / 395 Rust.
    // SW-OC-27: −7 on both sides — local story-worktree hydration retires with the Hydration
    // overlay: the worktree read, hydrate/dehydrate epic, closure preview and the three
    // hydration-policy reads/writes. → 369 TS / 388 Rust.
    // +1 on both sides — `withdraw_inbox_message` / `withdrawInboxMessage` (cancel a message
    // to an agent before any instance picks it up). → 370 TS / 389 Rust.
    // +2 on both sides — the local activity capability upgrade review and confirmed update
    // (`terminal_activity_upgrade_status` / `update_terminal_activity_server`). → 372 TS / 391 Rust.
    // +3 on both sides — the page bridge's `context()` / `openFile` resolvers
    // (`bridge_doc_context`, `bridge_resolve_open_target`) and its typed frontmatter
    // write (`update_frontmatter` / `updateFrontmatter`). → 375 TS / 394 Rust.
    // +1 on both sides — `set_local_identity` (the human-identity editor saves the
    // viewer's display name + chosen avatar to the local profile). → 376 TS / 395 Rust.
    // +7 on both sides — DESIGN-remote-source.md: the remote source + editor
    // Tauri commands (resolve_tab_source_repo_remote, source_remote_file_read,
    // source_remote_file_write, source_remote_changeset, source_remote_file_redline,
    // source_remote_watch_start, source_remote_watch_stop). The baseline on
    // origin/main scans at 377 TS / 395 Rust (one TS off from the last
    // "set_local_identity" line's comment — a pre-existing drift this branch
    // doesn't fix); +7 brings us to 384 TS / 402 Rust.
    // +1 on both sides — DESIGN-remote-phase-2.md Phase 2 scale probe
    // (`source_remote_tracked_count` / `sourceRemoteTrackedCount`). → 385 TS / 403 Rust.
    // +1 Rust — SW-AWX-21 (#1906) inbox_idle trigger / unread counts in list_sessions
    // added a Tauri command that wasn't reflected in the expected count. → 404 Rust.
    // +2 on both sides — the Controller's face saved on its team agent
    // (`agent_set_controller_avatar`) and sharing a personal avatar pack with the team
    // (`avatar_pack_share_with_team`). → 387 TS / 406 Rust.
    // +1 on both sides — SW-TP CAP-11/13 (`session_cwd_cmd` / `sessionCwd`) wraps the
    // existing pty_backend::session_cwd for the per-tab toolbar cwd slot. → 388/407.
    // +5 on both sides — the agents read model for the page bridge
    // (`agent_read_sessions`, `agent_read_agent`, `agent_read_inbox`, `agent_read_roles`)
    // and the human's dashboard send (`send_inbox_as_human`). → 393 TS / 412 Rust.
    // +6 on both sides — the page bridge's routine and run reads, each its MCP twin's
    // shape (`routine_read_list`, `routine_read_get`, `routine_read_runs`,
    // `routine_read_run`, `run_read_list`, `run_read`). → 399 TS / 418 Rust.
    // +2 on both sides — the page bridge's run evidence and work-product reads, each its
    // MCP twin's shape (`run_read_evidence`, `run_read_work_product`). → 401 TS / 420 Rust.
    // +1 on both sides — DESIGN-remote-phase-2.md §3 Bug A fix: remote worktree
    // single-level directory listing (`source_remote_list_directory` /
    // `sourceRemoteListDirectory`) so the SSH-remote Editor tab can browse its
    // tree instead of showing "Worktree isn't ready yet." → 402 TS / 421 Rust.
    // −1 on both sides — SW-AWC-3 removed the Controller, and with it
    // `agent_set_controller_avatar` / `agentSetControllerAvatar`. → 401 TS / 420 Rust.
    // SW-XS-3: −6 on both sides — set_forward_sync_default, set_forward_sync_overrides,
    // get_out_of_date_surfaces, trigger_startup_wave, switch_team_install and
    // retry_worktree_sync are gone (nothing syncs a workspace on its own) → 395 TS / 414 Rust.
    // SW-XS-4: −4 on both sides — set_reverse_sync_overrides, set_reverse_sync_default,
    // resolve_conflict and list_conflicts are gone (nothing publishes edits on its own);
    // +2 on both sides — get_unpublished_context_notice / ack_unpublished_context_notice
    // (the one-time "N unpublished context edits" migration notice) → 393 TS / 412 Rust.
    // SW-XS-4 review (M4): +1 on both sides — worktree_unpublished_context (the
    // removal confirmation's unpublished-edit count) → 394 TS / 413 Rust.
    // SW-XS-17: −5 on both sides — set_worktree_cap, ensure_worktree_capacity,
    // set_worktree_idle_threshold, set_worktree_pins and reap_worktrees_now are gone (no
    // worktree cap, no idle reaper) → 389 TS / 408 Rust.
    // +1 on both sides — SW-XS-9 `update_workspace_base` / `updateWorkspaceBase`
    // (move a machine workspace to its newest base). → 390 TS / 409 Rust.
    // SW-XS-5: −3 on both sides — directory ownership is gone: the nav's surface
    // read, its per-file read and its per-dir write → 388 TS / 407 Rust.
    // SW-AGR-9: +1 on both sides — `wake_history` / `wakeHistory` (a tab's wake history
    // for its timer popover). → 389 TS / 408 Rust.
    // SW-AGR-3: +3 on both sides — `remind_session` / `remindSession`, `session_timers` /
    // `sessionTimers`, `cancel_session_timer` / `cancelSessionTimer` ("Remind this agent…"
    // and the timer popover). → 392 TS / 411 Rust.
    // SW-XS-14: −3 on both sides — the view overrides (add_view_override,
    // remove_view_override, load_view_overrides) are gone; +1 on both sides —
    // agent_dismiss_context_suggestion (dismiss the projections.yml suggestion)
    // → 390 TS / 409 Rust.
    // SW-XS-15: +2 on both sides — the left nav's agent context overlay:
    // `agent_context_preview` / `agentContextPreview` and `agent_context_update` /
    // `agentContextUpdate` → 392 TS / 411 Rust.
    // SW-XS-15: −1 on both sides — `resolve_composite_for_repo` /
    // `resolveCompositeForRepo` (the nav's install-tier composite overlay) is gone
    // → 391 TS / 410 Rust.
    // SW-ODC-11: +1 on both sides — `ensure_sample_repo` / `ensureSampleRepo` (place the
    // bundled sample source repo under the CodeSense home; seed-data demo). → 392 TS / 411 Rust.
    // SW-AGR-5: +1 TS / +1 Rust — `routine_runs_since` (the Agents tab Schedule's Recent:
    // every enabled routine's runs of the last days in one bounded read). → 393 TS / 412 Rust.
    // +3 TS — the note history wrappers (`notes_history`, `notes_read_at`,
    // `notes_restore`), forward-declared TS-only until their Rust handlers land.
    // → 396 TS / 412 Rust.
    // +5 on both sides — the worktree editor: committed bytes + quick-open list, local
    // (`worktree_file_head`, `worktree_list_files`) and remote (`source_remote_file_head`,
    // `source_remote_list_files`), plus the remote open-file poll
    // (`source_remote_file_inspect`). → 401 TS / 417 Rust.
    // +2 on both sides — one session's conversation across teams and the inbox rollup
    // (`list_session_inbox` / `listSessionInbox`, `list_inbox_rollup` / `listInboxRollup`).
    // +1 on both sides — the terminal caption's prompt segment (cwd, home, Git status)
    // read on the session's own host (`session_prompt_cmd` / `sessionPrompt`). → 399/415.
    // Merged: 401 + 3 TS / 417 + 3 Rust → 404 TS / 420 Rust.
    // The note history wrappers' Rust handlers land (+3 Rust) and the retired notes agent's
    // `ensure_notes_provisioned` goes (-1 on both sides). → 403 TS / 422 Rust.
    // SW-AGR-8: +2 TS / +2 Rust — `routine_arm_on` / `routineArmOn` (move a routine between
    // this Mac and a machine) and `routine_handover_release` / `routineHandoverRelease`
    // (release a handover from a machine that is gone). → 405 TS / 424 Rust.
    // +5 on both sides — the context repo's status button, its cards and History
    // (`context_repo_status`, `resolve_context_conflict`, `hand_context_conflict_to_agent`,
    // `run_context_sync_action`, `context_repo_history`); -1 on both sides — the right-pane
    // History panel's `get_sync_history`. → 409 TS / 428 Rust.
    // SW-XS-10: +2 on both sides — `cancel_context_operation` / `cancelContextOperation`
    // (Stop on the workspace's operation chip) and `discard_context_edits` /
    // `discardContextEdits` ("Discard my edits"). → 411 TS / 430 Rust.
    // SW-XS-25: +1 on both sides — `routine_refresh_on_machine` / `routineRefreshOnMachine`
    // ("Refresh on machine" for a machine-armed routine). → 412 TS / 431 Rust.
    // +1 on both sides — `send_prompt_to_agent` / `sendPromptToAgent` (Notes' "Send to…"
    // reaches an agent wherever its tab runs, a machine's included). → 413 TS / 432 Rust.
    // +1 on both sides — `resolve_team_landing_page` / `resolveTeamLandingPage` (a team's
    // home page, resolved through symlinks inside its content). → 414 TS / 433 Rust.
    // +1 on both sides — `mark_inbox_messages_read` / `markInboxMessagesRead` (Updates'
    // "Mark all read" reads many at once, SW-AGR-18). → 415 TS / 434 Rust.
    // SW-XS-27: −2 +2 on both sides — the one-time SW-XS-4 notice
    // (get_unpublished_context_notice / ack_unpublished_context_notice) is replaced by the
    // live count (`team_file_edits_status` / `teamFileEditsStatus`) and its Publish
    // (`publish_context_edits` / `publishContextEdits`). → 415 TS / 434 Rust.
    // +1 on both sides — `discard_team_file_edits` / `discardTeamFileEdits` (the list's
    // Discard, requested by the app). → 416 TS / 435 Rust.
    // +2 on both sides — SW-AGR-19 `machine_terminal_server_update_mode` /
    // `machineTerminalServerUpdateMode` and `reinstall_machine_terminal_server` /
    // `reinstallMachineTerminalServer` (the confirmed update fallback). → 418 TS / 437 Rust.
    // +2 on both sides — SW-AGR-19 PR-4 `machine_pending_tabs` / `machinePendingTabs` and
    // `reopen_machine_pending_tabs` / `reopenMachinePendingTabs` (reopen an unfinished
    // update's tabs). → 420 TS / 439 Rust.
    // +4 on both sides — SW-WC-1 `archive_workspace` / `archiveWorkspace`,
    // `unarchive_workspace` / `unarchiveWorkspace`, `get_workspace_archive_state` /
    // `getWorkspaceArchiveState`, `list_workspace_archive_states` /
    // `listWorkspaceArchiveStates` (per-workspace archive flag). → 424 TS / 443 Rust.
    // +2 on both sides — SW-AGR-19 PR-5 `local_terminal_server_update` /
    // `localTerminalServerUpdate` and `update_local_terminal_server` /
    // `updateLocalTerminalServer` (This Mac's update). → 426 TS / 445 Rust.
    // +4 on both sides — SW-WC-2 `workspace_chat_send` / `workspaceChatSend`,
    // `workspace_chat_read` / `workspaceChatRead`, `workspace_chat_list_workspaces` /
    // `workspaceChatListWorkspaces`, `workspace_chat_archive` / `workspaceChatArchive`
    // (the chat engine's dashboard-facing Tauri surface). → 430 TS / 449 Rust.
    // +2 on both sides — SW-AGR-19 PR-7 `list_stray_terminal_servers` /
    // `listStrayTerminalServers` and `stop_stray_terminal_server` /
    // `stopStrayTerminalServer` (This Mac's stray terminal servers). → 432 TS / 451 Rust.
    // +3 on both sides — SW-AGR-19 PR-8 `review_terminal_server_update` /
    // `reviewTerminalServerUpdate`, `start_terminal_servers_update` /
    // `startTerminalServersUpdate`, `terminal_servers_update_status` /
    // `terminalServersUpdateStatus` (the backend Update all run). → 435 TS / 454 Rust.
    // +2 on both sides — SW-CR-8 `check_install_leftovers_cmd` /
    // `checkInstallLeftoversCmd` and `heal_install_tree_cmd` /
    // `healInstallTreeCmd` (retired install-tree fold validate + heal). → 437 TS / 456 Rust.
    // +2 TS wrappers for the registered `move_workspace_bridge`
    // and `list_move_intents_bridge`; include those handlers in the Rust snapshot.
    // → 439 TS / 458 Rust.
    // +1 on both sides — `resolve_agent_cli` / `resolveAgentCli` (the launch-time agent CLI
    // path for frontend launches). → 440 TS / 459 Rust.
    // SW-FX-131: +1 on both sides — `resolve_open_workspace_machine` / `resolveOpenWorkspaceMachine`
    // (the viewer's verdict on an `open_workspace` machine open). → 441 TS / 460 Rust.
    // SW-PB-9: +4 on both sides — `license_state`, `license_activate`, `license_check_now`,
    // `license_deactivate` (beta channels only; the wrappers live in features/license/api.ts).
    // main had drifted to 447 TS / 466 Rust (+2 each, uncounted) before SW-PB-10.
    // SW-PB-10: +2 on both sides — `feedback_context`, `feedback_send` (beta channels only;
    // the wrappers live in features/feedback/api.ts). → 449 TS / 468 Rust.
    // `avatar_packs_list_for_team` / `avatarPacksListForTeam` (a team's avatar packs by its key)
    // replaces `avatar_packs_list` on both sides, so the counts stay put.
    // +1 on both sides — `session_read_model_rates` / `sessionReadModelRates` (the page's
    // `modelRates`: every stored rate). → 450 TS / 469 Rust.
    // −3 on both sides — the retired sample project's `create_sample_repo`,
    // `remove_sample_repos` and `sample_project_sessions` with their wrappers. → 447 TS / 466 Rust.
    // +1 on both sides — `worktree_live_session_count` / `worktreeLiveSessionCount` (Remove
    // worktree… refuses while something else runs in the folder). → 448 TS / 467 Rust.
    // -1 on both sides — `dashboard_stop_all` / `dashboardStopAll` (Stop all left the Agents
    // pane and nothing else called it). → 447 TS / 466 Rust.
    // SV-7: +5 on both sides — `terminal_server_overview`, `stop_terminal_server`,
    // `start_terminal_server`, `replace_terminal_server` and `end_unowned_terminal_session` with
    // their wrappers (Settings manages this instance's terminal server). → 452 TS / 471 Rust.
    // SW-PB-23: +1 on both sides — `license_restart_to_update` / `licenseRestartToUpdate`
    // (beta channels only; the wrapper lives in features/license/api.ts). → 453 TS / 472 Rust.
    // SW-PB-23: +1 on both sides — `license_cancel_restart` / `licenseCancelRestart` (the
    // restart button's reset disarms the restart). → 454 TS / 473 Rust.
    // SW-PB-23: +1 on both sides — `license_headless_runs` / `licenseHeadlessRuns` (Restart to
    // update asks before it ends agent runs without a terminal). → 455 TS / 474 Rust.
    // SV-8: +1 on both sides — `set_terminal_server_menu_bar` / `setTerminalServerMenuBar` (the
    // opt-in menu-bar item). → 456 TS / 475 Rust.
    // SV-2: +1 on both sides — the missing machine workspace removal command and its wrapper.
    // → 457 TS / 476 Rust.
    // SV-PBF-34: +2 on both sides for private navigation and the panels menu.
    // → 459 TS / 478 Rust.
    // SV-PBF-16: +9 on both sides for dashboard publishing management.
    // → 468 TS / 487 Rust.
    // Main frame/profile commands plus the three agent-list owner commands.
    // SV-ENG-5: +1 on both sides for dashboard viewer sign-out.
    // Routine preview and the two records wrappers are registered on both sides.
    // SV-PLG-13: +1 on both sides for a dashboard's active command set.
    // SV-PLG-13 PR B: +3 on both sides for the command registry's sync, list and run.
    // SV-NTP-4: +1 on both sides for records_prepare_native_view, whose count wasn't bumped.
    // SV-INP-10: bundled Inbox recovery independent of team discovery.
    // 2026-10-22: +2 on both sides for CP1's uncounted file_surface_call and file_surface_prepare_view.
    // SV-PBF-66: +1 on both sides for agent_definitions_prepare.
    // SV-ENG-18: +3 on both sides for native spend-cap and model-rate acceptance.
    // Caller-scoped DOM admission is registered on both sides.
    // SV-INP-14: +1 loadInboxMedia/load_inbox_media on both sides.
    // SV-PLG-19B: +1 on both sides for nav_model_publish (the native scan/mark owner's publish).
    // SV-INP-15: +1 on both sides for reactAsHuman/react_as_human.
    // CDE-14 session_read was present on main without a count update; CDE-13 adds why_line.
    // +1 open_diff_read / openDiffRead for source diff tabs.
    // SV-CDE-62: +1 on both sides for setup.
    // SV-ENG-120: +1 on both sides for getLogDebugEnabled/get_log_debug_enabled.
    // Main was already one command ahead of its recorded counts on both sides (518/537 vs a real 519/538), so this lands on 520/539.
    // SV-PBF-278 PR B: -2 on both sides for licenseActivate/license_activate and
    // licenseDeactivate/license_deactivate (no license to activate), so 518/537.
    // SV-PBF-286: +4 on both sides for dashboardLiveRequests/Confirm/Decline/TurnOff and
    // dashboard_live_requests/confirm/decline/turn_off, so 522/541.
    // SV-PBF-230: +1 on both sides for storyDefaultStartPoint/story_default_start_point, so 523/542.
    // SV-PBF-328: -2 on both sides for feedbackContext/feedback_context and
    // feedbackSend/feedback_send (the dead feedback form), so 521/540.
    // SV-LUC-17: +1 on both sides for unarchiveInboxMessages/unarchive_inbox_messages, so 522/541.
    const EXPECTED_TS_COMMAND_COUNT = 522;
    const EXPECTED_RUST_COMMAND_COUNT = 541;
    // The Rust count runs ahead of the TS count by the RUST_ONLY_COMMANDS that have no
    // renderer IPC wrapper (the access revocation and two session watchers plus the WV-B5/C6
    // scope-layer and harvest-norm Tauri commands whose FE wrappers land in later WV stories):
    // 19 today, so 524 − 19 = 505.
    expect(tsCommands.length).toBe(EXPECTED_TS_COMMAND_COUNT);
    expect(rustHandlers.length).toBe(EXPECTED_RUST_COMMAND_COUNT);
  });
});
