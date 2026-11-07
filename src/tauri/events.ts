// SPDX-License-Identifier: MIT
/**
 * Tauri event payload types. Co-located with `commands.ts` (the invoke
 * wrappers) but kept in a separate file because event payloads are
 * pure data contracts — they have no IPC plumbing of their own.
 *
 * Created by SS-4c to host the wave-engine event types
 * (`sync-wave-started` / `sync-wave-completed`). Future stories
 * (SS-13's domain events, SS-10's workbench events) co-locate here.
 */

/**
 * Discriminated union projection of the Rust `WaveReason` enum.
 * Serialized via `wave_reason_view` in `content_engine::sync::wave`.
 *
 * `LayerId` is intentionally a string-union stub — SS-13 ships the
 * shared definition. It appears on the `source_layer` fields of the
 * materialize-count payloads below.
 */
// SW-XS-8: the install tiers are gone; team content is the one layer.
export type LayerId = "TeamProjection";

// SW-XS-3: the automatic forward reasons (TeamSwitch, ContentLayerEvent,
// Startup, WorktreeDerive, EpicInheritance) are gone — no wave writes a
// workspace on its own any more. SW-XS-4: so are the automatic reverse reasons
// (ReverseSync, ReverseDirtyBatch, ReverseClaim) — nothing publishes edits back
// on its own. SW-XS-8: the explicit operations run on the preparation engine
// and report through `agent-prepare-progress` (keyed by `operationId`), not
// through waves; the only wave left is the migration cutover.
export type WaveReasonView = { kind: "MigrationCutover" };

export interface SyncWaveStartedPayload {
  /**
   * Monotonic per-process id. Iter-5 (Edge HIGH-1): the UI indicator
   * correlates started/completed by this id so a superseded wave's
   * completed event cannot fade the indicator while a later wave is
   * still running.
   */
  wave_id: number;
  reason: WaveReasonView;
  affected_repo_count: number;
  /** Absolute paths. macOS-only target; UTF-8 lossless serialization. */
  affected_repos: string[];
}

export interface WaveError {
  code_repo_path: string;
  logical_name: string | null;
  message: string;
}

export interface SyncWaveCompletedPayload {
  /** Matches the `wave_id` from the corresponding `sync-wave-started`. */
  wave_id: number;
  reason: WaveReasonView;
  /**
   * Full upfront list from `Wave.affected` (iter-5 / Auditor H2) — stable
   * across cancel. The UI's "synced X of N" reads N from this length,
   * which won't shrink if the wave bails before iterating every repo.
   */
  affected_repos: string[];
  /**
   * Per-wave aggregate counts. `skipped` is the sum of
   * `skipped_already_current + skipped_locally_modified` — AC-9c's
   * indicator collapses both into a single user-facing "skipped"
   * tally.
   */
  materialize_counts: {
    written: number;
    skipped: number;
    unmaterialized: number;
  };
  /**
   * SW-XS-4: files the explicit operation did NOT publish because they changed in
   * both the workspace and team content. Reported here, never as a background
   * `conflict-detected` event. (SW-SY-4's `changed_paths` is retired: no wave
   * writes per-file content a viewer shows, so consumers broad-refresh.)
   */
  conflicts?: WaveConflict[];
  errors: WaveError[];
  duration_ms: number;
  cancelled: boolean;
}

/** SW-XS-4: one file an explicit operation left unpublished because both sides changed. */
export interface WaveConflict {
  code_repo_path: string;
  logical_name: string | null;
  message: string;
}

/**
 * SLW-CO-8 (LD-B): the terminal completion signal for a per-repo dev-env
 * install. The per-run `worktree-run-{run_id}` channel carries only streamed log
 * lines (no exit/terminal event), and an epic's installs run fire-and-forget
 * backend-side — so the FE overlay has no way to observe when a repo's install
 * finished or whether it failed. The backend emits ONE of these on this shared
 * channel per install run; the epic-assembly listener filters by its expected
 * `run_id` set. `ok` is true only on a clean zero-exit run, so a failed `npm ci`
 * (non-zero exit) and a provenance refusal (surfaced as `error`) both count as
 * failures (AC-4).
 */
export interface WorktreeRunCompletedPayload {
  run_id: string;
  ok: boolean;
  exit_code: number;
  error: string | null;
}

