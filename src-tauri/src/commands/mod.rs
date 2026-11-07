// SPDX-License-Identifier: MIT
pub mod agent_accounts;
pub mod context_attachments;
pub mod context_operation;
pub mod context_query;
pub mod local_organization;
pub mod local_profile;
// SW-DX-17a: durable JSONL agent-lifecycle audit trail (spawn / kill / abort).
pub mod agent_audit;
pub(crate) mod agent_autoconnect;
pub(crate) mod agent_prompt;
pub mod agent_usage;
// The Agents tab (`agents_list`, `agent_detail`, archive, delete, copy, binding).
pub mod agents;
pub mod avatars;
// The frontend's interactive launch of a named agent (`agent_launch_prepare`).
pub mod agent_provision;
pub mod agents_launch;
pub mod story_create;
// SW-SD-2: account/CLI-agnostic headless agent runner + `analyze` bridge verb.
pub mod agent_runner;
// SW-AGR-14: no App Nap while remote machines are connected.
pub mod app_nap;
pub mod async_helpers;
pub mod attach_policy;
pub mod attachments;
pub mod auth;
pub mod remote_usage_pull;
// SW-AU-34: background-on-close policy — the quit-on-close preference mirror + the
// pure hide-vs-quit decision the window-close handler reads.
pub mod background_mode;
pub mod claude_session;
pub mod content_engine;
// SLW-PT-17: the agent-facing content-mobility spine (ContextId parse + §P typed
// gate ladder + context enumeration) the MCP tools adapt over.
pub mod context_git_watch;
pub mod context_mobility;
pub mod credential_store;
// SW-CN-15: unified DataSource model — one origin-tagged catalog over the builtin
// provider registry + the declared connector store (reconciles the two mechanisms).
pub mod context_repo_git;
pub mod deps;
#[cfg(target_os = "macos")]
pub mod export_pdf;
pub mod file_index;
pub mod file_surface;
pub mod files;
pub mod forge;
pub mod frontmatter_editor;
pub mod frontmatter_fields;
pub mod frontmatter_validation;
pub mod git;
pub mod git_sync;
pub mod host_connection;
pub mod host_connection_manager;
pub mod install;
// SW-CR-8: Tauri surface for the retired-install-tree fold migration (the
// validate finding + the heal action; `migrate_install_tree` is the shared
// core, also called from `load_org_cmd` for boot-time folds).
pub mod install_tree_heal;
// SW-OR-1 (AC-3): the trusted-side `Interactive` autonomy gate fronting the
// UI-driving MCP tools. Sibling of `dashboard_launch` (shares its `SpawnCap`).
pub mod interactive_gate;
// SW-WA-F11: launch telemetry — a per-session legibility record attached to each
// saved-agent launch (shell or argv) and surfaced on `list_sessions`.
pub mod launch_telemetry;
pub mod layout_resolver;
// Remote Machines: the machine registry and the SSH connector.
pub mod machines;
// SW-AN-42: the interactive launch's MCP scope projection (project_mcp_scope_config).
pub mod mcp_authoring;
// SW-OR-1 (AC-1): the trusted-side story-session launch spine `open_workspace`
// drives — the Rust mirror of the FE `launchAgentSession` orchestration.
pub mod managed_worktree_config;
pub mod mcp_scope;
pub mod mcp_workspace;
pub mod move_between_contexts;
// Who changed a note, the notes' local history, and the `notes-changed` event.
pub mod notes_history;
pub mod org;
// Custom right panes (`<space>/panes/<id>.html`): the `list_panes` command.
pub mod pane_bridge;
pub mod panes;
pub mod paths;
pub mod pipeline_format;
pub mod pipeline_scanner;
pub mod pty;
pub mod pty_backend;
pub mod pty_host;
pub mod pty_layout;
pub mod pty_runner;
pub mod reconcile_workspace;
pub mod registry;
pub mod registry_local;
pub mod remove_from_context;
pub mod scratch_identity;
pub mod scratch_space_view;
pub mod scratch_use;
pub mod scratch_watch;
// SW-LN-35: the user's login shell for terminals.
pub mod team_file_edits;
pub mod team_landing;
pub mod user_shell;
// SW-CN-10: the in-process OAuth2 token broker — the single mint authority
// (device completion + refresh grant + the SW-CN-23 auth-code seam).
// SLW-RI-2: CI backstop for the blocking-offload guardrail (test-only scan).
#[cfg(test)]
mod ri2_backstop;
// SW-LN-B20: the declared local guest session (the no-login first run).
pub mod guest_session;
// What the app knows of each tab's inbox waiter, through the tab's host.
pub(crate) mod host_inbox;
// DESIGN-remote-source.md — remote source/editor Tauri commands (host side of
// the per-tab primitives on a Machine-origin tab).
pub mod remote_source;
pub mod worktree_editor;
// SLW-WD-4: app-side bridge to WD-3's route/un-route verbs (shared with MCP).
pub mod route_to_context;
// The Routines tab's commands over the routine service.
pub mod routines;
pub mod scratch;
// SW-GT-6: the team config write commands (save_team_config, can_declare_team_field,
// declare_team_field) over the status-taxonomy / transitions / field-schema loaders.
pub mod config_editor;
// SW-GT-18: per-team metadata field schema (official ∪ custom) + the pure per-story field
// validation + the `{fieldName}` template-var extractor. Its live consumer is MCP
// `validate_candidate`.
pub mod field_schema;
// The one add-repo core: the add/attach forms and the MCP setup tools share it.
pub mod source_repo_add;
// SW-SB-2: per-team sprint definition registry loader (read side; SW-SB-11 owns
// the write/lifecycle side + SPRINTS.md generation).
pub mod sprint_registry;
// SW-CP-24: the merge-mark set/clear seam (C-layer) the CP-27 toggle + pack
// install invoke; the projection engine reads the store via `install::merge_marks`.
pub mod merge_marks;
// SW-CP-28: team-level projection-exclude read/write seam for the Settings section;
// the projection engine reads the store via `install::projection_excludes`.
pub mod projection_excludes;
// SW-GT-1: per-team status taxonomy source-of-truth + loader (SE-6 foundation;
// consumers migrate in GT-2/GT-3/GT-4).
pub mod status_taxonomy;
pub mod trigger_source;
// Run plumbing shared by routines and managed agent launches (claim loop, run identity,
// evidence, artifacts, transcripts).
pub mod sprint_run;
// SW-DX-6 AC-12: bounded concurrency cap for spawn-class dashboard actions
// (standalone primitive; SW-DX-7 wires it around real spawns).
pub mod spawn_cap;
// SW-DX-7: trusted-side storyId↔sessionId binding + SpawnCap consumer for the
// dashboard execution loop (openWorkspace / runPrompt).
// The inbox agent sessions and the human message each other through.
pub mod agent_inbox;
// One read model of agents and their inboxes for dashboards and agents (sessions, an
// agent's instances, the inbox, role queues, updates, the human's status).
pub mod agent_read_model;
pub mod data_read_model;
// SW-BSI-2: role-keyed delivery on top of the inbox — register_role,
// claim_role_messages, accept_role_message, ack_role_message, release_role,
// send_role_message.
pub mod dashboard_launch;
pub mod role_inbox;
pub(crate) mod role_startup;
pub mod roles;
pub mod system;
// SV-8: this instance's terminal server in the macOS menu bar.
pub mod terminal_server_tray;
pub mod tools;
pub mod update;
pub mod wake;
pub mod watch;
// RF-perf-8: shared self-pruning Arc registry (bounds SyncLockManager / cancels).
pub mod weak_registry;
// SW-WV-B3: the sibling-ledger Tauri command surface (roster + settle + watermark),
// thin delegations to the sanctioned `weft::` write handlers.
pub mod weft_ledger;
// SW-WV-B5: the scope-layer Tauri command surface (scopes + watches), thin
// delegations to the sanctioned `weft::` scope/watch handlers.
pub mod weft_scopes;
pub mod workspace_config;
// SW-WC-1: per-workspace archive flag (one nullable `archived_at_ms` field in
// a tiny registry under `~/.codesense/workspace-registry.json`; SW-WC-2 reads it).
pub mod workspace_registry;
// SW-WC-2: the workspace chat engine (per-workspace JSONL log + MCP tool pair
// + routine event trigger).
pub mod workspace_chat;
// `open_workspace` with a target: agents launch agents in a story or a team workspace.
pub(crate) mod workspace_open;
pub mod worktree;
// SW-XS-3 (K6): live-session worktree attribution, moved out of `watch`.
pub mod worktree_liveness;
pub mod worktree_provenance;
// SLW-WDE-3: lazy per-inset worktree file watcher (external-change reload).
pub mod worktree_watch;

// Re-exports for ergonomic call sites: `use crate::commands::await_blocking;`
pub use async_helpers::await_blocking;