/** Tauri topic strings — single source of truth for `listen` call sites. */
export const SYNC_WAVE_STARTED_EVENT = "sync-wave-started" as const;
export const SYNC_WAVE_COMPLETED_EVENT = "sync-wave-completed" as const;
export const WORKTREE_RUN_COMPLETED_EVENT = "worktree-run-completed" as const;
/** An agent's `command_run` or a page's `commands.run`: dispatch a slot command. */
export const COMMAND_RUN_EVENT = "commands:run" as const;
/** The payload of {@link COMMAND_RUN_EVENT} (Rust `RunCommand`). */
export interface RunCommandPayload {
  id: string;
  args?: unknown;
}

/**
 * SLW-WL-2 (OQ-3): a payloadless nudge emitted when a worktree leaves the managed
 * set — from `prune_worktree_locked`, covering BOTH the manual `remove_worktree`
 * command and the auto merge-teardown poll. The Worktrees panel refetches on it so
 * the list stays honest with no polling timer.
 */
export const WORKTREE_CHANGED_EVENT = "worktree-changed" as const;
/** The agent syncing the context repo reported how it went (payload: the repo path). */
export const CONTEXT_REPO_STATUS_CHANGED_EVENT = "context-repo-status-changed" as const;

/** The context repo's git state moved (a commit, push, fetch, checkout, merge or rebase by
 *  anyone), once per settled burst. Rust `CONTEXT_GIT_MOVED_EVENT`. */
export const CONTEXT_GIT_MOVED_EVENT = "context-git-moved" as const;

/** What moved: HEAD, a branch or remote ref, the index, a fetch, merge/rebase state. */
export type ContextGitMoveKind = "head" | "refs" | "index" | "fetch" | "mergeState";

/** Payload for {@link CONTEXT_GIT_MOVED_EVENT} (Rust `ContextGitMoved`). */
export interface ContextGitMovedPayload {
  /** The org root the watch was registered for. */
  repoPath: string;
  kinds: ContextGitMoveKind[];
}

/**
 * SW-AU-34 (AC-5): emitted the first time a window close is handled as
 * background-on-close (the app hides instead of quitting). Payloadless — the FE
 * shows a one-time explainer toast ("codesense is still running…"), gated on the
 * persisted `settings.hasSeenBackgroundExplainer` so it surfaces exactly once.
 */
export const BACKGROUND_MODE_ENTERED_EVENT = "background-mode-entered" as const;

/**
 * SW-OC-26: emitted by the footprint context index after it changes (a watched
 * edit, a periodic reconcile, a newly synced team). Live query surfaces
 * (`csquery`, panes, `codesense.subscribeContext`) re-query on it.
 */
export const CONTEXT_INDEX_CHANGED_EVENT = "context-index-changed" as const;

/** Payload for {@link CONTEXT_INDEX_CHANGED_EVENT} (Rust `IndexChange`). */
export interface ContextIndexChangedPayload {
  generation: number;
  /** Changed paths, capped (see `truncated`). */
  paths: string[];
  /** Every org with a changed file. */
  orgs: string[];
  /** True when all-orgs content changed (it shows in every org). */
  allOrgs: boolean;
  /** True when `paths` was capped or the whole index was replaced: re-run regardless of scope. */
  truncated: boolean;
}

/**
 * Emitted by the context-repo watcher on every relevant content edit
 * (`dispatch_context_event` / the `_codesense` internal handler). Drives the
 * seamless-sync watcher → viewer/nav refresh path.
 */
export const CONTEXT_FILE_CHANGE_EVENT = "context-file-change" as const;

/** Payload for {@link CONTEXT_FILE_CHANGE_EVENT} (Rust `base_event_payload`).
 *
 *  SW-FX-16: `repo_path` is the emitting subscriber's surface working dir
 *  (`ws.cwd`) — NOT a synthetic key — so refresh routing must key off
 *  `context_repo_path` (the surface-independent team/repo context dir) via
 *  `syntheticKeyForContextRepoPath`, not `repo_path`. */
export interface ContextFileChangePayload {
  repo_path: string;
  context_repo_path?: string;
  file_path?: string;
  /** SW-XS-20: set when `file_path` is in a parent team's content — the content
   *  dir of the active team that inherits it, whose nav shows that file. */
  inherited_by?: string;
}

/**
 * SW-SR-9: emitted by the `set_epic_repos` MCP tool after it rewrites an epic
 * doc's `repos:`. The app-level listener re-assembles the epic's integration
 * worktrees (fire-and-forget) so an agent's edit materializes — the same
 * convergence the UI add-path triggers, without blocking the tool call.
 */
export const EPIC_REPOS_CHANGED_EVENT = "mcp:epic-repos-changed" as const;

/** Payload for {@link EPIC_REPOS_CHANGED_EVENT}. */
export interface EpicReposChangedPayload {
  epicFilePath: string;
  repos: string[];
}

/**
 * SS-FU-17 AC-6/AC-7: emitted when a wave skips a code repo whose path no
 * longer resolves (moved / deleted / unmounted). The UI surfaces "this repo
 * looks gone" with Forget / Re-attach affordances rather than auto-pruning.
 */
export const SYNC_REPO_LOOKS_GONE_EVENT = "sync-repo-looks-gone" as const;

/** Payload for {@link SYNC_REPO_LOOKS_GONE_EVENT}. */
export interface RepoLooksGonePayload {
  owner: string;
  repo: string;
  code_repo_path: string;
}

/**
 * SW-WV-F1: emitted after a `wv_attention` write lands (via SW-WV-B3's
 * `wv_upsert_attention` handler) so a mounted Weft Board re-fetches immediately
 * instead of polling — the invalidation seam that makes the operator's attention
 * screen live. The string MUST match the Rust `WEFT_ATTENTION_CHANGED_EVENT` const
 * (`src-tauri/src/weft/events.rs`). Emitted fire-and-forget by B3's write handler, so
 * a missed event only costs latency until the next write/focus refetch. F1 only
 * SUBSCRIBES (`use-attention-board.ts`); it never emits.
 */
export const WEFT_ATTENTION_CHANGED_EVENT = "wv:attention-changed" as const;

/**
 * SW-WV-E1 / SW-WV-F4: the endorsement-gate assertion lifecycle events the
 * review-at-altitude header subscribes to, to keep its pending list live without a
 * poll. `posted` fires when an agent posts a pending assertion (`wv_post_assertion`
 * MCP verb, `handler.rs`); `ratified` / `revoked` fire after the owner's Tauri
 * `wv_ratify` / `wv_revoke` settles (`commands/weft_ledger.rs`). Ratified/revoked
 * carry `{ id, disposition }` (+ E1 `channel`/`claim` enrichment). The strings MUST
 * match the Rust `app.emit` literals; fire-and-forget, so a miss only costs latency.
 */
export const WV_ASSERTION_POSTED_EVENT = "mcp:wv-assertion-posted" as const;
export const WV_ASSERTION_RATIFIED_EVENT = "mcp:wv-assertion-ratified" as const;
export const WV_ASSERTION_REVOKED_EVENT = "mcp:wv-assertion-revoked" as const;

/**
 * SW-WA-F5: emitted by the trusted-side `open_workspace` MCP tool when it opens a
 * story workspace in a VISIBLE mode (headed or visible-unfocused). The trusted
 * process cannot open an FE tab directly, so it fires this reveal signal after the
 * session is spawned+bound; the renderer maps it to the SAME story-workspace open
 * the interactive path uses, focusing only when `focus` is true. Headless opens
 * (opt-in) emit nothing — they stay in the Background agents panel.
 *
 * The string MUST match the Rust `OPEN_WORKSPACE_REVEAL_EVENT` literal
 * (`src-tauri/src/mcp/handler.rs`); fire-and-forget, so a miss only costs latency.
 */
export const OPEN_WORKSPACE_REVEAL_EVENT = "mcp:open-workspace-reveal" as const;

/** How far a story workspace's preparation on a machine has got. Mirrors the
 *  Rust `REMOTE_WORKSPACE_PROGRESS_EVENT` (`mcp/remote_story_workspace.rs`). */
export const REMOTE_WORKSPACE_PROGRESS_EVENT = "remote-workspace-progress" as const;

/** A Step 1 phase (SW-XS-9), as a failure names it. Mirrors the Rust `Step1Phase`. */
export type Step1Phase =
  | "starting"
  | "fetching"
  | "bundling"
  | "copying"
  | "verifying"
  | "creatingWorktree"
  | "updatingWorktree"
  | "runningSetup";

/** One Step 1 progress reading (SW-XS-9): every phase, with determinate counts
 *  where Git or the transfer has them, and a failure that names its phase.
 *  Mirrors the Rust `PrepareProgress` (`mcp/remote_workspace.rs`). */