/// Total number of `#[tauri::command]` handlers declared in
/// `lib.rs:invoke_handler(tauri::generate_handler![…])`. SS-13 introduces
/// this constant (no equivalent existed on `origin/main` pre-SS-13) so
/// stories that change the command count must update it explicitly —
/// preventing the kind of silent contract drift between Rust handlers and
/// TS wrappers that would otherwise only surface at runtime as `IPC
/// command not registered` errors.
///
/// Baseline at SS-13 implementation = 128 (post-SS-4a). SS-13 adds 8
/// handlers → 136. When SS-9 / SS-11 retire the legacy install commands,
/// they decrement this constant in the same PR.
///
/// **Platform note** — the count is over the **declared** handler set
/// (lexical source-text count), not the runtime-registered set. The
/// three `commands::export_pdf::*` handlers are
/// `#[cfg(target_os = "macos")]`-gated; on Linux/Windows they don't get
/// registered, but they're still declared in `lib.rs`, so the lexical
/// counter — and this constant — match across platforms.
///
/// `allow(dead_code)`: only the `#[cfg(test)]` block below references
/// this; clippy in `--lib` mode (what CI runs) does not see the test
/// reference. The constant is the contract value, not a runtime input.
/// SW-WL-3: +2 for `lazy_attach_workspace` + `lazy_detach_workspace`
/// (surface-bound watcher attach/detach).
/// SW-CP-4: +1 for `resolve_conflict` (reverse-sync conflict resolution).
/// SW-TS-4: +1 for `establish_worktree_story_link` (browse→worktree
/// bridge: write the link + trigger the team-aware sync).
/// SW-TS-5: +1 for `reconcile_worktrees_on_launch` (launch reconcile across
/// worktree surfaces, §4.4).
/// SW-WT-6: −1 for `resync_worktree_symlinks` (removed with the
/// `symlink_from_main` schema field — materialization is the sync engine's job).
/// +1 reconciling pre-existing `main` drift: an upstream command landed without
/// bumping this constant (CI billing-blocked, so the guard didn't gate it).
/// SW-SC-2: +1 for `write_binary_content_file` (suppressed binary writer; the
/// Excalidraw PNG render is its first caller).
/// SW-SC-5: +4 for the canvas wireframe-library commands
/// (`list_canvas_libraries`, `list_canvas_library_names`, `add_canvas_library`,
/// `remove_canvas_library`).
/// SW-WT-8: -2 for the removed validation/rehearsal surface (`validate_worktree`,
/// `validate_worktree_config`) — the unbuilt "Test my setup" UI was their only
/// entry point.
/// SW-TS-3: +1 for `retry_worktree_sync` (one-click retry of a failed surface).
/// SW-SC-8: −4/+2 net −2 — the four SW-SC-5 commands collapse to
/// `read_canvas_library` + `write_canvas_library` (native panel is the only
/// management surface; one writable team tier).
/// SW-SC-14: +1 — `resolve_canvas_render`, the frontend→backend reply for the
/// `render_canvas` MCP verb's render bridge.
/// SW-EW-5: +1 — `ensure_epic_workspace`, registers the epic workspace dir as
/// a content-engine target (synthetic `_epic` slug).
/// SW-EW-6: +2 — `assemble_epic_worktrees` + `reconcile_epic_worktrees`
/// (integration-worktree assembly; SW-EW-45 renamed teardown → per-repo
/// reconcile-to-frontmatter teardown).
/// SW-CP-14: +2 — `delete_content_file` + `move_content_file` (structural
/// content ops: propagate delete + rename across the sync boundary).
/// SW-EW-9: +2 — `read_epic_repos` + `set_epic_repos` (epic-node repo
/// management; epic doc `repos:` is the source of truth).
/// PT-1: +3 — `subscribe_terminal` + `unsubscribe_terminal` + `session_is_idle`
/// (sovereign-PTY substrate: viewport attach/detach + idle/running primitive).
/// PT-5: +1 — `get_terminal_cwd` (OSC 7 working-directory readout).
/// SW-CP-18: -1 — `capture_pending_files` retired (the source→context walk is
/// subsumed by the reverse engine + claim/import; route-A removal).
/// PT-8: +2 — the warm-pool new-terminal command (since retired by SW-RT-7b) +
/// `reconfigure_warm_pool` (set pool size + spawn params from settings).
/// PT-6: +0 — the cross-quit disk-persistence surface (`serialize_sessions`,
/// `list_restorable_sessions`, `read_session_snapshot`,
/// `set_terminal_persistence_enabled`) was retired in SW-EW-46 once the disk
/// RESTORE went unread: the writes were dead I/O + unbounded on-disk growth.
/// The live-session enumeration (reload survival) stayed, counted under SR-7 below
/// (since retired by SW-RT-7b).
/// SW-GS-2: +1 — `get_publish_set` (the session-less picker transport, F4; the
/// other five publish/pull sync-engine verbs are MCP-only and do not count here).
/// SW-GS-2a: +1 — `session_has_live_agent` (true agent-process liveness probe,
/// AC-12; distinguishes claude-live from dropped-to-zsh). SW-CR-6 retired
/// `ensure_git_sync_provisioned` along with the git-sync agent kind.
// SW-AW-4: +1 — `resolve_ask_user` (the FE→Rust reply leg that unblocks the
// `ask_user` oneshot; `signal_done` is emit-only and adds no command).
/// SW-GS-8: +1 — `publish_node` (node-level commit + push; since retired).
/// SW-SN-1: +4 — plural notes model: -2 (`save_scratch_note`, `load_scratch_note`
/// retired) +6 (`list_notes`, `read_note`, `create_note`, `write_note`,
/// `update_note_meta`, `delete_note`).
/// SW-SN-7: +2 — on-demand notes agent: `notes_dir_path` (agent cwd) +
/// `ensure_notes_provisioned` (drops the `CLAUDE.md` prime).
/// SW-GS-19: +2 — in-UI PR review: `list_prs_for_review` + `approve_and_merge_pr`
/// (the tree-sourced PR diff rides the existing `get_publish_set`/`get_file_redline`
/// via an optional `pr_number`, so it adds no command).
/// SW-SN-11: +1 — `search_notes` (case-insensitive body/content scan backing the
/// `⌘O` switcher's body matches; id + matched-line snippet).
/// SLW-FX-3: +1 — `reconcile_stuck_worktrees` (event-driven in-session self-heal of
/// a worktree stranded `Pending` when record-write and wave-enqueue weren't
/// crash-atomic; stuck-only filter over the existing launch reconcile).
#[allow(dead_code)]
// SW-GS-27: 174 → 173 — removed the orphaned `pull_context_repo` command (its
// destructive auto-pull caller was deleted; no remaining callers).
// SW-GS-23: 173 → 174 — added `submit_pty_line` (agent-agnostic reliable line
// submit: settle → write → confirm-from-output → discrete-CR nudge).
// SLW-NC-4: 174 → 176 — added the nav's per-surface read + write (removed in SW-XS-5).
// SLW-SS-1: 176 → 177 — added `list_conflicts` (on-mount enumeration of open
// reverse-sync conflicts for the resolve surface, over `ConflictRegistry::list()`).
// SLW-UD-6: 177 → 178 — added the nav's per-file read for expanded dirs (removed in SW-XS-5).
// SW-LD-4: 178 → 172 — removed the 6 embedded Atlassian OAuth 3LO commands
// (`start_jira_auth`, `complete_jira_auth`, `poll_jira_auth`, `cancel_jira_auth`,
// `get_jira_issue`, `heartbeat_jira`) with the client secret. Intentional drop;
// Atlassian returns as a BYO-token passthrough in SW-LD-5 (not OAuth).
// SW-LD-2: 172 → 173 — added `get_proxy_capability` (hands the authenticated-fetch
// proxy's per-session capability secret to a trusted doc's viewer URL).
// SW-OB-10: 173 → 175 — added `switch_github_account` + `remove_github_account`
// (multi-account active-switch + per-account sign-out; add-account reuses the device flow).
// SW-OB-12: 175 → 176 — added `recheck_dependencies` (forces a full dep
// re-resolution + emits `dep-status-changed` for the claude guidance re-check;
// the file watcher can't observe a PATH-installed `claude`).
// SW-OB-12 (Claude-agnostic): 176 → 173 — codesense no longer manages the Claude
// binary or its login. Removed `install_claude`, `check_claude_auth`,
// `sign_out_claude` (−3). Detection (`get_dependency_status`/`recheck_dependencies`)
// and git's managed install are untouched.
// SLW-WD-1: 173 → 175 — added `source_worktree_changeset` + `source_file_redline`
// (the source worktree diff data the WD-2 review overlay reads). Read-only, no MCP.
// SW-CN-UX-11: the provider-specific credential command is retired; Jira now
// uses the declared connector credential command (−1).
// SLW-MD-1: in-file frontmatter metadata editor adds
// `get_metadata_model`, `list_source_repos_for_team`, `apply_metadata_edit` (+3).
// SLW-WD-4: 179 → 181 — app-side route bridge adds `route_file_to_context` +
// `unroute_file_from_context` (the source-diff overlay's "→ context" affordance;
// shares one orchestration with the MCP tools).
// SW-HB-1: 181 → 182 — Home Base read spine adds `detect_team_setup_progress`.
// SLW-NC-7: 182 → 184 — team-shared nav pins add `set_team_nav_pin` +
// `get_team_nav_pins` (write/read the git-shared team pins; UI gesture, not an
// agent action → no MCP registration).
// SW-SN-13: 184 → 185 — AC-4 adds `reap_empty_scratch_notes` (one-shot guarded
// cleanup of the empty duplicate "Scratch" notes the SN-12 lazy-create minted;
// FE-invoked on cold boot, not an agent action → no MCP registration).
// SW-FX-52: 185 → 186 — batched agent liveness adds `sessions_have_live_agent`
// (one process-table snapshot for N sessions; UI poll, no MCP registration).
// SLW-WDE-2: 186 → 187 — in-app editor save adds `write_worktree_file` (a
// NON-suppressed atomic worktree write; the editable counterpart to
// `write_content_file`, deliberately visible to git + the WDE-3 watcher).
// SLW-WDE-3: 187 → 189 — the worktree file watcher adds `lazy_attach_worktree_watch`
// + `detach_worktree_watch` (a lazy, per-open-inset `notify` watch rooted at
// `ws.cwd` emitting `worktree-file-change`; the observer half of WDE-2's
// non-suppressed writer — external-change reload / dirty-warn).
// SW-LT-4: 189 → 190 — `promote_content_entry` (promote a materialized install
// file to another team in the inheritance chain: relocates the source, rewrites
// the lockfile `source_path`, and fires a best-effort forward re-fan).
// SW-SD-2: 190 → 191 — `analyze` (account/CLI-agnostic headless agent-runner
// bridge verb: composes {prompt,data}, runs the configured CLI on the blocking
// pool with a single-flight latch, returns the analysis text or a typed error).
// SLW-WD-8: 191 → 194 — the worktree commit-history data adds `source_commit_graph`
// (all-refs paginated graph), `source_commit_changeset` (flat per-commit diff), and
// `source_commit_file_redline` (per-commit redline) — read-only source-git verbs,
// no MCP registration (FE-only consumers via `source-redline-source.ts`).
// SW-DX-1: 194 → 195 — `get_tag_schema` (read seam returning the org-global tag
// registry; extends the SLW-MD metadata surface, no MCP registration — FE-only).
// SW-DX-7: 195 → 201 — the dashboard execution loop adds six trusted-side (non-MCP)
// commands wiring the SW-DX-6 SpawnCap + the storyId↔sessionId binding:
// `dashboard_launch_begin` (acquire cap slot), `dashboard_launch_bind` (stamp
// sessionId), `dashboard_launch_abort` (free the slot on the FE failure path),
// `dashboard_resolve_session` (forgery-proof fire target), `preflight_mcp_json`
// (AC-4 .mcp.json-exists gate), and `resolve_story_path` (AC-11 storyId→managed
// path via the frontmatter cache). No new MCP verb, no new PTY-spawn path, no new
// agent runtime — the spawn reuses the agent-session-launcher.
// SW-HV-1: 201 → 203 — the Hydration overlay adds two trusted-side (non-MCP)
// commands: `read_worktree_hydration` (read-only three-way DTO projected from
// SW-HV-5's shared `read_worktree_hydration_core`) and `hydrate_epic_into_worktree`
// (the one write, delegating to SW-SY-5's `run_hydrate` core). No new MCP verb.
// SW-HV-2: +1 — `dehydrate_epic_from_worktree` (the release half, delegating to
// SW-SY-6's `run_dehydrate` engine). Still no new MCP verb (`dehydrate_story`
// shipped in SW-SY-6). → 204.
// SW-HV-6: +4 — hydration scope policies add four trusted-side (non-MCP)
// commands: `preview_hydration_closure` (read-only closure preview),
// `get_hydration_policy`, `set_worktree_hydration_policy`, and
// `set_team_hydration_policy` (config read/write). The write stays the WIDENED
// `hydrate_epic_into_worktree` (optional `policy?`) — no second write command.
// 204 → 208.
// SW-VE-7: +1 — `ingest_file_into_context` (drag-drop OS-file ingest: server-side
// byte copy through the context write core). 208 → 209.
// SW-DX-11: +1 — `read_workspace_file` (the `readWorkspace` bridge verb: a
// doc-scoped, managed-confined file read/dir-list for dashboards). 209 → 210.
// SW-DX-11: +1 — `run_skill` (the `runSkill` bridge verb: the write-capable,
// multi-turn, tool-enabled sibling of `analyze` with its own SpawnCap). 210 → 211.
// SW-DX-11: +1 — `dashboard_register_session` (cap-free registration of an
// app-opened epic/context terminal into the generalized handle registry, so
// `sendToSession` can address it). 211 → 212.
// SW-DX-17a: +2 — `dashboard_list_sessions` (enumerate every live registry entry
// with real-liveness `alive`, for the "Running agents" panel) and
// `dashboard_kill_by_handle` (kill by handle through the one `kill_session`
// choke so the cap slot frees). 212 → 214.
// SW-DX-16: +1 — `resolve_epic_path` (trusted-side `{team, epic}` → the epic's
// canonical mainDoc, so the `openWorkspace` epic target can open ANY managed
// epic, not just the board's own dir). 214 → 215.
// SW-DX-18: +1 — `ensure_branch_workspace` (check out an arbitrary branch/PR
// into a managed worktree + materialize the install layer, for the
// `openWorkspace` `branch` target). 215 → 216.
// SW-GW-3: +1 — `dashboard_probe_sessions_alive` (arbitrary-session `try_wait`
// liveness for the OBSERVE-half `getWorkspaceStatus` verb; `dashboard_list_sessions`
// is registry-filtered and can't see a manually-opened story's session). 216 → 217.
// (This module count trails the TS contract's Rust count by 1: `renderer_heartbeat`
// lives OUTSIDE `commands::`, so TS-side is 218 while this module-scoped count is 217.)
// RF-1b: -3 — retired the vestigial `.meta`/sortOrder commands `read_dir_meta` /
// `write_dir_meta` / `reconcile_meta`. This module-scoped count 217 → 214; the TS
// contract's Rust-handler count 218 → 215 and its TS-wrapper count 215 → 212.
// RF-2: -4 — deleted the dead `config-lock.yml` command surface `verify_installation`
// + `read_lockfile` / `write_lockfile_entry` / `remove_lockfile_entry` (all under
// `commands::`, no app caller). This module-scoped count 214 → 210; the TS contract's
// Rust-handler count 215 → 211 and its TS-wrapper count 212 → 209.
// RF-8: -4 — retired the unwired command surface `get_app_temp_dir` + `get_terminal_cwd`
// + `walk_user_picked_root` + `cleanup_temp_path` (all under `commands::`, zero FE/MCP/
// agent caller; the last orphaned the `temp_dir()` helper, removed with it). This
// module-scoped count 210 → 206; the TS contract's Rust-handler count 211 → 207 and its
// TS-wrapper count 209 → 205.
// SW-WB-1: +1 — new `switch_worktree_branch` (migrate-in-place reconcile). This
// module-scoped count 206 → 207; the TS contract's Rust-handler count 207 → 208
// and its TS-wrapper count 205 → 206.
// SW-SO-2 (③): net-zero swap — retired `has_worktree_config`, added
// `probe_worktree_config` (fused status+config read for the worktree-open path;
// `get_worktree_config` survives for its other callers). All three counts hold:
// this module 207, TS-contract Rust-handler 208, TS-wrapper 206.
// SW-DX-17d: +3 — the attach⇄detach verb pair + the user-owned limit:
// `dashboard_detach_session` (foreground tab → background agent, acquires a
// limit slot), `dashboard_foreground_session` (release the slot on foreground), and
// `dashboard_set_background_limit` (push the user-owned limit into Rust). This
// module-scoped count 207 → 210; the TS contract's Rust-handler count 208 → 211
// and its TS-wrapper count 206 → 209.
// SLW-WL-1: +1 — `pr_teardown::set_auto_teardown_enabled` (FE→Rust push gating the
// merge→teardown poll). 210 → 211; TS-wrapper count 209 → 210 to match.
// SLW-WL-2: +1 — `worktree::list_managed_worktrees` (enriched management view for the
// right-pane Worktrees panel). 211 → 212; TS-wrapper count 210 → 211 to match.
// SLW-WL-5: +1 — `watch::set_reverse_sync_overrides` (FE→Rust push of the per-worktree
// reverse-sync override map). 212 → 213; TS-wrapper count 211 → 212 to match.
// SLW-WL-8: +1 — the worktree "Sync now" command (manual one-shot reverse catch-up; thin
// wrapper over the agent reconcile enqueue, since removed). 213 → 214; TS-wrapper count
// 212 → 213 to match.
// RF-perf-22: +1 — `watch::get_out_of_date_surfaces` (FE pull that seeds per-window
// sync chips on mount; thin read-only wrapper over `hydration::out_of_date_paths`).
// 214 → 215; TS-wrapper count 213 → 214 to match.
// SR-3: +2 — the worktree-capacity cap push and the pre-flight admission gate
// (both removed by SW-XS-17). 215 → 217; TS-wrapper count 214 → 216.
// SR-7: +1 — the live-session enumeration (the live-session map as the source of truth
// for re-attaching terminals after a watchdog reload; no disk snapshot exists on a
// reload). 217 → 218; TS-wrapper count 216 → 217 to match.
// SR-4: +1 — the worktree bulk-cleanup command (user-driven one-click bulk
// cleanup of merged+clean worktrees). 218 → 219; TS-wrapper count 217 → 218 to match.
// SW-BA-4: +1 — `dashboard_launch::dashboard_stop_all` (the global Stop-all safety
// net: halt every unattended background agent through the one `kill_session` choke).
// 220 → 221; TS-wrapper count 219 → 220 to match.
// SW-AN-10: +3 — the user-local launch-profile authoring CRUD + merged view (since retired,
// the first live consumer of AN-2's `load_packs()`). 221 → 224; TS-wrapper count
// 220 → 223 to match.
// SW-AN-6: +1 — `agent_pack::list_agent_packs` (the read-only pack→FE bridge: the
// frontend reads agent identity/profiles from the installed pack registry instead of
// a hardcoded `"claude"`). 224 → 225; TS-wrapper count 223 → 224 to match.
// SW-AN-7: +1 — `agent_pack::resolve_launch_profile` (resolve a dashboard-selected
// launch profile → binary+args+escalation verdict for the launch gate + launcher).
// 225 → 226; TS-wrapper count 224 → 225 to match.
// SW-SB-2: +1 — `sprint_registry::load_sprint_registry` (per-team sprint
// definition loader: globs `{team}/content/sprints/*.md`, resolves the nullable
// active sprint for the board selector). 226 → 227; TS-wrapper count 225 → 226 to match.
// SW-GT-1: +1 — `status_taxonomy::load_status_taxonomy` (per-team status taxonomy
// loader: reads `{team}/content/status-taxonomy.yml`, resolves ordered role-keyed
// states + soft drift). 227 → 228; TS-wrapper count 226 → 227 to match.
// SW-ID-3: +1 — `registry_local::refresh_person_profile` (re-pull a roster
// member's name + avatar from GitHub, write people.yml local-only, return the
// refreshed Person). 228 → 229.
// SW-AN-14: +3 — `agent_pack_install::{install_agent_pack, uninstall_agent_pack,
// get_pack_manifest}` (the agent-pack install/uninstall/upgrade engine). 229 → 232;
// TS-wrapper count 231 → 234 to match.
// SW-GT-6: +5 — the phase & transition config editor commands
// (`transition_config::load_transitions`, `field_schema::load_field_schema`,
// `config_editor::{save_team_config, validate_team_run_metadata, preview_prompt}`).
// 232 → 237; TS-wrapper count 234 → 239 to match.
// SW-SB-24: +1 — `sprint_registry::save_sprint_registry` (atomic read-modify-write
// of the relocated `{team}/config/sprints.yaml` ledger, returns the reloaded
// registry — the whole lifecycle write path). 237 → 238.
// SW-AN-11: +1 — `agent_pack_install::install_agent_pack_from_path` (file-picker
// install; reads the picked path off-thread). 238 → 239; TS-wrapper count 240 → 241.
// SW-AN-18: +1 — `agent_pack_install::export_agent_pack` (rebuild an installed
// team pack into a shareable `.tar.gz` off-thread). 239 → 240; TS-wrapper count
// 241 → 242.
// SW-AN-16: +1 — `agent_activation::set_agent_enabled` (user-local enable/disable
// toggle; writes a per-user disabled marker read back into the pack DTO). 240 → 241;
// TS-wrapper count 242 → 243 to match.
// SW-AN-13: net 0 — −1 `agent_pack_install::install_agent_pack` (the bytes-in
// command is dead: BOTH the file-picker and drag-drop install from a path),
// +1 `agent_pack_install::peek_agent_pack_manifest` (pre-install manifest peek).
// 241 unchanged; TS-wrapper count 243 unchanged (−installAgentPack, +peekAgentPackManifest).
// SW-WM-3: +1 the worktree orphan-reconcile command (parent-gone orphan reconcile).
// 241 → 242.
// SW-AN-15: +3 — the `agent_pack_authoring` surface under the content model
// (validate_pack_manifest, probe_binary_on_path, save_pack_manifest). Export is
// delegated to SW-AN-18's `export_agent_pack`; the draft-scratch surface + its
// packer are retired. 242 → 245; TS-wrapper count 244 → 247.
// SW-WM-4: +3 reap-policy config commands: idle-threshold and pinned-set FE pushes and
// on-demand reaping (all removed by SW-XS-17). 245 → 248; TS-wrapper count 247 → 250.
// SW-AN-12: +1 `list_pack_payload_content` (pack-payload content walk for the editor's
// payload selector). 248 → 249; TS-wrapper count 250 → 251 to match.
// SW-SR-2: +1 `sprint_run::queue::get_ready_queue` (the trusted ready-queue read
// model / dispatch source). 249 → 250; TS-wrapper count 251 → 252 to match.
// SW-FX-71: +1 `pty` show-from-hidden rehydrate command (since deleted with the
// raw output ring). 250 → 251.
// SW-SR-3: +1 `sprint_run::dispatch::dispatch_ready_stories` (the trusted parallel
// CLI dispatch tick — launch up to N ready stories, claim-guarded + provenance-stamped).
// 251 → 252; TS-wrapper count 253 → 254 to match.
// SW-SR-11: +1 `sprint_run::dispatch::dispatch_epic_whole` (the trusted whole-epic
// single-dispatch — one worker rooted at the hydrated epic dir). 252 → 253; TS-wrapper
// count 254 → 255 to match.
// SW-SR-5: +4 `sprint_run::control::{get_sprint_run_state, pause_sprint_run,
// resume_sprint_run, stop_sprint_run}` (run-control core). 253 → 257; TS-wrapper count
// 255 → 259 to match. (`send_to_sessions` is a trusted primitive, not a command.)
// SW-CP-24: +2 `merge_marks::{set_content_merge_mark, clear_content_merge_mark}` (the
// merge-mark set/clear seam the CP-27 toggle + pack install invoke). 257 → 259;
// TS-wrapper count 259 → 261 to match.
// SW-SR-12: +2 `sprint_run::control::{get_sprint_run_held, approve_sprint_step}` (the
// run-control-policy human-review approval gate). 259 → 261; TS-wrapper count 261 → 263 to
// match. (The reconcile-on-status-change teardown is internal wiring in
// `update_pipeline_status`, not a new command — no count change.)
// SW-GT-8: +1 `sprint_run::validation::assert_validation` (the gate validation door,
// IPC sibling of the MCP tool). 261 → 262; TS-wrapper count 263 → 264 to match.
// SW-MC-1: +1 `clone_and_attach_source_repo` (managed clone + auto-attach —
// clones a tracked source repo over HTTPS+OAuth and auto-attaches it as
// `managed`). 262 → 263; TS-wrapper count +1 to match.
// SW-CP-27: +1 `merge_marks::list_content_merge_marks` (the merge-mark read seam
// the toggle uses to render the tree badge + Mark/Unmark label). 263 → 264;
// TS-wrapper count +1 to match.
// SW-GT-13: +2 `frontmatter_editor::override_status_advance` (the reasoned human
// override path) and `sprint_run::run_store::sprint_run_gate_state` (the
// query-by-story_id read model over the run/verdict store). 264 → 266.
// SW-AN-24: +1 `agent_pack_install::preview_uninstall_agent_pack` (the non-mutating
// uninstall blast-radius the console previews before confirm). 266 → 267.
// SW-CP-28: +2 `projection_excludes::{get,set}_projection_excludes` (the team-level
// projection-exclude read/write seam for the Settings section). 267 → 269.
// SW-AN-29: +1 `agent_pack_install::preview_install_agent_pack` (the non-mutating
// install-time overlap preview: file diffs + config value-pairs). 269 → 270.
// SW-GT-11: +1 the gate drawer's step-DAG read (the resolved
// full step-DAG skeleton the gate drawer renders and merges verdicts onto). 270 → 271.
// SW-AN-28: +1 `starter_pack::stage_bundled_starter_pack` (materialize the bundled
// starter to a temp path for onboarding import). 271 → 272.
// SW-GT-21: +1 `sprint_run::dispatch::run_epic_action` (run a generalized named action
// across an epic's stories via the shared reactor tick). 272 → 273.
// SW-CP-29: +2 `content_engine::{list_merge_preview_repos, preview_merge_marked_file}`
// (the repo-scoped merge-preview selector + read-only deep-merge compute seam). 273 → 275.
// SW-GT-28: +1 `sprint_run::slot_resolve::resolve_validate_ref_provenance` (team-level
// validator-slot provenance for the config editor "Edit door", read-only + off-thread). 275 → 276.
// SW-CP-30: retired `mcp_registration::{mcp_status, mcp_wire_assisted, mcp_unwire}` (the bespoke
// MCP-install UI; the bridge auto-provisions via config_additions) — those handlers register
// under the `mcp_registration::` prefix, which this lexical counter (`commands::`/`menu::`/`mcp::`
// only) does NOT track, so the count is UNCHANGED. Their removal is enforced by the FE IPC
// contract test (`commands.contract.test.ts`, 278/280 → 275/277), which does count them. Stays 276.
// SW-CN-1: +4 connector commands (register_connector, list_connectors, set_connector_credential,
// connector_test). 276 → 280.
// SW-CN-2: +1 connector command (connector_poll_delta — the pull delta read seam). 280 → 281.
// SW-CN-4: +1 `list_connections` (per-connector Connected status, AC-6). 281 → 282.
// SW-AU-5: +1 `automation_watch_states` (the watch-state read-model source). 282 → 283.
// SW-CN-7: +2 connector commands (preview_derived_transform — derived-op authoring preview;
// get_connector — load a full definition for the authoring surface). 283 → 285.
// Pre-existing drift: a command registered on main (`is_runner_available`, SW-AU-9) landed
// without bumping this constant; cleared here (inherited on rebase) so the count test passes. 285 → 286.
// SW-AU-3b: +2 automation read-model commands (list_automations, automations_matching). 286 → 288.
// SW-AU-8: +4 create-flow authoring commands (upsert_automation_into_pack, list_pack_workflows,
// validate_automation_target, agent_probe_floor_secs). 288 → 292.
// SW-AU-11: +1 edit-flow fetch command (automation_for_edit). 292 → 293.
// SW-AU-12: +2 lifecycle commands (remove_automation_from_pack entry-delete +
// duplicate_automation_into_pack). 293 → 295.
// SW-AU-14: +1 per-sprint-instance mute write (set_sprint_automation_mute). 295 → 296.
// SW-AU-17: +1 read-only dispatch dry-run command (dispatch_dry_run). 296 → 297.
// SW-AU-15: +5 workflow management commands (list_workflow_library, used-by, duplicate,
// remove, rename). 297 → 302.
// SW-MC-7: +1 mode-repair command (reclone_managed_source_repo — re-clone a Gone managed
// clone into its recorded path; the two mode-flips reuse save_source_repo_attachment). 302 → 303.
// SW-MC-3: +3 managed-clone lifecycle commands (update_managed_clone — manual ff-only pull;
// remove_managed_clone — dir+record delete, marker-gated + worktree-liveness-blocked;
// list_managed_clone_health — batch unhealthy-surface detection signal). 303 → 306.
// SW-AU-36: +2 pack-scoped workflow commands (an Action-scoped, non-clobbering pack
// drop-file save and the paired single-Action read). 306 → 308.
// SW-MC-5: +2 directory-level reconcile commands (reconcile_workspace — scan a workspace root
// and attach-found / clone-missing / re-home-moved against the team's tracked set; and
// preview_workspace_reconcile — the read-only dry-run behind the Screen-11 preview). 308 → 310.
// SLW-PT-6: +3 personal-team front-door commands (create_personal_team,
// stop_using_personal_team_in_teams, get_personal_team_status). 310 → 313.
// SLW-PT-3: +2 provider-promote commands (promote_provider_item — preview-first,
// keep-source copy of a provider content item into a shared team; and
// list_provider_content_items — the read-only lister behind the card picker).
// 313 → 315.
// SW-AU-16: +1 history-preserving move command (relocate one automation/workflow
// entry between team packs keeping its id, so run history + references survive). 315 → 316.
// SW-AU-21: +1 stage_bundled_automation_authoring_pack (stage the optional
// automation-authoring knowledge pack for install). 316 → 317.
// SW-AU-22: +1 reactive test-fire command (test_fire_event). 317 → 318.
// SW-MC-4: +2 re-home commands (preview_managed_clone_rehome — read-only divergence
// detection; rehome_managed_clones — the scoped move). 318 → 320.
// SW-AU-40: +1 join-layer command (the reused-by grouping over trigger identity,
// the ⋯ views Triggers list). 320 → 321.
// SW-AU-41: +1 trigger inspect + test-probe (test_probe — run a trigger's authored shell probe once,
// by key, off-thread). 321 → 322.
// SW-AU-49: +1 run-scoped read (one run's per-step verdicts for the
// last-run strip). 322 → 323.
// SW-AN-39: +1 upsert_automation_into_personal_scratch (mint-on-first-save into the personal
// team's scratch pack). 323 → 324.
// SW-AU-34: +1 background-on-close command (set_quit_on_close — FE→Rust mirror of the
// quit-on-close preference the window-close handler reads). 324 → 325.
// SW-AU-42: +8 shared named-trigger (watches:) commands (list_watch_library, watch_used_by,
// save_watch_into_pack, watch_for_edit, duplicate_watch_into_pack, remove_watch_from_pack,
// rename_watch, promote_automation_trigger). 325 → 333.
// SW-AN-42: +2 MCP scope authoring commands (a worktree MCP-server read for the picker, since
// retired; project_mcp_scope_config — project the interactive-launch scoped
// config). 333 → 335.
// SW-OR-10: +1 headless launch route (dashboard_launch_headless — route a mode:headless profile
// to the PTY-less run_agent path). 335 → 336.
// SW-WG-6: +3 widget grant-ledger commands (load, grant, revoke). 336 → 339.
// SW-CN-23: +1 auth-code connect route (start_auth_code_flow — begin the loopback + PKCE flow).
// 339 → 340.
// SW-CN-UX-3: +1 catalog read (list_data_sources — the FE-facing read of the one CN-15 catalog,
// wrapping the existing enumerate_data_sources). 340 → 341.
// SW-WG-8: +1 team-chain widget discovery (list_team_widgets). 341 → 342.
// SW-CN-UX-12: +2 connector-def authoring seams (save_connector_into_pack — author a def into a
// chosen writable team pack; list_editable_connectors — the personal-authored ids the edit surface
// may re-save in place). 342 → 344.
// SW-OR-12: +2 agent-result reads (read_run_outcome — durable outcome header by run_id;
// read_run_artifact — persisted transcript body by run_id). 344 → 346.
// SW-CN-UX-2: +3 connection-management commands (revoke_connector, delete_connector,
// rename_connector — the FE-reachable wrappers over the SW-AU-28 cores). 346 → 349.
// SW-CN-UX-14: −1 removed list_editable_connectors (superseded by owning-pack provenance on the
// list_data_sources catalog read; the FE gates "Edit" on that, not a separate id list). 349 → 348.
// SW-CN-W2a: +2 connector-write outcome seams (describe_write_op — the trusted-side confirm copy;
// extract_write_reason — the Rust-side blocked-reason extraction). 348 → 350.
// SW-WG-10 adds NO new command (auto-appear is FE; provenance/descriptors ride existing seams).
// RF-perf-24 adds NO new command (reverse-sync flush/preview are MCP tools; the awaited-reap-flush
// command was reverted — SLW-RS-4's teardown flush already covers removal).
// SW-CN-W4: +1 mint_connector_write_cap (webview-only mint of the single-use per-write consent
// capability the proxy requires in addition to the read token). 350 → 351.
// SW-CN-W3: +1 connector-write success seam (extract_write_url — the Rust-side result-URL extraction
// for the Written outcome's "View ↗"). 351 → 352.
// RF-perf-25: +1 set_reverse_sync_default (the global reverse-sync default push) — it landed on main
// bumping only the contract test, NOT this constant, so `main` is stale (red) here; folded in. 352 → 353.
// SW-AU-60: +1 pipeline-homed authoring command (save_status_transition — the trusted-side write that
// persists an authored status-transition automation onto config/pipeline.yml). 353 → 354.
// SW-CN-UX-13: +1 list_rejected_connectors (the negative-space diagnostics read). 354 → 355.
// SW-CN-UX-21: +1 get_rejected_connector_def (raw-read a rejected def from its drop-file for the
// author-form prefill, bypassing resolution). 355 → 356.
// SW-WG-7: +1 stage_bundled_story_review_pack (stage the capstone Story Review Tray widget
// dashboard pack for install). 356 → 357.
// SW-FG-4 rebase: reconcile pre-existing `main` drift — a further handler landed on `main`
// without bumping this constant (lib.rs registers 358), so `main` is stale (red) here; folded
// in as a separate chore. This PR touches neither lib.rs nor commands. 357 → 358.
// SW-FG-2b: register `rename_account` (set/clear a per-account user label). 358 → 359.
// SW-CN-UX-23 rebase: reconcile pre-existing `main` drift — a further handler landed on `main`
// without bumping this constant (lib.rs registers 360), so `main` is stale (red) here; folded
// in as a separate chore. This PR touches neither lib.rs's handler list nor commands. 359 → 360.
// SW-CN-UX-24: +1 cancel_auth_code_flow (active revoke of a still-pending auth-code flow on Cancel,
// dropping the entry by state instead of waiting out the TTL). 360 → 361.
// SW-FG-10a: +1 reconcile_required_providers (provider reconcile read-model seam). 361 → 362.
// SW-CN-UX-4: +2 revoke_account / set_default_account (thin generic per-account wrappers over the
// shipped remove_account / switch_active_account, for the multi-account management surface). 362 → 364.
// SW-CN-UX-5: +1 list_accounts (per-connector account catalog for the binder's Selector A + the
// acting-as chip; a projection of AuthManager::list_accounts). 364 → 365.
// SW-CN-UX-4b: +1 stage_connector_account (verify and stage a declared connector account before
// its optional label is committed). 368 → 369 after intervening mainline registrations.
// SW-AU-55: +1 list_run_outcomes (one batched durable-history read). 374 → 375.
// SW-AU-66: +1 remove_status_transition (exact projected pipeline-edge deletion). 375 → 376.
// SW-CN-UX-26: +1 the register-from-binding widget command. 376 → 377.
// SW-CN-UX-28: +1 retry_credential_load. 377 → 378.
// SW-AU-69: +1 set_transition_disabled. 378 → 379.
// SW-PO-5: +5 preview/drop/adopt/undo/recovery-discovery commands. 379 → 384.
// SW-PP-2: +2 preview_pack_promotion/promote_pack_override commands. 384 → 386
// (subsequent mainline registrations bring the current total to 394).
// SW-FG-10c: +1 set_org_connection_binding_cmd. 389 → 390 after intervening registrations.
// SW-FG-10f: pending-create recovery query + cancellation command.
// SW-PO-6: +2 plan/migrate commands for flattened pack installs.
// SW-PP-6: +1 claim_pack_as_owned.
// SW-FG-10g: +1 classify_join_remote_cmd.
// SW-PP-2: +2 preview/promote commands for owned overrides.
// SW-PP-4: +2 preview/apply commands for external-pack forks.
// Selected pack updates: +3 witnessed preview/one-shot-authorize/apply handlers.
// SW-PP-3: +2 preview/add-file-to-owned-pack commands.
// SW-OR-17: +1 read_run_work_product handler.
// SW-OR-22: +2 raw-terminal presence/read handlers.
// SW-OR-27: +3 net handlers (four account tools; obsolete renderer mint removed).
// SW-AU-77B: +1 preview_predicate handler; realigned a drifted const (main was
// 431 vs actual 432) to 433.
// SW-AU-77A: +3 handlers (resolve_subject, enumerate_subjects, query_related_subjects) → 436.
// SW-AU-80A: +1 dev/test-gated orchestration_store_spike_probe. The counter reads
// lib.rs source text, so the gated line is counted regardless of build profile.
// SW-AU-87: +1 handler (get_subject_automation_summary) → 438.
// SW-AU-89: +1 the automation audit-gate handler → 439.
// SW-EW-44: +1 epic-workspace reap handler → 440.
// SW-AU-77E: +2 (can_declare_team_field, declare_team_field) → 442.
// SW-AU-82A: +1 read_run_evidence handler → 443.
// SW-AU-74: +1 list_accepted_events handler (the accepted-events lane read) → 444.
// SW-AU-85D: +1 connector_account_delivery handler (per-account delivery read seam) → 445.
// SW-AU-93: +1 dev/test-gated status_effect_probe (AC-12), counted like the AU-80A probe → 446.
// SW-AU-75B: +1 simulate_trigger (the non-dispatching Test Trigger command) → 447.
// SLW-PT-4: +5 personal-backup handlers (bind/unbind/push/restore_personal_backup, get_personal_backup_status) → 452.
// SW-AU-86B: +3 Build-canvas commands (save_workflow, validate_workflow_graph,
// load_workflow_for_build). The SW-AU-79C by-id read wrappers stay forward-declared until AU-90
// registers them. → 455.
// SW-AU-90 (#1320): +1 get_run_focus_view (the run-focus overlay's read command, registered in
// lib.rs); the #1320 merge added the handler but did not bump this constant. → 456.
// SW-AU-78 (#1325): +2 composer commands (preview_automation_composition, assign_automation),
// registered in lib.rs; the #1325 merge added the handlers but did not bump this constant. → 458.
// SW-EW-46: -4 — retired the cross-quit disk-persistence surface (serialize_sessions,
// list_restorable_sessions, read_session_snapshot, set_terminal_persistence_enabled) as dead
// write-only I/O once the disk RESTORE went unread; the live-session enumeration stayed (since retired
// by SW-RT-7b). → 454.
// SW-IS-F10 (S1): +1 set_forward_sync_default (the forward sync-cadence push, symmetric with
// set_reverse_sync_default). SW-IS-F10 (S2): +1 set_forward_sync_overrides (the per-worktree
// forward-override push, symmetric with set_reverse_sync_overrides). → 456 (454 + 2).
// SW-WV-B3: +5 the sibling-ledger Tauri commands (wv_upsert_roster, wv_deprovision_roster,
// wv_list_roster, wv_settle_approval, wv_upsert_watermark). → 461 (456 + 5).
// SW-WV-B5: +8 the scope-layer Tauri commands (cmd_wv_scope_get, cmd_wv_scope_create,
// cmd_wv_scope_update_artifact_ptr, cmd_wv_scope_close, cmd_wv_watch_upsert,
// cmd_wv_watch_close, cmd_wv_watch_mark_quiet, cmd_wv_watch_list). → 469 (461 + 8).
// SW-WV-D2: +1 the watermark advance command (wv_advance_watermark_cmd). → 470 (469 + 1).
// SW-WV-C6: +4 the decision-harvest-norm Tauri commands (wv_set_harvest_artifact,
// wv_approve_harvest, wv_mark_harvest_not_required, wv_list_overdue_harvests). → 474 (470 + 4).
// SW-WV-E1: +3 the endorsement-gate Tauri commands (wv_record_recipient_ack, wv_ratify,
// wv_revoke) — Tauri-only, human UI boundary, never MCP verbs. → 477 (474 + 3).
// SW-WV-A3: +2 the worker-lifecycle transition Tauri commands (wv_worker_handoff,
// wv_worker_deprovision). → 479 (477 + 2).
// SW-WV-F1: +2 the Weft Board read commands (wv_list_attention_board, wv_get_lease_status) —
// controller-tier reads for the attention screen + lease badge. → 481 (479 + 2).
// SW-WV-F7: +1 the FE dispatch bridge command (wv_dispatch_worker) — a thin door over
// the C1 dispatch core, the M1 first-click seam. → 482 (481 + 1).
// SW-WV-F4: +1 the review-at-altitude read command (wv_read_pending_assertions) — reads the
// header's pending list off the UI thread. → 483 (482 + 1).
// SW-WV-C7: +1 stage_bundled_worker_exit_contract_pack (stage the opt-in worker
// exit-contract flavor pack for install). → 484 (483 + 1).
// SW-WV-F5: +2 the endorsement surface commands (wv_list_pending_approvals — the pending
// T2/T3 read the YOU-OWE surface merges; wv_mark_approval_surfaced — the surfaced-item guard
// arm, FE half of the E3 window). → 486 (484 + 2).
// SW-WV-RT1: the signal-done bridge (weft::dispatch_bridge::wv_signal_done_bridge) is
// registered under the `weft::` prefix, which this counter does not tally (it counts only
// `commands::`/`menu::`/`mcp::` lines), so the constant is unchanged.
// SW-WV-QH1 (#1431): +2 — the two harness-only seed commands (`wv_e2e_seed_dispatch`
// + `wv_e2e_seed_endorsement`) were registered in generate_handler![] but this count
// was not bumped. Both refuse unless `CODESENSE_E2E=1`, so they ship inert. → 488.
// SW-WA-E3: +1 — the harness-only lease driver (`wv_e2e_drive_lease`), inert
// unless `CODESENSE_E2E=1`. → 489.
// SW-OC-2: +3 — the three real Connections commands (`preview_connection`,
// `list_writable_packs`, `add_simple_connection`) moved from draft to registered. → 492.
// SW-CN-45: +1 — `connect_github_token` (GitHub manual-token connect path). → 493.
// SW-OC-3: +1 — `remove_source_repo_from_team` (detach a source repo from a team,
// preserving the local clone) moved from draft to registered. → 494.
// SW-WA-D4: +1 — the harness-only posture driver (`wv_e2e_drive_posture`), inert unless
// `CODESENSE_E2E=1`. → 495.
// SW-WA-E6: +1 — the operator gate-status list read (`wv_gate_status`), a pure read over
// the SW-WA-D5 gate store for the weft-gates panel + header chip. → 496.
// SW-WA-E2: +1 — the harness-only closed-gate park driver (`wv_e2e_drive_gate`), inert
// unless `CODESENSE_E2E=1`. → 497.
// SW-WA-E1: +2 — the two harness-only capstone-arc drivers (`wv_e2e_drive_e1` drives the
// whole Stage-1 happy-path arc through the real seams up to ratify; `wv_e2e_e1_reply_state`
// reads the post-ratify close-the-loop state back). Both inert unless `CODESENSE_E2E=1`. → 499.
// SW-WA-E4: +1 — the harness-only decoupling-reply driver (`wv_e2e_drive_decoupling_reply`),
// inert unless `CODESENSE_E2E=1`. → 500.
// SW-WA-E7: +3 — the three harness-only Stage-2 red-path drivers (`wv_e2e_drive_tier_spawn`,
// `wv_e2e_drive_fold`, `wv_e2e_drive_gate_open`), each inert unless `CODESENSE_E2E=1`. → 503.
// Tauri door the FE tab-open listener answers the MCP verb through. → 504.
// SW-WA-F4: +2 — the harness-only round-trip drivers (`wv_e2e_terminal_open_run` +
// `wv_e2e_terminal_read`), inert unless `CODESENSE_E2E=1`; they drive the real F1/F2/F3
// verbs open→run→read (open+run one-shot, read polled). → 506.
// (F2/F3 add MCP `#[tool]` verbs, dispatched by the router — NOT `generate_handler!`
// Tauri commands — so they do not move this count; only these e2e doors do.)
// SW-WA-A8: +2 — the operator "Arm controller" affordance (`wv_arm_controller`, enqueues the
// SAME durable controller fire A3 auto-arm fires) + the harness-only arm driver
// (`wv_e2e_arm_controller`, inert unless `CODESENSE_E2E=1`, replays the arm's first act so the
// weft-e2e cold-open scope-row proof runs without the RT1 bridge). → 508.
// SW-WA-A9: +1 — the harness-only agent-independent bootstrap driver
// (`wv_e2e_cold_open_bootstrap`, inert unless `CODESENSE_E2E=1`, drives the REAL
// `perform_bootstrap_arm` seam so the weft-e2e proves a cold-open seat WITHOUT the arm door). → 509.
// SLW-PT-9: +1 — `ensure_personal_team_root` (resolve-or-create the standalone Personal repo
// on demand for a new-personal-file save; the connector + notes save paths route through the
// same shared seam without adding a command). → 510.
// SLW-PT-15: +1 — `probe_backup_remote` (read-only probe of a re-point / restore
// target: empty vs non-empty + the logical paths that collide with the local
// personal tree, seeding the §L chooser + §O batch reconcile). → 511.
// SLW-PT-12: +1 — `move_content_between_contexts` (MOVE a content item between any
// two Contexts incl. Personal, both directions; atomic write-then-remove). → 512.
// SW-WA-E13: +3 — the harness-only bridge-free end-proof drivers (`wv_e2e_drive_e13`,
// `wv_e2e_drive_e13_reply`, `wv_e2e_e13_reply_state`, all inert unless `CODESENSE_E2E=1`)
// that drive the WHOLE self-driving loop with NO RT1 seed/done bridge on the happy path
// (real triage → real A10 waker → controller dispatch → real connector_reply to a loopback
// stub), registered in generate_handler![] and reached only by the QH weft-e2e harness. → 515.
// SLW-PT-14: +3 — `remove_content_from_context` (REMOVE a content item from a single
// Context; the §J personal sole-copy gate is decided on the FE), plus the "Stop using
// Personal on this machine" reversal pair: `preview_stop_using_personal` (read-only §C
// data-loss facts — existence + local item count + remote-bound) and
// `stop_using_personal_on_this_machine` (tears down the local Personal space + edges;
// re-entrant, re-creatable via SLW-PT-9). → 518.
// Host-owned terminal layout: +12 — the `pty_layout` layout verbs (snapshot, open/close/
// place/rename/move tab, set active/grid/view hint/tags/descriptor, close workspace). → 530.
// −1 — `detect_team_setup_progress` retired with the Guided Setup checklist it fed. → 529.
// Default validator gates: +3 — `default_validator::{resolve_default_validators,
// set_repo_test_command, load_default_gates}`. → 532.
// +5 — create a story from the app (`preview_new_story`, `create_story`,
// `validate_story_file`) and the sample project's repos (`create_sample_repo`,
// `remove_sample_repos`). → 537.
// +2: `read_gate_evidence`, `story_live_run_id`. → 539.
// +1: `story_repo_choices` (a story's epic repos, for setting a missing repo). → 540.
// +1 — `agents::list_agent_signals` (the built-in adapters' basename + attention channel
// for the terminal-tab dot floor). → 541.
// +1: `run_gate_for_move` (run the gate a status move crosses). → 542.
// +2: `pty::terminal_server_status`, `pty::restart_terminal_server` (the terminal
// server's version skew and its restart). → 544.
// +2: `sample_project_sessions` (sessions running in the sample project, for the removal
// confirmation) and the pack-automation turn-on command (a pack automation installs off). → 546.
// +2: the read and dismiss commands for the one-time notice when pack automations
// became opt-in. → 548.
// +1: the weft scope list (SW-AV-18, the who-opened-whom tree read). → 549.
// +1: the Agent Profiles catalog read (SW-AV-12). → 550.
// +2: `user_shell::terminal_shell_info`, `user_shell::set_terminal_shell_override` (the
// shell terminals spawn, and its Settings override). → 552.
// +1: `connectors::connector_event_vocabulary` (SW-AV-23, the per-connector event/filter
// vocabulary the Trigger editor draws from). → 553.
// +1: `guest_session::set_guest_session` (SW-LN-B20, the frontend declares a local guest). → 554.
// +1: `guest_session::get_guest_session` (SW-LN-B47, the frontend recovers the guest from the
// backend's on-disk marker when its webview store is empty). → 555.
// −12: SW-AV-27 removed the lease/controller/gate control plane's Tauri commands
// (`wv_arm_controller`, `wv_get_lease_status`, `wv_gate_status`, and the QH weft-e2e drivers
// `wv_e2e_arm_controller`/`wv_e2e_cold_open_bootstrap`/`wv_e2e_drive_lease`/`wv_e2e_drive_posture`/
// `wv_e2e_drive_gate`/`wv_e2e_drive_gate_open`/`wv_e2e_drive_tier_spawn`/`wv_e2e_drive_e1`/
// `wv_e2e_drive_e13`). → 543.
// +1: `pty::sessions_running_processes` (the close gesture's verdict: each shell's
// foreground and background jobs). → 544.
// −1: `dashboard_launch::dashboard_detach_session` (retired with "Send to background":
// Hide now keeps a tab in the tray, never detaching it to the Agents panel). → 543.
// SW-RT-7b: −5 — the viewer store became a projection of the host-owned terminal layout,
// retiring the direct spawn / warm-open / kill commands (a terminal opens and closes through
// the layout's verbs), the live-session enumeration (the layout snapshot restores the tabs)
// and the MCP open-tab reply bridge (`open_terminal_tab` opens through the layout). → 538.
// SW-FU-B4: +2 — `adopt_org_clone_cmd` ("Use my clone" on join) and
// `set_aside_org_target_cmd` (move what's in the team's folder aside). → 540.
// +1: `pty_layout::terminal_respawn_tab` (a new process in an exited tab, same id and
// place: Restart and Relaunch on an ended pane). → 541.
// Remote Machines: +10 — the machine registry and SSH connector (`list_machines`,
// `list_ssh_sources`, `capture_machine_passphrase`, `test_machine_connection`,
// `install_terminal_server`, `save_machine`, `set_machine_hidden`,
// `set_machine_auto_connect`, `unshare_machine`, `remove_machine`). → 551.
// +5: `connect_machine`, `disconnect_machine`, `retry_machine`,
// `prepare_machine_terminal_server_replacement`, `restart_machine_terminal_server`. → 556.
// +1: `terminal_layout_admission` (SW-RM-5a-a registered it without the bump). → 557.
// +1: `reserve_terminal_stream` (a viewport stream id allocated before its
// subscribe, so the frontend listens first). → 558.
// The constant lagged one registered handler behind lib.rs; +1:
// `pty::read_terminal_text` (a session's emulator terminal as plain text). → 560.
// Remote workspace terminals (#1729) registered one more without the bump. → 561.
// −1: the byte-delta rehydrate command, deleted with the raw output ring (every
// read comes from the host's emulator terminal). → 560.
// Scoped agent MCP over private session connections (#1740) registered one
// more without the bump. → 561.
// +13: the Routines tab's `routine_*` commands. → 574.
// +4: the routine editor's save, schedule preview and restore, and the
// authoring agent it opens. → 578.
// −4: the standalone-automation preflight commands and the overlap-skip
// audit read, deleted with the standalone cadence (clock-driven work is a
// routine). → 574.
// +2: `add_source_repo_to_team` and `attach_source_repo_clone` (the one
// add-repo core the forms and MCP share); −1: `is_git_working_tree`, now
// checked inside that core. → 575.
// Local profiles + local context attachment (#1772): −93 retired service and
// managed-Git commands (connectors, connectors_pull, oauth_authcode, auth,
// github, org clone/connect, personal_team backup, git/git_sync, attachments,
// registry PRs, …); +9 (context attachments, local organization, local
// profile, routine results). → 491.
// +1: `agent_launch_prepare` (the frontend's interactive launch of a named agent). → 492.
// +8: the Agents tab (`agents_list`, `agent_detail`, `agent_archive`,
// `agent_unarchive`, `agent_delete`, `agent_copy_to_mine`, `agent_binding_set`,
// `agent_binding_clear`). → 500.
// +5: the agent editor (`agent_save`, `agent_preview`, `agent_restore_revision`,
// `agent_startup_catalog`, `agent_open_authoring_agent`). → 505.
// −1: the Agent Profiles catalog read, retired with the pack-YAML profile model (agents
// replace it). → 504.
// −4: the user launch-profile overlay's three CRUD commands and its MCP
// picker's server read; agents replace them. → 500.
// −2: the harness-only `wv_e2e_drive_e13_reply` / `wv_e2e_e13_reply_state` connector-reply
// drivers, retired with connectors. → 498.
// +1: `ensure_codesense_docs` (place + register the bundled CodeSense docs repo). → 499.
// −3: the dead weft triage / reply-owed surface — `wv_advance_watermark_cmd` (the triage
// watermark advance) and the harness-only `wv_e2e_e1_reply_state` /
// `wv_e2e_drive_decoupling_reply` completion-reply drivers. → 496.
// −2 (SLW-PT-19): `ensure_personal_team_root` and `list_provider_content_items`, retired
// with the `_personal` org (the scratch space resolves per org; nothing called them). → 494.
// +1: the read-only remote MCP compatibility catalog command (landed on main without this
// constant moving). → 495.
// +1: `local_default_branch` (the branch a new agent worktree starts from, named in the
// launch picker). → 496.
// SLW-PT-20: +4 scratch-use commands (get/set_scratch_team_use,
// list_scratch_overlay_files, set_scratch_file_use_team). → 500.
// +1: resolve_rendered_document replies to an MCP live document read. → 501.
// −18: commands whose only callers were the right pane's Agents/Workflows/Triggers panels
// and the Worktree lens, all deleted (the weft worker probe went too, but registers under
// `weft::`, which this count does not track). → 483.
// SLW-PT-21/23: −5 retired Personal UI commands (create_personal_team,
// stop_using_personal_team_in_teams, get_personal_team_status,
// preview_stop_using_personal, stop_using_personal_on_this_machine); +2 the Scratch
// space tab (list_scratch_space, create_scratch_file); −1 `promote_provider_item`
// (the retired card's copy-into-a-team; nothing calls it). → 479.
// −1: `preflight_mcp_json`, retired with every write of CodeSense's own config into a
// launch folder (launches carry it per session). → 478.
// −2: `plan_pack_migration` and `migrate_flattened_pack_installs`, deleted with the
// flat-payload migration. → 476.
// −3: the bundled starter, automation-authoring and worker-exit-contract staging commands
// (the starter is gone, the skills ship as docs examples, Weft stages its contract). → 473.
// +1: `retire_legacy_terminal_server` (stop the shared-account server a removed or
// switched legacy machine left running). → 474.
// SW-WA-G-MSG: +3 `list_inbox_messages`, `reply_as_human`, `mark_inbox_message_read`
// (the Agents panel's Messages list). → 477.
// SW-BSI-2: +6 role-keyed delivery commands (register_role_cmd, claim_role_messages_cmd,
// accept_role_message_cmd, ack_role_message_cmd, release_role_cmd, send_role_message_cmd).
// → 483.
// Inbox state: +4 (archive, now batched, `list_agent_updates`,
// `get_human_status`, `set_human_status`). +3 drift absorbed since the SW-BSI-2
// squash (upstream handlers landed on main between the merge and this PR). → 490.
// SW-PK-4: −17 pack override, provenance, fork and update commands. → 474.
// SW-BSI-3: +1 (`inbox_attachment_status` — resolve what the inbox knows about an
// attached managed path ahead of opening it, so a chip can offer a precise recovery
// instead of generic unavailable). → 475.
// −40: pipeline.d automations fold into Routines — the automation read-model, authoring,
// mute and audit-gate commands, the workflow and watch libraries and Build canvas, the
// status-transition authoring trio, trigger simulation, dry-run, test-fire and Run now, the
// subject automation summary/preview/assign trio, the run-focus graph (it drew workflow
// revisions), the accepted-events lane and the runner-availability probe. → 435.
// +1 `prepare_remote_story_workspace` (create a story workspace on a machine). → 436.
// +1: `list_panes` (custom right panes), and +3 drift absorbed from handlers that
// landed on main without a count update. → 440.
// SW-PK-2: −3 — every launch names an agent: `resolve_launch_profile`, the profile-mode
// `dashboard_launch_headless` route and `list_remote_mcp_profiles` are gone. → 437.
// −6: JSON widgets retire for HTML panes — the widget grant ledger (load/grant/revoke),
// team widget discovery, register-from-binding and the bundled story-review pack stager. → 431.
// SW-BSI-3 Phase 2: +1 `trigger_hydrate_from_inbox` (the UI's hydrate-attachment button
// delegates to the shared remote-hydration engine). → 432.
// +3 unreadable machine files: list, open as text, remove; +1 `agent_launch_open` main registered without a bump. → 436.
// −14: the pack system is gone — the installed-pack list, install/uninstall/peek/preview/
// export/manifest, the authoring surface and the user-local enable toggle. → 422.
// SW-OC-25b: −15 — the status gate retires: the gate door and gate-for-move run, the human
// override, the gate-state read, gate evidence, validate_ref provenance, the default validator
// trio, the field-schema read, the run-metadata validation and prompt preview, the pipeline
// document-type and source-repo writers, and the sprint-ledger write. → 407.
// SW-OC-25c: −14 — sprint auto-run and `pipeline.yml` transitions retire: the ready queue,
// sprint/epic dispatch and run-epic-action, run-state pause/resume/stop, the held-step read and
// approve, the story live-run lookup, the transitions read, and the two dev/test orchestration
// probes. → 393.
// SW-OC-25d: −4 — the automation subject provider (`resolve_subject`, `enumerate_subjects`,
// `query_related_subjects`) and `preview_predicate`, deleted with the predicate engine. → 389.
// SW-OC-27: −7 — the story-worktree Hydration overlay retires with explicit selected
// context: the hydration read, pull-in, release, closure preview and the three policy
// read/writes. → 382.
// +1 `withdraw_inbox_message` (cancel a message to an agent before it is picked up). → 383.
// +2: explicit local activity capability upgrade review and confirmed update.
// +3: the page bridge's `context()` / `openFile` resolvers (`bridge_doc_context`,
// `bridge_resolve_open_target`) and its typed frontmatter write (`update_frontmatter`). → 388.
// +1 `set_local_identity` (the Settings identity editor saves the viewer's name and avatar). → 389.
// +1 `resolve_tab_source_repo` (a terminal tab's source repo, for its per-tab source/diff). → 390.
// +7 the remote source/editor commands (`resolve_tab_source_repo_remote`, `source_remote_*`). → 397.
// +1 `source_remote_tracked_count` (Phase 2 scale probe for the fsmonitor nudge + preset selection). → 398.
// +2 `agent_set_controller_avatar` (the Controller's face, saved on the team's Controller) and
// `avatar_pack_share_with_team` (copy a personal avatar pack into the team). → 400.
// +5 the agents read model for the page bridge (`agent_read_sessions`, `agent_read_agent`,
// `agent_read_inbox`, `agent_read_roles`) and the human's dashboard send
// (`send_inbox_as_human`). → 406.
// +6 the page bridge's routine and run reads, each its MCP twin's shape
// (`routine_read_list`, `routine_read_get`, `routine_read_runs`, `routine_read_run`,
// `run_read_list`, `run_read`). → 412.
// +2 the page bridge's run evidence and work-product reads, each its MCP twin's shape
// (`run_read_evidence`, `run_read_work_product`). → 414.
// +1 DESIGN-remote-phase-2.md §3 Bug A fix: remote worktree single-level directory
// listing (`source_remote_list_directory`) so the SSH-remote Editor tab can browse
// its tree instead of showing "Worktree isn't ready yet." → 415.
// −1 `agent_set_controller_avatar`: no agent is special, so no Controller face command. → 414.
// SW-XS-3: −6 — automatic forward sync is gone: `switch_team_install`,
// `trigger_startup_wave`, `retry_worktree_sync`, `set_forward_sync_default`,
// `set_forward_sync_overrides`, `get_out_of_date_surfaces`. → 408.
// SW-XS-4: −4 +2 — automatic reverse sync is gone: `set_reverse_sync_overrides`,
// `set_reverse_sync_default`, `resolve_conflict`, `list_conflicts`; added the one-time
// upgrade notice `get_unpublished_context_notice` / `ack_unpublished_context_notice`.
// +1 `worktree_unpublished_context` (the remove confirmation's unpublished count). → 407.
// SW-XS-17: −5 — no worktree cap and no idle reaper: the cap push, the creation
// admission gate, the idle-threshold push, the pin push and reap-now. → 402.
// +1 SW-XS-9 `update_workspace_base` (move a machine workspace to its newest base). → 403.
// SW-AGR-1: +1 `agent_restore_routines` (unarchiving an agent offers its routines back). → 404.
// SW-XS-5: −3 — directory ownership is gone: the nav's surface read, its per-file
// read and its per-dir write. → 401.
// SW-AGR-9: +1 `wake_history` (a tab's wake history for its timer popover). → 402.
// SW-AGR-3: +3 "Remind this agent…" and the timer popover, as the person in the app
// window: `remind_session`, `session_timers`, `cancel_session_timer`. → 405.
// SW-XS-14: −3 +1 — view overrides are gone (`add_view_override`,
// `remove_view_override`, `load_view_overrides`); added
// `agent_dismiss_context_suggestion` (the one-time `projections.yml` suggestion). → 403.
// SW-XS-15: +2 — the left nav's agent context overlay: `agent_context_preview` (the
// want set for an agent in a repo) and `agent_context_update` (add/remove from it). → 405.
// SW-XS-15: −1 — the nav's install-tier composite overlay (`resolve_composite_for_repo`)
// is gone; the pivot reads `agent_context_preview`. → 404.
// SW-ODC-11: +1 `ensure_sample_repo` (place the bundled sample source repo under
// the CodeSense home so the shipped seeded DEMO epic resolves to a real clone). → 405.
// SW-AGR-5: +1 `routine_runs_since` (the Agents tab Schedule's Recent: every enabled
// routine's runs of the last days in one bounded read). → 406.
// +5 the worktree editor: a file's committed bytes and the quick-open list, local
// (`worktree_file_head`, `worktree_list_files`) and remote (`source_remote_file_head`,
// `source_remote_list_files`), plus the remote open-file poll
// (`source_remote_file_inspect`). → 411.
// +2 one session's conversation across teams and the inbox rollup
// (`list_session_inbox`, `list_inbox_rollup`). → 408.
// +1 the terminal caption's prompt segment read on the session's host
// (`session_prompt_cmd`). → 409.
// Merged: 411 + 3 (inbox) = 414.
// +3 notes history (`notes_history`, `notes_read_at`, `notes_restore`), -1 the retired
// notes agent's `ensure_notes_provisioned`. → 416.
// +5 the context repo's status button and History (`context_repo_status`,
// `resolve_context_conflict`, `hand_context_conflict_to_agent`, `run_context_sync_action`,
// `context_repo_history`), -1 the right-pane History panel's `get_sync_history`. → 420.
// +2 machine-armed routines (`routine_arm_on`, `routine_handover_release`). → 422.
// +2 context operations (cancel_context_operation, discard_context_edits) (SW-XS-10) → 424
// +1 "Refresh on machine" for a machine-armed routine (`routine_refresh_on_machine`,
// SW-XS-25). → 425.
// +1 `send_prompt_to_agent` (Notes' "Send to…" reaches an agent wherever its tab runs,
// a machine's included). → 426.
// +1 `resolve_team_landing_page` (a team's home page; landed on main without the bump). → 427.
// +1 `mark_inbox_messages_read` (Updates' "Mark all read" reads many at once, SW-AGR-18).
// → 428.
// SW-XS-27: −2 +2 — the one-time SW-XS-4 notice (`get_unpublished_context_notice`,
// `ack_unpublished_context_notice`) is replaced by the live count
// (`team_file_edits_status`) and its Publish (`publish_context_edits`). → 428.
// +1 `discard_team_file_edits` (the list's Discard, requested by the app). → 429.
// SW-AGR-19: +2 `machine_terminal_server_update_mode`, `reinstall_machine_terminal_server`
// (the confirmed update fallback). → 431.
// SW-AGR-19 PR-4: +2 `machine_pending_tabs`, `reopen_machine_pending_tabs`
// (reopen an unfinished update's tabs). → 433.
// SW-WC-1: +4 `archive_workspace`, `unarchive_workspace`,
// `get_workspace_archive_state`, `list_workspace_archive_states` (per-workspace
// archive flag — one nullable timestamp in `~/.codesense/workspace-registry.json`;
// SW-WC-2's chat compose box reads it). → 437.
// SW-AGR-19 PR-5: +2 `local_terminal_server_update`, `update_local_terminal_server`
// (This Mac's update). → 439.
// SW-WC-2: +4 `workspace_chat_send`, `workspace_chat_read`,
// `workspace_chat_list_workspaces`, `workspace_chat_archive` (the chat engine's
// dashboard-facing Tauri surface — JSONL per workspace under
// `~/.codesense/workspace-chat/`). → 443.
// SW-AGR-19 PR-7: +2 `list_stray_terminal_servers`, `stop_stray_terminal_server`
// (This Mac's stray terminal servers). → 445.
// SW-AGR-19 PR-8: +3 `start_terminal_servers_update`, `terminal_servers_update_status`,
// `review_terminal_server_update` (the app's Update all, run by the backend). → 448.
// +2 `check_install_leftovers_cmd`, `heal_install_tree_cmd` (fold the legacy
// install tree into content; landed on main without the bump). → 450.
// +2 `move_workspace_bridge`, `list_move_intents_bridge` (move a workspace to
// another machine; landed on main without the bump). → 452.
// +1 `resolve_agent_cli` (the launch-time agent CLI path for frontend launches). → 453.
// +1 `resolve_open_workspace_machine` (SW-FX-131: the viewer's verdict on an `open_workspace`
// machine open, so the call returns its outcome). → 454.
// +1 SW-FX-138 (#2233): the page bridge's session usage read (`session_read_usage`),
// token use and spend per session, from each CLI's own reports. → 455.
// +1 SW-FX-138 (#2233): the usage rate command (`session_set_model_rate`), which prices unpriced
// usage. → 456.
// +1 `session_read_model_rates` (the page's `modelRates`: every stored rate, seeded or set). → 457.
// −3 `create_sample_repo`, `remove_sample_repos`, `sample_project_sessions` (the retired
// sample project). → 454.
// +1 `worktree_live_session_count` (Remove worktree… refuses while something else runs in
// the folder, and waits for the sessions it ended). → 455.
// -1 `dashboard_stop_all` (Stop all left the Agents pane and nothing else called it). → 454.
// SV-7: +5 `terminal_server_overview`, `stop_terminal_server`, `start_terminal_server`,
// `replace_terminal_server`, `end_unowned_terminal_session` (Settings manages this
// instance's terminal server). → 459.
// SV-8: +1 `set_terminal_server_menu_bar` (the opt-in menu-bar item). → 460.
// SV-2: +1 `forget_missing_workspace` (Remove from list for a machine workspace whose
// folder is gone). → 461.
// It counts only the `generate_handler!` entries under `commands::`, `menu::` and `mcp::`, so
// it runs 22 behind the Rust count in `commands.contract.test.ts`, which also counts
// `license::commands` (7), `weft` (5), `renderer_watchdog` (1) and `dashboard_publish` (9).
// SV-PLG-3 plus frame mint/release/resolve, minus session-secret getter.
// SV-PLG-5 (dfb6856d9, merged be0ae51c8/#2393): dashboard profile read/write. → 471.
// SV-INP-10: independent bundled Inbox recovery.
// SV-ENG-18: native spend-cap and model-rate acceptance.
// SV-INP-14: +1 `load_inbox_media` (explicit image loading under inbox grants).
// SV-PLG-19B (#2602) +1 `nav_model_publish` and SV-INP-15 (#2606) +1 `react_as_human` merged
// together with one bump between them (SV-PBF-176). → 493.
// +1 session_read: the joined session facts read.
// +1 open_diff_read: revalidated read-only source diff tabs.
// SV-PBF-267 / SV-PBF-242 item 5: 498 once the count test could see the wrapped
// block again. +1 is `commands::data_read_model::setup`; the other +2 were already
// drift on main (SV-PBF-242 saw 496 vs 494) while the test was blind.
// SV-PBF-287: +1 story_totals, the batch story totals read.
// SV-ENG-120: +1 get_log_debug_enabled, the log-level read behind the bridge-timing logs.
// SV-PBF-230: +1 `story_default_start_point` (the base a new story branch takes). → 501.
pub const EXPECTED_COMMAND_COUNT: usize = 501;

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts identifiers inside the `generate_handler!` block in `lib.rs`
    /// at build time and asserts it equals [`EXPECTED_COMMAND_COUNT`]. A
    /// mismatch surfaces a missed registration / leftover dead entry as
    /// a clear test failure with both numbers logged.
    ///
    /// Parses `lib.rs` lexically. Review-iter-1 hardening: strips a
    /// leading `#[cfg(...)]` attribute IN PLACE before checking the
    /// `commands::`/`menu::`/`mcp::` prefix, so a future rustfmt run that
    /// collapses `#[cfg(target_os = "macos")] commands::foo,` onto one
    /// line still counts the command.
    #[test]
    fn registered_command_count_matches_expected() {
        let lib_rs = include_str!("../lib.rs");
        let start = lib_rs
            .find("tauri::generate_handler![")
            // SV-PBF-267: lib.rs wraps the macro in `invoke_stats::counted(...)`, so
            // match the macro itself, not the `invoke_handler(` call around it.
            .expect("generate_handler! block not found in lib.rs");
        let end_offset = lib_rs[start..]
            .find("])")
            .expect("closing `])` of generate_handler! not found");
        let block = &lib_rs[start..start + end_offset];

        let mut count = 0usize;
        for line in block.lines() {
            let mut trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.is_empty() {
                continue;
            }
            // Strip a leading `#[...]` attribute (cfg-gate, etc.) so the
            // remainder of the line is still inspected for a command
            // identifier. Tracks bracket depth to handle nested.
            while trimmed.starts_with("#[") {
                let mut depth = 0i32;
                let mut end_idx = None;
                for (i, b) in trimmed.bytes().enumerate() {
                    match b {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                end_idx = Some(i + 1);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                match end_idx {
                    Some(i) => trimmed = trimmed[i..].trim_start(),
                    None => break,
                }
            }
            if trimmed.starts_with("commands::")
                || trimmed.starts_with("menu::")
                || trimmed.starts_with("mcp::")
                || trimmed.starts_with("records::commands::")
            {
                count += 1;
            }
        }
        assert_eq!(
            count, EXPECTED_COMMAND_COUNT,
            "registered command count drift: lib.rs has {} commands, EXPECTED_COMMAND_COUNT = {}. \
             Either update the constant in commands/mod.rs or add/remove the missing handler.",
            count, EXPECTED_COMMAND_COUNT
        );
    }

    /// SW-XS-17 (AC-1/AC-2): CodeSense no longer caps worktrees or reaps idle ones,
    /// so none of the cap, admission, idle-threshold, pin or reap-now commands is
    /// registered, and the module that held them is gone. Names are assembled at
    /// runtime so this guard does not match itself in the source scan below.
    #[test]
    fn cap_and_idle_reaper_commands_are_gone() {
        let lib_rs = include_str!("../lib.rs");
        let start = lib_rs
            .find("tauri::generate_handler![")
            .expect("generate_handler! block not found in lib.rs");
        let end = lib_rs[start..].find("])").expect("closing `])` not found");
        let block = &lib_rs[start..start + end];
        let cap = ["worktree", "_cap"].concat();
        for name in [
            format!("set_{cap}"),
            ["ensure_worktree", "_capacity"].concat(),
            ["set_worktree", "_idle_threshold"].concat(),
            ["set_worktree", "_pins"].concat(),
            ["reap_worktrees", "_now"].concat(),
        ] {
            assert!(
                !block.contains(&format!("::{name},")),
                "`{name}` is still registered; SW-XS-17 removed the worktree cap and reaper"
            );
        }
        let module = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/commands")
            .join(format!("{cap}.rs"));
        assert!(!module.exists(), "{} still exists", module.display());
    }

    /// SW-XS-17 (AC-5): the crate's source holds no trace of the cap, the reap
    /// planner, the automatic reap primitive or the idle clock. The one allowed
    /// mention of the old idle-clock file is the launch cleanup that deletes it
    /// (`install/worktree_index.rs`).
    #[test]
    fn no_cap_or_idle_reaper_code_remains() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let idle_clock_file = ["session-activity", ".state"].concat();
        let tokens = [
            ["worktree", "_cap"].concat(),
            ["plan", "_reap"].concat(),
            ["enforce_worktree", "_cap"].concat(),
            ["flush_then_prune", "_worktree"].concat(),
            ["note_session", "_activity"].concat(),
            ["WORKTREE_REAP", "ED_EVENT"].concat(),
            ["WORKTREE_REAP", "ING_EVENT"].concat(),
            idle_clock_file.clone(),
        ];
        let mut hits = Vec::new();
        for entry in walkdir::WalkDir::new(&src)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        {
            let rel = entry.path().strip_prefix(&src).unwrap().to_path_buf();
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            for token in &tokens {
                if *token == idle_clock_file
                    && rel == std::path::Path::new("install/worktree_index.rs")
                {
                    continue;
                }
                if text.contains(token.as_str()) {
                    hits.push(format!("{} contains `{token}`", rel.display()));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "leftover cap/reaper code:\n{}",
            hits.join("\n")
        );
    }
}

pub mod private_nav;

pub mod dashboard_commands;
pub mod frames;
pub(crate) use crate::live_hub::agent_list::preferences as agent_list_preferences;