export type Step1Progress =
  | { phase: "fetching"; objects: number | null; total: number | null }
  | { phase: "bundling"; objects: number | null }
  | { phase: "copying"; sent: number; total: number }
  | { phase: "verifying" }
  | { phase: "creatingWorktree" }
  | { phase: "updatingWorktree" }
  | { phase: "failed"; failedPhase: Step1Phase; message: string };

export type RemoteWorkspaceProgressPayload = { requestId: string } & Step1Progress;

/** A local story worktree's `worktree.yaml` setup (SW-XS-9). Mirrors the Rust
 *  `WORKSPACE_SETUP_PROGRESS_EVENT` (`commands/mcp_workspace.rs`). */
export const WORKSPACE_SETUP_PROGRESS_EVENT = "workspace-setup-progress" as const;

export type WorkspaceSetupProgressPayload = { storyId: string; worktreePath: string } & (
  | { phase: "runningSetup"; command: string }
  /** `worktree.yaml` is team-synced and not approved for unattended setup:
   *  offer Approve, then Retry. */
  | { phase: "needsApproval"; message: string }
  | { phase: "done" }
  | { phase: "failed"; failedPhase: Step1Phase; message: string }
);

/** An explicit `update_workspace_base` (SW-XS-9), keyed by the workspace
 *  handle (or the id it was named by). Mirrors the Rust
 *  `WORKSPACE_BASE_PROGRESS_EVENT` (`mcp/workspace_base.rs`). */
export const WORKSPACE_BASE_PROGRESS_EVENT = "workspace-base-progress" as const;

export type WorkspaceBaseProgressPayload = { workspaceHandle: string } & Step1Progress;

/** The phase a remote launch is in, and the budget that phase has (SW-XS-2).
 *  Mirrors the Rust `REMOTE_LAUNCH_PROGRESS_EVENT`
 *  (`commands/pty_layout/remote_shell.rs`). Each phase gets its own budget from
 *  when it starts; there is no flat timeout. */
export const REMOTE_LAUNCH_PROGRESS_EVENT = "remote-launch-progress" as const;

export type RemoteLaunchProgressPayload = ({ requestId: string } | { handle: string }) & {
  phase: "admitting" | "diffing" | "transferring" | "provisioning" | "spawning";
  /** Bytes this report covers (hashed or copied), when known. */
  bytes: number | null;
  /** Time left in the phase as of this report. */
  budgetMs: number;
};

/** Step 2 (agent preparation) progress, SW-XS-7. Mirrors the Rust
 *  `PREPARE_PROGRESS_EVENT` (`agents/prepare/progress.rs`); the golden
 *  sequence is `src/tauri/fixtures/agent-prepare-progress.golden.json`.
 *  `diff`, then `transfer` / `apply` when anything is copied, then an optional
 *  `warning` (pinned startup files that differ from the agent definition) and
 *  exactly one of `ready` / `failed` / `skipped` — `ready` only after Step 2
 *  and the launch's post-checks completed (X5). SW-XS-8: an explicit operation
 *  (Step 3) emits the same shape with an `operationId`, and may end
 *  `cancelled`. */
export const AGENT_PREPARE_PROGRESS_EVENT = "agent-prepare-progress" as const;

export type AgentPreparePhase =
  | "diff"
  | "transfer"
  | "apply"
  | "warning"
  | "ready"
  | "failed"
  | "skipped"
  /** SW-XS-8: an explicit operation (hydrate / publish) was cancelled. */
  | "cancelled";

/** What `ready` / `failed` carry: lists capped at 50 paths, full counts. */
export interface AgentPrepareSummary {
  status: "ready" | "failed" | "skipped" | "cancelled";
  wanted: number;
  written: number;
  bytesWritten: number;
  unchanged: number;
  keptModified: string[];
  keptModifiedCount: number;
  keptUnmanaged: string[];
  keptUnmanagedCount: number;
  skippedTracked: string[];
  skippedTrackedCount: number;
  /** `[path, reason]` */
  skippedOther: [string, string][];
  skippedOtherCount: number;
  pinnedDrift: string[];
  /** `[path, reason]` */
  failed: [string, string][];
  failedCount: number;
  leftovers: number;
  elapsedMs: number;
}

export interface AgentPrepareProgressPayload {
  workspace: string;
  agent: string;
  phase: AgentPreparePhase;
  /** Files done in this phase. */
  files: number;
  /** Bytes done in this phase. */
  bytes: number;
  /** Wanted files (diff, terminal phases) or bytes to move (transfer, apply). */
  total: number;
  /** Terminal phases and `warning`: the startup note, or why. */
  note?: string;
  /** `ready` / `failed` / `skipped`. */
  summary?: AgentPrepareSummary;
  /** SW-XS-8: the explicit operation (hydrate_worktree, hydrate_story,
   *  reverse_sync_worktree) this event belongs to; absent on a launch's Step 2.
   *  Its sequence is `src/tauri/fixtures/explicit-operation-progress.golden.json`. */
  operationId?: string;
  /** SW-XS-8 (review r1): which explicit operation the event belongs to. */
  operationKind?: "hydrate" | "hydrateStory" | "publish";
}

/** Payload for {@link OPEN_WORKSPACE_REVEAL_EVENT}: the opaque story handle to
 *  reveal, the team the story lives in, and whether the open should steal focus
 *  (headed) or stay visible but unfocused (the controller's auto-dispatch
 *  default). */
export interface OpenWorkspaceRevealPayload {
  /** `session`: a running session no host workspace shows, adopted into a tab by the one
   *  Show. `runOutput`: a headless run, shown as its run output. */
  target?: "story" | "team-workspace" | "session" | "runOutput";
  /** The session to show, for `session` / `runOutput`. */
  sessionId?: string;
  /** A headless run's id, for `runOutput`. */
  runId?: string;
  workspaceId?: string;
  cwd?: string;
  /** SW-FX-131: set when an `open_workspace` call with a `machineId` waits for this reveal's
   *  outcome; the reveal reports it back through `resolveOpenWorkspaceMachine`. */
  requestId?: string;
  handle: string;
  teamKey: string;
  focus: boolean;
  /** Open (creating on first open) the story's workspace on this machine. */
  machineId?: string;
  workspaceKind?: string;
  workspaceName?: string;
  branch?: string;
}

/** A message was stored or read in the agents' inbox; the Agents panel's Messages list
 *  refetches. No payload. Must match the Rust `AGENT_INBOX_CHANGED_EVENT` literal
 *  (`src-tauri/src/commands/agent_inbox.rs`). */
export const AGENT_INBOX_CHANGED_EVENT = "agent-inbox-changed" as const;

/** Role registrations or queues, or the human's status changed; live pages re-read. No
 *  payload. Must match the Rust `AGENT_STATE_CHANGED_EVENT` literal
 *  (`src-tauri/src/commands/agent_read_model.rs`). */
export const AGENT_STATE_CHANGED_EVENT = "agent-state-changed" as const;

/**
 * SW-MC-5 (AC-13): emitted once per tracked repo as a `reconcile_workspace` run
 * resolves it — drives the Screen-12 N-of-M progress bar. Fire-and-forget: a
 * missed event only costs a progress tick; the command's returned
 * `ReconcileOutcome[]` is the source of truth for the terminal screen. The
 * string MUST match the Rust `PROGRESS_EVENT` const (commands/reconcile_workspace.rs).
 */
export const RECONCILE_WORKSPACE_PROGRESS_EVENT = "reconcile-workspace-progress" as const;

/** A machine list may have changed: a registry write (save, remove, share,
 *  unshare, hide, trust, auto-connect) or a connection-state transition.
 *  `scope` is the layer that changed; `null` means re-read every list. The
 *  payload grants no visibility — lists are re-read through their fences. */
export const MACHINES_CHANGED_EVENT = "machines-changed" as const;
export interface MachinesChangedPayload {
  scope: import("./commands").MachineScope | null;
  machineId: string;
}

/** Backend-issued, headed remote shell reveal. Never grants runtime authority. */
export const REMOTE_TERMINAL_REVEAL_EVENT = "mcp:remote-terminal-reveal" as const;
export interface RemoteTerminalRevealPayload {
  teamKey: string;
  workspaceId: string;
  outcome: import("./commands").RemoteTabOutcome;
  privateAuthorityEpoch: string;
}

/** A custom right pane's file (or an asset beside it) changed on disk. Debounced and
 *  coalesced per pane. Mirrors Rust `panes::PANE_CHANGED_EVENT`. */
export const PANE_CHANGED_EVENT = "pane-changed" as const;

/** Payload for {@link PANE_CHANGED_EVENT}. `id` absent: an asset or the `panes/` dir
 *  itself changed, so every pane of that layer reloads. */
export interface PaneChangedPayload {
  scope: "team" | "private";
  org: string;
  team?: string;
  id?: string;
}

/** An agent asked (MCP `pane_open`) to place and reveal a pane. Mirrors Rust
 *  `panes::PANE_OPEN_EVENT`. */
export const PANE_OPEN_EVENT = "pane-open" as const;

/** Payload for {@link PANE_OPEN_EVENT}. */
export interface PaneOpenPayload {
  panelId: string;
  path: string;
  intent?: Record<string, unknown>;
}

/** A routine's definition, local state or one of its runs changed; any view of
 *  routines re-reads. Mirrors Rust `routines::events::ROUTINES_CHANGED`. */
export const ROUTINES_CHANGED_EVENT = "routines:changed" as const;

/** Payload for {@link ROUTINES_CHANGED_EVENT}. */
export interface RoutinesChangedPayload {
  routine_key: string;
  /** The run that changed; null when the routine itself did. */
  run_id: string | null;
}

/** An agent's definition, or this user's account binding for it, changed (from the Agents
 *  tab or an agent's MCP tools); any view of agents re-reads. Mirrors Rust
 *  `agents::events::AGENTS_CHANGED`. */
export const AGENTS_CHANGED_EVENT = "agents:changed" as const;

/** A note changed — written from the Notes pane or by an agent over MCP (`notes_write`, …).
 *  An open notes view reloads when an agent changed it. Mirrors Rust
 *  `commands::notes_history::NOTES_CHANGED`. */
export const NOTES_CHANGED_EVENT = "notes-changed" as const;

/** Payload for {@link NOTES_CHANGED_EVENT}. */
export interface NotesChangedPayload {
  source?: "records";
  /** The org whose notes changed; null for the machine-local store. */
  orgSlug: string | null;
  id: string;
  /** The history commit the change was saved as; absent while the person's typing waits
   *  for its quiet period (a second event follows with it). */
  commit?: string;
  actor: import("./commands").NoteActor;
  kind: "create" | "write" | "restore" | "delete" | "meta";
}

/** A face was added to an avatar pack (MCP `avatar_add`); open face lists re-read. Mirrors
 *  Rust `avatars::AVATARS_CHANGED`. */
export const AVATARS_CHANGED_EVENT = "avatars:changed" as const;

/** Payload for {@link AGENTS_CHANGED_EVENT}. */
export interface AgentsChangedPayload {
  key: string;
  /** SW-XS-15: the revision a definition write produced (absent for a binding change). */
  revision?: number;
}

/** An org's registry (`teams.yml` / `people.yml` / `org.yml`) changed — from the
 *  UI or from an agent over MCP. Any view of the org reloads it from disk.
 *  Mirrors Rust `commands::registry_local::ORG_REGISTRY_CHANGED_EVENT`. */
export const ORG_REGISTRY_CHANGED_EVENT = "org-registry-changed" as const;

/** Payload for {@link ORG_REGISTRY_CHANGED_EVENT}. */
export interface OrgRegistryChangedPayload {
  /** Captured local principal that owns the edited registry. */
  principal: string;
  orgSlug: string;
  /** The edit's `RegistryEdit` variant name, e.g. `"CreateTeam"`. */
  edit: import("./commands").RegistryEdit["type"];
  /** The team the edit touched — the NEW name for a rename. */
  team?: string;
  /** The team's name before a `RenameTeam`. */
  previousTeam?: string;
}

/** An agent switched the active team over MCP; the backend is already
 *  switched and the UI follows the way a team click does. Mirrors Rust
 *  `mcp::setup_tools::ACTIVE_TEAM_SWITCH_EVENT`. */
export const ACTIVE_TEAM_SWITCH_EVENT = "mcp:set-active-team" as const;

/** Payload for {@link ACTIVE_TEAM_SWITCH_EVENT}. */
export interface ActiveTeamSwitchPayload {
  orgSlug: string;
  team: string;
}

/** A local clone was bound to, or unbound from, a source repo — from a form or
 *  from an agent over MCP. Views that show where a repo lives re-read the
 *  attachments. No payload. Mirrors Rust
 *  `commands::source_repo_add::SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT`. */
export const SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT = "source-attachments-changed";

/** SLW-PT-20: a team's scratch-use settings changed (`{ orgSlug, team }`). */
export const SCRATCH_USE_CHANGED_EVENT = "scratch-use-changed" as const;
