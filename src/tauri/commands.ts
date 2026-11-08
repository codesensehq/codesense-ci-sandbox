// SPDX-License-Identifier: MIT
import type { AgentIdentityChoice } from "@/shared/lib/agent-identity-choice";
import { rendererPrincipal } from "@/shared/lib/local-profile-session";
// All Tauri invoke() wrappers go here. No raw invoke() calls elsewhere.

import { invoke } from "@tauri-apps/api/core";
import type { WakeHistoryEntry } from "@/shared/lib/wake-events";
import type { ContextRepoInfo, LoadedOrg } from "@/shared/types";
import type {
  Attachment,
  CredentialRef,
  OrgSummary,
  Person,
  SourceRepoSlug as DomainSourceRepoSlug,
} from "@/shared/types/domain";
import type { NotesScope } from "@/shared/lib/notes-scope";
import type { InstanceCountsSnapshot } from "@/shared/lib/fe-instance-registry";
import type { RustAgentStatus } from "@/shared/lib/agent-status-config";
import type { RunOutcomeRead, RunWorkProduct } from "@/shared/lib/run-outcome";
import type {
  ContextQuery,
  ContextFilter,
  ContextQueryResult,
} from "@/shared/lib/context-query/types";
import type { RunEvidence } from "@/shared/lib/run-evidence";
import type { SessionTranscript } from "@/shared/lib/session-transcript";

// === SW-FX-64: renderer liveness ack (webview watchdog) ===

/** Push a liveness ack to the Rust-side renderer watchdog. Fire-and-forget on
 *  an interval from `startRendererHeartbeat`; if the renderer stops acking, the
 *  watchdog reloads the webview. Returns nothing.
 *
 *  RF-perf-5: optionally carries the FE `performance.memory.usedJSHeapSize`
 *  (`heapBytes`, AC-2) and cumulative `unhandledRejections` (AC-4) so the gated
 *  Rust memory sampler can log them beside the renderer RSS; `init` (AC-4) marks
 *  the first beat of a fresh renderer instance so Rust logs a cause-tagged re-init
 *  breadcrumb.
 *  RF-perf-7: also carries a `renderTick` (a `requestAnimationFrame` paint counter
 *  that freezes when a wedged tree stops painting, AC-2) and `visible`
 *  (`document.visibilityState`), so the watchdog can distinguish a JS-alive-but-blank
 *  renderer from a healthy or legitimately-hidden one.
 *  RF-perf-4: also carries `instanceCounts` (AC-1) — the per-resource FE instance
 *  tally (editors/iframes/WebGL-terminals/mounted-teams) the gated memory
 *  line surfaces beside the RSS to attribute the renderer footprint per resource class.
 *  `rootEmpty` says the app root has no children, which the watchdog logs as a blank window.
 *  All optional — omitted when the engine gates `performance.memory` / rAF. Adding an
 *  optional arg to an existing command does not change the command-count contract. */
export const rendererHeartbeat = (
  heapBytes?: number,
  unhandledRejections?: number,
  init?: boolean,
  renderTick?: number,
  visible?: boolean,
  instanceCounts?: InstanceCountsSnapshot,
  rootEmpty?: boolean,
) =>
  invoke<void>("renderer_heartbeat", {
    heapBytes,
    unhandledRejections,
    init,
    renderTick,
    visible,
    instanceCounts,
    rootEmpty,
  });

// === SS-4c: content-engine wave triggers ===

export type SourceRepoSlug = DomainSourceRepoSlug;

export interface SetActiveTeamForWaveParams {
  codesenseHome: string;
  githubUserId: number;
  orgSlug: string;
  team: string;
  sourceRepos: SourceRepoSlug[];
}

/** SS-4c AC-13/AC-14: activate a team — re-registers its content-layer
 *  watches and commits the active team the MCP server reads. Never writes
 *  content into a workspace (SW-XS-3). */
export const setActiveTeamForWave = (params: SetActiveTeamForWaveParams) =>
  invoke<void>("set_active_team_for_wave", {
    codesenseHome: params.codesenseHome,
    githubUserId: params.githubUserId,
    orgSlug: params.orgSlug,
    team: params.team,
    sourceRepos: params.sourceRepos,
  });

// === SW-EW-5: epic workspace backend primitive ===

export interface EnsureEpicWorkspaceParams {
  codesenseHome: string;
  githubUserId: number;
  orgSlug: string;
  team: string;
  /** SW-EW-15: absolute path to the epic doc. The backend derives the epic id
   *  (`{parent}__{leaf}`) from it — the single derivation site. The FE never
   *  reproduces the id; it reads `dirPath` back from the returned handle. */
  epicFilePath: string;
}

/** The registered epic-workspace target returned by `ensureEpicWorkspace`. */
export interface EpicWorkspaceHandle {
  /** The synthetic `_epic/{epic-id}` slug, as an `owner/repo` string. */
  slug: string;
  /** Absolute path to the epic workspace dir (peer of the clone, never inside it). */
  dirPath: string;
  /** `true` once the dir exists and is registered as a content-engine target. */
  registered: boolean;
}

/** SW-EW-5: ensure the epic's workspace dir exists on disk and is registered
 *  as a content-engine target. Idempotent. Awaits the inheritance materialize
 *  (install + context layers, incl. `.claude/settings.json` + `codesense-*`
 *  hooks) so the agent-state notify hook is on disk before the terminal spawns
 *  — callers `await` this before opening the epic terminal. Flips the EW-1
 *  integration-workspace row from stub to real when it resolves. */
export const ensureEpicWorkspace = (params: EnsureEpicWorkspaceParams) =>
  invoke<EpicWorkspaceHandle>("ensure_epic_workspace", {
    codesenseHome: params.codesenseHome,
    githubUserId: params.githubUserId,
    orgSlug: params.orgSlug,
    team: params.team,
    epicFilePath: params.epicFilePath,
  });

// === SW-DX-18: branch workspace backend primitive ===

export interface EnsureBranchWorkspaceParams {
  codesenseHome: string;
  githubUserId: number;
  orgSlug: string;
  /** Active team — the install-layer context sourced into the worktree. */
  team: string;
  /** Local clone path of the source repo to check the branch out from
   *  (FE-resolved from the `repo` slug via `resolveRealRepoPath`). */
  repoPath: string;
  /** `owner/repo` slug — scopes the per-repo install layer + the lockfile. */
  repoSlug: string;
  /** The branch/PR ref to check out (e.g. a teammate's `dev_branch`). */
  branch: string;
  /** PR head repository; differs from `repoSlug` for fork-based PRs. */
  reviewHeadRepo?: string;
  /** GitHub PR number used to fetch the base repo's trusted `refs/pull/N/head`. */
  pullNumber?: number;
  /** How the branch is checked out. Absent → `review`: the cached PR head
   *  (`origin/<branch>`). `existing` opens a branch that already exists locally;
   *  `new` creates it from the repo's local default branch. None of them fetch. */
  checkout?: BranchCheckoutMode;
}

/** How `ensureBranchWorkspace` checks a branch out (see `EnsureBranchWorkspaceParams`). */
export type BranchCheckoutMode = "review" | "existing" | "new";

/** The registered branch-workspace target returned by `ensureBranchWorkspace`. */
export interface BranchWorkspaceHandle {
  /** The source-repo slug the worktree is a checkout of (`owner/repo`). */
  slug: string;
  /** Absolute path to the branch worktree (under `users/_branch/…`, managed). */
  dirPath: string;
  /** `true` once the worktree exists, is confined managed, and is registered. */
  registered: boolean;
}

/** SW-DX-18: check out `branch` of `repoPath` into a MANAGED worktree (under
 *  `users/_branch/…`) and materialize the active team's install layer
 *  (`.claude/skills`, hooks) into it, so an agent spawned there can
 *  run installed skills. Awaited (like `ensureEpicWorkspace`) so skills are on
 *  disk before the terminal spawns. Rejects `unavailable:` if the checkout would
 *  land outside a managed surface (the `branch` target of `openWorkspace`). */
export const ensureBranchWorkspace = (params: EnsureBranchWorkspaceParams) =>
  invoke<BranchWorkspaceHandle>("ensure_branch_workspace", {
    codesenseHome: params.codesenseHome,
    githubUserId: params.githubUserId,
    orgSlug: params.orgSlug,
    team: params.team,
    repoPath: params.repoPath,
    repoSlug: params.repoSlug,
    branch: params.branch,
    reviewHeadRepo: params.reviewHeadRepo,
    pullNumber: params.pullNumber,
    checkout: params.checkout,
  });

/** The branch a new agent worktree in `repoPath` starts from: its local default
 *  branch, or `HEAD` when none resolves. Reads local refs only. */
export const localDefaultBranch = (repoPath: string) =>
  invoke<string>("local_default_branch", { repoPath });

/** The start point a new story branch in `repoPath` takes when no explicit one is
 *  set: the worktree.yaml `defaultStartPoint` (a branch name as its cached
 *  `refs/remotes/origin/<name>` when that exists), else the repo's local default branch
 *  (a cached remote one as `refs/remotes/origin/<name>`). Rejects, naming the refs tried
 *  and the fix, when none resolves. Reads local refs only. */
export const storyDefaultStartPoint = (repoPath: string, defaultStartPoint: string | null) =>
  invoke<string>("story_default_start_point", { repoPath, defaultStartPoint });

// === Workspace config (R-10) ===

export interface WorkspaceRepoOverrides {
  mainDoc?: string;
}

export interface WorkspaceConfig {
  repos: Record<string, WorkspaceRepoOverrides>;
}

export const loadWorkspaceConfig = (codesenseHome: string) =>
  invoke<WorkspaceConfig>("load_workspace_config", { codesenseHome });

export const saveWorkspaceConfig = async (codesenseHome: string, config: WorkspaceConfig) =>
  invoke<void>("save_workspace_config", {
    codesenseHome,
    config,
    expectedPrincipal: await rendererPrincipal(),
  });

export const configurationPrepare = (
  topic: "profile" | "workspaceConfig" | "prefs",
  principal: string,
  org?: string,
  scope?: "global" | "principal",
) =>
  invoke<import("@/features/dashboard/lib/live-hub").HubSnapshot>("configuration_prepare", {
    topic,
    org,
    scope,
    expectedPrincipal: principal,
  });

/** PTS-3: Delegate to Rust's `path_to_team_content_dir` helper.
 *  Returns the absolute canonical per-team context-repo content dir
 *  (`{orgRoot}/_team/{team}/content/`). Pure path math — no filesystem
 *  touch. Used by `buildSyntheticRepo` to populate
 *  `RepoState.teamContentPath`.
 *
 *  SF-8: prefer the synchronous `pathToTeamContentDirSync` mirror in
 *  `@/shared/lib/team-content-path` for new code — saves an IPC hop
 *  and unblocks same-frame mounts. This command stays registered for
 *  any other callers that may want the round-trip-validated value. */
export const resolveTeamContentDir = (orgRoot: string, team: string) =>
  invoke<string>("resolve_team_content_dir", { orgRoot, team });

// === Frontmatter cache ===

/** R-3: Atomic init for a workspace's frontmatter cache + file watchers.
 *  Clears prior in-memory cache, then builds + persists + merges. SW-WL-3:
 *  watcher attach is no longer done here — it is surface-bound (driven by the
 *  WorkspaceOpenSet per open workspace). Returns the indexed entry count. */
export const initWatchedCache = (orgRoot: string, cachePath: string) =>
  invoke<number>("init_watched_cache", { orgRoot, cachePath });

// SW-CP-18: `capturePendingFiles` / `capture_pending_files` retired. The
// source→context walk it drove is subsumed by the reverse engine + claim/import
// (closed-window new files are adopted at launch; live writes round-trip via the
// reverse watch).

// SW-XS-14: the view overrides (`add_view_override`, `remove_view_override`,
// `load_view_overrides`) and the staleness filter they tuned are retired.

// === App state operations ===

export const saveAppState = async (state: string, patch = false) =>
  invoke<void>("save_app_state", { state, patch, expectedPrincipal: await rendererPrincipal() });

export const loadAppState = async () =>
  invoke<string>("load_app_state", { expectedPrincipal: await rendererPrincipal() });

/** Which kind of space a custom right pane lives in. */
export type PaneScope = "team" | "private";

export interface DashboardRef {
  scope: "built-in" | "team" | "private";
  org: string;
  team?: string;
  id: string;
  installedFrom?: DashboardRef;
}

export type DashboardCommandScope = "global" | "slot-visible" | "slot-focused";

/** A command a slot dashboard declares in its manifest or `codesense:commands` meta. */
export interface DashboardCommand {
  id: string;
  label: string;
  /** A Lucide name, as for pane icons. */
  icon?: string;
  /** Canonical `CmdOrCtrl+Shift+Alt+Key`. */
  defaultChord?: string;
  scope: DashboardCommandScope;
  humanOnly?: boolean;
}

export interface DashboardDeclaration {
  /** Architecture: static hidden-delivery opt-in; Policy: omitted by default. */
  badge?: "live";
  slots: string[];
  surface?: string;
  capabilities: string[];
  commands?: DashboardCommand[];
}

export type DashboardSlotChoice = DashboardRef | DashboardRef[] | "native";
export interface DashboardProfile {
  rightPaneSurfaces?: Record<string, DashboardRef | "native">;
  id: string;
  name: string;
  posture: string;
  slots: Record<string, DashboardSlotChoice>;
  cliLayout: string;
  chrome: Record<string, unknown>;
}
export const writeDashboardProfile = async (
  org: string,
  patch: Partial<DashboardProfile>,
  expectedPrincipal?: string,
) =>
  invoke<DashboardProfile>("write_dashboard_profile", {
    org,
    patch,
    expectedPrincipal: expectedPrincipal ?? (await rendererPrincipal()),
  });

/** One custom right pane, `<space>/panes/<id>.html`. */
export interface PaneEntry {
  id: string;
  scope: PaneScope;
  /** The team whose `panes/` holds it; absent for a private pane. */
  team?: string;
  /** The document's `<title>`, or the id. */
  title: string;
  /** The Lucide icon name its `<meta name="codesense:icon">` asks for, as written. */
  icon?: string;
  /** Surface declared by the manifest or HTML metadata, in any scope. */
  surface?: string;
  dashboardRef?: DashboardRef;
  declaration?: DashboardDeclaration;
  status?: "ready" | "rejected";
  rejectedReason?: string;
  updateAvailable?: boolean;
  installSourceVersion?: string;
  /** The pane file's absolute path. */
  path: string;
  /** The `panes/` dir its relative assets resolve from. */
  panesDir: string;
  /** The right-pane panel id: `pane:<org>:<team>/<id>` or `pane:private:<org>/<id>`. */
  panelId: string;
  /** RFC3339. */
  modified?: string;
}

/** One entry in a `panes/pane-manifest.yaml`: a declarative overlay on a
 *  discovered pane. Advisory metadata — pane discovery is still filename-based,
 *  so a manifest entry whose id has no HTML file is a build-time lint failure
 *  (see `scripts/build-codesense-docs.mjs`) and will never reach the viewer. */
export interface PaneManifestEntry {
  slots?: string[];
  surface?: string;
  capabilities?: string[];
  /** Stable key: matches the pane's filename stem (`tutor.html` → `tutor`). */
  id: string;
  /** Pane title; manifest wins over the HTML's `<title>` when both are set. */
  title?: string;
  /** HTML file name inside the `panes/` dir. */
  html: string;
  /** Lucide icon override; manifest metadata wins over the HTML. */
  icon?: string;
  /** Pane-context keys where this pane wants to appear. `"*"` = always. */
  showsOn?: string[];
  /** Stack ordering within a `showsOn` bucket; lower = nearer the top. */
  priority?: number;
  /** Start folded; the user expands when they want it. */
  collapsedByDefault?: boolean;
  /** Informational: `subscribeContext` payload keys the pane watches. */
  reactsTo?: string[];
}

/** The parsed `pane-manifest.yaml` for one layer in a listing's chain. */
export interface PaneManifest {
  version: number;
  panes: PaneManifestEntry[];
}

/** One layer's parsed `pane-manifest.yaml`, tagged with where it came from. */
export interface PaneManifestLayer {
  scope: PaneScope;
  team?: string;
  manifest: PaneManifest;
}

/** The panes a team sees, and the content root their reads resolve against. */
export interface PaneListing {
  /** Bundled Inbox for recovery, including when a team or private copy overrides it. */
  builtinInbox?: PaneEntry | null;
  contentRoot: string;
  /** The team's resolution chain, nearest first: the teams whose panes it sees. */
  chain: string[];
  panes: PaneEntry[];
  /** Optional overlays from each chain layer's `pane-manifest.yaml`, nearest first. */
  manifests?: PaneManifestLayer[];
  sources?: import("@/features/dashboard/lib/live-hub").HubSnapshot;
}

export interface PaneSourceRow {
  id: string;
  dashboardRef: DashboardRef;
  entry: PaneEntry | null;
  contentRoot: string;
  sourceRevision: number;
  /** Reader-independent document and asset revision for singleton pane hosts. */
  documentRevision?: number;
  listed: boolean;
}

/** The panes `org/team` sees: its chain's team panes (nearest first), then the
 *  private space's. The same id in two layers is two entries. */
export const listPanes = (
  org: string,
  team: string,
  source?: {
    sourceView: string;
    sourceSeq: number;
    selectedRefs: DashboardRef[];
    refreshRef?: DashboardRef;
  },
) => invoke<PaneListing>("list_panes", { org, team, ...source });

/** Bundled descriptors and a validated current-team root, independent of space discovery. */
export const listBuiltinPanes = (org: string, team: string) =>
  invoke<PaneListing>("list_builtin_panes", { org, team });

// === macOS menu bar (C.16) ===

/**
 * Rebuild the native macOS menu with the user's current shortcut
 * overrides. No-op on Windows/Linux. Fire-and-forget from the caller's
 * perspective — failures are logged on the Rust side.
 */
export interface PanelMenuEntry {
  id: string;
  label: string;
  checked: boolean;
}

/** One registry-driven nav command pushed into the native Go submenu. The
 *  `accelerator` is the resolved effective keystroke (Tauri accelerator grammar),
 *  so a rebind flows through on the next rebuild with no Rust change. */
export interface NavMenuEntry {
  id: string;
  label: string;
  accelerator: string;
}

export const rebuildMenu = (
  overrides: Record<string, string>,
  panels: PanelMenuEntry[] = [],
  nav: NavMenuEntry[] = [],
) => invoke<void>("rebuild_menu", { overrides, panels, nav });

// === MCP Viewer State ===

export const pushViewerState = async (snapshot: Record<string, unknown>) =>
  invoke<void>("push_viewer_state", { snapshot, expectedPrincipal: await rendererPrincipal() });

export type AgentIdentityMap = Record<string, Record<string, AgentIdentityChoice>>;
export const agentListRefresh = async () =>
  invoke<void>("agent_list_refresh", { expectedPrincipal: await rendererPrincipal() });
export const agentListIdentity = async (legacy?: AgentIdentityMap) =>
  invoke<AgentIdentityMap>("agent_list_identity", {
    legacy,
    expectedPrincipal: await rendererPrincipal(),
  });
export const agentListSetIdentity = async (
  teamKey: string,
  handle: string,
  choice: AgentIdentityChoice | null,
) =>
  invoke<AgentIdentityMap>("agent_list_set_identity", {
    teamKey,
    handle,
    choice,
    expectedPrincipal: await rendererPrincipal(),
  });

// === Scratch Notes ===

// SW-SN-1: plural notes model. Bodies are markdown files under
// `{scratch_dir}/notes/{id}.md`; metadata lives in an `index.json` sidecar.
// The legacy single `notes.md` is migrated into the first note on first list.

/** Who changed a note: the person ("You") or an agent session. */
export interface NoteActor {
  kind: "human" | "agent";
  name: string;
  sessionId?: string;
  handle?: string;
  /** The face the agent wore, as `pack:id`. */
  avatar?: string;
}

/** Per-note metadata (the `list_notes` return + `index.json` entries). */
export interface NoteMeta {
  id: string;
  title: string;
  pinned: boolean;
  lastOpenedAt: number;
  /** When the body last changed; absent on notes not changed since tracking began. */
  updatedAt?: number;
  /** Who last changed the body. */
  updatedBy?: NoteActor;
  /** Its first lines' words, as the notes list shows them under the title. */
  preview?: string;
  /** The agents its lines were handed to. */
  refs?: { handle: string; name: string }[];
  /** The commit of its newest revision, when its history has one. */
  headCommit?: string;
}

/** One revision of a note (`notes_history`), newest first. */
export interface NoteRevision {
  commit: string;
  /** ms since the epoch. */
  at: number;
  actor: NoteActor;
  change: "create" | "write" | "restore" | "delete" | "meta";
  summary: string;
  /** Body lines added and removed by this revision. */
  added: number;
  removed: number;
  restoredFrom?: string;
}

/** A note's text at one revision (`notes_read_at`). */
export interface NoteAtCommit {
  id: string;
  commit: string;
  title?: string;
  content: string;
}

/** A note's metadata + its markdown body (`read_note` / `create_note`). */
export interface Note extends NoteMeta {
  content: string;
}

/** A `list_notes` row: the note's metadata plus its first non-empty line. */
export type NoteRow = NoteMeta & { preview: string };

/** One `search_notes` match. Title matches sort before text-only matches; `snippet`
 *  is the matching body line, or the title when only the title matched. */
export interface NoteHit {
  id: string;
  title: string;
  matched: "title" | "text" | "both";
  snippet: string | null;
}

// SLW-PT-19: every notes command takes a `NotesScope` — the profile and the org
// whose scratch-space notes it reads or writes. The org is captured by the caller
// (never resolved by Rust at call time); `orgSlug: null` / a logged-out `0` uses
// the machine-local store.
const scopeArgs = (scope: NotesScope) => ({
  githubUserId: scope.githubUserId,
  orgSlug: scope.orgSlug,
});

export const listNotes = (scope: NotesScope) => invoke<NoteRow[]>("list_notes", scopeArgs(scope));

export const readNote = (scope: NotesScope, id: string) =>
  invoke<Note>("read_note", { ...scopeArgs(scope), id });

/** Case-insensitive substring search over note titles and bodies for the ⌘O
 *  switcher; empty/whitespace query → `[]`. */
export const searchNotes = (scope: NotesScope, query: string) =>
  invoke<NoteHit[]>("search_notes", { ...scopeArgs(scope), query });

export interface NoteWritten {
  org: string;
  note: Note;
  changed: boolean;
  commit: string | null;
  merged: boolean;
  overlaps: { ours: string; theirs: string; at: number }[];
}

export const createNote = async (
  scope: NotesScope,
  title: string,
  opts: { content?: string; pinned?: boolean } = {},
) => (await invoke<NoteWritten>("create_note", { ...scopeArgs(scope), title, ...opts })).note;

/** Save a note's body. `base` is the body the edit started from: when the note
 *  changed on disk since (an agent wrote it), the backend merges the two instead of
 *  replacing the other change. Resolves to the body now on disk. */
export const writeNote = async (
  scope: NotesScope,
  id: string,
  content: string,
  base?: string,
  requestId?: string,
) =>
  (
    await invoke<NoteWritten>("write_note", {
      ...scopeArgs(scope),
      id,
      content,
      ...(base === undefined ? {} : { base }),
      ...(requestId === undefined ? {} : { requestId }),
    })
  ).note.content;

export const updateNoteMeta = async (
  scope: NotesScope,
  id: string,
  patch: { title?: string; pinned?: boolean; lastOpenedAt?: number },
) => (await invoke<NoteWritten>("update_note_meta", { ...scopeArgs(scope), id, ...patch })).note;

export const deleteNote = async (scope: NotesScope, id: string) => {
  await invoke<NoteWritten>("delete_note", { ...scopeArgs(scope), id });
  return listNotes(scope);
};

/** A note's revisions, newest first. The person's typing becomes a revision after ~30s
 *  of quiet; a `notes-changed` event with its `commit` follows. */
export const notesHistory = (scope: NotesScope, id: string, limit?: number) =>
  invoke<NoteRevision[]>("notes_history", { ...scopeArgs(scope), id, limit });

/** A note's text as it was at `commit` (from `notesHistory`). */
export const notesReadAt = (scope: NotesScope, id: string, commit: string) =>
  invoke<NoteAtCommit>("notes_read_at", { ...scopeArgs(scope), id, commit });

/** Bring a note's body back to `commit` as a new change (the current text is kept in
 *  history). With `undo`, take back only what revision `commit` changed, keeping every
 *  later edit; refused when later edits rewrote the same lines. */
export const notesRestore = async (
  scope: NotesScope,
  id: string,
  commit: string,
  opts?: { undo?: boolean },
) =>
  (
    await invoke<NoteWritten>("notes_restore", {
      ...scopeArgs(scope),
      id,
      commit,
      ...(opts?.undo ? { undo: true } : {}),
    })
  ).note;

/** SW-SN-13 AC-4: one-shot, guarded reap of the empty duplicate "Scratch" notes
 *  the SN-12 null-active-note lazy-create minted. `bootIds` is the snapshot of
 *  note ids present at session start — only those are reap-eligible, so a note
 *  created concurrently (e.g. via the panel's 3s editable hatch) is never reaped.
 *  Preserves `activeId`; never deletes a note with body content, a user-renamed
 *  title, a pinned note, or the sole survivor. Idempotent. Returns the survivors. */
export const reapEmptyScratchNotes = (
  scope: NotesScope,
  activeId: string | undefined,
  bootIds: string[],
) => invoke<NoteMeta[]>("reap_empty_scratch_notes", { ...scopeArgs(scope), activeId, bootIds });

/** The absolute notes directory (each note is `{dir}/notes/{id}.md`); the path is
 *  derived in Rust, so it must come from a command. A note handed to an agent
 *  names its file from here. */
export const notesDirPath = (scope: NotesScope) =>
  invoke<string>("notes_dir_path", scopeArgs(scope));

// === Utility ===

export const ensureDirectory = (path: string) => invoke<void>("ensure_directory", { path });

// === PTY operations ===

/**
 * Optional PT-1 session metadata for a terminal's spawn (see
 * {@link TerminalTabSpawn}). The correlation keys are opaque (Rust stores them
 * without validation).
 */
export interface SpawnPtyOptions {
  /** Story-home binding (PT-4 aggregation). */
  storyId?: string;
  /** Soft-close shadow binding (PT-10). */
  workspaceId?: string;
  /** Epic-workspace binding (PT-11). */
  epicId?: string;
  /** PT-5: shell integration (OSC 7/133 + inline prompt); defaults on Rust-side. */
  shellIntegration?: boolean;
  /** Lines of history the session's terminal keeps — its display and the
   *  backend emulator agree on it. The host ignores it until it has an emulator. */
  scrollback?: number;
  /** SW-GS-2a (AC-7): env var names to strip from the inherited child env (e.g.
   *  `["ANTHROPIC_API_KEY"]` for a subscription-billed `claude` session).
   *  Default omitted → unaffected. */
  envRemove?: string[];
  /** Account-selection intent; trusted Rust resolves it again at spawn. */
  account?: ResolveAgentAccountInput;
  /** Opaque one-use backend binding for MCP scope environment. */
  environmentBinding?: string;
  /** SW-WA-F10: argv to spawn as PID-1 of the PTY — bypasses the shell wrapper.
   *  When set, `command` / `interactive` no longer pick PID-1. Set by the
   *  trusted-side managed launch so a saved-agent session runs the agent CLI
   *  directly, no interactive shell in front of it. */
  commandArgv?: string[];
  /** SW-WA-F10: the native-launch provider the spawn runs — `"claude"` /
   *  `"codex"` for the managed-launch argv-direct path, so the host reserves
   *  an activity invocation and binds `CODESENSE_ACTIVITY_SOCKET` /
   *  `_TOKEN` onto the child before exec. Omitted = no native binding. */
  nativeLaunch?: "shell" | "claude" | "codex" | "opencode" | "hermes";
}

/**
 * PT-8: set the warm-pool target size + spawn params. Called by the frontend at
 * launch once settings hydrate, and on any change to pool size / starship preset
 * / shell integration. Size is clamped to 0–5 Rust-side (0 disables).
 */
export const reconfigureWarmPool = (
  size: number,
  starshipPreset?: string,
  shellIntegration?: boolean,
) =>
  invoke<void>("reconfigure_warm_pool", {
    size,
    starshipPreset,
    shellIntegration,
  });

/** SW-LN-35: the shell new terminals spawn, and where it came from. */
export interface TerminalShellInfo {
  path: string;
  kind: "zsh" | "bash" | "fish";
  source: "override" | "env" | "passwd" | "fallback";
  /** The shell detected without the override (`$SHELL` → passwd → `/bin/zsh`). */
  detectedPath: string;
  overridePath: string | null;
  /** The override is set but isn't an executable zsh/bash/fish, so it is ignored. */
  overrideRejected: boolean;
  /** A login shell CodeSense can't integrate with, which is why zsh is used. */
  unsupportedShell: string | null;
}

export const terminalShellInfo = () => invoke<TerminalShellInfo>("terminal_shell_info");

/** SW-LN-35: set (or clear with empty/undefined) the Settings shell override. */
export const setTerminalShellOverride = (shell?: string) =>
  invoke<TerminalShellInfo>("set_terminal_shell_override", { shell: shell?.trim() || null });

export const writePty = (sessionId: RuntimeSessionReference, data: string) =>
  invoke<void>("write_pty", { sessionId, data });

/**
 * SW-GS-23: reliably submit a single line to an agent CLI's PTY. Unlike
 * `writePty`, the backend watches the session's own output bytes to (1) settle —
 * wait until the agent is idle at its prompt before writing, so the write isn't
 * batched as a paste and its Enter dropped — and (2) confirm the submit from
 * output resumption, nudging a discrete carriage return if the line is held.
 * Agent-agnostic (Claude/Codex/Cursor), no hooks. `line` is the raw message; the
 * backend owns the submit key. Rejects on a missing session.
 *
 * SW-GS-30: resolves `{ submitted }` — `true` when the submit was CONFIRMED from
 * output resumption, `false` when the line stayed held after every nudge / the
 * overall timeout. Only the backend sees the output bytes, so only it can tell
 * "left the box" from "still held"; callers that must know (the guarded auto-inject
 * path) map `submitted:false` to a distinct `send-not-submitted` reason. Callers
 * that don't care (the user chat/conflict send) ignore the result.
 */
export const submitPtyLine = (
  sessionId: RuntimeSessionReference,
  line: string,
  opts: {
    quietMs?: number;
    confirmMs?: number;
    timeoutMs?: number;
    maxNudges?: number;
  } = {},
) =>
  invoke<{ submitted: boolean }>("submit_pty_line", {
    sessionId,
    line,
    quietMs: opts.quietMs ?? SUBMIT_QUIET_MS,
    confirmMs: opts.confirmMs ?? SUBMIT_CONFIRM_MS,
    timeoutMs: opts.timeoutMs ?? SUBMIT_TIMEOUT_MS,
    maxNudges: opts.maxNudges ?? SUBMIT_MAX_NUDGES,
  });

/** What became of a person's prompt (`send_prompt_to_agent`). `unknown` may have
 *  arrived — never retry it automatically. An ended remote tab or unavailable host
 *  is `refused` with a reason code. `typed` means written without a confirmed submit,
 *  including an explicit `submit:false` locally or remotely. A page maps a rejected
 *  command to `invalid`. */
export interface AgentPromptResult {
  status: "submitted" | "typed" | "notSent" | "unknown" | "refused" | "invalid";
  submitted: boolean | null;
  /** The machine's label when the agent runs on one; null on this Mac. */
  machine: string | null;
  /** The remote input path's reason code (`staleOrigin`, `notReady`, `deliveryUnknown`,
   *  `ended`, …); null on this Mac. */
  code: string | null;
  /** Why it was not sent, naming the machine. */
  message: string | null;
}

/** Type `text` into the agent listed as `handle`, locally or remotely, and submit by
 *  default. `submit:false` pastes without Enter. `sessionId` is the session the roster
 *  showed for it; pages use null to resolve the session from its handle. */
export const sendPromptToAgent = (
  handle: string,
  sessionId: string | null,
  text: string,
  submit?: boolean,
) => invoke<AgentPromptResult>("send_prompt_to_agent", { handle, sessionId, text, submit });

/** SW-GS-23 submit-timing defaults (ms / count). Tuned so an idle agent submits
 *  promptly while a still-rendering one is left to settle first. */
const SUBMIT_QUIET_MS = 200;
const SUBMIT_CONFIRM_MS = 400;
const SUBMIT_TIMEOUT_MS = 4000;
const SUBMIT_MAX_NUDGES = 3;

// ── SW-DX-7: dashboard execution-loop broker ────────────────────────────────
// Trusted-side wiring for the `openWorkspace`/`runPrompt` bridge verbs. The
// storyId↔sessionId binding + SpawnCap live in the Rust process so a forgeable
// dashboard can neither exceed the concurrency cap nor redirect a fire to
// another session (SW-DX-7 AC-8/AC-10/AC-12).

// SW-DX-11 §2: the registry is keyed by an opaque HANDLE (a storyId, an epic's
// `epicDocPath`, a `team::epic`, or a `repo::branch`). These wrappers pass a
// `handle`; the storyId case is just one handle shape.

/** AC-8: reserve a concurrency-cap slot for a dashboard launch. Rejects `busy`
 *  when the cap is saturated (non-blocking — never stalls the bridge). Resolves
 *  `true` when this call freshly reserved the slot, `false` when the handle was
 *  already launching/launched (idempotent focus). */
export const dashboardLaunchBegin = (handle: string) =>
  invoke<boolean>("dashboard_launch_begin", { handle });

/** AC-12: stamp the spawned session id onto the handle, trusted-side. SW-GW-8:
 *  `launcher` records the launching surface (the dashboard `docPath`) so the
 *  Background-agents panel row shows provenance. */
export interface AgentAccountProvenance {
  runtime: AgentProvider;
  provider: string;
  source: string;
  reason: string;
  accountId: string | null;
  label: string | null;
}

/** Where a frontend launch ran and which agent revision it runs, stamped on the session
 *  at bind so a finished agent reaches the routines it is in reach of. */
export interface LaunchBindScope {
  teamKey?: string;
  /** The named agent the session runs (`<key>@<revision>`, from `agentLaunchPrepare`).
   *  Recorded only when the backend finds that revision for the signed-in account. */
  agentRevision?: string;
}

export const dashboardLaunchBind = (
  handle: string,
  sessionId: string,
  launcher?: string,
  scope?: LaunchBindScope,
) =>
  invoke<void>("dashboard_launch_bind", {
    handle,
    sessionId,
    launcher: launcher ?? null,
    teamKey: scope?.teamKey ?? null,
    agentRevision: scope?.agentRevision ?? null,
  });

/** AC-8: free a reserved cap slot on the FE failure path (open failed between
 *  `begin` and a live session), so a reserved-but-unbound permit can't leak. */
export const dashboardLaunchAbort = (handle: string) =>
  invoke<void>("dashboard_launch_abort", { handle });

/** SW-DX-11 / SW-GW-8: register a session the APP opened (not a dashboard) under
 *  `handle`, CAP-FREE, so it is `sendToSession`-addressable AND shows in the
 *  Background agents panel — the cap is for dashboard-launched sessions only.
 *  Covers app-opened epic/context terminals (SW-DX-11) and nav-opened story /
 *  generic-dir sessions (SW-GW-8). `launcher` is the provenance surface
 *  (`"codesense"` for a nav/manual open); a `None` re-register never clobbers a
 *  prior launcher (first-writer-wins, trusted-side). `teamKey` is the team the
 *  session was opened in, where its inbox lives (first-writer-wins too). */
export const dashboardRegisterSession = (
  handle: string,
  sessionId: string,
  launcher?: string,
  teamKey?: string,
) =>
  invoke<void>("dashboard_register_session", {
    handle,
    sessionId,
    launcher: launcher ?? null,
    teamKey: teamKey ?? null,
  });

/** AC-10/AC-12: resolve the authoritative fire target for a handle (the bound
 *  session id). Rejects a handle with no live binding rather than trusting a
 *  caller-supplied id. ONE lookup for every handle kind (§2). */
export const dashboardResolveSession = (handle: string) =>
  invoke<string>("dashboard_resolve_session", { handle });

/** SW-DX-17a: one live agent session in the "Running agents" panel (mirror of
 *  Rust `SessionRow`). `sessionId` is null in the brief `begin`→`bind` window;
 *  `kind` is `"dashboard-launched"` (cap-holding), `"registered"` (cap-free), or
 *  `"headless"` (the PTY-less `run_agent` pseudo-session — SW-OR-10 `KIND_HEADLESS`;
 *  the FE branches on it via `isHeadlessSession` to withhold TTY-only affordances);
 *  `alive` is REAL child-process liveness (a naturally-exited session reads
 *  false even though it is retained for replay — for a headless entry it is
 *  registry presence, not a PTY probe). */
export interface AgentSessionRow {
  id?: string;
  detected?: {
    cli: string | null;
    dot: import("@/shared/lib/terminal-ai-status").TerminalAiStatus | null;
    processOnly?: boolean;
  };
  rowNames?: { primary: string; secondary: string | null; full: string; source?: "progress" };
  queueRole?: string | null;
  roleJoinError?: string | null;
  chatWorkspaceId?: string | null;
  /** Inbox-owner fields joined by projection consumers. */
  unread?: number | null;
  groupUnread?: number;
  groupReadId?: number;
  /** Captured native artifact, including after its tab closes. */
  outputRunId?: string | null;
  handle: string;
  sessionId: string | null;
  kind: string;
  alive: boolean;
  /** Coarse process lifecycle; effectiveStatus is the displayed row status. */
  status?: RustAgentStatus;
  effectiveStatus?: import("@/shared/lib/agent-status-config").AgentStatus;
  hasAgent?: boolean;
  managedState?: import("@/shared/lib/managed-session").ManagedSessionState;
  needsYouKind?: import("@/shared/lib/terminal-ai-status").NeedsYouKind | null;
  crashId?: string;
  /** Host-owned reason for ending a session, retained with its crash mark. */
  endReason?: string;
  activityAtMs?: number;
  statusSinceMs?: number;
  subagents?: number;
  /** See {@link SessionListRow.releaseQualified}. */
  releaseQualified?: false;
  releaseNote?: string;
  /** See {@link SessionListRow.inboxWaiter}. */
  inboxWaiter?: boolean | "unknown";

  /** SW-DX-17d (AC-3): true while backgrounded (unattended) — the panel tallies
   *  these for its "N of M running unattended" header. Adopted/attended → false,
   *  and does NOT count toward the user's background-agent limit. */
  backgrounded: boolean;
  /** SW-DX-17d (AC-2): the "Started with" task/prompt, or null → the row degrades
   *  to `{kind}`. User-facing label is "Started with" — never "prompt". */
  startedWith: string | null;
  /** SW-DX-17d (AC-2): epoch-ms birth anchor; the panel ticks elapsed = now − this. */
  startedAtMs: number | null;
  /** SW-GW-8 (AC-5): the launching SURFACE — a dashboard `docPath`, `"codesense"`
   *  for a nav/manual open, or null for an origin-less background. The panel shows
   *  it so "which surface opened this" is legible, not a soup. */
  launcher: string | null;
  /** Context captured from the launcher, used only when explicitly requested. */
  launcherContext?: string | null;
  accountProvenance?: AgentAccountProvenance | null;
  /** SW-SR-4 (AC-2): the auto-run provenance stamp, present ONLY on a worker the
   *  sprint reactor dispatched (SW-SR-3 `stamp_provenance`). The run monitor keys
   *  off its presence to pick out reactor-dispatched workers and groups them by
   *  `runId`. Absent (`undefined`) for a hand-launched session, and briefly absent
   *  in the post-launch begin→bind→stamp window. The Rust `SessionRow` already
   *  serializes this field (camelCase) — declared here only so the FE can read it. */
  provenance?: RunProvenance;
  /** The named agent the session runs, as `<key>@<revision>`; null for a launch that named none. */
  agentRevision?: string | null;
  /** SW-OR-19: a live headless run's last-output epoch-ms — the row ticks "last output
   *  Ns ago" = now − this against the shared clock, so a working agent reads fresh and a
   *  wedged one visibly climbs. Null for non-headless / no-output-yet rows. The snapshot
   *  seeds it; the `mcp:agent-heartbeat` event freshens the one row between snapshots. */
  lastOutputMs: number | null;
  /** The name an agent gave this session when it launched it through `open_workspace`
   *  ("Sprint 1", or the story id). Absent/null for any other session. */
  displayName?: string | null;
  /** Who launched it through `open_workspace`: the launcher's handle (or raw session id
   *  when it has none); null when a person did. Display only. */
  launchedBy?: string | null;
  /** The story an agent launched through `open_workspace` works on; null otherwise. */
  story?: string | null;
  /** The managed dir the session runs in, when the launch recorded one. */
  workspace?: string | null;
  /** The role the launch named for this session ("manager", "worker", or a free label such as
   *  "reviewer"). Display only. */
  role?: string | null;
  /** The avatar the launch named for this session (a built-in portrait id, a colour, or a short
   *  glyph). Display only. */
  avatar?: string | null;
  /** epoch-ms deadline the deadline sweep enforces on a bounded launch;
   *  absent/null for an unbounded or remote session. Once the deadline fires and
   *  the kill releases the row, the row disappears from `list_sessions`. */
  deadlineAtMs?: number | null;
  /** true only during the ~30s grace between the sweep's `/exit` and
   *  its kill. Serialized ONLY when set, so an unbounded row omits the key. */
  expiring?: boolean;
  /** Where the session runs, from its launch record: a remote machine's id, or null on this
   *  Mac. Known before the machine's layout shows its tab. Every row `dashboard_list_sessions`
   *  lists carries it; a durable roster row with no live counterpart leaves it out, which
   *  means unknown (after a restart, a remote launch's record comes back only once its
   *  machine's layout is read). */
  machineId?: string | null;
  /** The team a remote launch was made in, whose machine list names its machine; null on this
   *  Mac, and left out where `machineId` is. */
  launchTeamKey?: string | null;
}

/** SW-DX-17a (AC-1/AC-3): enumerate every live agent session CodeSense is
 *  running — including ones with no terminal tab (the headless case). Runs off
 *  the UI thread trusted-side. */
export const dashboardListSessions = () => invoke<AgentSessionRow[]>("dashboard_list_sessions");

/** Per-message click action chosen by the sender. The Inbox UI branches on
 *  `type`: `goto-agent` jumps to the agent's running terminal; `open-file` opens
 *  a path in the viewer; `show-message` (or omitted) opens the conversation view
 *  (the ordinary default). */
export type MessageOnClick =
  | { type: "goto-agent"; agentId: string }
  | { type: "open-file"; path: string }
  | { type: "show-message" };

/** How to render a message body. `markdown` is the inbox renderer default
 *  (GFM via the shared viewer plugin set). `plain` preserves whitespace with
 *  no formatting. `html` to the human is a live dashboard (the same frame and
 *  bridge an HTML dashboard file gets); between agents it is sanitized. */
export type MessageContentType = "plain" | "markdown" | "html";

/** One message in the agents' inbox, as the Agents panel's Messages list shows it.
 *  `from`/`to` are session handles or `"human"`; the names are the launch display
 *  names (else the handle). */
export interface InboxMediaAttachment {
  kind: "image";
  sha256?: string;
  mime?: string;
  width?: number;
  height?: number;
  bytes?: number;
  alt: string;
  sourceUrl?: string;
  sourceHost?: string;
  state: "cached" | "pending" | "blocked" | "failed";
  reason?: string;
  retryable?: boolean;
}
export const loadInboxMedia = (team: InboxTeam, messageId: number) =>
  invoke<void>("load_inbox_media", { team, messageId });

export interface InboxMessageRow {
  conversation?: string;
  toRole?: string | null;
  messageId: number;
  from: string;
  fromName: string;
  to: string;
  toName: string;
  text: string;
  createdAtMs: number;
  inReplyTo: number | null;
  /** Original inbox identity; null for non-replies or unknown legacy provenance. */
  inReplyToTeam: InboxTeam | null;
  /** Addressed to the human, who can reply to it. */
  toHuman: boolean;
  unread: boolean;
  /** SW-BSI-2: optional managed-file reference the human previews (Markdown/HTML).
   *  Omitted/null for standalone messages — the UI opens NO document tab for those. */
  primaryFile?: string | null;
  attachments?: InboxMediaAttachment[];
  /** SW-BSI-UX: when set, the human archived this message. The Inbox UI drops
   *  archived threads out of the default "needs you" lane into "done". */
  archivedAtMs?: number | null;
  /** Where a message sent to an agent's role inbox stands, or its withdrawal. Null for a
   *  plain message nobody withdrew. */
  delivery?: InboxDelivery | null;
  /** Per-message click action chosen by the sender (goto-agent / open-file /
   *  show-message). Omitted/null → the row opens the conversation view. */
  onClick?: MessageOnClick | null;
  /** How to render the body. Omitted/null → `markdown` (the renderer default,
   *  see `DESIGN-inbox-ux.md` §3). */
  contentType?: MessageContentType | null;
  /** Where replies to this message go instead of its sender, like an email Reply-To:
   *  a session handle, `human`, or `role:<id>`. Omitted → replies go to `from`. */
  replyTo?: string | null;
  /** How `replyTo` shows: a session's current tab name, or the role id. */
  replyToName?: string | null;
  /** A reply would go to a session that is no longer running (the `replyTo` session,
   *  else the sender): it waits unread in that inbox. */
  replyTargetEnded?: boolean;
  /** The sender's one-line plain-text preview (≤280 chars), shown in every list in
   *  place of a preview derived from the body. Null/omitted when they set none. */
  summary?: string | null;
  /** SW-AGR-18: what it asks of the human (see {@link MessageKind}). */
  kind: MessageKind;
  /** The emoji reactions on it, grouped by emoji in first-seen order. A reaction is not
   *  mail: it leaves `unread` and every count alone. */
  reactions?: InboxReaction[];
}

/** One emoji on a message and the distinct reactors holding it, first-seen order:
 *  `human` or a session handle. `count` is `reactors.length`. */
export interface InboxReaction {
  emoji: string;
  count: number;
  reactors: string[];
}

export type ReactResult =
  | {
      status: "reacted";
      messageId: number;
      emoji: string;
      outcome: "added" | "removed" | "unchanged";
    }
  | { status: "invalid"; message: string };

/** The human reacts to a message: `remove` omitted toggles, `true` removes, `false` adds.
 *  One reaction per (message, reactor, emoji), enforced by the engine. */
export const reactAsHuman = (
  team: InboxTeam,
  messageId: number,
  emoji: string,
  remove: boolean | null = null,
) => invoke<ReactResult>("react_as_human", { team, messageId, emoji, remove });

/** SW-AGR-18: what a message asks of the human, which picks its lane in the human inbox:
 *  `needs-you` waits on them (Needs you), `update` is informational (Updates). The
 *  sender's kind, else `needs-you` when it carries a `replyTo` naming someone other than
 *  the human, else `update`. Mirror of Rust `MessageKind`. */
export type MessageKind = "needs-you" | "update";

/** Where a message sent to an agent's role inbox stands. */
export type InboxDeliveryState = "queued" | "reading" | "accepted" | "done" | "withdrawn";

export interface InboxDelivery {
  /** queued: no running instance has it; reading: an instance claimed (picked up) it;
   *  accepted: an instance accepted it, as work or as no work (also while a reclaim adopts
   *  it); done: acknowledged; withdrawn: the sender cancelled it before anyone picked it up. */
  state: InboxDeliveryState;
  /** When an instance first picked it up (epoch ms); null until then. Kept if a claim later
   *  expires back to queued. */
  pickedUpAtMs: number | null;
  /** The instance id that holds or accepted it (whatever it registered with: a session id or
   *  a handle); null while queued. */
  instance: string | null;
  /** When it was accepted (epoch ms, from the acceptance receipt). */
  acceptedAtMs: number | null;
  /** The accepting instance's disposition and note. */
  disposition: "work" | "no_work" | null;
  note: string | null;
  /** When the sender withdrew it (epoch ms). */
  withdrawnAtMs: number | null;
}

/** SW-BSI-UX: which recipient class `listInboxMessages` should return. */
export type InboxScope = "to-human" | "to-agents" | "all";

/** The outcome of a reply: stored, or an input the inbox cannot hold. */
export type InboxSendResult =
  | { status: "sent"; messageId: number }
  | { status: "invalid"; message: string };

/** The Messages list: the newest messages of the active team's inbox, newest first, and
 *  how many messages to the human are unread in the whole inbox. */
export interface InboxConversationSummary {
  with: string;
  team: InboxTeam;
  unread: number;
  messageCount: number;
  lastAt: number;
  lastMessageId: number;
  lastPreview: string;
}

export interface InboxView {
  conversations: InboxConversationSummary[];
  hasMore: boolean;
  messages: InboxMessageRow[];
  humanUnread: number;
}

/** The team whose inbox an app command reads or answers. */
export interface InboxTeam {
  org: string;
  team: string;
}

/** A stored message's actual Reply-To or sender, read by ID without marking it read. */
export const inboxReplyRecipient = (team: InboxTeam, messageId: number) =>
  invoke<string | null>("inbox_reply_recipient", { team, messageId });

/** `team`'s inbox for the Messages list. Marks nothing read. `scope` defaults to
 *  `to-human` on the server side when omitted. `recipientAgentId` (an agent definition id)
 *  narrows it to that agent's conversation: its role inbox and the replies under it. */
export const listInboxMessages = (
  team: InboxTeam,
  scope?: InboxScope,
  recipientAgentId?: string,
  /** With `recipientAgentId`: the handles of its running instances, whose own inboxes are
   *  part of its conversation. */
  sessions?: readonly string[],
) =>
  invoke<InboxView>(
    "list_inbox_messages",
    recipientAgentId === undefined
      ? { team, scope }
      : { team, scope, recipientAgentId, ...(sessions?.length ? { sessions } : {}) },
  );

/** The human read message `messageId` of `team`'s inbox; only a message to the human
 *  changes. */
export const markInboxMessageRead = (team: InboxTeam, messageId: number) =>
  invoke<void>("mark_inbox_message_read", { team, messageId });

/** SW-AGR-18: the human read every message of `messageIds` in `team`'s inbox at once (one
 *  transaction, one inbox-changed event). Resolves how many changed. */
export const markInboxMessagesRead = (team: InboxTeam, messageIds: readonly number[]) =>
  invoke<number>("mark_inbox_messages_read", { team, messageIds });

/** Restore unread state for human-directed messages without changing archive state. */
export const markInboxMessagesUnread = (team: InboxTeam, messageIds: readonly number[]) =>
  invoke<number>("mark_inbox_messages_unread", { team, messageIds });

/** Archive and read human-directed messages in one transaction. Resolves how many changed. */
export const markInboxMessagesArchived = (team: InboxTeam, messageIds: readonly number[]) =>
  invoke<number>("mark_inbox_messages_archived", { team, messageIds });

/** Undo an archive in one transaction: unarchive human-directed messages and make the ones in
 *  `restoreUnread` unread again. Resolves how many were unarchived. */
export const unarchiveInboxMessages = (
  team: InboxTeam,
  messageIds: readonly number[],
  restoreUnread: readonly number[],
) => invoke<number>("unarchive_inbox_messages", { team, messageIds, restoreUnread });

/** Outcome of `withdrawInboxMessage`. */
export type WithdrawInboxMessageResult =
  | { status: "withdrawn"; withdrawnAtMs: number }
  | { status: "tooLate"; state: "reading" | "accepted" | "done" }
  | { status: "notFound" };

/** Cancel message `messageId` before any agent picks it up. Atomic against a concurrent
 *  claim. */
export const withdrawInboxMessage = (team: InboxTeam, messageId: number) =>
  invoke<WithdrawInboxMessageResult>("withdraw_inbox_message", { team, messageId });

/** The human answers message `messageId` of `team`'s inbox; the reply goes to its
 *  sender, who is nudged. */
export const replyAsHuman = (
  team: InboxTeam,
  messageId: number,
  text: string,
  contentType: MessageContentType | null = null,
  summary: string | null = null,
) => invoke<InboxSendResult>("reply_as_human", { team, messageId, text, contentType, summary });

/** SW-BSI-UX: an agent → UI progress notification persisted for the Inbox's
 *  Updates lane. Non-actionable: no reply, no attachment, no archive-ness. */
export interface AgentUpdate {
  id: number;
  sessionId: string;
  agentName: string;
  role: string;
  text: string;
  createdAtMs: number;
}

export const listAgentUpdates = (team: InboxTeam, limit?: number) =>
  invoke<AgentUpdate[]>("list_agent_updates", { team, limit });

/** SW-BSI-UX: the closed set of human-status values and the view shape returned
 *  by `getHumanStatus` / `setHumanStatus`. */
export type HumanStatusValue = "online" | "away" | "busy" | "offline";
export interface HumanStatusView {
  status: HumanStatusValue;
  note?: string;
  updatedAtMs: number;
}

export const getHumanStatus = (team: InboxTeam) =>
  invoke<HumanStatusView>("get_human_status", { team });

// ── The agents read model (the page bridge's `sessions`/`agent`/`inbox`/`roles`; the
// MCP tools `list_sessions`/`agent_get`/`inbox_list`/`role_state` return the same shapes).

/** A session as `list_sessions` lists it. */
export interface SessionListRow {
  /** Current CodeSense/native session identity; handles can be reused by another run. */
  sessionId?: string | null;
  /** Projection row identity; equal to the reusable handle. */
  id?: string;
  team?: InboxTeam | null;
  queueRole?: string | null;
  roleJoinError?: string | null;
  chatWorkspaceId?: string | null;
  groupUnread?: number;
  groupReadId?: number;
  handle: string;
  displayName: string;
  /** running | idle (turn finished) | needs-you (awaiting permission) | exited |
   *  offline (its machine is disconnected) | unknown (no observation). */
  state: "running" | "idle" | "needs-you" | "exited" | "offline" | "unknown";
  launchedBy: string | null;
  /** Unread messages to its own handle; null when its inbox could not be read. */
  unread: number | null;
  /** RFC 3339 time of its oldest unread message, or null. */
  oldestUnreadAt: string | null;
  /** Whether its agent was told about that unread mail by its inbox notice: told
   *  (all of it; told is not read), pending (newer mail not announced yet), or null
   *  with none unread. */
  mailNotice?: "told" | "pending" | null;
  /** RFC 3339 time its agent was last told about its mail, or null. */
  notifiedAt?: string | null;
  /** Whether new mail wakes its agent at once (an inbox waiter is armed). */
  inboxWaiter?: boolean | "unknown";
  /** How many of its agent's subagents run now; absent at none. `state` stays the
   *  agent's own. */
  subagents?: number;
  story?: string;
  workspace?: string;
  role?: string;
  avatar?: string;
  machine?: string;
  /** The saved Agent it runs (an agent id). */
  agent?: string;
  /** Its latest `signal_progress`, once it reported one. */
  progress?: SessionProgress;
  /** `false` only when its launch recorded an agent CLI release outside the range CodeSense
   *  is qualified on; absent when qualified or unknown. */
  releaseQualified?: false;
  /** The launch warning that says which release and which range, with `releaseQualified`. */
  releaseNote?: string;
}

/** A session's latest `signal_progress` (mirror of Rust `SessionProgress`). */
export interface SessionProgress {
  phase: "planning" | "dev" | "review" | "pr" | "blocked" | "done";
  /** `step` of `total`: an N-of-M sub-progress. */
  step?: number;
  total?: number;
  note?: string;
  /** When it reported (epoch ms). */
  atMs: number;
}

export const agentReadSessions = () => invoke<SessionListRow[]>("agent_read_sessions");

/** A saved Agent as `agent_get` returns it, with the sessions running it. */
export interface AgentWithInstances {
  id: string;
  scope: string;
  created_by: string;
  revision: number;
  archived: boolean;
  definition: Record<string, unknown>;
  revisions: { n: number; author: unknown; at: string }[];
  resolved: Record<string, unknown>;
  instances: SessionListRow[];
  /** SW-AGR-1/9: its routines, each with this Mac's switch, next fire and the wake
   *  queued on a busy session (golden: `fixtures/wake/agent-routines-queued.json`). */
  routines?: AgentRoutineItem[];
  /** SW-XS-14: the one-time suggestion converted from the team's legacy
   *  `projections.yml` ("Add these to ‹Agent›'s context?"). Absent/null once the
   *  agent's `context` holds every entry or the suggestion was dismissed. Never
   *  applied silently: accepting is an `agent_update` of `context`. */
  context_suggestion?: AgentContextSuggestion | null;
}

/** A routine's wake queued on a busy session, waiting for it to be idle (mirror of
 *  Rust `delivery::read_model::QueuedWake`; `agent_get.routines[].queued`). */
export interface QueuedWake {
  runId: string;
  machineId: string;
  sessionId: string;
  marker: string;
  /** Host clock of the queued deadline (epoch ms); null until the host answered. */
  deadlineMs: number | null;
  /** Sent but never answered: it may be queued, typed, or nowhere ("may be due"). */
  unconfirmed: boolean;
}

/** One routine of an agent, as `agent_get` lists it. */
export interface AgentRoutineItem {
  id: string;
  name: string;
  mode: RoutineMode;
  /** RFC 3339, or null when it has no scheduled fire from here. */
  nextRunAt: string | null;
  enabled: boolean;
  queued: QueuedWake | null;
}

/** SW-XS-14: a `projections.yml`-derived context suggestion for one agent. */
export interface AgentContextSuggestion {
  source: "projections.yml";
  entries: { path: string; dest?: string }[];
  /** Identifies this exact suggestion; dismissing records it. */
  fingerprint: string;
}

export const agentReadAgent = (key: string, frameId?: string, page?: string) =>
  invoke<AgentWithInstances>("agent_read_agent", { key, frameId, page });

/** SW-XS-14: dismiss an agent's `projections.yml` context suggestion (by its
 *  fingerprint) without changing the agent. */
export const agentDismissContextSuggestion = (key: string, fingerprint: string) =>
  invoke<void>("agent_dismiss_context_suggestion", { key, fingerprint });

/** What `inbox()` / `inbox_list` filter by. Default scope: `to-human` with no filter,
 *  `all` with one. */
export interface InboxQuery {
  conversation?: string;
  before?: number;
  scope?: InboxScope;
  agentId?: string;
  sessionId?: string;
  role?: string;
  /** Only messages of this kind, applied before `limit`. */
  kind?: MessageKind;
  limit?: number;
}

export const agentReadInbox = (team: InboxTeam, query: InboxQuery) =>
  invoke<InboxView>("agent_read_inbox", { team, query });

export type RoleDeliveryState = "pending" | "claimed" | "accepted" | "acknowledged";

/** One role's mailbox: its registered instances and its delivery queue (epoch ms). */
export interface RoleQueue {
  role: string;
  roleName: string | null;
  registrations: {
    instanceId: string;
    sessionId?: string;
    instanceName: string | null;
    generation: number;
    registeredAtMs: number;
    lastRenewedAtMs: number;
    expiresAtMs: number;
    live: boolean;
  }[];
  counts: Record<RoleDeliveryState, number>;
  deliveries: {
    deliveryId: number;
    messageId: number;
    state: RoleDeliveryState;
    from: string;
    fromName: string | null;
    excerpt: string;
    createdAtMs: number;
    firstClaimedAtMs: number | null;
    claimOwner: string | null;
    claimOwnerName: string | null;
    claimOwnerGeneration: number | null;
    claimExpiresAtMs: number | null;
    claimLapsed: boolean;
    receipt: {
      receiptId: string;
      instanceId: string;
      generation: number;
      kind: "work" | "no_work";
      note: string | null;
      /** @deprecated always null for new receipts; role work is not mirrored into wv_queue. */
      queueItemId: number | null;
      acceptedAtMs: number;
    } | null;
  }[];
  deliveriesTruncated: boolean;
}

export const agentReadRoles = (
  team: InboxTeam,
  role?: string,
  limit?: number,
  frameId?: string,
  page?: string,
) => invoke<{ roles: RoleQueue[] }>("agent_read_roles", { team, role, limit, frameId, page });

/** The outcome of the human's send from a dashboard. */
export type HumanSendResult =
  | { status: "sent"; messageId: number; deliveryId?: number }
  | { status: "invalid"; message: string };

/** The human sends to `to`: `@session` (handle or session id), `#role`, or a bare name
 *  (a session when one answers to it, else a role). Only that recipient gets it.
 *  `replyTo` (a running session, `human` or `role:<id>`) is where replies go instead of
 *  to the human. `summary` is the message's one-line list preview. `inReplyTo` is the
 *  message of `team`'s inbox it answers (marked read for the human). */
export const sendInboxAsHuman = (
  team: InboxTeam,
  to: string,
  text: string,
  contentType: MessageContentType | null,
  onClick: MessageOnClick | null,
  attachFile: string | null,
  replyTo: string | null = null,
  summary: string | null = null,
  inReplyTo: number | null = null,
) =>
  invoke<HumanSendResult>("send_inbox_as_human", {
    team,
    to,
    text,
    contentType,
    onClick,
    attachFile,
    replyTo,
    summary,
    inReplyTo,
  });

/** One message as a session's conversation and the rollup show it (mirror of Rust
 *  `ConversationMessage`). Ids are unique only within `team`'s inbox. */
export type ConversationMessage = {
  conversation?: string;
  id: number;
  team: { org: string; team: string };
  /** A session handle, or "human". */
  from: string;
  fromName: string;
  /** A session handle or "human"; null when addressed to a role queue. */
  to: string | null;
  toName: string | null;
  /** The role queue it was addressed to. */
  toRole: string | null;
  text: string;
  contentType: "plain" | "markdown" | "html" | null;
  createdAtMs: number;
  /** The human archived this message; it remains in conversation history. */
  archivedAtMs: number | null;
  inReplyTo: number | null;
  /** "terminal" when the text was typed into the recipient's terminal (send_to_session). */
  via: "terminal" | null;
  /** Where replies to it go instead of its sender (email Reply-To): a session handle,
   *  "human", or "role:<id>". Null: replies go to the sender. */
  replyTo: string | null;
  /** The sender's one-line plain-text preview, when it set one. */
  summary: string | null;
  primaryFile?: string;
  attachments?: InboxMediaAttachment[];
  onClick?:
    | { type: "goto-agent"; agent_id: string }
    | { type: "open-file"; path: string }
    | { type: "show-message" };
  /** Unread for its recipient (a role message: no instance has claimed it yet). */
  unread: boolean;
  delivery: null | {
    state: "pending" | "claimed" | "accepted" | "acknowledged" | "withdrawn";
    claimedBy: string | null;
  };
  /** SW-AGR-18: what it asks of the human; picks its lane (Needs you or Updates). */
  kind: MessageKind;
  /** Its emoji reactions, as {@link InboxMessageRow.reactions}. */
  reactions?: InboxReaction[];
};

/** One session's conversation across its own and every synced team (mirror of Rust `SessionInboxDto`). */
export type SessionInboxDto = {
  /** The session asked about, by handle. */
  handle: string;
  /** Roles this session currently holds. */
  roles: string[];
  /** Oldest first: every row it sent or received, and every row queued for a role it holds. */
  messages: ConversationMessage[];
};

/** Every synced team's messages and role queues (mirror of Rust `RollupDto`). */
export type RollupDto = {
  conversations?: InboxConversationSummary[];
  messages: ConversationMessage[];
  humanUnread?: number;
  roles: {
    role: string;
    team?: InboxTeam | null;
    holderCount?: number;
    holder: string | null;
    pending: number;
    note: string;
  }[];
};

/** One session's conversation (a handle or a tab session id; hand-started tabs work).
 *  Marks nothing read, claims nothing. Refetch on `agent-inbox-changed`. */
export const listSessionInbox = (handle: string) =>
  invoke<SessionInboxDto>("list_session_inbox", { handle });

/** Every synced team's messages and role queues, for the full-screen rollup. */
export const listInboxRollup = (frameId?: string, page?: string) =>
  frameId || page
    ? invoke<RollupDto>("list_inbox_rollup", { frameId, page })
    : invoke<RollupDto>("list_inbox_rollup");

export const setHumanStatus = (team: InboxTeam, status: HumanStatusValue, note?: string) =>
  invoke<HumanStatusView>("set_human_status", { team, status, note });

/**
 * SW-BSI-3: what the inbox knows about an attached managed file ahead of
 * actually opening it. `ready` means the owning story is linked and the
 * attachment should resolve on open (the viewer's own missing-file state still
 * governs the final rendering); `owningStoryUnknown` means no linked worktree
 * record claims the path's subtree, so the UI falls through to a generic
 * unavailable chip and offers no hydrate action.
 */
/** A remote machine's workspace to resolve the attachment against. Absent =
 *  local resolution; present = ask that remote whether it has hydrated the
 *  owning story. The UI passes the current workspace's remote target (if any)
 *  so the chip can offer a precise `[Hydrate story: X]` button. */
export interface InboxRemoteTarget {
  machineId: string;
  workspaceHandle: string;
}

export type AttachmentStatus =
  | { kind: "ready"; owningStoryId: string }
  | {
      kind: "notHydratedOnRemote";
      machineId: string;
      workspaceHandle: string;
      owningStoryId: string;
    }
  | { kind: "owningStoryUnknown" }
  | { kind: "tombstoned" }
  | { kind: "missing" };

export const inboxAttachmentStatus = (
  team: InboxTeam,
  primaryFile: string,
  remoteTarget?: InboxRemoteTarget,
) =>
  invoke<AttachmentStatus>("inbox_attachment_status", {
    team,
    primaryFile,
    remoteTarget: remoteTarget ?? null,
  });

/** SW-BSI-3 Phase 2 — the UI calls this on `[Hydrate story: X]`. Thin wrapper
 *  over the shared remote-hydration engine; the backend returns the engine's
 *  result verbatim (useful for the UI's pending/error states). */
export const triggerHydrateFromInbox = (
  machineId: string,
  workspaceHandle: string,
  storyId: string,
) =>
  invoke<unknown>("trigger_hydrate_from_inbox", {
    machineId,
    workspaceHandle,
    storyId,
  });

// -----------------------------------------------------------------------------
// SW-BSI-2 role-keyed delivery wrappers. Role mailboxes are addressed as
// (user, org, team, role); role is any name the agents agree on. See
// `_bmad-output/.../25-builder-skills-integration/GROOMING-PLAN.md`.

export type RoleAcceptKind = "work" | "no_work";

/** One accepted-but-unacknowledged delivery for a role, returned on
 *  registration so a re-registering instance can adopt its predecessor's work. */
export interface RoleAcceptedWork {
  deliveryId: number;
  role: string;
  receiptId: string;
  messageId: number;
  kind: RoleAcceptKind;
  /** @deprecated always null for new receipts; role work is not mirrored into wv_queue. */
  queueItemId: number | null;
}

/** Outcome of `registerRole`. */
export interface RoleRegistrationOutcome {
  generation: number;
  expiresAt: number;
  pendingCount: number;
  acceptedUnacked: RoleAcceptedWork[];
}

/** Prior-receipt summary surfaced on a reclaim-after-expiry. */
export interface RoleReceiptSummary {
  receiptId: string;
  kind: RoleAcceptKind;
  /** @deprecated always null for new receipts; role work is not mirrored into wv_queue. */
  queueItemId: number | null;
}

/** One claimed delivery. */
export interface RoleClaimedDelivery {
  deliveryId: number;
  messageId: number;
  role: string;
  claimToken: string;
  claimExpiresAt: number;
  existingReceipt: RoleReceiptSummary | null;
}

/** Disposition payload for `acceptRoleMessage`. */
export interface RoleAcceptDisposition {
  kind: RoleAcceptKind;
  noteOrReason: string;
}

/** Outcome of `sendRoleMessage`. */
export type SendRoleMessageResult =
  | { status: "sent"; messageId: number; deliveryId: number }
  | { status: "conflict" }
  | { status: "invalid"; message: string };

/** Register this instance as a live member of `role` in `team`. */
export const registerRole = (team: InboxTeam, role: string, instanceId: string) =>
  invoke<RoleRegistrationOutcome>("register_role_cmd", { team, role, instanceId });

/** Atomically claim up to `limit` pending deliveries for `role`. */
export const claimRoleMessages = (
  team: InboxTeam,
  role: string,
  instanceId: string,
  generation: number,
  limit: number,
) =>
  invoke<RoleClaimedDelivery[]>("claim_role_messages_cmd", {
    team,
    role,
    instanceId,
    generation,
    limit,
  });

/** Commit the acceptance receipt (role work is not mirrored into `wv_queue`). */
export const acceptRoleMessage = (
  team: InboxTeam,
  deliveryId: number,
  claimToken: string,
  instanceId: string,
  generation: number,
  disposition: RoleAcceptDisposition,
) =>
  invoke<string>("accept_role_message_cmd", {
    team,
    deliveryId,
    claimToken,
    instanceId,
    generation,
    disposition,
  });

/** Acknowledge an accepted delivery. */
export const ackRoleMessage = (
  team: InboxTeam,
  deliveryId: number,
  claimToken: string,
  instanceId: string,
  generation: number,
  receiptId: string,
) =>
  invoke<void>("ack_role_message_cmd", {
    team,
    deliveryId,
    claimToken,
    instanceId,
    generation,
    receiptId,
  });

/** Release this instance's registration + any un-accepted claims it owns. */
export const releaseRole = (
  team: InboxTeam,
  role: string,
  instanceId: string,
  generation: number,
) => invoke<number>("release_role_cmd", { team, role, instanceId, generation });

/** Write an inbox row addressed to `role` and a pending `wv_role_delivery`
 *  row in one IMMEDIATE transaction. `requestId` enables idempotent retry.
 *  `attachFile` is an optional managed-file reference (logical path) stored
 *  verbatim on the inbox row; it participates in the request-id payload hash
 *  so a retry with a different attachment under the same `requestId` returns
 *  `status: 'conflict'`, not a replay. */
export const sendRoleMessage = (
  team: InboxTeam,
  sender: string,
  role: string,
  text: string,
  requestId: string | null,
  attachFile: string | null = null,
  onClick: MessageOnClick | null = null,
  contentType: MessageContentType | null = null,
  summary: string | null = null,
) =>
  invoke<SendRoleMessageResult>("send_role_message_cmd", {
    team,
    sender,
    role,
    text,
    requestId,
    attachFile,
    onClick,
    contentType,
    summary,
  });

/** SW-WV-B3: one durable roster row from `wv_roster` in `weft.sqlite3`. The
 *  controller tree's machine-checkable source of truth — `relation`
 *  (`"peer"` | `"child:<scope>"`) + `parent` distinguish observe-only peers from
 *  scope-leased children. The roster store maps this into {@link AgentSessionRow}
 *  (SW-WV-B3 Task 7). */
export interface WvRosterRow {
  name: string;
  workspace: string | null;
  pane: string | null;
  kind: string | null;
  subclass: string | null;
  lifecycle: string;
  owned: string | null;
  orchOwner: string | null;
  relation: string;
  parent: string | null;
  currentTask: string | null;
  cwd: string | null;
  status: string | null;
  sessionId: string | null;
  runId: string | null;
}

/** SW-WV-B3: read the durable roster from `wv_roster`. The roster store loads this
 *  at mount (superseding the ephemeral `dashboard_list_sessions` initial load) and
 *  then applies live `pty-exit` / `session-state-changed` patches on top. */
export const wvListRoster = () => invoke<WvRosterRow[]>("wv_list_roster");

/** SW-WV-B3: upsert a roster row on the `name` PK. Tauri-only (the roster is
 *  written by the controller automation + FE, never by worker agents). */
export const wvUpsertRoster = (row: WvRosterRow) => invoke<void>("wv_upsert_roster", { row });

/** SW-WV-B3: deprovision (remove) a roster row by name. Refuses a `peer` row.
 *  Resolves `true` when a child row was removed, `false` on a no-op. */
export const wvDeprovisionRoster = (name: string) =>
  invoke<boolean>("wv_deprovision_roster", { name });

/** SW-WV-A3: advance a worker `signal → handoff` (the optional handoff-doc step).
 *  Rejects an illegal predecessor with a typed refusal; emits `wv-roster-changed`. */
export const wvWorkerHandoff = (name: string) => invoke<void>("wv_worker_handoff", { name });

/** SW-WV-F3: the compact roster projection the fleet-view row reads for ONE confirmed
 *  Weft worker — kept OFF the hot `dashboard_list_sessions` snapshot (a keyed lookup,
 *  not a Weft-only widening of that enumerate-everything contract). `null` for a
 *  non-Weft session; the row then renders identically to today. `status` is the
 *  LIFECYCLE value (`provision`|`dispatch`|`signal`|`handoff`|`deprovision`|
 *  `needs-attention`) — the FE close-gate keys on `"dispatch"`. */
export interface WvRosterRowView {
  subclass: string | null;
  kind: string | null;
  current_task: string | null;
  status: string | null;
}

/** SW-WV-F3 (AC-1/AC-2/AC-3): read the fleet-view roster projection for a live
 *  session, keyed by its PTY session id. Resolves `null` for a non-Weft session (a
 *  missing `weft.sqlite3` or no matching row is graceful, never an error). */
export const wvRosterForSession = (sessionId: string) =>
  invoke<WvRosterRowView | null>("wv_roster_for_session", { sessionId });

/** SW-WV-F3 (AC-5): the operator override — Close a still-`dispatch` worker that never
 *  declared done, marking it `needs-attention` (never silently lost). Reuses A4's
 *  audited single-writer CAS; a worker that signalled between the click and the write
 *  is a no-op. Emits `mcp:agent-done` so the roster re-fetches. */
export const wvWorkerForceClose = (handle: string) =>
  invoke<void>("wv_worker_force_close", { handle });

/** SW-WV-A3: deprovision a worker after the four-condition closure check.
 *  `packetDelivered` confirms packet delivery; `force` overrides offered-worker
 *  protection. A still-alive PTY logs a non-blocking warning; emits
 *  `wv-roster-changed` on success. */
export const wvWorkerDeprovision = (name: string, packetDelivered: boolean, force: boolean) =>
  invoke<void>("wv_worker_deprovision", { name, packetDelivered, force });

/** SW-WV-B3: settle an approval on owner reply — the UI's reply path for both T2
 *  (window-expiry) and T3 (owner 👍). `disposition` is `ratified` | `revoked`. */
export const wvSettleApproval = (id: number, disposition: string) =>
  invoke<boolean>("wv_settle_approval", { id, disposition });

/** SW-WV-B3: advance a per-source read watermark keyed on `(source, scope)`. */
export const wvUpsertWatermark = (source: string, scope: string, lastHandled: string) =>
  invoke<void>("wv_upsert_watermark", { source, scope, lastHandled });

/** SW-WV-QH1 (#1431): the seeded assertion + its YOU-OWE attention row returned by
 *  `wvE2eSeedEndorsement`. The weft-e2e suite ratifies `assertionId` and asserts the
 *  board row keyed `attentionId` moves YOU-OWE → DONE-UNACKED. */
export interface WvSeededEndorsement {
  assertionId: number;
  attentionId: number;
}

/** SW-WV-QH1 (#1431): harness-only — seed a pending agent assertion plus its matching
 *  YOU-OWE `wv_attention` row so a real endorsement-gate row exists to ratify. Uses the
 *  same sanctioned writers the MCP endorsement path uses (no schema bypass). Refused
 *  unless `CODESENSE_E2E=1`, so it can never seed state in a real session. */
export const wvE2eSeedEndorsement = (sessionId: string, claim: string, channel: string) =>
  invoke<WvSeededEndorsement>("wv_e2e_seed_endorsement", { sessionId, claim, channel });

/** SW-WV-QH1 (#1431): harness-only — seed the ledger state a real worker leaves at
 *  dispatch time (a durable dispatch record + a roster row advanced provision →
 *  dispatch bound to `sessionId`), minus the live PTY, so the RT1 done-hooks can be
 *  driven end-to-end against real rows headlessly. Refused unless `CODESENSE_E2E=1`. */
export const wvE2eSeedDispatch = (storyId: string, sessionId: string) =>
  invoke<void>("wv_e2e_seed_dispatch", { storyId, sessionId });

/** SW-WV-E1: record a recipient reaction (👀 seen / ✅ acknowledged / 🚫 declined)
 *  on an assertion. SEPARATE from owner ratification — a decline NEVER ratifies; it
 *  routes a finding back to the originating worker. Resolves `{recorded, ackId}`. */
export const wvRecordRecipientAck = (
  assertionId: number,
  recipient: string,
  ack: "seen" | "acknowledged" | "declined",
) =>
  invoke<{ recorded: boolean; ackId: number }>("wv_record_recipient_ack", {
    assertionId,
    recipient,
    ack,
  });

/** SW-WV-E1: the owner 👍 ratifies a pending assertion (the wall moves). Tauri-only —
 *  ratification is a human action through the UI boundary, never an MCP verb, so no
 *  agent can ratify an agent. Idempotent: resolves `{status: "alreadySettled",
 *  disposition}` if the assertion already left `pending`, else `{status: "ok", id}`. */
export const wvRatify = (assertionId: number) =>
  invoke<{ status: string; id?: number; disposition?: string }>("wv_ratify", {
    assertionId,
  });

/** SW-WV-E1 / SW-WV-F4: the symmetric owner action — revoke a pending assertion
 *  (Send-back). Same UI-boundary-only contract as {@link wvRatify}. `note` (AC-6) is
 *  the operator's send-back reason: when present the Rust handler emits
 *  `mcp:agent-message` to the worker session before the revoked event — no separate
 *  FE call. Omit it (or pass `undefined`) for a bare revoke. */
export const wvRevoke = (assertionId: number, note?: string) =>
  invoke<{ status: string; id?: number; disposition?: string }>("wv_revoke", {
    assertionId,
    note: note ?? null,
  });

/** SW-WV-F4 (AC-1): one pending assertion the review-at-altitude header lists. */
export interface WvPendingAssertionDto {
  id: number;
  sessionId: string;
  runId: string | null;
  claim: string;
  channel: string;
  postedAt: string;
  disposition: string;
}

/** SW-WV-F4 (AC-1): read every `pending` assertion for a run — the review header's
 *  source. The Rust command filters to `pending` in SQL off the UI thread. Read-only. */
export const readWeftPendingAssertions = (runId: string) =>
  invoke<WvPendingAssertionDto[]>("wv_read_pending_assertions", { runId });

/** SW-WV-F5 (AC-1, AC-3): one pending `wv_approvals` row the YOU-OWE endorsement surface
 *  merges alongside pending assertions — the T2/T3 rows. The window fields map to E3's
 *  durable columns (`ttlS`=`window_duration_s`, `acquiredAt`=`window_opened_at`).
 *  `notSurfacedButExpired` is `true` for a spent, still-pending T2 that did not
 *  auto-close (AC-3's "carries to next session"). */
export interface WvApprovalRow {
  id: number;
  tier: "T1" | "T2" | "T3";
  claim: string;
  postedAt: string;
  source: string | null;
  windowState: "open" | "paused" | "expired";
  disposition: "pending" | "ratified" | "revoked";
  ttlS: number | null;
  acquiredAt: number | null;
  pauseAccumulatedS: number;
  surfacedSession: string | null;
  notSurfacedButExpired: boolean;
}

/** SW-WV-F5 (AC-1, AC-3): read every pending `wv_approvals` row — the T2/T3 half of the
 *  YOU-OWE endorsement surface. The Rust command filters to `pending` in SQL and computes
 *  `notSurfacedButExpired` at the current epoch, off the UI thread. Read-only. */
export const wvListPendingApprovals = () =>
  invoke<WvApprovalRow[]>("wv_list_pending_approvals", {});

/** SW-WV-F5 (AC-4) / SW-WV-E3: mark a pending T2 approval row surfaced in the current
 *  session — the FE half of the surfaced-item guard. Arms the E3 revocation window
 *  (stamps `surfacedSession`) so it may only auto-close in THIS session. Idempotent:
 *  a second call for an already-armed window is a no-op success, so the FE fires once
 *  per mount and Rust-side idempotency handles cross-mount safety. */
export const wvMarkApprovalSurfaced = (params: {
  id: number;
  sessionId: string;
  durationS?: number;
}) =>
  invoke<void>("wv_mark_approval_surfaced", {
    id: params.id,
    sessionId: params.sessionId,
    durationS: params.durationS ?? null,
  });

/** SW-WV-F1: one attention-board row (the Weft Board's neutral row shape). `summary`
 *  is the ~140-char standalone sentence a reader lands on; `source`/`ref` are the
 *  opaque B3 AC-8 routing refs the chevron routes on (a null/unknown pair is a
 *  no-op). Rows are read-only — the FE never hand-edits them. */
export interface AttentionRowDto {
  id: number;
  summary: string;
  source: string | null;
  ref: string | null;
  /** SW-WA-E5: who started the row — `"agent"` (a 🤖 agent-owned / agent-claimed row)
   *  or `"human"` (a triage row the operator owns). Drives the 🤖 provenance marker.
   *  SW-AV-3: renamed from `"controller"` (the 🤖 marker means agent-owned). */
  initiator: "agent" | "human";
  /** SW-AV-3: what the row IS — the Activity chip derives from the kind, not the lane
   *  it landed in (design 4.3). The backend always populates it; optional here so an
   *  old-shaped record still types (SW-AV-4 consumes it for the per-kind chip). */
  kind?: AttentionKind;
  /** SW-WA-E5: the ON-AGENT live-terminal target (a dispatched row's opaque worker
   *  handle). The FE guards resolvability — an unresolvable handle earns no one-click
   *  affordance (never a dead-end click). Absent for non-live rows. */
  liveSessionId?: string | null;
  /** SLW-TL-4: the DECLARED need this item carries (from the fixed set), or absent
   *  for a legacy weft row / a running-agent row. The generalized Inbox row renderer
   *  keys its plain label + matching action (SLW-TL-7) off this. */
  need?: AttentionNeed | null;
}

/** SLW-TL-4: the fixed set of declared needs a board item may carry (§2.3) — what the
 *  item needs from the human, so the Inbox maps need → the matching action. Mirrors
 *  the Rust `AttentionNeedDto` (kebab-case wire values). */
export type AttentionNeed =
  | "awaiting-answer"
  | "needs-approval"
  | "needs-attention"
  | "awaiting-validation"
  | "done";

/** SW-AV-3: the attention-row kind vocabulary — mirrors the Rust `AttentionKindDto`.
 *  The SW-AV-4 chip maps each value to its own icon+label (design 4.3). */
export type AttentionKind =
  | "assertion"
  | "approval"
  | "question"
  | "parked"
  | "blocked"
  | "running"
  | "done";

/** SW-WV-F1: the five Weft Board slices, each an ordered `AttentionRowDto[]`. YOU-OWE
 *  / OWED-TO-YOU / DONE-UNACKED are `wv_attention` rows (by `state`); ON-AGENT is
 *  `wv_queue` rows in `claimed`+`dispatched` (both active — ARCHITECTURE §2.4); PARKED
 *  is `wv_queue` rows in `pending`. */
export interface AttentionBoardDto {
  youOwe: AttentionRowDto[];
  owedToYou: AttentionRowDto[];
  onAgent: AttentionRowDto[];
  parked: AttentionRowDto[];
  doneUnacked: AttentionRowDto[];
}

/** SW-WV-F1 (AC-1): read the five-section attention board for the active team. The
 *  Rust command opens `weft.sqlite3` and assembles all five slices in one read
 *  transaction off the UI thread. Read-only. */
export const listAttentionBoard = () => invoke<AttentionBoardDto>("wv_list_attention_board", {});

/** SW-LN-B20: declare (or end) the local guest session, so backend commands that derive the
 *  ambient user work in the guest's local workspace while nobody is signed in. A code-host
 *  sign-in always wins over the guest on the backend. */
export const setGuestSession = (active: boolean) => invoke<void>("set_guest_session", { active });

/** SW-LN-B47: whether the backend records a local guest session (its on-disk marker,
 *  read at startup). The frontend reads it when its webview store is empty, so the
 *  store is only a cache of the guest choice. */
export const getGuestSession = () => invoke<boolean>("get_guest_session");

/** SW-WV-F2: the dispatch preflight verdict. One compact status line: `ok` → green
 *  "Ready to dispatch"; `!ok` → amber `message` naming the blocking dimension. */
export interface WvPreflightResult {
  ok: boolean;
  queueRowOk: boolean;
  capacityOk: boolean;
  message: string | null;
}

/** SW-WV-F2: run the dispatch preflight for the active team before enabling
 *  "Dispatch & release". `queueId` is the selected Board row's id (or `null` from the
 *  header "+ Dispatch"); `subclass` is carried through the one call shape. The Rust
 *  command derives the calling context itself (session identity is never a caller
 *  argument) and runs all reads off the UI thread. */
export const preflightWvDispatch = (queueId: number | null, subclass: string) =>
  invoke<WvPreflightResult>("preflight_wv_dispatch", { queueId, subclass });

/** SW-WV-F2: the 4-field dispatch brief on the wire. Tauri maps these camelCase JS
 *  keys to the Rust command's snake_case params automatically. */
export interface WvDispatchParams {
  queueId: number | null;
  subclass: string;
  goal: string;
  context: string;
  constraints: string;
  doneWhen: string;
}

/** SW-WV-F2: the resolved dispatch (the Rust `wv_dispatch_worker` bridge is owned by
 *  SW-WV-F7 — a thin door over the C1 dispatch core). */
export interface WvDispatchResult {
  ok: boolean;
  dispatch_id: string;
  worker_handle: string;
  done_when: string;
}

/** SW-WV-F2: "Dispatch & release". Invokes the `wv_dispatch_worker` Tauri bridge
 *  (Rust side owned by SW-WV-F7). Blocking work runs on the Rust pool; the form
 *  unmounts on the resolved promise and the Board's live channel surfaces the new row
 *  — there is no watch view. Until F7 lands its Rust handler this invoke name has no
 *  matching handler and is allow-listed in the command-parity contract test. */
export const dispatchWeftWorker = (params: WvDispatchParams) =>
  invoke<WvDispatchResult>("wv_dispatch_worker", params as unknown as Record<string, unknown>);

/** SW-GW-3 (AC-2): probe REAL child liveness for an ARBITRARY set of session ids
 *  and return the alive subset (input order). Unlike `dashboardListSessions`
 *  (registry-filtered), this answers for a session opened by ANY means — a story
 *  worktree opened in the viewer never enters the launch registry — so the
 *  OBSERVE-half liveness query covers manually-opened workspaces. `alive` = real
 *  `try_wait` (SW-DX-17a: never map membership, never "a tab exists"). */
export const dashboardProbeSessionsAlive = (sessionIds: string[]) =>
  invoke<string[]>("dashboard_probe_sessions_alive", { sessionIds });

/** SW-DX-17a (AC-2/AC-4): kill the session bound to `handle` through the one
 *  authoritative `kill_session` choke (frees the cap slot + writes an audit
 *  line). A handle with no live binding is a typed no-op, never an error. */
export const dashboardKillByHandle = (handle: string) =>
  invoke<void>("dashboard_kill_by_handle", { handle });

/** SW-DX-17d (AC-1/AC-3b): BRING TO FOREGROUND — release the background-limit slot
 *  held by `handle` as it becomes an attended tab (keeps the entry addressable).
 *  Pairs with the FE opening the tab; idempotent no-op if it held no slot. */
export const dashboardForegroundSession = (handle: string) =>
  invoke<void>("dashboard_foreground_session", { handle });

/** SW-DX-17d (AC-3): push the user-owned background-agent limit into Rust (mirror
 *  of `useBackgroundSetupCap`'s live-apply). Non-destructive when lowered. */
export const dashboardSetBackgroundLimit = (limit: number) =>
  invoke<void>("dashboard_set_background_limit", { limit });

/** AC-11: resolve a dashboard `storyId` to its managed story path + team,
 *  trusted-side via the frontmatter cache. Rejects an unknown/ambiguous id
 *  rather than trusting a caller-supplied path. */
export const resolveStoryPath = (storyId: string, frameId?: string, page?: string) =>
  invoke<{ storyFilePath: string; team: string }>("resolve_story_path", { storyId, frameId, page });

/** SW-DX-16: resolve an EXPLICIT `{team, epic}` to the epic's canonical mainDoc
 *  path + leaf dir name, trusted-side. The epic analog of `resolveStoryPath` —
 *  canonicalizes + confines the (doc-supplied) epic identifier under the team
 *  content root and picks the mainDoc by the scanner's precedence. Rejects
 *  unknown/traversal/unmanaged with a typed prefix before any spawn. The command
 *  context comes from the FE's already-resolved `EpicCommandContext`, never the
 *  doc. */
export const resolveEpicPath = (
  codesenseHome: string,
  githubUserId: number,
  orgSlug: string,
  team: string,
  epic: string,
  frameId?: string,
  page?: string,
) =>
  invoke<{ epicFilePath: string; epicDirName: string }>("resolve_epic_path", {
    codesenseHome,
    githubUserId,
    orgSlug,
    team,
    epic,
    frameId,
    page,
  });

export const resizePty = (sessionId: RuntimeSessionReference, cols: number, rows: number) =>
  invoke<void>("resize_pty", { sessionId, cols, rows });

// === The host-owned terminal layout ===
//
// Each terminal host (the in-process runtime, and the session-server sidecar
// when it runs) owns which workspaces hold its terminal tabs, their names,
// order, grid and placement. Every change is pushed on
// `terminal-layout-changed` as `{ epoch, rev, workspace, layout }` (`layout`
// is null once the workspace is gone); `rev` counts per host lifetime `epoch`.
// A host whose layout moved while the app could not hear it is announced on
// `terminal-layout-resync` as `{ epoch }`: re-fetch the snapshot. Mirrors of
// the Rust `layout` types.

export type TerminalPlacement = "tab" | "background";

export type TerminalEntryState =
  | { kind: "live" }
  | { kind: "exited"; exitCode: number | null; signal: string | null };

export type TerminalGridPreset = "columns" | "rows" | "grid";

export interface TerminalGridState {
  paned: string[];
  preset: TerminalGridPreset;
  collapsed: boolean;
}

/** What a client needs to render a workspace it did not create; the host
 *  stores it verbatim. `worktreePath` is a path on the host's disk. */
export interface TerminalWorkspaceDescriptor {
  displayName: string;
  kind: string;
  org?: string | null;
  team?: string | null;
  storyId?: string | null;
  repo?: string | null;
  worktreePath?: string | null;
  branch?: string | null;
}

/** One terminal entry; `session` is both the tab id and the PTY session id. */
export interface TerminalTabEntry {
  session: string;
  name: string;
  role: string | null;
  placement: TerminalPlacement;
  state: TerminalEntryState;
  tags: Record<string, string>;
}

export interface TerminalWorkspaceLayout {
  descriptor: TerminalWorkspaceDescriptor;
  tabs: TerminalTabEntry[];
  lastActive: string | null;
  grid: TerminalGridState;
  nextOrdinal: number;
}

export interface TerminalViewHint {
  scrollFromBottom: number;
}

/** This viewer's own focus, scroll hints, and the exited entries it no longer
 *  shows (it relaunched or closed them while another viewer still shows them). */
export interface TerminalViewerLayout {
  active: Record<string, string>;
  views: Record<string, TerminalViewHint>;
  hidden: string[];
}

export interface TerminalLayoutSnapshot {
  schema: number;
  serverId: string;
  serverName: string;
  epoch: string;
  rev: number;
  workspaces: Record<string, TerminalWorkspaceLayout>;
  viewer: TerminalViewerLayout;
}

/** How a layout tab's session is spawned: a command, or a shell preferring
 *  the warm pool. */
export type TerminalTabSpawn =
  | ({
      kind: "cold";
      command: string;
      cwd: string;
      size: { cols: number; rows: number };
      interactive?: boolean;
      starshipPreset?: string;
    } & SpawnPtyOptions)
  | ({
      kind: "warm";
      targetCwd: string;
      size: { cols: number; rows: number };
      starshipPreset?: string;
    } & Omit<SpawnPtyOptions, "envRemove" | "account" | "environmentBinding">);

export interface TerminalOpenTabRequest {
  machineId: string;
  workspace: string;
  session: string;
  /** Required to create the workspace; replaces the stored one when given. */
  descriptor?: TerminalWorkspaceDescriptor;
  name?: string;
  role?: string;
  tags?: Record<string, string>;
  placement: TerminalPlacement;
  index?: number;
  activate: boolean;
  spawn: TerminalTabSpawn;
}

export type TerminalCloseMode = "keepIfBusy" | "kill";
export type TerminalCloseOutcome = "killed" | "backgrounded" | "removed";

/** A member's `LayoutChanged` domain event: its whole layout of one workspace
 *  after a change (`layout` null once the workspace is gone), numbered by `rev`
 *  within the member's lifetime `epoch`. */
export interface TerminalLayoutChanged {
  epoch: string;
  rev: number;
  workspace: string;
  layout: TerminalWorkspaceLayout | null;
}

/** `terminal-layout-resync`: a host's layout moved while the app could not
 *  hear it (`epoch` is the lifetime now serving) — re-fetch the snapshot. */
export const TERMINAL_LAYOUT_RESYNC_EVENT = "terminal-layout-resync";
export interface TerminalLayoutResync {
  privateAuthorityEpoch?: string;
  epoch: string;
}

/** `terminal-runtime-event`: a runtime member's domain event, stamped with the
 *  admitted member that emitted it. The viewer reads its `LayoutChanged`. */
export const TERMINAL_RUNTIME_EVENT = "terminal-runtime-event";
export interface ActivityIdentity {
  hostBootId: string;
  sessionIncarnation: string;
  invocationId: string;
}
/** Authoritative owner position when a managed completion was accepted. */
export interface ActivityCompletion {
  identity: ActivityIdentity;
  turnGeneration: number;
  revision: number;
}
export interface ActivitySnapshot {
  session: string;
  identity: ActivityIdentity;
  revision: number;
  turnGeneration: number;
  provider: "claude" | "codex";
  state: "idle" | "thinking" | "working" | "needs-permission" | "done" | "errored" | "crashed";
  observed: boolean;
  limited: boolean;
  /** Explicit tool evidence; older hosts omit it. */
  toolOpen?: boolean;
  /** Age of the latest actual activity at host capture, in milliseconds. */
  activityAgeMs?: number;
  /** The subagents its agent runs now; omitted at none. `state` stays the agent's own. */
  subagentsOpen?: number;
  exit: { code: number | null; signal: string | null; requested: boolean } | null;
  asks: string[];
  /** Why the state holds, when the provider said: an error kind (`rate_limit`,
   *  `authentication_failed`, `billing_error`, `overloaded`, `server_error`, …) while
   *  `errored`; `question` while waiting on an answer. */
  reason?: string | null;
}
export interface ActivitySnapshotReply {
  availability?: "available" | "upgrade-required";
  target: RuntimeMemberTarget;
  privateAuthorityEpoch?: string;
  snapshots: ActivitySnapshot[];
}
export const terminalActivitySnapshot = (target: RuntimeMemberTarget) =>
  invoke<ActivitySnapshotReply>("terminal_activity_snapshot", { target });
export const terminalPrepareShellActivity = (
  session: RuntimeSessionReference,
  provider: "claude" | "codex",
) =>
  invoke<{ identity: ActivityIdentity; helper: string; bootstrap: string }>(
    "terminal_prepare_shell_activity",
    { session, provider },
  );
export const terminalLaunchActivityLine = (
  session: RuntimeSessionReference,
  provider: "claude" | "codex",
  line: string,
  submit = false,
) =>
  invoke<{ submitted: boolean }>("terminal_launch_activity_line", {
    session,
    provider,
    line,
    submit,
  });
export interface TerminalRuntimeEvent {
  privateAuthorityEpoch?: string;
  target: RuntimeMemberTarget;
  event: {
    LayoutChanged?: TerminalLayoutChanged;
    Activity?: { snapshot: ActivitySnapshot };
  } & Record<string, unknown>;
}

/** `terminal-layout-admission-changed`: one member's new admission row. */
export const TERMINAL_LAYOUT_ADMISSION_CHANGED_EVENT = "terminal-layout-admission-changed";

/** Every admitted member's layout for this app launch, one sealed reply per
 *  member: live with its snapshot, historical, or unavailable. A new
 *  `clientBootId` (a relaunch) prunes the exited tabs this viewer was showing;
 *  a reload keeps them. */
export const terminalLayoutMemberSnapshots = (clientBootId: string) =>
  invoke<RuntimeLayoutReply[]>("terminal_layout_snapshot", { clientBootId });

export const terminalOpenTab = (request: TerminalOpenTabRequest) =>
  invoke<TerminalTabEntry>("terminal_open_tab", { request });

/** What a respawn re-projects for an agent launch: the inputs the launch gave
 *  `projectMcpScopeConfig`. */
export interface TerminalRespawnScope {
  cwd: string;
  /** `null`: a launch with no allowlist (the bridge on top of the agent's own servers). */
  mcpAllow: string[] | null;
  /** Captured bridge tool grant: null/absent is unrestricted; [] denies all. */
  bridgeTools?: string[] | null;
  account?: ResolveAgentAccountInput;
  launchArgs?: string[];
  /** The CLI's basename, which picks the runtime the scope is projected for. */
  binary?: string;
  /** A named agent's relaunch: its scope alone. */
  agent?: boolean;
  /** The launch runs the launcher's default args, not a resolved profile's. */
  defaultArgs?: boolean;
  /** A named agent's own environment variables, set again on the relaunch. */
  agentEnv?: Record<string, string>;
}

/** A respawned tab: its entry (same id, same place, now live), and a scoped
 *  launch's fresh `--mcp-config` arguments for the agent's line. */
export interface TerminalRespawnedTab {
  /** The backend already executed these freshly projected arguments. */
  execution?: ScopedExecution;
  entry: TerminalTabEntry;
  extraArgs: string[];
  removeArgs?: string[];
}

/**
 * Start a new process in `session`'s exited tab, under the same session id and
 * in the same place — Restart and Relaunch on an ended pane. Refused while the
 * tab or its process is still running. The host reaps the ended session first
 * (keeping the tab, its dashboard binding and launch provenance), removes its
 * old scope config and, with `scope`, projects a fresh one onto `spawn`. A
 * failed spawn leaves the tab exited.
 */
export const terminalRespawnTab = (
  machineId: string,
  session: RuntimeSessionReference,
  spawn: TerminalTabSpawn,
  scope?: TerminalRespawnScope,
) => invoke<TerminalRespawnedTab>("terminal_respawn_tab", { machineId, session, spawn, scope });

export const terminalCloseTab = (session: RuntimeSessionReference, mode: TerminalCloseMode) =>
  invoke<TerminalCloseOutcome>("terminal_close_tab", { session, mode });

export const terminalPlaceTab = (
  session: RuntimeSessionReference,
  workspace: string,
  placement: TerminalPlacement,
  index?: number,
  descriptor?: TerminalWorkspaceDescriptor,
) => invoke<void>("terminal_place_tab", { session, workspace, placement, index, descriptor });

export const terminalRenameTab = (session: RuntimeSessionReference, name: string) =>
  invoke<void>("terminal_rename_tab", { session, name });

export const terminalMoveTab = (session: RuntimeSessionReference, toIndex: number) =>
  invoke<void>("terminal_move_tab", { session, toIndex });

export const terminalSetActive = (
  workspace: RuntimeWorkspaceReference,
  session: RuntimeSessionReference | null,
) => invoke<void>("terminal_set_active", { workspace, session });

export const terminalSetGrid = (workspace: RuntimeWorkspaceReference, grid: TerminalGridState) =>
  invoke<void>("terminal_set_grid", { workspace, grid });

export const terminalSetViewHint = (session: RuntimeSessionReference, hint: TerminalViewHint) =>
  invoke<void>("terminal_set_view_hint", { session, hint });

/** A tag patch: a string sets the tag, `null` removes it. */
export const terminalSetTags = (
  session: RuntimeSessionReference,
  patch: Record<string, string | null>,
) => invoke<void>("terminal_set_tags", { session, patch });

export const terminalSetDescriptor = (
  workspace: RuntimeWorkspaceReference,
  descriptor: TerminalWorkspaceDescriptor,
) => invoke<void>("terminal_set_descriptor", { workspace, descriptor });

export const terminalCloseWorkspace = (workspace: RuntimeWorkspaceReference) =>
  invoke<void>("terminal_close_workspace", { workspace });

// === The terminal server ===
//
// A terminal server (the session-server sidecar) at another protocol version
// than this app refuses new terminals until it is restarted, which ends the
// sessions it runs. It is reported on `terminal-server-incompatible` (and by
// `terminalServerStatus` for a view that mounts later), and again on
// `terminal-server-refused` whenever the user tries to open a terminal; a
// server that restarted (or stopped) under the app is reported on
// `terminal-server-restarted`, and viewport streams a reconnect could not carry
// across on `terminal-streams-lost`.

/** Mirrors the Rust `IncompatibleTerminalServer`. `liveSessions` is the
 *  server's own count of the sessions a restart ends; `null` when it is too
 *  old to report one. */
export interface IncompatibleTerminalServer {
  liveSessions: number | null;
  serverProtocol: number;
  appProtocol: number;
  reason: string;
}

/** Mirrors the Rust `TerminalServerRestarted`: the open sessions it ended. */
export interface TerminalServerRestarted {
  endedSessions: number;
}

/** A reconnect to the terminal server could not carry these viewport streams
 *  (by the ids `subscribeTerminal` returned) across: each pane viewing one
 *  re-subscribes, so it never freezes silently. Mirrors the Rust
 *  `TerminalStreamsLost`. */
export const TERMINAL_STREAMS_LOST_EVENT = "terminal-streams-lost";
export interface TerminalStreamsLost {
  streams: number[];
}

/** The terminal server as last found incompatible, or `null`. */
export const terminalServerStatus = () =>
  invoke<IncompatibleTerminalServer | null>("terminal_server_status");

export interface LocalActivityUpgrade {
  serverId: string;
  epoch: string;
  pid: number;
  sessionIds: string[];
}
export const terminalActivityUpgradeStatus = () =>
  invoke<LocalActivityUpgrade | null>("terminal_activity_upgrade_status");
export const updateTerminalActivityServer = (expected: LocalActivityUpgrade) =>
  invoke<void>("update_terminal_activity_server", { expected });

/** Mirrors the Rust `RestartError`: a restart's rejection. `oldStopped` when
 *  the old server is gone and only the new one did not start; `message` is
 *  for logs, not for display. */
export interface RestartTerminalServerError {
  oldStopped: boolean;
  message: string;
}

/** End the running terminal server (ending its sessions) and start this
 *  app's in its place, within a bounded time. Resolves to how many sessions
 *  the old server said it ran, or `null` when it did not say; rejects with a
 *  {@link RestartTerminalServerError}. */
export const restartTerminalServer = () => invoke<number | null>("restart_terminal_server");

/** SV-7: whether this instance's terminal server runs. Mirrors the Rust
 *  `TerminalServerRun`. */
export type TerminalServerRun =
  | "running"
  | "stopped"
  | "incompatible"
  | "unreachable"
  | "notApplicable";

/** SV-7: a session no app holds — opened by a CodeSense instance gone for 5
 *  minutes or more, and shown by no viewport. Mirrors the Rust
 *  `UnownedSession`. */
export interface UnownedTerminalSession {
  session: string;
  /** What it runs (its foreground job), or `null` for a bare shell, an exited
   *  session, or when it cannot be told. */
  command: string | null;
  startedAtMs: number;
  running: boolean;
  /** It runs an agent CLI. */
  runningAgent: boolean;
}

/** SV-7: this instance's terminal server, as Settings shows it. Counts are
 *  `null` when it does not run or is older than the question (`detail` says
 *  why); `expected` is what a stop or restart binds to. Mirrors the Rust
 *  `TerminalServerOverview`. */
export interface TerminalServerOverview {
  state: TerminalServerRun;
  pid: number | null;
  version: string | null;
  startedAtMs: number | null;
  sessions: number | null;
  running: number | null;
  runningAgents: number | null;
  exitedKept: number | null;
  /** The running sessions a stop or restart ends (`expected`'s); `null` when
   *  unknown — never a guessed 0. */
  ends: number | null;
  connectedInstances: number | null;
  otherInstances: number | null;
  unowned: UnownedTerminalSession[];
  expected: LocalActivityUpgrade | null;
  incompatible: IncompatibleTerminalServer | null;
  detail: string | null;
}

/** SV-7: what a stop ended. Mirrors the Rust `StoppedTerminalServer`. */
export interface StoppedTerminalServer {
  ended: number;
}

/** SV-7: this instance's terminal server, read without ever starting one. */
export const terminalServerOverview = () =>
  invoke<TerminalServerOverview>("terminal_server_overview");
/** SV-7: stop the terminal server after the person confirmed ending exactly
 *  `expected`'s sessions; rejects with a {@link RestartTerminalServerError}. */
export const stopTerminalServer = (expected: LocalActivityUpgrade) =>
  invoke<StoppedTerminalServer>("stop_terminal_server", { expected });
/** SV-7: start the terminal server when none runs. */
export const startTerminalServer = () => invoke<TerminalServerOverview>("start_terminal_server");
/** SV-7: restart a running terminal server (any build) after the person
 *  confirmed ending exactly `expected`'s sessions; rejects with a
 *  {@link RestartTerminalServerError}. */
export const replaceTerminalServer = (expected: LocalActivityUpgrade) =>
  invoke<number>("replace_terminal_server", { expected });
/** SV-7: end a session no connected app holds. */
export const endUnownedTerminalSession = (session: string) =>
  invoke<void>("end_unowned_terminal_session", { session });

/** SV-8: show (or remove at once) this instance's terminal server in the macOS
 *  menu bar. Pushed from `useTerminalServerMenuBarSetting`; a no-op off macOS. */
export const setTerminalServerMenuBar = (enabled: boolean) =>
  invoke<void>("set_terminal_server_menu_bar", { enabled });

/** SW-AU-34 (AC-5): push the `quitOnClose` preference into Rust so the window-close
 *  handler can decide hide-to-background vs. quit with a lock-free atomic read (no IPC
 *  on the close path). `false` = background-on-close (default). Fire-and-forget from
 *  `useQuitOnCloseSetting`. */
export const setQuitOnClose = (quitOnClose: boolean) =>
  invoke<void>("set_quit_on_close", { quitOnClose });

/** SW-XS-27: what an unsaved team edit is. */
export type TeamFileEditKind = "changed" | "new" | "removed" | "conflict" | "cannotPublish";

/** SW-XS-27: where publishing a file would send it. */
export type TeamFileEditDestination =
  | { kind: "team"; name: string }
  | { kind: "parentTeam"; name: string }
  | { kind: "private" }
  | { kind: "repo" }
  | { kind: "other"; label: string };

/** SW-XS-27: one team file changed in a workspace and not back in the team. */
export interface TeamFileEdit {
  /** Workspace-relative. */
  path: string;
  kind: TeamFileEditKind;
  destination: TeamFileEditDestination;
  /** When the workspace file last changed (This Mac), or when CodeSense first saw
   *  the edit (a machine). */
  changedAtMs?: number;
  /** A conflict's or a can't-publish reason, in the engine's words. */
  reason?: string;
  /** The team file it publishes to: an absolute path on This Mac (team content
   *  always lives here, also for a machine workspace). View reads it. */
  teamPath: string;
  /** `blake3:<hex>` of the workspace bytes as counted (absent: not read). An action
   *  passes it back so a file rewritten since is left alone. */
  workspaceHash?: string;
  /** `blake3:<hex>` of the team file a publish expects. */
  teamHash?: string;
  /** `size:mtime_ns` of a new file on This Mac listed without being read: an action
   *  passes it back in place of a hash. */
  workspaceStat?: string;
  /** A `cannotPublish` edit Discard can still settle. */
  discardable?: boolean;
}

/** SW-XS-27: how a workspace's count stands. */
export type TeamFileEditsState =
  | "checked"
  | "notChecked"
  | "missing"
  | "offline"
  | "unfinishedCopy"
  | "unknown";

/** SW-XS-27: one workspace's unsaved team edits. */
export interface TeamFileEditsWorkspace {
  target:
    | { kind: "local"; codeRepoPath: string }
    | { kind: "remote"; machineId: string; workspaceHandle: string };
  name: string;
  state: TeamFileEditsState;
  reason?: string;
  counts?: {
    changed: number;
    new: number;
    removed: number;
    conflicts: number;
    cannotPublish: number;
  };
  files?: TeamFileEdit[];
  lastChangedAtMs?: number;
  checkedAtMs?: number;
  /** A machine workspace: whether its machine is connected now. */
  machineOnline?: boolean;
  /** The machine’s display name. */
  machineName?: string;
  /** A machine workspace: its directory on the machine. */
  cwd?: string;
  /** Counted on this read (else the last count, as old as `checkedAtMs`). */
  fresh?: boolean;
}

/** SW-XS-27: the active team's unsaved team edits (`get_sync_status.unpublished`). */
export interface TeamFileEditsStatus {
  total: number;
  cleanWorkspaces: number;
  workspaces: TeamFileEditsWorkspace[];
  /** The machine workspaces couldn't be listed (the local ones still are). */
  remoteError?: string;
  /** Every machine workspace of the team, clean ones too (so it can be counted
   *  again). */
  machineWorkspaces?: {
    target: { kind: "remote"; machineId: string; workspaceHandle: string };
    name: string;
    machineOnline: boolean;
    checkedAtMs?: number | null;
  }[];
}

/** SW-XS-27: a workspace the app asks an explicit operation for, as itself (no
 *  agent session): one of the active team's workspaces on This Mac, or one of its
 *  machine workspaces (open in the viewer or not). */
export type AppWorkspaceTarget =
  | { kind: "local"; codeRepoPath: string }
  | { kind: "remote"; machineId: string; workspaceHandle: string };

/** SW-XS-27: what the person saw of a file in the list (its hashes): an action
 *  leaves alone a file that no longer matches. */
export type SeenTeamFile = { workspaceHash?: string; teamHash?: string; workspaceStat?: string };

/** SW-XS-27: the active team's unsaved team edits — "unsaved changes" for team
 *  files edited inside workspaces. Reads only. Workspaces on This Mac are counted
 *  now; a machine workspace reports its last count unless `count` names it (one
 *  read-only call; a disconnected machine is never dialled). */
export const teamFileEditsStatus = (
  count?:
    | { kind: "local"; codeRepoPath: string }
    | { kind: "remote"; machineId: string; workspaceHandle: string },
) => invoke<TeamFileEditsStatus>("team_file_edits_status", count ? { count } : {});

export type ForgetMissingWorkspaceResult =
  | { status: "removed" }
  | {
      status: "refused";
      state: TeamFileEditsState;
      reason: string;
      edits?: Partial<TeamFileEditsWorkspace>;
    };

export const forgetMissingWorkspace = (target: { machineId: string; workspaceHandle: string }) =>
  invoke<ForgetMissingWorkspaceResult>("forget_missing_workspace", {
    target: { machineId: target.machineId, workspaceHandle: target.workspaceHandle },
  });

/** SW-XS-27: Publish exactly `paths` of a workspace to the team — the explicit
 *  publish (`reverse_sync_worktree`'s engine), requested by the app. A file the team
 *  changed too, or that changed since it was listed (`seen`), is refused, never
 *  overwritten. `includeDeletes` publishes a removal (only for a file removed here).
 *  Its progress lights the operation chip. */
export const publishContextEdits = (
  target: AppWorkspaceTarget,
  paths: string[],
  includeDeletes = false,
  seen?: Record<string, SeenTeamFile>,
) =>
  invoke<ContextOperationResult & { entries?: unknown[]; conflicts?: unknown[] }>(
    "publish_context_edits",
    { target, paths, includeDeletes, ...(seen ? { seen } : {}) },
  );

/** SW-XS-27: Discard exactly `paths` of a workspace, requested by the app: a copied
 *  file gets the team's version back, a new file is deleted. A file that changed
 *  since it was listed (`seen`) keeps its edit. */
export const discardTeamFileEdits = (
  target: AppWorkspaceTarget,
  paths: string[],
  seen?: Record<string, SeenTeamFile>,
) =>
  invoke<ContextOperationResult>("discard_team_file_edits", {
    target,
    paths,
    ...(seen ? { seen } : {}),
  });

/** SW-XS-10: Stop a running explicit context operation (hydrate_worktree,
 *  hydrate_story, reverse_sync_worktree) from its chip — the cancel the
 *  `cancel_context_operation` MCP tool asks for. `true` when it was running
 *  (its `cancelled` progress event follows); `false` when it had ended. */
export const cancelContextOperation = (operationId: string) =>
  invoke<boolean>("cancel_context_operation", { operationId });

/** The workspace an explicit context operation names: a local worktree (with a
 *  live tab session there, its admission) or a machine's workspace handle. */
export type ContextOperationTarget =
  | { kind: "local"; codeRepoPath: string; sessionId: string }
  | { kind: "remote"; machineId: string; workspaceHandle: string };

/** What a hydrate returns (the fields the UI reads). */
export interface ContextOperationResult {
  operationId?: string;
  status?: string;
  written?: number;
  /** Already the team's version: nothing to replace. */
  unchanged?: number;
  /** Named, but no longer in team content: nothing to replace them with. */
  notInTeam?: string[];
  /** Still kept because they were modified (the first 50). */
  keptModified?: string[];
  keptModifiedCount?: number;
  failedCount?: number;
  failed?: [string, string][];
  note?: string;
  /** SW-XS-27: new files a Discard deleted from the workspace. */
  removedNew?: string[];
}

/** SW-XS-10 (SPEC §11.2): "Discard my edits" — replace exactly `paths` (files a
 *  run kept because they were modified) with the team's version:
 *  `hydrate_worktree {paths, overwriteModified:true}`. Refused without paths, so
 *  it never runs unscoped. Its progress lights the operation chip. */
export const discardContextEdits = (
  target: ContextOperationTarget,
  paths: string[],
  operationId?: string,
) =>
  invoke<ContextOperationResult>("discard_context_edits", {
    target,
    paths,
    ...(operationId ? { operationId } : {}),
  });

/** SW-XS-27: a worktree's team-file edits not back in the team (copied files
 *  edited, new files, removals, conflicts) — what removing it would throw away. */
export interface WorktreeUnpublishedContext {
  count: number;
  files: string[];
}

/** SW-XS-27: the team-file edits a worktree holds, so a removal can say what it
 *  would throw away. Rejects with a string error when they can't be counted. */
export const getWorktreeUnpublishedContext = (codeRepoPath: string) =>
  invoke<WorktreeUnpublishedContext>("worktree_unpublished_context", { codeRepoPath });

/**
 * The host emulator's terminal a display is built from: written into a fresh
 * xterm of `cols × rows`, `vt` reproduces it exactly as of stream offset `seq`
 * and the `resize_gen`-th resize. The stream then carries output with
 * `seq > snapshot.seq` and resizes with `resize_gen > snapshot.resize_gen`.
 */
export interface TerminalSnapshot {
  cols: number;
  rows: number;
  /** Byte offset of the last output the snapshot holds. */
  seq: number;
  /** Resizes applied up to `seq`. */
  resize_gen: number;
  /** The session's scrollback-lines setting. */
  scrollback: number;
  vt: string;
}

/**
 * PT-1 viewport subscription. `subscribe_terminal` returns the emulator's
 * {@link TerminalSnapshot} plus a `stream_id`; live output and resizes then
 * arrive, in order, on the `pty-output-stream-{stream_id}` event
 * ({@link TerminalStreamEvent}). Fields are snake_case to match the Rust payload.
 */
export interface TerminalSubscription {
  /** Backend presentation stamp for a private subscription. */
  privateAuthorityEpoch?: string | null;
  snapshot: TerminalSnapshot;
  /** Live-stream identifier; pass to `unsubscribeTerminal` on unmount. */
  stream_id: number;
  story_id: string | null;
  workspace_id: string | null;
  epic_id: string | null;
}

/** The session's PTY changed size: the output before it was written at the
 *  old size. `at_seq` is the stream offset then (it consumes none);
 *  `resize_gen` counts the session's size changes, one per event. */
export interface TerminalStreamResize {
  at_seq: number;
  resize_gen: number;
  cols: number;
  rows: number;
}

/** One `pty-output-stream-{id}` event: output ending at byte offset `seq`, or a
 *  resize. */
export type TerminalStreamEvent = (
  | { data: string; seq: number; resized?: undefined }
  | { resized: TerminalStreamResize; data?: undefined; seq?: undefined }
) & { privateAuthorityEpoch?: string | null };

/** What {@link readTerminalText} reads: the last lines of the normal buffer (soft-wrapped
 *  rows joined), or the rows on screen in the active buffer. */
export type TerminalTextMode = "normal" | "screen";

/** A session's emulator terminal as plain text, as of stream offset `seq`. */
export interface TerminalText {
  text: string;
  seq: number;
  alt_screen: boolean;
}

/** The last `lines` lines of a session's emulator terminal as text. */
export const readTerminalText = (
  ptyId: RuntimeSessionReference,
  lines: number,
  mode: TerminalTextMode,
) => invoke<TerminalText>("read_terminal_text", { ptyId, lines, mode });

/** Reserve a viewport stream id before subscribing, so a listener on
 *  `pty-output-stream-{id}` is up before the first byte can be sent. Released by
 *  `unsubscribeTerminal` if never bound. */
export const reserveTerminalStream = () => invoke<number>("reserve_terminal_stream");

/** `stream`: an id from {@link reserveTerminalStream} already being listened on.
 *  Without it the subscription gets a fresh id. */
export const subscribeTerminal = (ptyId: RuntimeSessionReference, stream?: number) =>
  invoke<TerminalSubscription>("subscribe_terminal", { ptyId, stream });

export const unsubscribeTerminal = (streamId: number) =>
  invoke<void>("unsubscribe_terminal", { streamId });

export const sessionIsIdle = (ptyId: RuntimeSessionReference) =>
  invoke<boolean>("session_is_idle", { ptyId });

/** What a shell is running: its foreground job (the terminal's foreground process
 *  group is not the shell's) and its background jobs, prompt helpers excluded.
 *  Both empty at a bare prompt. */
export interface RunningJobs {
  foreground: string | null;
  background: string[];
}

/** Each session's {@link RunningJobs}, in `ptyIds` order (a gone session reads
 *  idle). One process scan for the whole batch; works for sidecar-hosted sessions. */
export const sessionsRunningProcesses = (ptyIds: RuntimeSessionReference[]) =>
  invoke<RunningJobs[]>("sessions_running_processes", { ptyIds });

/** SW-TP CAP-11 / CAP-13: the working directory `sessionId` last reported, as a
 *  display-ready string. Lazy-on-demand: callers invoke this when the per-tab
 *  toolbar mounts or an action needs the cwd — never in a background loop. `null`
 *  when the host doesn't yet know the cwd (e.g. a fresh session with no OSC 7
 *  signal and no cwd probe yet). */
export const sessionCwd = (sessionId: string) =>
  invoke<string | null>("session_cwd_cmd", { sessionId });

/** A checkout's state as the terminal caption's prompt segment shows it
 *  (`git_prompt.rs`). Counts are of paths. */
export interface GitPromptStatus {
  /** The checked-out branch; null on a detached HEAD. */
  branch: string | null;
  detached: boolean;
  /** The HEAD commit (full hex); null before the first commit. */
  sha: string | null;
  upstream: string | null;
  staged: number;
  modified: number;
  untracked: number;
  conflicted: number;
  ahead: number;
  behind: number;
}

/** A session's prompt segment, read on the host that runs it. */
export interface SessionPrompt {
  cwd: string | null;
  /** The host account's home, so the caption can say `~`. */
  home: string | null;
  /** Null outside a Git checkout (or from a remote server too old to say). */
  git: GitPromptStatus | null;
}

/** The terminal caption's prompt segment for a session — local or a remote
 *  Machine's (pass its admitted target). Each host caches per path. */
export const sessionPrompt = (ptyId: RuntimeSessionReference) =>
  invoke<SessionPrompt>("session_prompt_cmd", { ptyId });

/** SW-GS-2a (AC-12): true agent-process liveness probe.
 *  Unlike {@link sessionIsIdle} (process-tree idle — reads idle for BOTH a live
 *  agent at its prompt AND a shell with no agent child), this returns whether
 *  the named agent process (e.g. the resolved `claude` basename) is alive in
 *  the session's subtree. `agentName` is a parameter so every agent surface
 *  reuses it. SW-WA-F10: for an SW-WA-F10 managed launch the agent is PID-1 of
 *  the PTY, so the two signals collapse to "session alive"; the "no agent
 *  child" state is reachable only for a mixed-mode tab (user typed `claude`
 *  by hand into a plain shell). */
export const sessionHasLiveAgent = (ptyId: RuntimeSessionReference, agentName: string) =>
  invoke<boolean>("session_has_live_agent", { ptyId, agentName });

/** SW-FX-52 (AC-3): batched agent liveness — the same signal as
 *  {@link sessionHasLiveAgent} for MANY sessions in ONE call sharing ONE
 *  process-table snapshot. The single consolidated liveness poller calls this
 *  once per tick with every registered session, collapsing N per-session probes
 *  (N full process scans/tick) into one. Results come back in query order, one
 *  per query, and that order is part of the contract: one session can be probed
 *  under two agent names, so the echoed `ptyId` alone cannot say which query a
 *  result answers. Callers pair `results[i]` with `queries[i]`. */
export const sessionsHaveLiveAgent = (queries: SessionLivenessQuery[]) =>
  invoke<SessionLivenessResult[]>("sessions_have_live_agent", { queries });

export interface SessionLivenessQuery {
  ptyId: string;
  target?: RuntimeSessionTarget;
  agentName: string;
  /** Also return the session's output sequence + trailing output. */
  tail?: boolean;
}

export interface SessionLivenessResult {
  ptyId: string;
  /** Null means the owning host could not provide evidence. */
  alive: boolean | null;
  /** Present when the query asked for `tail` and the session exists. */
  output?: { seq: number; tail: string };
}

/** How an agent CLI raises needs-you (its built-in adapter's `signaling.attention`). */
export type AgentAttention = "hooks" | "mcp" | "idle-output";

export interface AgentSignalInfo {
  /** The process basename the liveness probe matches. */
  basename: string;
  attention: AgentAttention | null;
  /** Quiet window for `idle-output` attention. */
  quietSecs: number;
}

/** The built-in agent CLIs (Claude, Codex) the terminal-tab dot floor probes for. */
export const listAgentSignals = () => invoke<AgentSignalInfo[]>("list_agent_signals");

// === File index ===

export interface ContextFile {
  name: string;
  relativePath: string;
  fullPath: string;
}

export const listContextFiles = (rootPath: string) =>
  invoke<ContextFile[]>("list_context_files", { rootPath });

// === Tool detection ===

export interface Tool {
  id: string;
  label: string;
}

export interface DetectedTools {
  terminals: Tool[];
  editors: Tool[];
  default_terminal: string;
  default_editor: string;
}

export const detectTools = () => invoke<DetectedTools>("detect_tools");

export const openTerminal = (
  terminal: string,
  customCmd: string,
  repoPath: string,
  runCommand: string = "",
) => invoke<void>("open_terminal", { terminal, customCmd, repoPath, runCommand });

export const openEditor = (editor: string, customCmd: string, repoPath: string) =>
  invoke<void>("open_editor", { editor, customCmd, repoPath });

// === File operations ===

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  extension: string | null;
  /** File size in bytes; `null` for directories (and unreadable metadata).
   *  SLW-WDE-1 §B: lets the in-app editor gate an oversized file to a
   *  placeholder before reading it. */
  size: number | null;
}

export const checkPathExists = (path: string) => invoke<boolean>("check_path_exists", { path });

export const readFile = (path: string) => invoke<string>("read_file", { path });

/** SW-DX-11: one child of a `readWorkspace` directory listing. */
export interface WorkspaceDirEntry {
  name: string;
  isDir: boolean;
  /** File byte size; `0` for a directory. */
  size: number;
}

/** SW-DX-11: `readWorkspace` result — EITHER file `content` OR a dir `entries`
 *  listing (never both; the FE branches on which field is present). */
export interface WorkspaceReadResult {
  content?: string;
  entries?: WorkspaceDirEntry[];
}

/**
 * SW-DX-11: the `readWorkspace` bridge verb's command — a doc-scoped file
 * read / dir-list confined to `docPath`'s OWN directory subtree on a managed
 * surface (Rust is the wall). `allowedExts` is supplied by the TRUSTED bridge
 * entry, never the doc. Rejects are typed: `traversal:`, `unmanaged:`,
 * `unsupported-type:`, `too-large:`, `binary:`, `not-found:`.
 */
export const readWorkspaceFile = (
  docPath: string,
  relPath: string,
  allowedExts: string[],
  frameId?: string,
) => invoke<WorkspaceReadResult>("read_workspace_file", { docPath, relPath, allowedExts, frameId });

/**
 * Atomically write UTF-8 text back to a context-repo file (tmp + fsync +
 * rename). Sandboxed to `~/.codesense/users/` / the app contexts dir by
 * the same `validate_context_path` gate as `readFile`. The general
 * editable-content writer — the Excalidraw canvas save is its first user.
 */
/** Pass `expectedSha256` (the SHA-256 of the text the editor's buffer is based
 *  on, or `"absent"`) and a file that changed on disk since is refused with an
 *  error starting `conflict:` instead of being overwritten. */
export const writeContentFile = (path: string, content: string, expectedSha256?: string) =>
  invoke<void>("write_content_file", { path, content, expectedSha256 });

/**
 * SLW-WDE-2: atomically write UTF-8 text to a **worktree** file (tmp + fsync +
 * rename), sandboxed by the same `validate_context_path` gate as {@link readFile}
 * but **NOT** watcher-suppressed — the editable counterpart to
 * {@link writeContentFile}. The in-app code editor's ⌘S save is its caller.
 * The non-suppression is deliberate: a worktree edit must stay visible to
 * `git status` and to the worktree watcher (WDE-3), unlike a context-repo write
 * whose watcher fan-out we suppress. Pass `expectedSha256` (the hash of the
 * content the buffer was loaded from) and a file that changed on disk since is
 * refused with an error starting `conflict:` instead of being overwritten.
 */
export const writeWorktreeFile = (path: string, content: string, expectedSha256?: string) =>
  invoke<void>("write_worktree_file", { path, content, expectedSha256 });

/** Prefix of the `writeWorktreeFile` error for a save whose baseline moved. */
export const WORKTREE_WRITE_CONFLICT_PREFIX = "conflict:";

/** A local worktree file's committed (`HEAD`) text, for the editor's change
 *  gutter; `null` for a new/untracked file or a path outside any repository. */
export const worktreeFileHead = (path: string) =>
  invoke<string | null>("worktree_file_head", { path });

/** Quick-open's file list: tracked plus untracked-not-ignored paths, relative
 *  to the worktree root. */
export interface WorktreeFileList {
  paths: string[];
  truncated: boolean;
  /** Checked-out branch (short commit when detached); `null` when unknown. */
  branch: string | null;
}

/** Quick-open's file list for a local worktree root. */
export const worktreeListFiles = (root: string) =>
  invoke<WorktreeFileList>("worktree_list_files", { root });

/**
 * Atomically write base64-decoded binary bytes back to a context-repo file —
 * the binary companion to {@link writeContentFile} (same sandbox + atomic +
 * watcher-suppressed write). SW-SC-2's Excalidraw PNG render is its first
 * caller. Distinct from `writeBinaryFile` (PDF/screenshot export to a
 * user-picked path outside the watched tree, un-suppressed).
 */
export const writeBinaryContentFile = (path: string, base64: string) =>
  invoke<void>("write_binary_content_file", { path, base64 });

/**
 * SW-CP-14: delete a context-layer content file OR directory and propagate the
 * deletion across the sync boundary to every projected dest (so no stale
 * duplicate round-trips back). Git-backed: revertible. Distinct from
 * {@link deletePath} (raw, un-suppressed, non-propagating) — content deletes
 * must route through here so the projection engine cleans up the dests.
 */
export const deleteContentFile = (path: string) => invoke<void>("delete_content_file", { path });

/**
 * SW-CP-14: rename/move a context-layer content file OR directory, preserving
 * content + frontmatter, and propagate the relocate across the sync boundary
 * to every projected dest (no copy left at the old path). The content-aware,
 * propagating counterpart to {@link renamePath}.
 */
export const moveContentFile = (fromPath: string, toPath: string) =>
  invoke<void>("move_content_file", { fromPath, toPath });

/**
 * SW-SC-14: reply leg of the `render_canvas` MCP render bridge. The frontend
 * render listener calls this after rendering a canvas, routing the base64 PNG
 * bytes (or an `error`) back to the waiting Rust MCP verb keyed by `requestId`.
 * Exactly one of `pngBase64` / `error` is meaningful.
 *
 * MCP architectural reframe (2026-10): previously carried a `pngPath` after
 * the frontend wrote an `.excalidraw.png` sidecar; now carries the bytes
 * DIRECTLY so the MCP verb can embed them inline in its tool response. No
 * sidecar is written.
 */
export type RenderedDocumentCapture = { content: string; capturedAt: string; truncated: boolean };
export const admitRenderedDocument = (requestId: string, frameId?: string) =>
  invoke<void>("admit_rendered_document", { requestId, frameId });
export const resolveRenderedDocument = (
  requestId: string,
  capture: RenderedDocumentCapture | null,
  error: string | null,
) => invoke<void>("resolve_rendered_document", { requestId, capture, error });

export const resolveCanvasRender = (
  requestId: string,
  pngBase64: string | null,
  error: string | null,
) => invoke<void>("resolve_canvas_render", { requestId, pngBase64, error });

/**
 * SW-FX-131: reply leg of an `open_workspace` machine open. Called once the viewer has
 * opened (or failed to open) the story's workspace on the machine; `error` is null on
 * success, else why it did not open. Unanswered, the MCP call times out after 10 minutes.
 */
export const resolveOpenWorkspaceMachine = (requestId: string, error: string | null) =>
  invoke<void>("resolve_open_workspace_machine", { requestId, error });

/**
 * SW-AW-4: reply leg of the blocking `ask_user` MCP verb. The answer bridge
 * calls this when the user answers/dismisses a question, unblocking the waiting
 * Rust oneshot keyed by `correlationId`. `action` is `accept` | `decline` |
 * `cancel` | `handled` (settled outside the question); `content` carries the answer on
 * `accept`. A late/duplicate call (already resolved, timed out, or swept) is a
 * harmless no-op on the Rust side.
 */
export const resolveAskUser = (
  correlationId: string,
  action: "accept" | "decline" | "cancel" | "handled",
  content: string | null,
) => invoke<void>("resolve_ask_user", { correlationId, action, content });

export const listDirectory = (dirPath: string) =>
  invoke<FileEntry[]>("list_directory", { dirPath });

/** One page a team's landing page may be, relative to its content folder. */
export interface LandingCandidate {
  path: string;
  /** Match the file name in any letter case (README/index fallbacks). */
  anyCase: boolean;
}

/** Rust `team_landing::LandingPage`. */
export type LandingPageResolution =
  /** `path` is `{contentRoot}/{relative path as spelled on disk}`. */
  | { kind: "found"; path: string }
  /** The content folder is there and has none of the candidates. */
  | { kind: "none" }
  /** The content folder is missing, empty or unreadable (not cloned/synced yet). */
  | { kind: "unavailable" };

/** The first candidate that is a regular file REALLY inside `contentRoot`
 *  (symlinks resolved — one leading out never resolves), so the viewer may open
 *  it unprompted. */
export const resolveTeamLandingPage = (contentRoot: string, candidates: LandingCandidate[]) =>
  invoke<LandingPageResolution>("resolve_team_landing_page", { contentRoot, candidates });

export const createFile = (path: string, content: string, frameId?: string, pagePath?: string) =>
  invoke<void>("create_file", {
    path,
    content,
    ...(frameId ? { frameId } : {}),
    ...(pagePath ? { pagePath } : {}),
  });

/**
 * SW-VE-7: ingest a drag-dropped OS file (`srcPath`, an absolute path outside
 * the managed tree) into a validated context content path (`destPath`). The
 * byte copy happens server-side — binary- and text-safe, and non-blocking (no
 * file bytes cross the IPC boundary). Rejects folders / oversized / collisions /
 * unmanaged dests with a typed error message.
 */
export const ingestFileIntoContext = (srcPath: string, destPath: string) =>
  invoke<void>("ingest_file_into_context", { srcPath, destPath });

export const renamePath = (oldPath: string, newPath: string) =>
  invoke<void>("rename_path", { oldPath, newPath });

export const deletePath = (path: string) => invoke<void>("delete_path", { path });

// === PDF export (R.4) ===

export const exportPdfFromUrl = (url: string, outputPath: string) =>
  invoke<void>("export_pdf_from_url", { url, outputPath });

export const exportPdfFromHtml = (html: string, outputPath: string) =>
  invoke<void>("export_pdf_from_html", { html, outputPath });

export const writeBinaryFile = (base64Data: string, outputPath: string) =>
  invoke<void>("write_binary_file", { base64Data, outputPath });

// === Pipeline scanner (story 30.7) ===

import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

/**
 * PTS-2: takes `teamContentPath` directly; `viewerRoot` retired.
 * SW-EW-32: `summaryHeadings` is the configured ordered list the epic-banner
 * summary keys off (first match wins); omit/empty falls back to the backend
 * defaults (`["Purpose", "Story"]`).
 */
export const scanPipelineTree = (
  teamContentPath: string,
  pipelineRoot: string,
  scopePath: string,
  mainDoc: string,
  summaryHeadings?: readonly string[],
  // SLW-MD-1: the active team's CONFIGURED source-repo slugs + configured team
  // names, resolved once from the loaded org config (locked decision #5) — drive
  // the nav's invalid-metadata badge. Omit / empty configured set ⇒ source_repo
  // membership badging disabled (a well-formed `source_repo` is checked
  // syntactic-only, never flagged on membership; a malformed slug is still
  // flagged). No drift for callers that don't pass them.
  configuredSourceRepos?: readonly string[],
  validTeams?: readonly string[],
) =>
  invoke<DerivedPipelineTree>("scan_pipeline_tree", {
    teamContentPath,
    pipelineRoot,
    scopePath,
    mainDoc,
    summaryHeadings: summaryHeadings ?? null,
    configuredSourceRepos: configuredSourceRepos ?? null,
    validTeams: validTeams ?? null,
  });

/**
 * R-14: On-demand fetch for body-extracted fields (H1 title + `## Story`
 * section). The scanner no longer carries these on `StoryRef` — consumers
 * call this through `useStoryDetails` (which caches client-side with
 * watcher-driven invalidation).
 */
export interface StoryDetails {
  title: string | null;
  storyText: string | null;
  /** SW-LN-34: frontmatter `title:` / HTML `cs:title`; cards prefer it over `title`.
   *  Always sent by the backend; optional so fixtures that predate it stay valid. */
  titleMeta?: string | null;
}

/**
 * SW-EW-32: `summaryHeadings` is the configured ordered list the summary keys
 * off (first match wins); omit/empty falls back to the backend defaults.
 */
export const getStoryDetails = (filePath: string, summaryHeadings?: readonly string[]) =>
  invoke<StoryDetails>("get_story_details", {
    filePath,
    summaryHeadings: summaryHeadings ?? null,
  });

/**
 * Move a story to `status`. SW-SB-22: `rank` (optional) sets the board-ordering rank
 * atomically alongside status — a cross-column drop writes both in one cycle. Omit for a
 * status-only write.
 */
export const updatePipelineStatus = (filePath: string, status: string, rank?: string) =>
  invoke<void>("update_pipeline_status", { filePath, status, rank: rank ?? null });

/** PTS-2: takes `teamContentPath` directly; `viewerRoot` retired. */
export const findNearestValidScope = (
  teamContentPath: string,
  pipelineRoot: string,
  scopePath: string,
) =>
  invoke<string>("find_nearest_valid_scope", {
    teamContentPath,
    pipelineRoot,
    scopePath,
  });

// === Worktree discovery + validation (story 29.2) ===

import type { WorktreeConfig } from "@/shared/types/worktree-config";

export interface WorktreeInfo {
  path: string;
  branch: string | null;
  head_sha: string;
}

/**
 * SLW-WL-2: one codesense-managed worktree as the managed-worktree roster sees
 * it — the enriched management view (`list_managed_worktrees`). camelCase, unlike
 * the raw snake_case {@link WorktreeInfo}: this is the durable, SDK-shaped contract
 * a future dashboard consumes via `codesense.invoke`. `branch`/`dirty`/`repoPath`
 * are `null` when the worktree can't be read on disk (the row is still listed).
 * `syncStatus` is kebab-case (`"syncing" | "synced" | "failed" | "pending"`).
 */
/** SW-WU-13: the three unified worktree kinds — the discriminant the panel partitions
 *  and groups on (mirrors Rust `WorktreeKind`). */
export type WorktreeKind = "story" | "epicIntegration" | "orphan" | "context";

/** SW-WU-13: git facts every unified [`WorktreeRow`] carries, regardless of kind. */
interface WorktreeRowBase {
  kind: WorktreeKind;
  /** Grouping key the panel folds on: `storyId` (story), `epicId` (epic), or the
   *  parent-clone path (orphan). Realizes "a workspace owns 1..N worktrees" as data. */
  workspaceKey: string;
  path: string;
  branch: string | null;
  dirty: boolean | null;
  /** Parent repo for `git worktree remove`; `null` ⇒ vanished/detached. */
  repoPath: string | null;
  liveCliSession: boolean;
}

/** SW-WU-13 `kind="story"`: a codesense-managed, story-linked worktree — the full
 *  affordance set. (Was `ManagedWorktreeInfo`; now a variant of the unified
 *  [`WorktreeRow`], carrying `kind` + `workspaceKey` on top of the story fields.) */
export interface ManagedWorktreeInfo extends WorktreeRowBase {
  kind: "story";
  storyId: string;
  sourceRepo: string;
  syncStatus: string;
  failureReason: string | null;
  /** Story file this worktree works — the `storyFilePath` for `openOrFocusStoryWorkspace`. */
  storySourcePath: string;
  /** Org slug; with `team`, builds the workspace `teamKey` for the Open action. */
  orgSlug: string;
  /** Team the story resolves to (`_team/<team>/` segment), or `null` (neutral story). */
  team: string | null;
  /** SW-WM-3 (AC-1/AC-4): is this worktree's git/registry linkage broken? `true` when
   *  the parent working copy has vanished (`repoPath == null`) or the backing story file
   *  is gone. Drives the "Orphaned" dot + the one-click Reconcile action. */
  orphaned: boolean;
  /** RF-perf-24 (AC-5): unix-ms of the last reverse-sync that promoted ≥1 dest edit
   *  back into the content layer, or `null` before this worktree ever reverse-synced.
   *  Drives the row's "Synced by … · <ago>" sub-line (relative time formatted FE-side). */
  lastReverseSyncAt: number | null;
  /** RF-perf-24 (AC-5): who triggered `lastReverseSyncAt` — `"auto"` (ambient / catch-up),
   *  `"user"` ("Sync now"), or `"agent"` (MCP flush) — or `null` before the first sync. */
  lastReverseSyncBy: "auto" | "user" | "agent" | null;
  /** SV-CDE-7: whether git-ai line-attribution capture was working at the last agent
   *  launch here; `null` (or absent) before any launch. */
  attribution?: AttributionState | null;
}

/** Why line-attribution capture is off for a worktree. `notes_would_push`: the git-ai in
 *  use cannot keep this repo's notes local, including an old managed daemon awaiting
 *  restart before capture can use the pinned build. */
export type AttributionOffReason =
  | "disabled_for_repo"
  | "notes_would_push"
  | "not_managed_worktree"
  | "repo_not_attached";

export type AttributionStatus =
  | { kind: "active" }
  | { kind: "off"; reason: AttributionOffReason }
  | { kind: "not_installed" }
  | { kind: "installing" }
  /** Includes bundle_missing, sha_mismatch or version_mismatch; survives app restart
   *  until a successful retry. Restart or explicit attribution:on requests a retry. */
  | { kind: "install_failed"; message: string }
  | { kind: "unsupported_version"; found: string }
  /** Known reasons include CLI/platform limits, newer (own numeric core is newer)
   *  and user_pinned (manual restore; clear with save_source_repo_attachment
   *  {repoPath: absolute checkout path, attribution: "on"}). */
  | { kind: "unsupported"; reason: string }
  | { kind: "error"; message: string };

export interface AttributionState {
  status: AttributionStatus;
  gitAiVersion: string | null;
  /** Unix milliseconds the launch computed it at. */
  checkedAt: number;
}

/** Persisted replacement facts, also visible after the app restarts. */
export interface AttributionPreviousBinary {
  version: string;
  replacement: string;
  /** Recorded previous-copy path; a manual mv revert may have moved that file. */
  path: string;
}

/** Last completed checkpoint import pass (startup sweep or watcher clone pass), retained per
 *  home since app start. watchErrors is the latest watch reconciliation's registration
 *  failures; lastError is the latest still-unresolved error. */
export interface AttributionCapture {
  lastSweepAt: number | null;
  trigger: "startup" | "watcher" | null;
  clonesSwept: number;
  logsSeen: number;
  logsNew: number;
  traceRowsStored: number;
  sessionMappingRowsStored: number;
  watchErrors: number;
  lastError: string | null;
}

/** describe_setup's data.attribution: machine-level installation/daemon status.
 *  A worktree's attribution field separately records its last launch decision. */
export interface AttributionDoctor {
  status: AttributionStatus;
  binary: string | null;
  gitAiVersion: string | null;
  daemonReachable: boolean;
  /** Exact accepted raw pin, including the fork suffix. */
  supportedVersions: string;
  previousBinary: AttributionPreviousBinary | null;
  capture: AttributionCapture;
  /** The notes ref capture writes (`sense`); its notes stay on this Mac. */
  notesRef: string;
  /** Remote keys of repos set `notes: "push"`; every other repo's notes stay local. */
  notesPushRepos: string[];
  /** How many attached clones CodeSense lists in git-ai's `daemon_repo_roots`; 0 = no list
   *  (every repo); null while none resolves and the list written before is kept. */
  daemonRepoRoots: number | null;
  /** Includes a durable one-time replacement notice (acknowledged in its receipt on
   *  first read), restart pending, user_copy_present, user_hooks_present, installation
   *  failure, or the MCP recovery call for user_pinned. */
  warnings: string[];
}

export interface CommandResult {
  command: string;
  passed: boolean;
  stdout: string;
  stderr: string;
  exit_code: number | null;
  phase: "setup" | "validate" | "cleanup";
  step_index: number;
}

export interface WorktreeRunResult {
  results: CommandResult[];
  exit_code: number;
}

/**
 * Three-state status of a repo's worktree config file. Lets the settings
 * pane distinguish "no config yet — go run discovery" (Missing) from
 * "config exists but is broken — fix it" (Invalid) without collapsing both
 * into a silent disabled button.
 */
export type WorktreeConfigStatus =
  | { kind: "missing" }
  | { kind: "present" }
  | { kind: "invalid"; error: string };

/**
 * SW-SO-2 (③): fused worktree.yaml probe — the three-state `status` AND the
 * parsed `config` in ONE backend read. Replaces the `hasWorktreeConfig` +
 * `getWorktreeConfig` pair the story-open path used back-to-back (`config` is
 * non-null only when `status.kind === "present"`). `getWorktreeConfig` survives
 * for its other callers. SS.8 path resolution (new
 * `{orgRoot}/_codesense/worktree/{owner}/{repo}.yaml`, legacy fallback) is
 * unchanged.
 */
export interface WorktreeConfigProbe {
  status: WorktreeConfigStatus;
  config: WorktreeConfig | null;
}

export const probeWorktreeConfig = (repoPath: string, orgRoot: string) =>
  invoke<WorktreeConfigProbe>("probe_worktree_config", { repoPath, orgRoot });

/**
 * Load and return the parsed `WorktreeConfig` for a repo. Resolves to
 * `null` when the file is missing; rejects when the file is present but
 * malformed. Config-only counterpart to `probeWorktreeConfig` (which returns
 * status + config in one read for the open path); this bare loader backs the
 * callers that only need the `WorktreeConfig` (source-diff, setup, default
 * start-point).
 *
 * `orgRoot` plumbing matches `probeWorktreeConfig` (SS.8).
 */
export const getWorktreeConfig = (repoPath: string, orgRoot: string) =>
  invoke<WorktreeConfig | null>("get_worktree_config", { repoPath, orgRoot });

/** SLW-NC-7 (AC-1): add or remove a TEAM nav pin (a git-shared shortcut to a nav
 *  path — dir or file — pinned to the top of the left nav for the whole team).
 *  Persists to the `pin` list in the team's git-shared `designations.yml`.
 *  `path` is repo-relative. Rejects on write failure so the caller can revert the
 *  optimistic state + toast (AC-10). */
export const setTeamNavPin = (orgRoot: string, team: string, path: string, pinned: boolean) =>
  invoke<void>("set_team_nav_pin", { orgRoot, team, path, pinned });

/** SLW-NC-7 (AC-8): the team's git-shared nav pins (repo-relative paths). Never
 *  rejects — a missing/malformed store ⇒ an empty list. Re-fetch on the NC-5
 *  composite-refresh nonce so a teammate's pulled pin appears without a restart. */
export const getTeamNavPins = (orgRoot: string, team: string) =>
  invoke<string[]>("get_team_nav_pins", { orgRoot, team });

export const createWorktree = (repoPath: string, branchName: string, worktreePath: string) =>
  invoke<string>("create_worktree", { repoPath, branchName, worktreePath });

export interface FetchAndCreateResult {
  worktree_path: string;
  from_remote: boolean;
}

/** Open a story branch in a managed worktree from local refs. A new branch starts at
 *  `startPoint`, used verbatim; `null` leaves the base to the backend, which resolves it
 *  only when the branch is new: the worktree.yaml `defaultStartPoint` (a branch name as
 *  its cached `origin/` ref when that exists), else the repo's default branch, rejecting,
 *  naming every ref it tried, when none resolves. */
export const fetchAndCreateWorktree = (
  repoPath: string,
  branchName: string,
  worktreePath: string,
  startPoint: string | null,
  defaultStartPoint: string | null,
) =>
  invoke<FetchAndCreateResult>("fetch_and_create_worktree", {
    repoPath,
    branchName,
    worktreePath,
    startPoint,
    defaultStartPoint,
  });

/**
 * Remove a worktree. Two SEPARATE consents (SW-XS-4): `force` discards uncommitted
 * CODE (`git worktree remove --force`); `discardContext` discards unpublished
 * context edits (ignored, managed files). One never implies the other.
 */
export const removeWorktree = (
  repoPath: string,
  worktreePath: string,
  force?: boolean,
  discardContext?: boolean,
) => invoke<void>("remove_worktree", { repoPath, worktreePath, force, discardContext });

/**
 * SW-WB-1: migrate-in-place reconcile — switch an EXISTING worktree onto
 * `branchName`, carrying uncommitted changes across (preserves WIP + session).
 * Never-clobber: rejects (typed error) if the switch would overwrite local
 * modifications or the branch is checked out elsewhere — the caller can then
 * offer recreate.
 */
export const switchWorktreeBranch = (repoPath: string, worktreePath: string, branchName: string) =>
  invoke<FetchAndCreateResult>("switch_worktree_branch", { repoPath, worktreePath, branchName });

// ---------- SW-EW-6: integration-worktree assembly + teardown ----------

export interface AssembledWorktree {
  owner: string;
  repo: string;
  path: string;
  branch: string;
  createdBranch: boolean;
  alreadyExisted: boolean;
  warnings: string[];
}

export interface WorktreeFailure {
  owner: string;
  repo: string;
  error: string;
}

/** SLW-CO-8 (LD-A): a per-repo dev-env install run the epic overlay tracks to
 *  aggregate batch readiness. `runId` is the channel its streamed
 *  `worktree-run-{runId}` log lines AND terminal `worktree-run-completed` event
 *  fire on. Returned explicitly because the FE cannot reconstruct these ids —
 *  they embed the backend-derived `{parent}__{leaf}` epic id it can't know. */
export interface DevEnvRun {
  owner: string;
  repo: string;
  runId: string;
}

export interface AssembleEpicResult {
  assembled: AssembledWorktree[];
  failures: WorktreeFailure[];
  branch: string;
  /** SLW-CO-8 (LD-A): the per-repo dev-env installs. `M = devEnvRuns.length` —
   *  NOT `assembled.length` (a repo with no resolved clone gets no run; an
   *  empty-config repo still gets one and must be counted). */
  devEnvRuns: DevEnvRun[];
}

export interface EpicWorktreeParams {
  codesenseHome: string;
  githubUserId: number;
  orgSlug: string;
  team: string;
  /** Absolute path to the epic doc — the backend reads its `repos:`/`branch:`
   *  frontmatter, resolves each repo to its local clone, AND (SW-EW-15) derives
   *  the epic id (`{parent}__{leaf}`) from it. */
  epicFilePath: string;
}

/** SW-EW-6: assemble a writable integration worktree per assigned repo into
 *  the epic workspace dir, on the epic's integration branch (created off the
 *  default if absent). The backend reads `epic.repos` from the doc and
 *  resolves clones itself; bounded concurrency, per-repo failures collected
 *  (never fatal). **Non-blocking** — fire fire-and-forget after the epic
 *  opens; the reused worktree toast reports progress. */
export const assembleEpicWorktrees = (params: EpicWorktreeParams) =>
  invoke<AssembleEpicResult>("assemble_epic_worktrees", {
    codesenseHome: params.codesenseHome,
    githubUserId: params.githubUserId,
    orgSlug: params.orgSlug,
    team: params.team,
    epicFilePath: params.epicFilePath,
  });

/** SW-EW-9: read an epic doc's configured `repos:` (canonical `owner/repo`
 *  slugs) — the source of truth for the epic-node card. Read on expand; never
 *  from app state. Malformed frontmatter entries are dropped by the backend. */
export const readEpicRepos = (epicFilePath: string) =>
  invoke<string[]>("read_epic_repos", { epicFilePath });

/** SW-EW-9: replace an epic doc's `repos:` and return the re-read list (source
 *  of truth in one round-trip). Entries are validated/normalized to
 *  `owner/repo` and de-duplicated; an empty array removes the field. The write
 *  is surgical (format-preserving). Markdown epic docs only. */
export const setEpicRepos = (epicFilePath: string, repos: string[]) =>
  invoke<string[]>("set_epic_repos", { epicFilePath, repos });

export interface TeardownEpicResult {
  removed: string[];
  skippedInUse: string[];
  warnings: string[];
}

/** SW-EW-45: reconcile an epic's on-disk integration worktrees to its `repos:`
 *  frontmatter — tears down the worktree for any repo no longer assigned (e.g.
 *  after a repo-deselect). Skips any worktree an open session references
 *  (`inUsePaths` = open terminal/integration cwds), warns-and-preserves dirty
 *  worktrees, keeps the branch, and emits `worktree-changed` on removal.
 *  Fire-and-forget from the remove handler. (`team` is unused by the backend —
 *  reconcile resolves the workspace + branch from disk, so it isn't forwarded.) */
export const reconcileEpicWorktrees = (params: EpicWorktreeParams, inUsePaths: string[]) =>
  invoke<TeardownEpicResult>("reconcile_epic_worktrees", {
    codesenseHome: params.codesenseHome,
    githubUserId: params.githubUserId,
    orgSlug: params.orgSlug,
    epicFilePath: params.epicFilePath,
    inUsePaths,
  });

/** SW-TS-4 — the browse→worktree bridge. Once a worktree exists on disk,
 *  persist the worktree→story link and trigger the SW-TS-1 team-aware sync
 *  (team derived from the story's location — no team picker). Non-blocking:
 *  the backend writes the record (sub-ms) and enqueues-and-returns the
 *  derive, so the caller must NOT await this on the user's landing path —
 *  fire-and-forget with `.catch()`. Idempotent (safe to re-invoke on an
 *  existing worktree). `sourceRepo` is the `owner/repo` slug; `storyId`
 *  and `storySourcePath` identify the story being worked. */
export const establishWorktreeStoryLink = (
  codeRepoPath: string,
  storySourcePath: string,
  storyId: string,
  sourceRepo: string,
) =>
  invoke<boolean>("establish_worktree_story_link", {
    codeRepoPath,
    storySourcePath,
    storyId,
    sourceRepo,
  });

/** SW-TS-5 §4.4: fire-and-forget launch reconcile of every known worktree.
 *  Call once on mount, un-awaited — the backend offloads the iterate +
 *  re-derive loop onto a blocking task and returns immediately, so boot/UI
 *  never waits on the worktree sync. Team-agnostic (reconciles all teams'
 *  worktrees, independent of the root's current team). */
export const reconcileWorktreesOnLaunch = (codesenseHome: string, githubUserId: number) =>
  invoke<void>("reconcile_worktrees_on_launch", {
    codesenseHome,
    githubUserId,
  });

/** SLW-FX-3 (AC-1): event-driven in-session self-heal of a worktree stranded
 *  `Pending` (record written but its derive wave never enqueued — a create
 *  interrupted between the two non-atomic steps). Unlike
 *  `reconcileWorktreesOnLaunch` (unconditional, once-per-uid on mount), this is
 *  safe to fire on frequent in-session events (window focus/visibility, worktree
 *  open): the backend filters to only `Pending`/`Failed` surfaces, so it's a
 *  no-op on the healthy siblings — no wave storm. Fire-and-forget, non-blocking. */
export const reconcileStuckWorktrees = (codesenseHome: string, githubUserId: number) =>
  invoke<void>("reconcile_stuck_worktrees", {
    codesenseHome,
    githubUserId,
  });

export const checkWorktreeDirty = (worktreePath: string) =>
  invoke<{ dirty: boolean }>("check_worktree_dirty", { worktreePath });

/** How many live sessions were started inside a worktree folder (`null` when a
 *  terminal host can't be asked) — Remove worktree… refuses while one outside the
 *  workspace runs there, and waits for the ones it ended. */
export const worktreeLiveSessionCount = (worktreePath: string) =>
  invoke<number | null>("worktree_live_session_count", { worktreePath });

export const listWorktrees = (repoPath: string) =>
  invoke<WorktreeInfo[]>("list_worktrees", { repoPath });

/**
 * SW-WU-8 / SW-WU-13 `kind="epicIntegration"`: an epic INTEGRATION worktree (created by
 * `assemble_epic_worktrees` under `_epic/{org}/{epicId}/{owner}/{repo}`). NOT story-linked
 * — discovered from git, so it carries no `storyId`/`storySourcePath`/`syncStatus`, only
 * the epic leaf coordinates. A variant of the unified [`WorktreeRow`].
 */
export interface EpicWorktreeInfo extends WorktreeRowBase {
  kind: "epicIntegration";
  epicId: string;
  owner: string;
  repo: string;
}

/**
 * SW-WU-13 `kind="orphan"`: a discovered worktree under a managed clone that matched no
 * record, no unique-live story branch, and no `_epic/` shape (e.g. a hand-made or
 * CLI-agent worktree). Carries only git facts — reveal-only in the panel. A variant of
 * the unified [`WorktreeRow`].
 *
 * NOTE: this orphan *kind* is a DIFFERENT concept from `ManagedWorktreeInfo.orphaned` (the
 * story-row flag meaning a story-linked record whose parent/story file vanished → offers
 * "Reconcile"). An orphan-kind row has no record and no Reconcile affordance; never route
 * it through the story-only `reconcileOrphanedWorktree` path.
 */
export interface OrphanWorktreeInfo extends WorktreeRowBase {
  kind: "orphan";
}

/**
 * SW-WU-13: the unified Worktrees-panel row — one discriminated union spanning story,
 * epic-integration and orphan worktrees (mirrors Rust `WorktreeRow`). Partition on
 * `kind`; group on `workspaceKey`.
 */
export interface ContextWorktreeInfo extends WorktreeRowBase {
  kind: "context";
  id: string;
  name: string;
  orgSlug: string;
  team: string;
  launchCwd: string;
  baseSha: string;
  createdAt: number;
  lastUsedAt: number;
  agents: { handle: string; name: string; role: string | null }[];
}
export type WorktreeRow =
  | ManagedWorktreeInfo
  | EpicWorktreeInfo
  | OrphanWorktreeInfo
  | ContextWorktreeInfo;

/**
 * SLW-WL-9 (AC-1b/1c) / SW-WU-13: the managed-worktree list result — the ONE unified
 * `rows` (story + epic + orphan) plus how many ghost records the backend's
 * reconcile-against-disk pass reaped THIS call (the event-driven roster load ignores
 * `reaped`; the periodic-reap toast owns that signal). Mirrors Rust
 * `ManagedWorktreeListing`.
 */
export interface ManagedWorktreeListing {
  rows: WorktreeRow[];
  reaped: number;
}

/** Token counts for one scope of usage. */
export interface SessionTokens {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

/** The story a usage session was spent on. Title, status and epic come from the story file's
 *  frontmatter and are null when it does not say. */
export interface SessionUsageStory {
  id: string;
  title: string | null;
  status: string | null;
  epic: string | null;
  /** `associated` when only a context worktree's name ties the session to the story. */
  basis?: "associated";
}

/** One agent session's token use and spend, from its agent CLI's own reports (`session_read_usage`).
 *  `costUsd` is null when any of its models has no rate in the price list. */
export interface SessionUsageRow {
  /** The agent CLI the session ran in. The same session id in two CLIs is two rows. */
  cli: "claude" | "codex" | "opencode" | "hermes";
  sessionId: string;
  /** Who ran the session, recorded at launch. Null for sessions CodeSense did not launch. */
  org: string | null;
  team: string | null;
  /** The saved agent it ran, e.g. `builtin/claude`. */
  agent: string | null;
  /** The machine it ran on, as `listMachines` names it: `local` for this Mac. A session with
   *  usage from more than one machine names the machine of its latest call (at a tie, the other
   *  machine over this Mac). A removed machine keeps its id. */
  machineId: string;
  cwd: string | null;
  /** The story the session was spent on (its story worktree, else the story its launch was
   *  opened for), or null when it is not tied to one. */
  story: SessionUsageStory | null;
  models: string[];
  firstAtMs: number;
  lastAtMs: number;
  messages: number;
  tokens: SessionTokens;
  costUsd: number | null;
  unpricedModels: string[];
}

/** One local day's usage across every session and model. */
export interface SessionUsageDay {
  /** Local calendar day, `YYYY-MM-DD`. */
  day: string;
  tokens: SessionTokens;
  /** The priced usage of the day. `unpricedModels` names what it leaves out. */
  costUsd: number;
  unpricedModels: string[];
}

export interface SessionUsageReport {
  /** Newest activity first. */
  sessions: SessionUsageRow[];
  /** One entry per local day with usage, newest day first. */
  days: SessionUsageDay[];
  totals: {
    tokens: SessionTokens;
    costUsd: number;
    pricedSessions: number;
    unpricedSessions: number;
  };
  /** Current full count of matching sessions before any page cut. */
  total: number;
  /** Present only when more sessions follow; pass it back as `cursor` for the next page. */
  nextCursor?: string;
  /** Present while this Mac's managed Claude settings send session usage elsewhere: why newer
   *  Claude sessions are missing. */
  unavailableReason?: string;
}

/** A long prompt's prices for `sessionSetModelRate`: a call whose prompt (input, cache reads and
 *  cache writes) is over `fromTokens` is priced at these (USD per million tokens). */
export interface LongRateArgs {
  fromTokens: number;
  inputPerMtok: number;
  outputPerMtok: number;
  /** Omitted: 0.1x the long input price. */
  cacheReadPerMtok?: number;
}

/** `session_set_model_rate`: set a model's rate from a local day, and price the unpriced usage it
 *  covers. Resolves how many costs it changed. */
export const sessionSetModelRate = (args: {
  modelPrefix: string;
  effectiveFrom: string;
  inputPerMtok: number;
  outputPerMtok: number;
  cacheReadPerMtok?: number;
  long?: LongRateArgs;
}) =>
  invoke<number>("session_set_model_rate", {
    params: {
      modelPrefix: args.modelPrefix,
      effectiveFrom: args.effectiveFrom,
      inputPerMtok: args.inputPerMtok,
      outputPerMtok: args.outputPerMtok,
      cacheReadPerMtok: args.cacheReadPerMtok ?? null,
      long: args.long ?? null,
    },
  });

/** One stored rate (`session_read_model_rates`), USD per million tokens. The seeded list prices are
 *  stored rates too. */
export interface StoredModelRate {
  modelPrefix: string;
  /** The local day it applies from, `YYYY-MM-DD`. A seeded list price applies from `1970-01-01`. */
  effectiveFrom: string;
  inputPerMtok: number;
  outputPerMtok: number;
  /** Null: 0.1x input. */
  cacheReadPerMtok: number | null;
  /** Null: one price at every prompt length. Its `cacheReadPerMtok` null: 0.1x the long input price. */
  long: (Omit<LongRateArgs, "cacheReadPerMtok"> & { cacheReadPerMtok: number | null }) | null;
  /** `set by the user` (from a page), `set by an agent` (from the MCP `set_model_rate`), or the seeded list price's note. */
  source: string;
}

/** `session_read_model_rates`: every stored rate, by model prefix and then by day. */
export const sessionReadModelRates = () => invoke<StoredModelRate[]>("session_read_model_rates");

/** The joined session read shared with MCP session_read. */
export interface SessionReadParams {
  sessionId: string;
  /** `conversation` pages the turns in full; `turns` pages summaries (never both). */
  include?: ("conversation" | "turns")[];
  cursor?: number;
  limit?: number;
  turn?: number;
  toolUseId?: string;
}
export const sessionRead = (params: SessionReadParams, frameId?: string, page?: string) =>
  invoke<unknown>("session_read", {
    params,
    ...(frameId !== undefined ? { frameId } : {}),
    ...(page !== undefined ? { page } : {}),
  });

/** Committed file attribution shared with MCP why_line. */
export interface WhyLineParams {
  repo: string;
  rev?: string;
  path: string;
  start: number;
  end: number;
}
export const whyLine = (params: WhyLineParams, frameId?: string, page?: string) =>
  invoke<unknown>("why_line", {
    params,
    ...(frameId !== undefined ? { frameId } : {}),
    ...(page !== undefined ? { page } : {}),
  });

/** This machine's attribution and capture status, as MCP describe_setup reports it. */
export const setupStatus = () => invoke<unknown>("setup");

/** `session_read_usage`: token use and spend per session. Times are epoch milliseconds; a
 *  counts by whole local day: `sinceMs`'s day is included, `untilMs`'s day is excluded. */
export const sessionReadUsage = (
  args: {
    sinceMs?: number;
    untilMs?: number;
    sessionId?: string;
    cwd?: string;
    /** Only the sessions spent on this story (a story id). */
    story?: string;
    limit?: number;
    /** The previous page's `nextCursor`, with the same other fields. */
    cursor?: string;
  },
  frameId?: string,
  page?: string,
) =>
  invoke<SessionUsageReport>("session_read_usage", {
    ...(frameId !== undefined ? { frameId } : {}),
    ...(page !== undefined ? { page } : {}),
    filter: {
      sinceMs: args.sinceMs ?? null,
      untilMs: args.untilMs ?? null,
      sessionId: args.sessionId ?? null,
      cwd: args.cwd ?? null,
      story: args.story ?? null,
      limit: args.limit ?? null,
      cursor: args.cursor ?? null,
    },
  });

/**
 * SLW-WL-2: every codesense-managed worktree for the active user, enriched for the
 * management panel. User-global — no repo arg (the backend keys off the active
 * context, the same set the auto-teardown poll operates on). SLW-WL-9: the backend
 * reconciles the registry against disk first and returns the reaped ghost count.
 */
export const listManagedWorktrees = () => invoke<ManagedWorktreeListing>("list_managed_worktrees");

export const runWorktreeSetup = (
  worktreePath: string,
  sourceWorktreePath: string,
  config: WorktreeConfig,
  runId: string,
  // org_root resolves the on-disk worktree.yaml for the provenance gate +
  // execution (SW-WT-9). SW-WT-1: worktree projection moved off this command
  // onto the WorktreeDerive wave, so `team` / `stalenessThreshold` are no
  // longer threaded here.
  orgRoot: string,
  // SW-WT-9: hash of the config the user approved running. Omit/undefined
  // when provenance is local; the backend guard only requires it for
  // team-synced configs (and re-checks it against live disk content).
  approvedConfigHash?: string,
) =>
  invoke<WorktreeRunResult>("run_worktree_setup", {
    worktreePath,
    sourceWorktreePath,
    config,
    runId,
    orgRoot,
    approvedConfigHash,
  });

/** SW-WT-9: provenance of a repo's worktree.yaml setup config. */
export type SetupProvenance = "local" | "team_synced";

export interface SetupProvenanceReport {
  provenance: SetupProvenance;
  /** SHA-256 of the on-disk config bytes; null when there is no SS-8 config. */
  config_hash: string | null;
}

/**
 * SW-WT-9: evaluate whether a repo's worktree.yaml setup originated from a
 * teammate's commit (`team_synced`) or was authored/edited locally
 * (`local`). Pure local read — no fetch, no disk mutation. Drives the
 * one-time preview-and-approve gate before teammate-authored setup runs.
 */
export const evaluateSetupProvenance = (repoPath: string, orgRoot: string) =>
  invoke<SetupProvenanceReport>("evaluate_setup_provenance", { repoPath, orgRoot });

export const cancelWorktreeRun = (runId: string) => invoke<void>("cancel_worktree_run", { runId });

/** F-29: Pre-flight check — does a branch exist locally or on the remote? */
export const checkBranchExists = (repoPath: string, branchName: string) =>
  invoke<boolean>("check_branch_exists", { repoPath, branchName });

// === Watch operations ===

export interface WatchConfig {
  repo_path: string;
  context_repo_path: string;
}

/** Build a WatchConfig from a repo-shaped object (camelCase → snake_case).
 *  SW-CP-18: `viewer_root` + `install_dirs` retired — the watcher re-sources
 *  its watched dirs from the live whole-root/manifest dest set
 *  (`query_projection_dests`), so no per-repo viewer root is threaded. */
export function toWatchConfig(repo: { repoPath: string; contextRepoPath: string }): WatchConfig {
  return {
    repo_path: repo.repoPath,
    context_repo_path: repo.contextRepoPath,
  };
}

// SW-WL-6: monotonic lifecycle sequence minted HERE. JS is single-threaded, so
// it assigns in true call order — the only intent-ordered token Rust can trust
// (an `async` Tauri command's Rust prologue runs at first-poll on a
// multi-threaded runtime, so it cannot self-order a stop-vs-restart). Rust
// `fetch_max`es this seq and lets the highest — the last intent — win the race,
// which matters now that `stop_watching` is async and no longer drains on the
// main thread before a following `start_watching`.
//
// Seeded from `sessionStorage` so the sequence stays monotonic across webview
// reloads / Vite HMR: Rust's `watcher_lifecycle_seq` high-water survives a
// reload (the backend process outlives the webview), so a plain module counter
// that reset to 0 would make every post-reload start/stop bail as "superseded"
// and silently kill the watcher. `sessionStorage` shares the webview-window
// lifetime with the backend process (both reset together only on app relaunch),
// keeping the two counters aligned. Falls back to an in-process counter where
// `sessionStorage` is unavailable (e.g. non-webview test envs).
//
// INVARIANT: this assumes a single webview issuing seqs against one backend
// (true today — `tauri.conf.json` declares one window, no `WebviewWindow` is
// created). `sessionStorage` is per-window, so a SECOND window would seed its
// own counter from 0 and lose `== max` against the shared Rust high-water. If
// multi-window is ever added, seed the counter from the backend instead (the
// AtomicU64 is the real single source of truth).
const WATCHER_SEQ_KEY = "codesense.watcherLifecycleSeq";
const readPersistedWatcherSeq = (): number => {
  try {
    return Number(sessionStorage.getItem(WATCHER_SEQ_KEY)) || 0;
  } catch {
    return 0;
  }
};
let watcherLifecycleSeq = readPersistedWatcherSeq();
const nextWatcherLifecycleSeq = (): number => {
  watcherLifecycleSeq += 1;
  try {
    sessionStorage.setItem(WATCHER_SEQ_KEY, String(watcherLifecycleSeq));
  } catch {
    // best-effort persistence; the in-process counter stays monotonic regardless
  }
  return watcherLifecycleSeq;
};

export const startWatching = async (watchConfigs: WatchConfig[]) =>
  invoke<void>("start_watching", {
    watchConfigs,
    seq: nextWatcherLifecycleSeq(),
    expectedPrincipal: await rendererPrincipal(),
  });

export const stopWatching = async () =>
  invoke<void>("stop_watching", {
    seq: nextWatcherLifecycleSeq(),
    expectedPrincipal: await rendererPrincipal(),
  });

export const addWatch = async (watchConfig: WatchConfig) =>
  invoke<void>("add_watch", { watchConfig, expectedPrincipal: await rendererPrincipal() });

export const removeWatch = async (watchConfig: WatchConfig) =>
  invoke<void>("remove_watch", { watchConfig, expectedPrincipal: await rendererPrincipal() });

/** SW-WL-3 (AC-2/AC-5): attach a workspace's surface-bound watcher on its 0→1
 *  open transition, then enqueue reconcile-on-attach (axes a+c) for the
 *  workspace's repo. `workspaceId` + `sourceRepoSlug` scope the reconcile. */
export const lazyAttachWorkspace = async (
  workspaceId: string,
  sourceRepoSlug: string,
  watchConfig: WatchConfig,
) =>
  invoke<void>("lazy_attach_workspace", {
    workspaceId,
    sourceRepoSlug,
    watchConfig,
    expectedPrincipal: await rendererPrincipal(),
  });

/** SW-WL-3 (AC-2 / AC-9): detach a workspace's surface-bound watcher on its
 *  1→0 close transition. Routes through WL-2's wave-in-flight gate. */
export const lazyDetachWorkspace = async (workspaceId: string, watchConfig: WatchConfig) =>
  invoke<void>("lazy_detach_workspace", {
    workspaceId,
    watchConfig,
    expectedPrincipal: await rendererPrincipal(),
  });

/** SLW-WDE-3 (AC-1/AC-6): attach a lazy `notify` watch rooted at the workspace's
 *  worktree `cwd` when the in-app editor inset opens. Relevant changes emit a
 *  `worktree-file-change` event `{ wsId, changedPath }` (changedPath is RELATIVE
 *  to `cwd`) the App listener routes back to the inset (external-change reload /
 *  dirty-warn). Returns a monotonic **token** the caller passes to
 *  {@link detachWorktreeWatch} so an out-of-order attach/detach (StrictMode /
 *  rapid re-open) can't strand the inset watcher-dead. */
export const lazyAttachWorktreeWatch = (wsId: string, cwd: string) =>
  invoke<number>("lazy_attach_worktree_watch", { wsId, cwd });

/** SLW-WDE-3 (AC-6): detach the workspace's worktree watcher when the inset
 *  closes / the workspace tears down. Token-matched (only removes the watch the
 *  `token` attach created) + leak-free (drops the OS watch). */
export const detachWorktreeWatch = (wsId: string, token: number) =>
  invoke<void>("detach_worktree_watch", { wsId, token });

// === Context repo operations ===

export const createContextRepo = (repoPath: string) =>
  retiredRepositoryAction<ContextRepoInfo>("create_context_repo", { repoPath });

export const getContextRepoPath = (repoPath: string) =>
  invoke<string>("get_context_repo_path", { repoPath });

export const checkContextRepoRemote = (repoPath: string) =>
  invoke<string | null>("check_context_repo_remote", { repoPath });

export const connectContextRemote = (repoPath: string, remoteUrl: string) =>
  retiredRepositoryAction<ContextRepoInfo>("connect_context_remote", { repoPath, remoteUrl });

export const revealContextRepo = (repoPath: string) =>
  invoke<void>("reveal_context_repo", { repoPath });

/** Open an arbitrary path in Finder. Rust side rejects paths outside
 *  the codesense-managed roots (`~/.codesense/users`, `~/.codesense/core`). */
export const revealPath = (path: string) => invoke<void>("reveal_path", { path });

export interface GitHubUser {
  id: number;
  login: string;
  name: string | null;
  avatarUrl: string;
}

export interface GitHubOrg {
  login: string;
}

export interface GitHubPullRequest {
  number: number;
  htmlUrl: string;
  state: string;
}

// SW-CN-UX-7: start the 3-legged authorization-code (loopback + PKCE) flow for a
// source id (provider or declared connector). The backend (SW-CN-32) registers a
// single-use pending flow, opens the system browser once, and returns the
// authorize URL so the connect card can offer [Reopen browser] WITHOUT minting a
// second pending flow. SW-CN-UX-24: also returns the flow's single-use `state` so
// the card can actively revoke the pending entry on Cancel. Both are ref-only on
// the FE — never rendered (D6).
export interface AuthCodeFlowStart {
  authorizeUrl: string;
  state: string;
  operationId: number;
}

export const startAuthCodeFlow = (id: string, intent?: "add" | "reconnect", account?: string) =>
  retiredServiceAction<AuthCodeFlowStart>("start_auth_code_flow", {
    id,
    ...(intent ? { intent } : {}),
    ...(account ? { account } : {}),
  });

// SW-CN-UX-24: actively drop a still-pending auth-code flow the user cancelled,
// keyed by the `state` from startAuthCodeFlow — so the server holds no dangling
// flow and the authorize origin's allowlist shrinks at once rather than on the
// 300 s TTL. Idempotent: an unknown/already-claimed/expired state is a no-op.
export const cancelAuthCodeFlow = (state: string) =>
  retiredServiceAction<void>("cancel_auth_code_flow", { state });

// === GitHub API ===

export const getAuthenticatedUser = () =>
  retiredServiceAction<GitHubUser>("get_authenticated_user");

export const listUserOrgs = () => retiredServiceAction<GitHubOrg[]>("list_user_orgs");

export const checkRepoExists = (owner: string, repo: string) =>
  retiredServiceAction<boolean>("check_repo_exists", { owner, repo });

export const createGithubRepo = (
  owner: string,
  repoName: string,
  isPrivate: boolean,
  credentialRef?: CredentialRef | null,
) =>
  retiredRepositoryAction<string>("create_github_repo", {
    owner,
    repoName,
    private: isPrivate,
    credentialRef,
  });

export const attachOrgRemote = (orgRoot: string, remoteUrl: string) =>
  retiredRepositoryAction<void>("attach_org_remote_cmd", { orgRoot, remoteUrl });

export const createPullRequest = (
  owner: string,
  repo: string,
  title: string,
  body: string,
  head: string,
  base: string,
) =>
  retiredRepositoryAction<GitHubPullRequest>("create_pull_request", {
    owner,
    repo,
    title,
    body,
    head,
    base,
  });

export const mergePullRequest = (owner: string, repo: string, prNumber: number) =>
  retiredRepositoryAction<boolean>("merge_pull_request", { owner, repo, prNumber });

// === Publish/pull sync engine ===
// The session-less picker transport (F4) for the publish set. Returns SW-GS-1's
// locked PublishSet view-model — the single source of truth for the shape.
// The five other sync-engine verbs are MCP-only (no Tauri wrapper here).
import type {
  ApproveMergeOutcome,
  FileRedline,
  IncomingStatus,
  PrReviewSummary,
  PublishFile,
  PublishOutcome,
  PublishSet,
  RefLabel,
  SourceCommitGraphEntry,
} from "@/features/git-sync/lib/types";
// SLW-WD-4: the route-to-context command outcome contract.
import type { RouteOutcome } from "@/features/viewer/lib/route-to-context";

// SW-GS-19 (A1): `prNumber` switches the source — when set, returns that PR's
// changed files (head-tree vs merge-base) instead of the working-tree set.
// SW-GS-13: `commitSha` (when no `prNumber`) returns a landed commit's changed
// files (tree vs first-parent). Same shape + grouping; only the diff source
// differs. The three sources are mutually exclusive (PR wins over commit).
// `pathPrefix` (optional) scopes the working-tree diff to the active team's
// content subtree via a git pathspec — omit/empty for the org-wide set (today's
// behavior). Only applies to the working-tree source (no PR/commit).
export const getPublishSet = (
  repoPath: string,
  workspaceRef?: string,
  prNumber?: number,
  commitSha?: string,
  pathPrefix?: string,
  worktree?: string,
) =>
  invoke<PublishSet>("get_publish_set", {
    repoPath,
    workspaceRef,
    prNumber,
    commitSha,
    pathPrefix,
    worktree,
  });

// Cheap name-only "how many files changed" count for the CLOSED-pane sync badge
// — no rename detection, no binary blobs, no EOL inflation (unlike getPublishSet).
// `pathPrefix` scopes the diff to the active team's subtree. A rename counts as 2
// (delete + add); the exact count appears when the pane opens.
export const changedPathCount = (repoPath: string, pathPrefix?: string) =>
  invoke<number>("changed_path_count", { repoPath, pathPrefix });

// SW-GS-8: commit a node's slice (absolute `filePaths` under `repoPath`, the
// context-repo git root) with `message` + push to origin/main. Deterministic —
// blocks cleanly if behind (returns `{status:"behind"}`), never auto-resolves.
// SW-GS-17: `mode` selects the door — `"direct"` (default) pushes to `main`;
// `"pr"` commits to a `gs/*` branch and opens a PR against main (`scope` names the
// branch). Omitting `mode`/`scope` preserves the SW-GS-8 direct behavior.
// SW-GS-21: `prBody` is the agent-drafted PR DESCRIPTION for `pr` mode (→
// `create_pull_request`'s body); empty/omitted falls back to the deterministic
// `pr_body()`. Ignored for `direct`. Not a new command — an optional param, so no
// count-guard change.
export const publishNode = (
  repoPath: string,
  filePaths: string[],
  message: string,
  mode?: "direct" | "pr",
  scope?: string,
  prBody?: string,
) =>
  retiredRepositoryAction<PublishOutcome>("publish_node", {
    repoPath,
    filePaths,
    message,
    mode,
    scope,
    prBody,
  });

// SW-GS-6: read-only out-of-sync probe — fetches origin + returns the behind
// count for the "N incoming" badge. Cheap (commit-graph count, no tree diff);
// callers invoke it fire-and-forget (never awaited in a render/click path).
export const incomingStatus = (repoPath: string) =>
  invoke<IncomingStatus>("incoming_status", { repoPath });

// SW-GS-3: before/after content for one changed file (org_root-relative path),
// diffed against the same publish baseline as get_publish_set. Feeds the
// rendered-markdown redline viewer. SW-GS-14: `oldPath` (org_root-relative) is
// set only for a rename so "before" is read from the pre-rename baseline path.
// SW-GS-19 (A1): `prNumber` redlines the file across that PR (merge-base blob vs
// PR-head blob) instead of baseline-vs-working-tree. `oldPath` is honored for a
// PR source too — a renamed doc's "before" reads from the pre-rename path in the
// merge-base tree (else it would read as all-added).
// SW-GS-13: `commitSha` (when no `prNumber`) redlines the file across a landed
// commit (first-parent blob vs commit blob) — local git objects only. `oldPath`
// is honored for a commit source too (a renamed doc's "before" reads the
// pre-rename path in the first-parent tree).
export const getFileRedline = (
  repoPath: string,
  filePath: string,
  oldPath?: string,
  prNumber?: number,
  commitSha?: string,
  worktree?: string,
) =>
  invoke<FileRedline>("get_file_redline", {
    repoPath,
    filePath,
    oldPath,
    prNumber,
    commitSha,
    worktree,
  });

// SLW-WD-1: the source worktree change-set — the FLAT list of files that differ
// between a worktree's merge-base (fork-point) baseline and its working tree
// (incl. index + untracked), classified identically to get_publish_set but with
// no context-repo bucketing. `baseRef` (RD-7) is the optional fork-parent branch
// for a STACKED worktree; omit it to merge-base against origin/main. Read-only,
// off the UI thread; feeds the WD-2 source-diff review overlay.
export const sourceWorktreeChangeset = (repoPath: string, baseRef?: string) =>
  invoke<PublishFile[]>("source_worktree_changeset", { repoPath, baseRef });

// SLW-WD-1: per-file before/after redline for a source worktree file, computed
// against the SAME merge-base baseline as sourceWorktreeChangeset — byte-identical
// shape to getFileRedline (binary ⇒ `binary:true` + empty content), so WD-2
// renders it with the existing components. `oldPath` (repo-relative) is set only
// for a rename; `baseRef` mirrors the change-set's fork-parent override.
export const sourceFileRedline = (
  repoPath: string,
  filePath: string,
  oldPath?: string,
  baseRef?: string,
) => invoke<FileRedline>("source_file_redline", { repoPath, filePath, oldPath, baseRef });

// SLW-WD-4: route a worktree file OUT of the source diff and into the team
// content layer (adopt + git-exclude), and its inverse. The app-side bridge to
// WD-3's verbs — shares ONE Rust orchestration with the MCP `#[tool]`s
// (`commands::route_to_context`). `codeRepoSlug` is the `"owner/repo"` string;
// `logicalName` is the file's repo-relative path; `layer` defaults to the team
// workspace (WD-3's only routable tier) when omitted. Non-blocking (spawn_blocking
// behind a per-repo lock); returns the outcome the overlay maps to a row state.
export const routeFileToContext = (
  codeRepoPath: string,
  codeRepoSlug: string,
  logicalName: string,
  layer?: string,
  frameId?: string,
  pagePath?: string,
) =>
  invoke<RouteOutcome>("route_file_to_context", {
    codeRepoPath,
    codeRepoSlug,
    logicalName,
    layer,
    ...(frameId ? { frameId } : {}),
    ...(pagePath ? { pagePath } : {}),
  });

export const unrouteFileFromContext = (
  codeRepoPath: string,
  codeRepoSlug: string,
  logicalName: string,
) => invoke<void>("unroute_file_from_context", { codeRepoPath, codeRepoSlug, logicalName });

// === The context repo: status button, its cards, History ===
// Agents do the git; these read the clone on disk. The one write settles a conflicted
// file (writes and stages the chosen side) and leaves the merge for the agent to finish.

/** What is happening to the context repo's sync. `offline` is never sent: the nav reads
 *  it from the browser. */
export type ContextSyncState = "ok" | "syncing" | "failed" | "offline" | "signin" | "retrying";

/** One side of a conflicted file: who last changed it there, and when. */
export interface ContextConflictSide {
  who: string | null;
  subject: string | null;
  /** Unix ms. */
  at: number | null;
  /** That side deleted the file. */
  deleted: boolean;
}

/** One file changed both here and on GitHub, waiting on a decision. */
export interface ContextConflict {
  /** Repo-relative path. */
  path: string;
  mine: ContextConflictSide;
  theirs: ContextConflictSide;
  /** The agent that last wrote the file on this Mac's side: the hand-off default. */
  lastWriter: string | null;
  /** Changes from GitHub held back until this is settled (repo-relative paths). */
  heldBack: string[];
}

export interface ContextRepoStatus {
  lastCommitAt?: number | null;
  worktree?: {
    id: string;
    name: string;
    path: string;
    mergeBase: string;
    uncommittedPaths: string[];
  };
  uncommitted?: number;
  owner?: { kind: "person" } | { kind: "agent"; handle: string; name: string } | null;
  agents?: { handle: string; name: string; role: string | null; avatar?: string | null }[];
  /** The checked-out branch, or `null` when HEAD is detached. */
  branch: string | null;
  headSha: string;
  hasUpstream: boolean;
  /** The upstream's short name, e.g. `origin/main`. */
  upstream: string | null;
  /** SW-VE-16: forge provider id (`github`/`gitlab`/`bitbucket`) for the `origin` remote,
   *  or `null` when there's no origin, the URL doesn't parse, or the host isn't a
   *  supported forge. Lets the file-viewer toolbar render `vs GitHub (origin/main)` without
   *  hardcoding GitHub. */
  remoteProvider: string | null;
  /** Commits on the branch GitHub does not have. */
  ahead: number;
  /** Commits on GitHub the branch does not have. */
  behind: number;
  sync: {
    state: ContextSyncState;
    /** Unix ms of the last fetch or reported sync. */
    lastSyncedAt: number | null;
    reason?: string;
    attempt?: number;
    maxAttempts?: number;
    progress?: string;
    /** The clone has no remote: nothing will send its changes. */
    noSender?: boolean;
  };
  conflicts: ContextConflict[];
}

export const contextRepoStatus = (repoPath: string, worktree?: string) =>
  invoke<ContextRepoStatus>("context_repo_status", { repoPath, worktree });

/** Keep this Mac's version, take GitHub's, or keep both (GitHub's lands beside it). */
export type ContextConflictResolution = "useMine" | "useTheirs" | "keepBoth";

export const resolveContextConflict = (
  repoPath: string,
  path: string,
  resolution: ContextConflictResolution,
) => invoke<void>("resolve_context_conflict", { repoPath, path, resolution });

/** Ask an agent, through the inbox, to settle a conflicted file. */
export const handContextConflictToAgent = (
  team: InboxTeam,
  repoPath: string,
  path: string,
  agentName: string,
) => invoke<HumanSendResult>("hand_context_conflict_to_agent", { team, repoPath, path, agentName });

/** The card's ways out of a failed sync, each asked of an agent. */
export type ContextSyncAction = "retry" | "signIn" | "setUp" | "commit";

export const runContextSyncAction = (
  team: InboxTeam,
  repoPath: string,
  action: ContextSyncAction,
  agentName: string,
  reason: string | null,
) =>
  invoke<HumanSendResult>("run_context_sync_action", {
    team,
    repoPath,
    action,
    agentName,
    reason,
  });

/** Which commits a History page walks. */
export type HistoryBranches = { kind: "all" } | { kind: "main" } | { kind: "branch"; name: string };

/** How a commit's lane is drawn. */
export type LaneKind = "main" | "merged" | "open" | "local" | "backup";

export interface HistoryCommit {
  sha: string;
  shortSha: string;
  parents: string[];
  refs: RefLabel[];
  subject: string;
  author: string;
  byYou: boolean;
  /** Unix seconds. */
  committedAt: number;
  pr?: number;
  lane: LaneKind;
  /** Not on GitHub yet. */
  localOnly: boolean;
  agent?: { name: string; avatar?: string };
  /** A one-branch page ends here: where the branch left main. */
  forkPoint: boolean;
  /** On a page filtered by path: the files it changed (repo-relative). */
  files?: string[];
}

export interface HistoryPage {
  commits: HistoryCommit[];
  nextCursor: string | null;
  openPrs: { number: number; sha: string; branch?: string }[];
  /** Every branch a filter can pick, main first. */
  branches: string[];
}

/** One page of the context repo's history, newest first. `paths` keeps commits that
 *  changed a file under one of those repo-relative paths. */
export const contextRepoHistory = (
  repoPath: string,
  branches: HistoryBranches,
  paths: string[] | null,
  limit: number,
  cursor: string | null,
) => invoke<HistoryPage>("context_repo_history", { repoPath, branches, paths, limit, cursor });

// SLW-WD-8: the source worktree commit graph — a paginated, newest-first window
// of commits reachable from ALL local + remote branch refs, each with its parents
// (edges) + ref decorations (incl. the synthetic HEAD). NO per-commit diff at walk
// time; `cursor` (a sha) resumes AFTER it for load-older paging under a fixed ref
// set. Read-only, local git objects only. Feeds the WD-9 commit-graph overlay.
export const sourceCommitGraph = (repoPath: string, limit: number, cursor?: string) =>
  invoke<SourceCommitGraphEntry[]>("source_commit_graph", { repoPath, limit, cursor });

// SLW-WD-8: the FLAT per-commit change-set (a landed commit's tree vs its
// first-parent tree) for a source worktree — same PublishFile shape as
// sourceWorktreeChangeset, no context-repo bucketing. Fetched lazily when WD-9
// opens a commit node.
export const sourceCommitChangeset = (repoPath: string, sha: string) =>
  invoke<PublishFile[]>("source_commit_changeset", { repoPath, sha });

// SLW-WD-8: per-commit per-file redline for a source worktree (first-parent blob
// vs commit blob — local git objects). Byte-identical FileRedline shape to
// sourceFileRedline (binary ⇒ `binary:true` + empty), so WD-9 renders it with the
// existing components. `oldPath` is set only for a rename.
export const sourceCommitFileRedline = (
  repoPath: string,
  sha: string,
  filePath: string,
  oldPath?: string,
) => invoke<FileRedline>("source_commit_file_redline", { repoPath, sha, filePath, oldPath });

// SW-GS-19 (A3): live list of open PRs for the in-UI review panel (source of
// truth, not the session openPrs map). Owner/repo resolved server-side.
export const listPrsForReview = (repoPath: string) =>
  retiredRepositoryAction<PrReviewSummary[]>("list_prs_for_review", { repoPath });

// SW-GS-19 (A2): approve (when `approve`) + merge a PR in one call; owner/repo
// resolved server-side. FE pulls down separately via pull_ff under the op-lock.
export const approveAndMergePr = (repoPath: string, prNumber: number, approve: boolean) =>
  retiredRepositoryAction<ApproveMergeOutcome>("approve_and_merge_pr", {
    repoPath,
    prNumber,
    approve,
  });

// === Workspace permissions (C.6b Part 1) ===

export type RepoPermission = "admin" | "write" | "read";

export interface WorkspaceRepoAccess {
  permission: RepoPermission;
  defaultBranch: string;
}

export interface ParsedGitRemote {
  host: string;
  provider: string;
  owner: string;
  repo: string;
}

export const getRepoPermissions = (owner: string, repo: string) =>
  retiredServiceAction<WorkspaceRepoAccess>("get_repo_permissions", { owner, repo });

export const getBranchProtection = (owner: string, repo: string, branch: string) =>
  retiredServiceAction<boolean>("get_branch_protection", { owner, repo, branch });

export const parseGitRemote = (url: string) =>
  retiredServiceAction<ParsedGitRemote>("parse_git_remote", { url });

// === Registry-edit PR flow (C.6b Part 3) ===

/** User-intent variants for `commit_registry_edit_as_pr`. Matches the
 *  Rust `RegistryEdit` enum; `type` is the serde tag. */
export type RegistryEdit =
  | { type: "RenameWorkspace"; new_display_name: string }
  | { type: "AddPersonByUsername"; username: string }
  | { type: "AddPersonByName"; display_name: string }
  | {
      type: "AddIdentityToPerson";
      person_id: string;
      provider: string;
      handle: string;
      provider_user_id: string | null;
    }
  | { type: "RemovePersonByUserId"; github_user_id: number }
  | { type: "RemovePersonById"; person_id: string }
  | {
      type: "AddSourceRepoToTeam";
      team_name: string;
      owner: string;
      repo: string;
      provider?: string;
      host?: string;
    }
  | {
      type: "RemoveSourceRepoFromTeam";
      team_name: string;
      owner: string;
      repo: string;
      provider?: string;
      host?: string;
    }
  | {
      type: "CreateTeam";
      team_name: string;
      creator_id: number;
      creator_username: string;
      creator_display_name: string | null;
      creator_avatar_url: string | null;
    }
  // SW-HB-7: local-only team lifecycle. DeleteTeam de-registers (teams.yml
  // entry stripped, content left on disk); ArchiveTeam toggles the reversible
  // `archived` flag. Both ride `commit_registry_edit_local` — no remote/PR.
  | { type: "DeleteTeam"; team_name: string }
  | { type: "ArchiveTeam"; team_name: string; archived: boolean }
  | { type: "RenameTeam"; old_name: string; new_name: string }
  // SLW-IN-5: set (or clear) a team's parent. `parent_team: null` clears the
  // link (team becomes a root). Rides `commit_registry_edit_local` — no new
  // command, so the contract count-guard is unaffected.
  | { type: "SetTeamParent"; team_name: string; parent_team: string | null }
  // Set (or clear) a team's starter page (`teams.yml` `landing_page:`, relative
  // to its content folder). `landing_page: null` falls back to README/index.
  | { type: "SetTeamLandingPage"; team_name: string; landing_page: string | null }
  // SW-MC-2 (AC-3/AC-9): set (or clear) the per-org managed-clone workspace
  // root in `org.yml::config_flags`. `workspace_root_dir: null` reverts to the
  // hidden default. Rides `commit_registry_edit_local` — org.yml-only, no PR.
  | { type: "SetWorkspaceRoot"; workspace_root_dir: string | null };

export interface OpenedPullRequest {
  prNumber: number;
  htmlUrl: string;
  branch: string;
}

export const commitRegistryEditAsPr = (
  githubUserId: number,
  orgSlug: string,
  remoteUrl: string | null,
  defaultBranch: string,
  edit: RegistryEdit,
) =>
  retiredRepositoryAction<OpenedPullRequest>("commit_registry_edit_as_pr", {
    githubUserId,
    orgSlug,
    remoteUrl,
    defaultBranch,
    edit,
  });

// === Source repo attachment (W.2) ===

export const resolveSourceRepoAttachment = (repoPath: string, githubUserId: number) =>
  invoke<Attachment | null>("resolve_source_repo_attachment", { repoPath, githubUserId });

export type SaveAttachmentError =
  | { kind: "ambiguousAccount"; provider: string; accounts: string[] }
  | { kind: "providerMismatch"; expected: string; actual: string }
  | { kind: "failure"; message: string };

export const saveSourceRepoAttachment = async (
  githubUserId: number,
  attachment: Attachment,
  inferCredential = false,
) => {
  try {
    await invoke<void>("save_source_repo_attachment", {
      githubUserId,
      attachment,
      inferCredential,
    });
  } catch (cause) {
    if (cause && typeof cause === "object" && "kind" in cause) {
      const detail = cause as SaveAttachmentError;
      const message =
        detail.kind === "ambiguousAccount"
          ? `Choose a ${detail.provider} connection for this repository.`
          : detail.kind === "providerMismatch"
            ? `The repository uses ${detail.expected}, not ${detail.actual}.`
            : detail.message;
      throw Object.assign(new Error(message), detail, { cause });
    }
    throw cause;
  }
};

/** SW-MC-1: structured success of a managed clone+attach (serde `tag: "kind"`,
 *  camelCase variants, snake_case fields — mirrors Rust `CloneAttachOutcome`). */
export type CloneAttachOutcome =
  | { kind: "cloned"; repo_path: string }
  | { kind: "alreadyExisted"; repo_path: string };

/** SW-MC-1: structured failure of a managed clone+attach (mirrors Rust
 *  `CloneAttachError`). The caller branches on `kind` to drive Screen 16
 *  (clone-failure fork) and Screen 17 (occupied / attach-instead). */
export type CloneAttachError =
  | { kind: "alreadyAttached"; owner: string; repo: string }
  | { kind: "targetOccupied"; path: string }
  | { kind: "authFailed" }
  | { kind: "remoteNotFound"; remote_url: string }
  | { kind: "networkError"; message: string }
  | { kind: "emptyRemote"; remote_url: string }
  | { kind: "internal"; message: string };

/** SW-MC-1: clone a tracked source repo over HTTPS+OAuth into the default
 *  managed area and auto-attach it as `managed`. Resolves with a
 *  {@link CloneAttachOutcome}; rejects with a {@link CloneAttachError}.
 *  SW-FG-8 (AC-8): `credentialRef` binds the managed record to the connection whose
 *  token clones (provider-neutral, re-keyed off `githubAccountId`). */
export const cloneAndAttachSourceRepo = (
  owner: string,
  repo: string,
  githubUserId: number,
  credentialRef: CredentialRef | null,
  provider = "github",
  host: string | null = null,
) =>
  retiredRepositoryAction<CloneAttachOutcome>("clone_and_attach_source_repo", {
    sourceRepo: { owner, repo, provider, host },
    githubUserId,
    credentialRef,
  });

/** SW-MC-7 (AC-3): re-clone a managed clone whose directory went missing,
 *  restoring it into the RECORDED `repoPath` (records-are-truth). The clone runs
 *  on the blocking pool backend-side, so this never stalls the UI. Reuses the
 *  same `CloneAttachOutcome` shape as the initial managed clone. */
export const recloneManagedSourceRepo = (githubUserId: number, repoPath: string) =>
  retiredRepositoryAction<CloneAttachOutcome>("reclone_managed_source_repo", {
    githubUserId,
    repoPath,
  });

/** F-6: returns a Map keyed by lowercase `"owner/repo"` of the user's
 *  attachments (one per remote, server-side tie-break on `attached_at`).
 *  Used by `use-workspace-viewer-sync` to populate `localPath` on synthetic
 *  workspace `RepoState`s. Returns an empty Map when `attachments.yml` is
 *  absent. */
type AttachmentsByRemoteRecord = Record<string, Attachment>;
export const listAttachmentsByRemote = async (
  githubUserId: number,
): Promise<Map<string, Attachment>> => {
  const record = await invoke<AttachmentsByRemoteRecord>("list_attachments_by_remote", {
    githubUserId,
  });
  return new Map(Object.entries(record ?? {}));
};

export const readSourceRepoOriginUrl = (repoPath: string) =>
  invoke<string | null>("read_source_repo_origin_url", { repoPath });

/** A tab's resolved source repo — the per-tab primitives toolbar's Diff gate.
 *  `null` for a cwd outside any (non-bare) git working tree. Mirrors Rust
 *  `commands::git::TabSourceRepo`. */
export interface TabSourceRepo {
  /** Canonical working-tree root. */
  root: string;
  /** `true` iff the cwd itself is the working-tree root (not a subdir of it). */
  isRoot: boolean;
}

/** A tab's resolved source folder — the per-tab primitives toolbar's Source
 *  gate. `kind:"folder"` lets consumers discriminate against `TabSourceRepo`.
 *  Local tabs carry the on-disk cwd; a remote tab carries the virtual
 *  `remote://<machineId>/<handle>` root (opaque — never treated as a filesystem
 *  path by downstream code, AD-6). Synthesized FE-side from the workspace's
 *  own `cwd` + remote `(machineId, handle)` — no backend round-trip; a plain
 *  folder is viewable iff the kind is `sourceCapable` and the cwd is known. */
export interface TabSourceFolder {
  kind: "folder";
  cwd: string;
}

/** The combined "what can this tab offer?" result: Source icon mounts when
 *  `folder != null`; Diff icon mounts when `repo` resolves to a non-null repo.
 *  The two signals degrade independently — a plain folder in a non-git dir
 *  resolves `{folder, repo:null}`, a git worktree resolves both. `repo` is
 *  tri-state so the Diff icon can wait for the backend without hiding the
 *  (always-cheap) folder-sourced Source icon. */
export interface TabSourceResolution {
  folder: TabSourceFolder | null;
  /** `undefined` = still resolving; `null` = resolved, not a git working
   *  tree; a `TabSourceRepo` otherwise. */
  repo: TabSourceRepo | null | undefined;
}

/** Resolve the source repo for a tab's cwd (or `null` if none). Blocking git I/O
 *  on the backend's blocking pool, so the hot path never stalls. */
export const resolveTabSourceRepo = (cwd: string) =>
  invoke<TabSourceRepo | null>("resolve_tab_source_repo", { cwd });

// === DESIGN-remote-source.md §1 — remote source + editor commands ===

/** The typed error surface for every remote source/editor command. Every FE
 *  caller matches exhaustively — no silent swallow (AD-3). */
export type RemoteSourceError =
  | { kind: "disconnected" }
  | { kind: "unauthorized" }
  | { kind: "notFound" }
  | { kind: "boundsExceeded" }
  | { kind: "conflict"; observation: RemoteFileObservation }
  | { kind: "transportFailure"; message: string };

/** The remote-side observation of a file at a point in time (hash + mtime + size +
 *  mode). The write command returns one; the read command returns one; the
 *  conflict payload carries the current observation so the merge view can show
 *  "theirs" without a second read. Mirrors `codesense_pty_core::remote_files::Observation::Regular`. */
export interface RemoteFileObservation {
  /** The sha-256 of the file bytes as a 64-char lower-hex string. */
  hash: string;
  /** Modification time in nanoseconds since the epoch. */
  mtimeNs: number;
  /** The file size in bytes. */
  size: number;
  /** The POSIX mode bits. */
  mode: number;
}

/** Result of a remote file read. Bytes are base64-encoded (the host-proto wire
 *  shape) and the observation lets the editor seed the optimistic-lock baseline. */
export interface RemoteFileRead {
  /** Base64-encoded file bytes (up to `MAX_FILE_BYTES` on the remote). */
  base64: string;
  observation: RemoteFileObservation;
  /** True when the read was limited by `maxBytes` and more bytes exist. */
  truncated: boolean;
}

/** Result of a remote file write. The server returns the new observation so the
 *  editor adopts the fresh hash as the baseline for the next write. */
export interface RemoteFileWrite {
  observation: RemoteFileObservation;
}

/** DESIGN-remote-source.md §3.4 — resolve whether a remote tab's workspace is
 *  a git working tree. `handle` is the managed-workspace id
 *  (`remote-workspace-<sha256>`). Returns `null` when not a working tree. */
export const resolveTabSourceRepoRemote = (machineId: string, handle: string) =>
  invoke<TabSourceRepo | null>("resolve_tab_source_repo_remote", { machineId, handle });

/** DESIGN-remote-source.md §4.2 — read a file on the remote worktree.
 *  `expectedHash` is a short-circuit: if the server's current hash matches, it
 *  returns an empty-bytes `RemoteFileRead` with `truncated:false` and the
 *  observation (saves re-transferring bytes the caller already has). `limit`
 *  caps bytes read (default `MAX_FILE_BYTES` = 4 MiB). */
export const sourceRemoteFileRead = (
  machineId: string,
  handle: string,
  path: string,
  expectedHash?: string,
  limit?: number,
) =>
  invoke<RemoteFileRead>("source_remote_file_read", {
    machineId,
    handle,
    path,
    expectedHash,
    limit,
  });

/** DESIGN-remote-source.md §4.2 — atomic write to a remote worktree file,
 *  optimistic-locked against `expectedHash`. The server returns
 *  `RemoteSourceError::Conflict` with the current observation when the hash
 *  doesn't match; the editor's merge view consumes that observation. */
export const sourceRemoteFileWrite = (
  machineId: string,
  handle: string,
  path: string,
  base64: string,
  expectedHash?: string,
) =>
  invoke<RemoteFileWrite>("source_remote_file_write", {
    machineId,
    handle,
    path,
    base64,
    expectedHash,
  });

/** DESIGN-remote-phase-2.md §3 — single-level directory listing on a remote
 *  worktree. `path` is worktree-relative (empty string = root); each returned
 *  entry's `path` is also worktree-relative, so the FE's lister wrapper
 *  re-anchors them to the remote absolute root. Mirrors the shape of
 *  {@link listDirectory}; the Rust side proxies through the host-agent's
 *  `FileOperation::Enumerate` with one-level reduction (follow-up: a dedicated
 *  per-directory `List` wire op that also returns byte sizes). */
export const sourceRemoteListDirectory = (machineId: string, handle: string, path: string) =>
  invoke<FileEntry[]>("source_remote_list_directory", { machineId, handle, path });

/** Inspect one remote file without transferring it. The open editor polls this
 *  so an edit made on the machine is noticed — the path-set watcher cannot see
 *  an in-place edit. */
export const sourceRemoteFileInspect = (machineId: string, handle: string, path: string) =>
  invoke<RemoteFileObservation>("source_remote_file_inspect", { machineId, handle, path });

/** A remote file's committed bytes. `supported: false` on a host that predates
 *  the operation (the editor then shows no change markers). */
export interface RemoteFileHead {
  base64: string | null;
  supported: boolean;
}

export const sourceRemoteFileHead = (machineId: string, handle: string, path: string) =>
  invoke<RemoteFileHead>("source_remote_file_head", { machineId, handle, path });

/** Quick-open's remote file list. `includesUntracked: false` when an older
 *  host could only answer tracked files. */
export interface RemoteFileList {
  paths: string[];
  truncated: boolean;
  includesUntracked: boolean;
}

export const sourceRemoteListFiles = (machineId: string, handle: string, limit?: number) =>
  invoke<RemoteFileList>("source_remote_list_files", { machineId, handle, limit });

/** DESIGN-remote-source.md §4.1 — fetch the flat change-set of a remote source
 *  worktree. Same shape as {@link sourceWorktreeChangeset}; see §5 for the
 *  perf argument (one fetch per (machineId, handle, baseRef) tick via the seam
 *  dedupe). Routed via {@link sourceChangeset} — do not call directly. */
export const sourceRemoteChangeset = (machineId: string, handle: string, baseRef?: string) =>
  invoke<PublishFile[]>("source_remote_changeset", { machineId, handle, baseRef });

/** DESIGN-remote-source.md §4.1 — per-file redline for a remote worktree file,
 *  computed against the same merge-base baseline as {@link sourceRemoteChangeset}.
 *  Byte-identical shape to {@link sourceFileRedline}. Routed via
 *  {@link sourceRedline} — do not call directly. */
export const sourceRemoteFileRedline = (
  machineId: string,
  handle: string,
  filePath: string,
  oldPath?: string,
  baseRef?: string,
) =>
  invoke<FileRedline>("source_remote_file_redline", {
    machineId,
    handle,
    filePath,
    oldPath,
    baseRef,
  });

/** DESIGN-remote-source.md §3.3 — start a ref-counted remote-worktree watcher.
 *  The host-side polling task fingerprints the tracked-paths set and emits
 *  `remote-worktree-file-change-{machineId}-{handle}` events on changes.
 *  Returns a monotonic `token` the caller passes to
 *  {@link sourceRemoteWatchStop} so an out-of-order attach/detach can't strand
 *  the subscriber. `pollMs` defaults to 2000. */
export const sourceRemoteWatchStart = (machineId: string, handle: string, pollMs?: number) =>
  invoke<number>("source_remote_watch_start", { machineId, handle, pollMs });

/** DESIGN-remote-source.md §3.3 — detach the remote worktree watcher. */
export const sourceRemoteWatchStop = (token: number) =>
  invoke<void>("source_remote_watch_stop", { token });

/** DESIGN-remote-phase-2.md §0 / B-3 — one-shot probe of the remote
 *  worktree's tracked-paths count. The FE watcher calls this once per
 *  (machineId, handle) on its first successful attach to seed the
 *  `useRemoteCadence` large-repo threshold (5000 files flips the preset to
 *  `remoteLarge`). Returns `BoundsExceeded` if the remote list exceeded
 *  `MAX_PATHS` (100 000); the FE treats that as "definitely large". */
export const sourceRemoteTrackedCount = (machineId: string, handle: string) =>
  invoke<number>("source_remote_tracked_count", { machineId, handle });

/** What to add to a team: a remote URL (or GitHub `owner/repo`), or a folder on
 *  this machine. Mirrors Rust `source_repo_add::AddRepoInput`. */
export type AddRepoInput = { kind: "url"; url: string } | { kind: "localPath"; path: string };

/** A repo tracked on a team, with the local clone bound to it when the call bound
 *  one. Mirrors Rust `source_repo_add::TrackedSourceRepo`. */
export interface TrackedSourceRepo {
  owner: string;
  repo: string;
  provider: string;
  host: string;
  remoteKey: string;
  localPath: string | null;
  /** The clone's remote naming this repo (`upstream` in a fork workflow). */
  remoteName: string | null;
  remoteUrl: string | null;
}

/** Why an add or a clone bind was refused. Mirrors Rust
 *  `source_repo_add::AddRepoError`; every variant also carries a readable
 *  `message`. */
export type AddRepoError = { message: string } & (
  | { kind: "invalidInput"; reason: string }
  | { kind: "unknownTeam"; org: string; team: string }
  | { kind: "alreadyTracked"; owner: string; repo: string; team: string }
  | { kind: "notTracked"; repo: string; team: string }
  | { kind: "notADirectory"; path: string }
  | { kind: "notAGitRepository"; path: string }
  | { kind: "notRepoRoot"; path: string; root: string }
  | { kind: "noOrigin"; path: string }
  | { kind: "unsupportedRemote"; url: string; reason: string }
  | { kind: "originMismatch"; expected: string; actual: string }
  | { kind: "failure" }
);

const ADD_REPO_ERROR_KINDS: ReadonlySet<string> = new Set<AddRepoError["kind"]>([
  "invalidInput",
  "unknownTeam",
  "alreadyTracked",
  "notTracked",
  "notADirectory",
  "notAGitRepository",
  "notRepoRoot",
  "noOrigin",
  "unsupportedRemote",
  "originMismatch",
  "failure",
]);

function hasAddRepoErrorKind(value: unknown): boolean {
  return (
    typeof value === "object" &&
    value !== null &&
    "kind" in value &&
    typeof value.kind === "string" &&
    ADD_REPO_ERROR_KINDS.has(value.kind)
  );
}

/** Rethrow a rejected add/bind as an `Error` carrying the typed detail, so a
 *  caller can branch on `kind` and any error surface can read `message`. */
async function rethrowingAddRepoError<T>(call: Promise<T>): Promise<T> {
  try {
    return await call;
  } catch (cause) {
    if (
      hasAddRepoErrorKind(cause) &&
      typeof (cause as { message?: unknown }).message === "string"
    ) {
      const detail = cause as AddRepoError;
      throw Object.assign(new Error(detail.message), detail, { cause });
    }
    throw cause;
  }
}

/** Narrow a caught value to a typed add/bind refusal. */
export function isAddRepoError(e: unknown): e is Error & AddRepoError {
  return e instanceof Error && hasAddRepoErrorKind(e);
}

/** Add a repo to a team from a URL or a local folder. A folder is tracked as the
 *  repo its `origin` names (`local/<folder>` without one) and bound as its local
 *  clone; a repo the team already tracks is refused (`alreadyTracked`). Rejects
 *  with an `Error & AddRepoError`. */
export const addSourceRepoToTeam = (input: {
  githubUserId: number;
  orgSlug: string;
  teamName: string;
  input: AddRepoInput;
}) =>
  rethrowingAddRepoError(
    invoke<TrackedSourceRepo>("add_source_repo_to_team", {
      githubUserId: input.githubUserId,
      orgSlug: input.orgSlug,
      teamName: input.teamName,
      input: input.input,
    }),
  );

/** Bind the clone at `path` to a tracked repo. The folder must be a git working
 *  tree whose `origin` is that repo (else `notAGitRepository`, `noOrigin` or
 *  `originMismatch`). Rejects with an `Error & AddRepoError`. */
export const attachSourceRepoClone = (input: {
  githubUserId: number;
  repo: { owner: string; repo: string; provider: string; host: string | null };
  path: string;
}) =>
  rethrowingAddRepoError(
    invoke<TrackedSourceRepo>("attach_source_repo_clone", {
      githubUserId: input.githubUserId,
      repo: input.repo,
      path: input.path,
    }),
  );

// SS-FU-17: outcome of a detach teardown. Hash-guarded — files the user
// locally edited are preserved and reported in `skippedLocallyModified`
// (they become ordinary untracked files, no longer codesense-managed).
export interface TeardownReport {
  removed: string[];
  skippedLocallyModified: string[];
  errors: string[];
}

// W.6 + SS-FU-17: Detach = uninstall. Tears down the materialized install
// (hash-guarded) under the wave's per-repo lock, then removes the
// attachment record. Resolves to the teardown report.
export const deleteSourceRepoAttachment = (githubUserId: number, repoPath: string) =>
  invoke<TeardownReport>("delete_source_repo_attachment", { githubUserId, repoPath });

// SW-MC-3: managed-clone lifecycle — manual pull, remove, unhealthy-detection.

/** SW-MC-3 AC-1/AC-8: structured success of a managed-clone update. */
export type ManagedUpdateOutcome = { kind: "upToDate" } | { kind: "pulled"; commits: number };

/** SW-MC-3 AC-1/AC-5/AC-8: structured failure of a managed-clone update. The
 *  discriminant drives the busy/recoverable-failure/diverged states. */
export type ManagedUpdateError =
  | { kind: "notManaged" }
  | { kind: "notFound" }
  | { kind: "authFailed" }
  | { kind: "networkError"; message: string }
  | { kind: "diverged" }
  | { kind: "uncommitted" }
  | { kind: "internal"; message: string };

/** SW-MC-3 AC-1: manual, ff-only pull of a managed clone (`git fetch origin`
 *  + fast-forward of the clone's checked-out branch). Resolves with a
 *  {@link ManagedUpdateOutcome}; rejects with a {@link ManagedUpdateError}. */
export const updateManagedClone = (githubUserId: number, repoPath: string) =>
  retiredRepositoryAction<ManagedUpdateOutcome>("update_managed_clone", { githubUserId, repoPath });

/** SW-MC-3 AC-2/AC-4: structured outcome of a managed-clone remove.
 *  `blockedByLiveWorktrees` is a legible, non-error refusal (never
 *  auto-closes the worktrees). */
export type ManagedRemoveOutcome =
  | { kind: "removed" }
  | { kind: "blockedByLiveWorktrees"; count: number; worktreePaths: string[] }
  | { kind: "blockedOutsideManagedRoot"; path: string };

/** SW-MC-3 AC-2/AC-3: structured failure of a managed-clone remove. */
export type ManagedRemoveError =
  | { kind: "notManaged" }
  | { kind: "notFound" }
  | { kind: "internal"; message: string };

/** SW-MC-3 AC-2: remove a managed clone — delete the directory and forget
 *  the record — marker-gated, and refused (`blockedByLiveWorktrees`) while
 *  live EW integration worktrees hang off its object store. */
export const removeManagedClone = (githubUserId: number, repoPath: string) =>
  retiredRepositoryAction<ManagedRemoveOutcome>("remove_managed_clone", { githubUserId, repoPath });

/** SW-MC-3 AC-7: per-row health of a managed clone — whether codesense still
 *  recognizes its directory as a materialize surface. `recognized: false`
 *  drives the amber "not syncing" chip (MC-6). */
export interface ManagedCloneHealth {
  repoPath: string;
  recognized: boolean;
}

/** SW-MC-3 AC-7: batch the unhealthy-detection signal for ALL of the user's
 *  managed clones in one call (invoked on surface mount + on Re-check), so
 *  the FE renders health as a prop and never fans out a per-row IPC probe. */
export const listManagedCloneHealth = (githubUserId: number) =>
  invoke<ManagedCloneHealth[]>("list_managed_clone_health", { githubUserId });

// SW-MC-5: directory-level workspace reconcile — attach-found / clone-missing /
// re-home-moved against the team's tracked set, matched by git origin.

/** SW-MC-5 AC-7: the per-repo outcome of a reconcile pass (serde `tag: "kind"`,
 *  camelCase variants, snake_case fields — mirrors Rust `ReconcileOutcome`).
 *  The stable contract SW-MC-4's re-home UI and the bulk-setup terminal screen
 *  (Screens 11/12) render against — never re-derive outcomes from side effects. */
export type ReconcileOutcome =
  | { kind: "attached"; slug: string; repo_path: string }
  | { kind: "cloned"; slug: string; repo_path: string }
  | { kind: "moved"; slug: string; from: string; to: string }
  | { kind: "alreadyAttachedElsewhere"; slug: string; path: string }
  | { kind: "skippedInUse"; slug: string }
  | { kind: "skippedOccupied"; slug: string; path: string }
  | { kind: "skippedNotTracked"; origin: string; path: string }
  | { kind: "failed"; slug: string; reason: string };

/** SW-MC-5 AC-13: payload of the `reconcile-workspace-progress` event, emitted
 *  once per tracked repo as it resolves — drives the Screen-12 N-of-M bar. */
export interface ReconcileProgress {
  done: number;
  total: number;
  repo: string;
  phase: string;
}

/** SW-MC-5 (AC-2/AC-8): reconcile a workspace root against the team's tracked
 *  set — attach what's on disk, clone what's missing, re-home managed clones off
 *  an old root. The scan + clones + moves run on the blocking pool backend-side
 *  (AC-13), so this never stalls the UI; subscribe to `reconcile-workspace-progress`
 *  for live N-of-M. Resolves with one {@link ReconcileOutcome} per repo. */
export const reconcileWorkspace = (params: {
  githubUserId: number;
  /** Legacy single-provider binding; retained for older callers. */
  credentialRef?: CredentialRef | null;
  /** Active account per provider for mixed-forge reconcile batches. */
  credentialRefs?: CredentialRef[];
  orgRoot: string;
  orgSlug: string;
  team: string;
  workspaceRoot: string;
}) => invoke<ReconcileOutcome[]>("reconcile_workspace", params);

/** SW-MC-5 AC-10: one row of the reconcile preview (Screen 11) — what a run
 *  WOULD do, without doing it (serde `tag: "action"`, mirrors Rust `ReconcilePlan`).
 *  Shares the backend's `plan_reconcile`, so the preview can never drift from the run. */
export type ReconcilePlan =
  | { action: "attach"; slug: string; path: string }
  | { action: "preparationRequired"; slug: string; reason: string }
  | { action: "alreadyAttachedElsewhere"; slug: string; path: string }
  | { action: "noOp"; slug: string; path: string }
  | { action: "managedGone"; slug: string; path: string }
  | { action: "notTracked"; origin: string; path: string };

/** SW-MC-5 (AC-10): read-only preview of a reconcile — scan + classify only, no
 *  clone/move. Backs the Screen-11 preview (matched → attach, missing → prepare externally,
 *  ignored). Runs on the blocking pool backend-side, so it never stalls the UI. */
export const previewWorkspaceReconcile = (params: {
  githubUserId: number;
  orgRoot: string;
  orgSlug: string;
  team: string;
  workspaceRoot: string;
}) => invoke<ReconcilePlan[]>("preview_workspace_reconcile", params);

/** SW-MC-4 AC-4: one divergent managed clone — its slug, current path (`from`),
 *  and resolved destination (`to`). Mirrors Rust `RehomePlanItem`. */
export interface RehomePlanItem {
  slug: string;
  from: string;
  to: string;
}

/** SW-MC-4 AC-4: read-only divergence summary behind the Screen-13 badge.
 *  `targetRoot` is the effective root (count label + dialog destination);
 *  `atTarget` are the clones already home; `divergent` are the movable ones.
 *  Mirrors Rust `ManagedCloneRehomePreview`. */
export interface ManagedCloneRehomePreview {
  targetRoot: string;
  atTarget: string[];
  divergent: RehomePlanItem[];
}

/** SW-MC-4 AC-4 (AC-10): detect which managed clones sit outside the org's
 *  effective clone root (records-only, no filesystem scan; resolves the root —
 *  incl. the cleared→hidden-default reverse case — server-side). Cheap enough to
 *  back the always-on settings divergence badge. */
export const previewManagedCloneRehome = (params: {
  githubUserId: number;
  orgRoot: string;
  orgSlug: string;
  team: string;
}) => retiredRepositoryAction<ManagedCloneRehomePreview>("preview_managed_clone_rehome", params);

/** SW-MC-4 AC-2/AC-7 (AC-10): move ONLY the managed clones that diverge from the
 *  effective root, each through MC-5's landed engine. Deliberately NOT
 *  `reconcileWorkspace` — it never clones-missing or attaches-found. Runs on the
 *  blocking pool and streams `reconcile-workspace-progress`; resolves with one
 *  {@link ReconcileOutcome} per moved clone. */
export const rehomeManagedClones = (params: {
  githubUserId: number;
  credentialRef: CredentialRef | null;
  orgRoot: string;
  orgSlug: string;
  team: string;
}) => retiredRepositoryAction<ReconcileOutcome[]>("rehome_managed_clones", params);

export interface LocalEditOutcome {
  commit_message: string;
  person?: Person;
}

export const commitRegistryEditLocal = (
  githubUserId: number,
  orgSlug: string,
  edit: RegistryEdit,
) => invoke<LocalEditOutcome>("commit_registry_edit_local", { githubUserId, orgSlug, edit });

/**
 * SW-OC-3 — `remove_source_repo_from_team`. Detach (stop tracking) a source repo
 * from a team — the inverse of the `AddSourceRepoToTeam` registry edit the attach
 * flow commits. Composes the existing `RemoveSourceRepoFromTeam` registry edit
 * (local working-tree only, no PR) AND clears the matching attachment *record*.
 * **Does NOT delete the user's local clone on disk** — this is untrack, not
 * uninstall. Powers the "Remove" action in the Manage repos surface.
 */
export const removeSourceRepoFromTeam = (input: {
  githubUserId: number;
  orgSlug: string;
  teamName: string;
  owner: string;
  repo: string;
  /** The repo's forge; without it the team's tracked `owner/repo` is used. */
  provider?: string | null;
  host?: string | null;
}) =>
  invoke<void>("remove_source_repo_from_team", {
    githubUserId: input.githubUserId,
    orgSlug: input.orgSlug,
    teamName: input.teamName,
    owner: input.owner,
    repo: input.repo,
    provider: input.provider ?? null,
    host: input.host ?? null,
  });

/**
 * SW-ID-3: re-pull an existing roster member's name + avatar from GitHub,
 * writing the update to the working tree (local-only, no commit — syncs on
 * `/cs-publish`). Returns the refreshed `Person` so the caller can reflect it
 * in-store without a reload. `githubUserId` is the caller (for path
 * resolution); `targetGithubUserId` is the member being refreshed.
 */
export const refreshPersonProfile = (
  githubUserId: number,
  orgSlug: string,
  targetGithubUserId: number,
) =>
  retiredServiceAction<Person>("refresh_person_profile", {
    githubUserId,
    orgSlug,
    targetGithubUserId,
  });

export interface BootstrapResult {
  reposChecked: number;
  reposSeeded: number;
  alreadyComplete: number;
  errors: string[];
}

export const bootstrapWorkspaceContentCmd = (orgRoot: string) =>
  invoke<BootstrapResult>("bootstrap_workspace_content_cmd", { orgRoot });

/** SW-FG-10a: the pulled team config's REQUIRED providers reconciled against what the
 *  user actually has — `gaps` are required+supported+not-connected (10b can guide a
 *  connect), `unsupported` are required-but-no-driver. Read-only. Named alias (not an
 *  inline `Record`) so the IPC-contract test's invoke-type regex can parse it. */
export interface ReconcileReport {
  gaps: ReconcileEntry[];
  comingSoon: ReconcileEntry[];
  unsupported: ReconcileEntry[];
}

export interface ReconcileEntry {
  provider: string;
  repos: string[];
  /** Count omitted by the backend's bounded IPC projection. */
  omitted?: number;
}

export const reconcileRequiredProviders = (orgRoot: string) =>
  retiredServiceAction<ReconcileReport>("reconcile_required_providers", { orgRoot });

// === Git credential helper ===

export const getGitCredentialOverride = () =>
  retiredServiceAction<string[]>("get_git_credential_override");

// === Update commands ===

export interface UpdateInfo {
  version: string;
  currentVersion: string;
  body: string | null;
}

export interface UpdateError {
  kind: "auth" | "network" | "notFound" | "signature" | "unknown";
  message: string;
}

export const checkForUpdate = (simulate?: boolean) =>
  invoke<UpdateInfo | null>("check_for_update", { simulate });

export const downloadAndInstallUpdate = (simulate?: boolean) =>
  invoke<void>("download_and_install_update", { simulate });

// === Sync commands (pull, auto-commit, push) ===

// SW-GS-5 + SW-GS-20 — authenticated non-destructive pull (the Pull button).
// This is the ONE pull the app performs, and it is user-initiated: `pull_ff`
// never force-checks-out or auto-resolves. (SW-GS-27 removed the legacy
// `pull_context_repo`/`PullResult` auto-pull, whose force fast-forward silently
// clobbered uncommitted WIP.) Outcomes:
//  - `fastForwarded`  — clean tree behind origin → plain fast-forward.
//  - `mergedClean`    — SW-GS-20: dirty tree behind origin → inbound 3-way merge
//                        that preserved local WIP; `main` advanced, incoming
//                        landed, local edits kept uncommitted.
//  - `alreadyUpToDate`— nothing incoming.
//  - `needsResolve`   — a true same-line overlap (or local-only commits); the
//                        working tree is untouched, routed to the SW-GS-10
//                        conflict surface. Internally tagged on `kind`.
//  - `skippedDirty`   — SW-GS-31: `cleanOnly` was set (the background auto-sync
//                        path) and the tree was dirty at the fork, so the 3-way
//                        merge was deliberately skipped. A pure no-op — nothing
//                        merged, `main` unmoved. Only ever returned when the caller
//                        passes `cleanOnly: true`.
export type PullFfOutcome =
  | { kind: "fastForwarded"; from: string; to: string }
  | { kind: "mergedClean"; from: string; to: string }
  | { kind: "alreadyUpToDate" }
  | { kind: "needsResolve" }
  | { kind: "skippedDirty" };

// SW-GS-31: `cleanOnly` (default false) routes a DIRTY tree to `skippedDirty`
// instead of the SW-GS-20 3-way merge — the auto-sync-down driver's probe→lock
// race guard. The manual Pull button omits it (→ false, byte-identical behaviour).
export const pullFf = (repoPath: string, cleanOnly?: boolean) =>
  retiredRepositoryAction<PullFfOutcome>("pull_ff", { repoPath, cleanOnly });

export interface PushResult {
  status:
    | "committed_and_pushed"
    | "committed_local_only"
    | "diverged"
    | "network_error"
    | "no_remote";
  message?: string;
}

export const autoCommitAndPush = (repoPath: string, filePaths: string[], message: string) =>
  retiredRepositoryAction<PushResult>("auto_commit_and_push", { repoPath, filePaths, message });

// === Dependency manager ===

export interface GitStatus {
  available: boolean;
  path: string | null;
  version: string | null;
}

export interface AgentBinaryStatus {
  available: boolean;
  path: string | null;
  /**
   * SW-OB-12: detection of this agent's installed binary is still in flight
   * (background login-shell PATH probe). While true, treat `available: false`
   * as "not yet known" — show the splash, not the missing-dependency guidance.
   */
  resolving: boolean;
  /** The resolved binary's version (or a below-floor version found, when unavailable). */
  version: string | null;
  /** Where the binary was found: the login-shell PATH, or a fixed fallback location. */
  source: "loginPath" | "knownLocation" | null;
  /** Other installs found and not used. `onPath` is false for an install off the PATH. */
  shadowed: ShadowedAgentInstall[];
  /** Whether `version` is inside the range CodeSense is qualified on; null when unknown. */
  qualified: boolean | null;
  /** The qualified range, as a person reads it; null when this CLI has none. */
  supported: string | null;
}

export interface ShadowedAgentInstall {
  path: string;
  version: string | null;
  onPath: boolean;
}

export interface DependencyStatus {
  git: GitStatus;
  /**
   * Per-agent binary-detection status keyed by CLI basename (`claude`, `codex`).
   * (Distinct from the runtime `AgentStatus` union in `agent-status-config`.)
   */
  agents: Record<string, AgentBinaryStatus>;
}

export const getDependencyStatus = () => invoke<DependencyStatus>("get_dependency_status");

/** The path a launch of the agent CLI `basename` should spawn, checked by the backend at
 *  launch time (a moved or reinstalled CLI is re-resolved). `null` when none is usable. */
export const resolveAgentCli = (basename: string) =>
  invoke<string | null>("resolve_agent_cli", { basename });

/**
 * SW-SD-2: run the user's OWN configured headless agent CLI over `{prompt, data}`
 * and resolve with the analysis text. codesense stores no LLM credential — the CLI
 * owns its auth. Rejects with a typed error string the caller branches on:
 * `"busy"` (single-flight), `"timeout: …"`, or `"unavailable: …"` (no CLI / not
 * logged in). Exposed to dashboards via the `analyze` bridge verb
 * (`html-bridge.ts`), never called directly by doc content. */
export const analyze = (args: { prompt: string; data: string }) => invoke<string>("analyze", args);

/**
 * SW-DX-11: the `runSkill` bridge verb — the WRITE-CAPABLE, multi-turn,
 * tool-enabled sibling of {@link analyze}. Runs a registered `skill` (with
 * `args`) headless on the user's agent CLI and resolves the completion string.
 * Bounded by its OWN Rust `SpawnCap`; rejects with a typed prefix (`busy` /
 * `timeout: …` / `unavailable: …`). Exposed to trusted+granted dashboards via the
 * `runSkill` bridge verb, two-axis gated (verb grant + skill grant) trusted-side.
 */
export const runSkill = (skill: string, args: string[], frameId?: string, page?: string) =>
  invoke<string>("run_skill", { skill, args, frameId, page });

/** The runtime launch an agent resolves to (`agent_launch_prepare`'s `launch`): the CLI it
 *  runs and the argv after the agent path. */
export interface AgentLaunchProfile {
  id: string;
  binary: string;
  args: string[];
  escalating: boolean;
  /** The agent's MCP-server allowlist (`null`/absent = unfiltered). */
  mcp_allow?: string[] | null;
  /** Codex bridge restriction: null allows all, [] allows none. */
  bridge_tools?: string[] | null;
  effective_bridge_tools?: string[];
  mode: LaunchMode;
  /** The runtime's env-strip set (Codex → `OPENAI_API_KEY`, Claude → `ANTHROPIC_API_KEY`). */
  env_suppress?: string[] | null;
  /** The agent's own environment variables, set under CodeSense's (absent = none). */
  env?: Record<string, string>;
  account_requirement?: AgentAccountRequirement;
}

/** A warning shown before an agent runs; never enforced. */
export interface AgentLaunchWarning {
  code: string;
  message: string;
}

/** SW-OB-12: manual re-check — force a full dependency re-resolution off the
 * main thread, then emit `dep-status-changed` for the existing refresh path. */
export const recheckDependencies = () => invoke<DependencyStatus>("recheck_dependencies");

/** SW-OB-14: outcome of triggering the macOS Command Line Tools install (git). */
export interface CltInstallResult {
  status: "triggered" | "already_installed" | "unsupported" | "error";
  message?: string;
}

/** SW-OB-14: trigger Apple's Command Line Tools installer (`xcode-select --install`),
 * which provides git. Launches Apple's GUI installer and returns immediately — the
 * user completes the dialog, then hits Re-check. */
export const installCommandLineTools = () => invoke<CltInstallResult>("install_command_line_tools");

// SW-OB-12: codesense no longer manages the Claude binary or its login — it uses
// the user's own installed-and-logged-in `claude`. Removed installClaude,
// checkClaudeAuth, signOutClaude, and the ClaudeAuthResult type.

// Logging
export const getLogFilePath = () => invoke<string>("get_log_file_path");
export const getLogDebugEnabled = () => invoke<boolean>("get_log_debug_enabled");

// HTTP API
export const getHttpApiPort = () => invoke<number | null>("get_http_api_port");

// === Registry loader ===

export const loadOrgCmd = (
  slug: string,
  orgRoot: string,
  remoteUrl: string | null,
  principal: string | null = null,
) =>
  invoke<LoadedOrg>("load_org_cmd", {
    slug,
    orgRoot,
    remoteUrl,
    ...(principal === null ? {} : { principal }),
  });

// === SW-CR-8: retired install-tree fold / heal ======================

/**
 * SW-CR-8: a single file in `{team}/install/` the resolver no longer reads
 * (the retired install layer). One row of the validate finding; relpath is
 * the path under both `install/` and (if conflicting) `content/`.
 */
export type InstallLeftoverFinding = {
  team: string;
  relpath: string;
};

/** Per-team outcome of the heal action — moved + conflict counts. */
export type TeamHealResult = {
  team: string;
  moved: number;
  installRemoved: boolean;
  /** `true` if the team's `.folded` marker short-circuited this pass. */
  skippedMarker: boolean;
  conflicts: InstallLeftoverFinding[];
};

export type HealResult = {
  teams: TeamHealResult[];
  totalMoved: number;
  totalConflicts: number;
};

/** Read-only: files still under `{team}/install/` that would never be
 * delivered to a launch. Powers the validate finding. */
export const checkInstallLeftoversCmd = (orgRoot: string) =>
  invoke<InstallLeftoverFinding[]>("check_install_leftovers_cmd", { orgRoot });

/** Re-run the fold migration ignoring any `.folded` marker — the one-click
 * heal action. */
export const healInstallTreeCmd = (orgRoot: string) =>
  invoke<HealResult>("heal_install_tree_cmd", { orgRoot });

// === Org management ===

export const discoverLocalOrgsCmd = (githubUserId: number) =>
  invoke<OrgSummary[]>("discover_local_orgs", { githubUserId });

/** SW-OR-9: the declared execution model of a launch profile. Mirrors Rust
 *  `LaunchMode` (serde `rename_all = "lowercase"`). `headless` runs with no
 *  terminal tab (observed in the Agents panel); consumed by SW-OR-10's launcher. */
export type LaunchMode = "interactive" | "headless";

export const AGENT_PROVIDERS = ["claude", "codex"] as const;
export type AgentProvider = (typeof AGENT_PROVIDERS)[number];
export type AgentAccountAuthState =
  | "not_configured"
  | "authentication_required"
  | "ready"
  | "error";

export interface AgentAccountView {
  id: string;
  provider: AgentProvider;
  label: string;
  authState: AgentAccountAuthState;
  authCheckedAt?: number | null;
  authError?: string | null;
  identityDisplayHint?: string | null;
  identityVerification: "verified" | "unverified";
  generation: number;
  createdAt: number;
  updatedAt: number;
  detachedAt?: number | null;
}

export const listAgentAccounts = (githubUserId: number) =>
  invoke<AgentAccountView[]>("list_agent_accounts", { githubUserId });

export const listDetachedAgentAccounts = (githubUserId: number) =>
  invoke<AgentAccountView[]>("list_detached_agent_accounts", { githubUserId });

export const createAgentAccount = (githubUserId: number, provider: AgentProvider, label: string) =>
  invoke<AgentAccountView>("create_agent_account", {
    githubUserId,
    input: { provider, label },
  });

export const renameAgentAccount = (
  githubUserId: number,
  accountId: string,
  expectedGeneration: number,
  label: string,
) =>
  invoke<AgentAccountView>("rename_agent_account", {
    githubUserId,
    accountId,
    expectedGeneration,
    label,
  });

export const detachAgentAccount = (
  githubUserId: number,
  accountId: string,
  expectedGeneration: number,
) =>
  invoke<AgentAccountView>("detach_agent_account", {
    githubUserId,
    accountId,
    expectedGeneration,
  });

export const reattachAgentAccount = (
  githubUserId: number,
  accountId: string,
  expectedGeneration: number,
) =>
  invoke<AgentAccountView>("reattach_agent_account", {
    githubUserId,
    accountId,
    expectedGeneration,
  });

export interface AgentUsageWindow {
  key: string;
  label: string;
  usedPercentage: number | null;
  remainingPercentage: number | null;
  resetsAt?: number | null;
  required: boolean;
}

export interface AgentUsageSnapshot {
  accountProfileId: string;
  provider: AgentProvider;
  availability: "unknown" | "available" | "exhausted" | "auth_required" | "error";
  allowanceWindows: AgentUsageWindow[];
  sequence: number;
  usageReceivedAt: number;
  source: string;
  usageFreshness: "awaiting_first_use" | "live" | "stale" | "unsupported";
  refreshState: "idle" | "queued" | "running" | "backoff";
}

export const readAgentUsage = (githubUserId: number) =>
  invoke<AgentUsageSnapshot[]>("read_agent_usage", { githubUserId });

export const setAgentUsageVisibility = (githubUserId: number, viewerId: string, visible: boolean) =>
  invoke<void>("set_agent_usage_visibility", { githubUserId, viewerId, visible });

export const refreshAgentUsage = (
  githubUserId: number,
  accountId: string,
  expectedGeneration: number,
) =>
  invoke<{
    outcome: "started" | "coalesced" | "queued" | "cached" | "unsupported";
    refreshState: string;
  }>("refresh_agent_usage", { githubUserId, accountId, expectedGeneration });

export const startAgentAccountLogin = (
  githubUserId: number,
  accountId: string,
  expectedGeneration: number,
) =>
  invoke<"started" | "coalesced">("start_agent_account_login", {
    githubUserId,
    accountId,
    expectedGeneration,
  });

export const cancelAgentAccountLogin = (githubUserId: number, accountId: string) =>
  invoke<boolean>("cancel_agent_account_login", { githubUserId, accountId });

export interface AgentAccountLoginState {
  accountId: string;
  generation: number;
  state: "starting" | "browser_opened" | "waiting" | "verifying" | "ready" | "error" | "cancelled";
}

export const listAgentAccountLoginStates = (githubUserId: number) =>
  invoke<AgentAccountLoginState[]>("list_agent_account_login_states", { githubUserId });

export const acknowledgeAgentAccountLifecycleEvent = (
  githubUserId: number,
  accountId: string,
  eventId: string,
) =>
  invoke<boolean>("acknowledge_agent_account_lifecycle_event", {
    githubUserId,
    accountId,
    eventId,
  });

export type AgentAccountRequirement = "none" | "optional" | "required";
export type AgentAccountResolutionSource = "explicit" | "active" | "system_default" | "none";
export type AgentAccountResolutionReason =
  | "explicit-override"
  | "active-selection"
  | "optional-system-default"
  | "account-not-required"
  | "required-missing"
  | "detached"
  | "auth-required"
  | "provider-mismatch"
  | "generation-mismatch"
  | "account-missing"
  | "focus-account-mismatch";
export type AgentAccountResolutionAvailability =
  | "available"
  | "unavailable"
  | "exhausted"
  | "auth_required"
  | "error"
  | "unknown"
  | "not_applicable";
export type AgentAccountResolutionFreshness =
  | "awaiting_first_use"
  | "live"
  | "stale"
  | "unsupported"
  | "unknown"
  | "not_applicable";

export interface ActiveAgentAccount {
  provider: AgentProvider;
  accountId: string | null;
  accountGeneration: number | null;
  revision: number;
  updatedAt: number;
}

export interface ActiveAgentAccountMutation {
  status: "changed" | "conflict";
  previous: ActiveAgentAccount | null;
  current: ActiveAgentAccount | null;
}

export interface AgentAccountResolution {
  runtime: AgentProvider;
  source: AgentAccountResolutionSource;
  accountId?: string | null;
  provider?: AgentProvider | null;
  label?: string | null;
  generation?: number | null;
  reason: AgentAccountResolutionReason;
  availability: AgentAccountResolutionAvailability;
  freshness: AgentAccountResolutionFreshness;
  spawnEligible: boolean;
}

export interface ResolveAgentAccountInput {
  overrideAccountId?: string;
  overrideAccountGeneration?: number;
  useSystemDefault?: boolean;
  /** The runtime whose account policy applies; omitted = Claude. */
  runtime?: AgentProvider;
}

export const getActiveAgentAccount = (provider: AgentProvider) =>
  invoke<ActiveAgentAccount | null>("get_active_agent_account", { provider });

export const setActiveAgentAccount = (
  provider: AgentProvider,
  expectedRevision: number,
  accountId?: string,
  expectedAccountGeneration?: number,
) =>
  invoke<ActiveAgentAccountMutation>("set_active_agent_account", {
    provider,
    expectedRevision,
    accountId: accountId ?? null,
    expectedAccountGeneration: expectedAccountGeneration ?? null,
  });

export const previewAgentAccountLaunch = (input: ResolveAgentAccountInput) =>
  invoke<AgentAccountResolution>("preview_agent_account_launch", { input });

/** SW-AGR-9: a tab's wake history (its timer popover), oldest first. `machineId`
 * names a remote tab's machine; omitted, this Mac. */
export const wakeHistory = (sessionId: string, machineId?: string) =>
  invoke<WakeHistoryEntry[]>("wake_history", {
    sessionId,
    machineId: machineId ?? null,
  });

/** SW-AGR-3: what a session timer should do ("Remind this agent…"). Exactly one
 * of `in` (a duration: `20m`, `1h30m`) or `at` (RFC 3339 with offset); `every`
 * repeats (at least `1m`), then at most one of `until` or `count`. */
export interface SessionTimerAsk {
  in?: string;
  at?: string;
  every?: string;
  until?: string;
  count?: number;
  prompt: string;
  name?: string;
}

/** SW-AGR-3: a timer that was set. `nextAt` is RFC 3339 (UTC). */
export interface ScheduledSessionTimer {
  timerId: string;
  nextAt: string;
  name: string;
  machineId: string;
  sessionId: string;
}

/** SW-AGR-3: one active timer of a tab, as its terminal server holds it. */
export interface SessionTimerRow {
  timerId: string;
  name: string;
  prompt: string;
  nextAt: string;
  every: string | null;
  until: string | null;
  count: number | null;
  remaining: number | null;
  fires: number;
  createdBy: string;
  machineId: string;
  sessionId: string;
}

/** SW-AGR-3 "Remind this agent…": set a timer on a tab (local, or on `machineId`)
 * with the person as its author. Only the app window may call it. */
export const remindSession = (sessionId: string, timer: SessionTimerAsk, machineId?: string) =>
  invoke<ScheduledSessionTimer>("remind_session", {
    sessionId,
    machineId: machineId ?? null,
    timer,
  });

/** SW-AGR-3: a tab's active timers (its timer popover), soonest first. */
export const sessionTimers = (sessionId: string, machineId?: string) =>
  invoke<SessionTimerRow[]>("session_timers", {
    sessionId,
    machineId: machineId ?? null,
  });

/** SW-AGR-3: cancel one of a tab's timers; whether it was active. */
export const cancelSessionTimer = (sessionId: string, timerId: string, machineId?: string) =>
  invoke<boolean>("cancel_session_timer", {
    sessionId,
    machineId: machineId ?? null,
    timerId,
  });

export const grantTrustedCapability = (
  capability: "manage_agent_accounts",
  ttlSeconds: number,
  budget: number,
) => invoke<void>("grant_trusted_capability", { capability, ttlSeconds, budget });

/** Fresh, backend-selected argv for a runtime that must own its PTY directly. */
export interface ScopedExecution {
  argv: string[];
  nativeLaunch: NonNullable<SpawnPtyOptions["nativeLaunch"]>;
}

/** Renderer-safe scoped-launch additions. Environment values remain in Rust
 * and cross this boundary only as an opaque one-use binding. */
export interface McpScopeOutcome {
  execution?: ScopedExecution;
  serverAliases?: Record<string, string>;
  extraArgs: string[];
  removeArgs?: string[];
  environmentBinding?: string | null;
}

/** The account the terminal spawn of an agent launch resolves: the agent runtime's own
 *  policy, with this user's binding for the agent as the explicit override. */
export interface AgentLaunchAccount {
  runtime: AgentRuntime;
  overrideAccountId?: string | null;
  overrideAccountGeneration?: number | null;
}

/** A named agent prepared for one terminal session (`agent_launch_prepare`). */
export interface PreparedAgentLaunch {
  /** Canonical product root prepared by the backend, when applicable. */
  cwd?: string | null;
  /** The runtime's CLI as the backend resolved it; the launch line runs it. */
  agentPath: string;
  /** The runtime's launch profile; `args` already carry the per-launch settings flag. */
  launch: AgentLaunchProfile;
  account: AgentLaunchAccount;
  /** `<key>@<revision>` — pass it to `dashboardLaunchBind` as `agentRevision`. */
  agent: string;
  manifestHash: string;
  warnings: AgentLaunchWarning[];
  /** The agent's own first prompt, for a launch that brings none. */
  defaultPrompt?: string | null;
  /** The CLI receives the starting task and role facts on its own startup argv. */
  promptInArgv?: boolean;
  /** The agent's display name: the title its terminal tab gets when the launcher names none. */
  name: string;
  /** The MCP scope projected for the session. */
  scope: McpScopeOutcome;
}

/** Prepare the agent `agentKey` (a key, or an id in `team` then the personal space) for the
 *  terminal session `sessionId` about to spawn in `cwd`: the backend resolves it, projects its
 *  MCP scope and writes its permission files for that session. Spawn with exactly what comes
 *  back; `warnings` never block the launch. */
export const agentLaunchPrepare = (p: {
  newInstance?: boolean;
  agentKey: string;
  frameId?: string;
  pagePath?: string;
  team?: string;
  cwd: string;
  sessionId: string;
  mode?: LaunchMode;
  prompt?: string;
  /** An account for this launch only, at the generation the launcher saw; the user's
   *  binding stays as it is. */
  accountId?: string;
  accountGeneration?: number;
}) =>
  invoke<PreparedAgentLaunch>("agent_launch_prepare", {
    newInstance: p.newInstance ?? false,
    agentKey: p.agentKey,
    frameId: p.frameId ?? null,
    pagePath: p.pagePath ?? null,
    team: p.team ?? null,
    cwd: p.cwd,
    sessionId: p.sessionId,
    mode: p.mode ?? null,
    prompt: p.prompt ?? null,
    accountId: p.accountId ?? null,
    accountGeneration: p.accountGeneration ?? null,
  });

/** The CLI an agent runs on (mirror of Rust `agents::Runtime`). */
export type AgentRuntime = "claude" | "codex" | "opencode" | "hermes";

/** How a launch runs an agent unless the launcher overrides it. */
export type AgentLaunchDefault = "foreground" | "headless";

/** Which account this user would launch an agent with (mirror of Rust `AccountItem`). */
export interface AgentAccountChoice {
  /** `binding`: this user bound the agent to an account; `runtime_default`: the runtime's
   *  active account, or the CLI's own login; `chosen`: a launch named its own account. */
  source: "binding" | "runtime_default" | "chosen";
  bound_account_id: string | null;
  resolved_source: AgentAccountResolutionSource;
  account_id: string | null;
  provider: AgentProvider | null;
  label: string | null;
  reason: AgentAccountResolutionReason;
  availability: AgentAccountResolutionAvailability;
  spawn_eligible: boolean;
  store_error?: string;
  /** This user consented to a raw terminal snapshot of the agent's interactive launches. */
  capture_raw_terminal: boolean;
}

/** Whose space an agent lives in: yours, a team's, or `builtin` for the read-only starters
 *  CodeSense ships (`builtin/claude`, `builtin/codex`). */
export type AgentScope = "personal" | "team" | "builtin";

/** One `agents_list` row. */
export interface AgentRow {
  key: string;
  name: string;
  description: string;
  runtime: AgentRuntime;
  launch: AgentLaunchDefault;
  scope: AgentScope;
  org: string;
  team: string;
  /** The creator's display name. Attribution only. */
  created_by: string;
  /** The caller created it. Only credits it: anyone can change any agent. */
  created_by_you: boolean;
  archived: boolean;
  revision: number;
  default_prompt: string;
  /** The face the agent definition picked (`pack:id`), or null to let the renderer pool-deal
   *  a stable face from the agent id. Mirrors the Rust `Agent::avatar`. */
  avatar: string | null;
  role?: string | null;
  /** The model its args name (`--model`, `--model=`, `-m`); null is the CLI default. */
  model: string | null;
  account: AgentAccountChoice;
  /** The routines (that you can see) naming it, by key. */
  used_by: string[];
  /** Its own active routines (they live under it): archiving it archives them too, so the
   *  confirm lists them. */
  routines: AgentOwnedRoutine[];
  /** The routines its archive archived, still archived: offered back ("Restore routines")
   *  until restored. */
  archived_routines: AgentOwnedRoutine[];
  /** Live sessions running it, at any revision. */
  running_now: number;
}

/** One routine of an agent, as the archive confirm and the unarchive offer list it. */
export interface AgentOwnedRoutine {
  key: string;
  name: string;
}

/** The runtime's own permission config, passed to the CLI verbatim (mirror of Rust
 *  `agents::model::Permissions`). Only the block of the agent's runtime applies; the other is
 *  kept so switching runtime loses nothing. */
export interface AgentPermissions {
  claude?: {
    allow?: string[];
    ask?: string[];
    deny?: string[];
    additional_directories?: string[];
    default_mode?: string;
    disable_bypass_permissions_mode?: string;
  } | null;
  codex?: { approval_policy?: string; sandbox_mode?: string } | null;
  opencode?: { permission?: Record<string, string>; [key: string]: unknown } | null;
  hermes?: { approval?: "ask" | "yolo"; toolsets?: string[]; deny?: string[] } | null;
}

/** An agent definition as stored (mirror of Rust `agents::Agent`). */
export interface AgentDefinition {
  role?: string | null;
  id: string;
  name: string;
  description?: string;
  created_by: string;
  runtime: AgentRuntime;
  args?: string[];
  startup_settings?: Record<string, unknown> | null;
  /** Environment variables every launch sets on the CLI, by name. CodeSense's own launch
   *  variables win over one of the same name. The editor never sends it (a form save keeps it). */
  env?: Record<string, string>;
  launch?: AgentLaunchDefault;
  mcp?: { servers?: string[]; bridge_tools?: string[] | null };
  startup?: string[];
  /** SW-XS-13: the context sets a launch delivers; absent/null = the runtime default. The
   *  editor never sends it (a form save keeps it); the Context section edits it through
   *  `agentContextUpdate`. */
  context?: AgentContextEntry[] | null;
  permissions?: AgentPermissions | null;
  default_prompt?: string;
  /** The face the definition picked (`pack:id`), or null/absent to let the renderer pool-deal
   *  a stable face from the agent id. */
  avatar?: string | null;
  /** SW-AGR-19 PR-6: relaunched with a resume brief after a terminal server update. */
  resumeAfterRestart?: boolean;
  revision: number;
  archived_at?: string | null;
}

/** A generated launch entry: the agent's launch settings (its native permissions plus
 *  CodeSense's required allows), or (SW-XS-14, `workspace_file`) a merge-marked content file
 *  deep-merged across its layers. */
export interface AgentGeneratedEntry {
  id: string;
  target: "launch_settings" | "workspace_file";
  content: Record<string, unknown>;
  hash: string;
  /** The rules CodeSense added. */
  required: string[];
  /** `workspace_file` only: the workspace-relative path the merged file is written to. */
  path?: string;
  /** `workspace_file` only: the merge mark's array keys (dot path → key field). */
  merge_keys?: Record<string, string>;
}

/** A content layer: a team's or your personal space's content (mirror of Rust `ContentLayer`). */
export type AgentContentLayer =
  | { kind: "team"; org: string; team: string }
  /** `org` absent = the all-orgs scratch folder. */
  | { kind: "personal"; account: string; org?: string };

/** Why an entry is in a startup manifest. */
export type AgentEntryOrigin =
  | { kind: "declared" }
  | { kind: "dependency"; of: string }
  | { kind: "required" };

/** One startup path expanded to its files, from one layer. */
export interface AgentManifestEntry {
  path: string;
  origin: AgentEntryOrigin;
  layer: AgentContentLayer;
  files: Array<{ logical: string; bytes: number }>;
}

/** The resolved startup manifest applied by named local product launches. */
export interface AgentStartupManifest {
  agent: { key: string; revision: number; definition_hash: string };
  runtime: AgentRuntime;
  /** Declared paths in order, each followed by what it requires, then CodeSense's own (plain
   *  paths; nothing is classified by kind). */
  entries: AgentManifestEntry[];
  /** Generated items CodeSense always provides, which have no source file. */
  essentials: Array<{ id: string; reason: string }>;
  generated: AgentGeneratedEntry[];
  totals: { files: number; bytes: number };
  errors: Array<{ path: string | null; code: string; message: string }>;
  manifest_hash: string;
}

/** What a launch of an agent would run, for this user (mirror of Rust `ResolvedItem`). */
export interface AgentModelArguments {
  input_args: string[];
  model: string | null;
  edit: { args: string[]; clear_args: string[]; value_index: number } | null;
}
export interface AgentLaunchAdmission {
  model_arguments?: AgentModelArguments;
  permission_display?: string;
  initial_prompt_max_bytes?: number;
  initial_prompt_too_long?: string;
  allowed: boolean;
  reason: string | null;
  model_endpoint: boolean;
  model: { default?: string; provider?: string; base_url?: string } | null;
}

export interface ResolvedAgentView {
  launch_admission?: AgentLaunchAdmission;
  agent: { key: string; revision: number; definition_hash: string };
  /** `escalating`: the launch auto-approves tool use (a flag or a permission mode). Shown,
   *  never refused. */
  launch: { id: string; binary: string; args: string[]; escalating: boolean };
  account: AgentAccountChoice;
  manifest: AgentStartupManifest;
  provides: { commands: string[]; skills: string[] };
  warnings: AgentLaunchWarning[];
  /** SW-AN-50: every MCP server the target team's config offers (the bridge aside). Lets the
   *  editor and starter detail list what a copy could grant without a separate call. */
  offered_mcp_servers: string[];
  /** Whether all launch paths apply the manifest; false during the local product rollout. */
  manifest_applies_at_launch: boolean;
  archived: boolean;
}

/** One revision of an agent: who saved it, when, and the definition as saved. */
export interface AgentRevision {
  revision: number;
  author: RoutineAuthor;
  at: string;
  agent: AgentDefinition;
}

/** `agent_detail`: the definition, its history and what a launch would run. */
export interface AgentDetailView {
  key: string;
  scope: AgentScope;
  created_by: string;
  created_by_you: boolean;
  definition: AgentDefinition;
  revisions: AgentRevision[];
  resolved: ResolvedAgentView;
}

/** What an archive, unarchive or copy did; a stale base is an answer, not an error. */
export type AgentWriteReply =
  | {
      outcome: "saved";
      key: string;
      revision: number;
      archived: boolean;
      /** Archive: the routines archived with it. Unarchive: the routines its archive
       *  archived, still archived, to offer back (`agentRestoreRoutines`). */
      routines?: AgentOwnedRoutine[];
    }
  | { outcome: "conflict"; current_revision: number };

/** What a delete did: routines still using the agent refuse it and are listed. */
export type AgentDeleteReply =
  | { outcome: "deleted"; key: string }
  | {
      outcome: "in_use";
      referrers: string[];
      /** Those of `referrers` that are its own routines; the rest only wait on it finishing. */
      owned: string[];
    }
  | { outcome: "conflict"; current_revision: number };

/** This user's machine-local settings for an agent: the account it runs with and the
 *  raw-terminal consent. */
export interface AgentBinding {
  /** `null` runs the runtime's default account. */
  account_id: string | null;
  /** False when the bound account no longer exists. */
  account_exists: boolean;
  /** Keep a raw snapshot of the terminal of this agent's interactive launches. */
  capture_raw_terminal: boolean;
}

/** Every agent you can see (personal and every synced team's), with its account line, the
 *  routines using it and how many sessions run it. */
export const agentsList = (includeArchived = false) =>
  invoke<AgentRow[]>("agents_list", { includeArchived });

/** Where an avatar pack comes from: the active team's content, your own scratch space, or the app. */
export type AvatarPackScope = "team" | "personal" | "builtin";

/** One face of an avatar pack, ready to draw. */
export interface AvatarFaceView {
  id: string;
  /** `pack:id`, what a launch's `avatar` names. */
  ref: string;
  label: string | null;
  /** An image URL (a `data:` URL for a pack on disk). */
  src: string;
}

export interface AvatarPackView {
  id: string;
  name: string;
  scope: AvatarPackScope;
  /** A team pack you also have in your personal space (one you shared): listed once, as the
   *  team's. Absent for the built-in packs. */
  personal_copy?: boolean;
  faces: AvatarFaceView[];
}

/** The avatar packs on disk for the active team, and what was skipped. */
export interface AvatarPacksView {
  packs: AvatarPackView[];
  /** The pack the team deals faces from for agents nobody gave one; null = the built-in funny pack. */
  default_pack: string | null;
  /** Each agent's own default face (`pack:id`, a colour or an emoji), by agent key. */
  agent_avatars: Record<string, string>;
  warnings: string[];
}

/** A team's avatar packs and your personal ones in its org, for any team of yours, by its key
 *  (`org:team`). The built-in packs ship with the webview and are not listed. */
export const avatarPacksListForTeam = (teamKey: string) =>
  invoke<AvatarPacksView>("avatar_packs_list_for_team", { teamKey });

/** Set the active team's default pack (null: back to the built-in funny pack). Written to the
 *  team's content, so teammates get it with the next sync. */
export const avatarDefaultPackSet = (pack: string | null) =>
  invoke<void>("avatar_default_pack_set", { pack });

/** Copy one of your own avatar packs into the active team's content, so every teammate gets its
 *  faces under the same `pack:id` (your pack stays, for your org's other teams). Rejects when
 *  the team already has a pack with that id or is not synced here. */
export const avatarPackShareWithTeam = (pack: string) =>
  invoke<void>("avatar_pack_share_with_team", { pack });

export const agentDetail = (key: string) => invoke<AgentDetailView>("agent_detail", { key });

export const agentDefinitionsPrepare = (
  detailKey?: string,
  frameId?: string,
  visible = true,
  sequence = 0,
) =>
  invoke<import("@/features/dashboard/lib/live-hub").HubSnapshot>("agent_definitions_prepare", {
    detailKey,
    frameId,
    visible,
    sequence,
  });

/** Archive an agent. One with routines is refused unless `archiveRoutines` (the user
 *  confirmed the list): they are archived with it. */
export const agentArchive = (key: string, archiveRoutines = false) =>
  invoke<AgentWriteReply>("agent_archive", { key, archiveRoutines });

/** Unarchive the routines an agent's archive archived, after its unarchive offered them. */
export const agentRestoreRoutines = (key: string) =>
  invoke<AgentOwnedRoutine[]>("agent_restore_routines", { key });

export const agentUnarchive = (key: string, baseRevision: number) =>
  invoke<AgentWriteReply>("agent_unarchive", { key, baseRevision });

/** Delete an agent for good, unless routines still use it (`in_use`, with them listed). */
export const agentDelete = (key: string, baseRevision: number) =>
  invoke<AgentDeleteReply>("agent_delete", { key, baseRevision });

/** Clone a visible agent into your scratch space (`team` absent, the default) or into
 *  a locally-synced team you can write to (`team` set). The New agent dialog uses the
 *  team form so a template clone can land on the team directly. */
export const agentCopyToMine = (key: string, team?: { org: string; team: string }) =>
  invoke<AgentWriteReply>("agent_copy_to_mine", team ? { key, team } : { key });

/** Run the agent with one of your accounts and/or set your raw-terminal consent for it; what
 *  is left out stays. Only your own machine-local binding changes. */
export const agentBindingSet = (
  key: string,
  change: { accountId?: string; captureRawTerminal?: boolean },
) =>
  invoke<AgentBinding>("agent_binding_set", {
    key,
    accountId: change.accountId ?? null,
    captureRawTerminal: change.captureRawTerminal ?? null,
  });

/** Go back to your default account for the agent's runtime. */
export const agentBindingClear = (key: string) => invoke<boolean>("agent_binding_clear", { key });

/** The agent editor's form: exactly the fields the `agent_create` tool takes, plus where the
 *  agent lives. `key` names the agent an edit replaces; a create places it by `scope`. Every
 *  field is always sent, so an edit replaces the whole definition (a permission block left out
 *  is removed). */
export interface AgentForm {
  role?: string | null;
  key?: string;
  scope?: "personal" | "team";
  org?: string;
  team?: string;
  name: string;
  description: string;
  runtime: AgentRuntime;
  args: string[];
  startupSettings?: Record<string, unknown> | null;
  launch: AgentLaunchDefault;
  mcpServers: string[];
  /** Codex bridge names; omit to keep a saved restriction, null to clear it. */
  mcpBridgeTools?: string[] | null;
  startup: string[];
  permissions: AgentPermissions | null;
  defaultPrompt: string;
  /** SW-AGR-19 PR-6: relaunched with a resume brief after a terminal server update. */
  resumeAfterRestart?: boolean;
}

/** What an editor save or restore did. A stale base is an answer (`conflict`), and a refused
 *  input names its field (`invalid`), exactly as the agent MCP tools do. */
export type AgentSaveReply =
  | { outcome: "saved"; key: string; revision: number; warnings: AgentLaunchWarning[] }
  | { outcome: "conflict"; current_revision: number }
  | { outcome: "invalid"; field: string | null; message: string };

/** What the editor's live preview resolved, or which input stops the draft resolving. */
export type AgentPreviewReply =
  | {
      outcome: "resolved";
      resolved: ResolvedAgentView;
      /** The MCP servers the team it resolved against offers, the bridge aside. */
      servers: string[];
    }
  | { outcome: "invalid"; field: string | null; message: string };

/** A team by org and name. */
export interface AgentTeamRef {
  org: string;
  team: string;
}

/** One page of one directory level of a team's content across its layers, for the startup
 *  picker. */
export interface AgentStartupCatalog {
  /** The directory listed; empty for the content root. */
  dir: string;
  /** This page: folders first, then files, each by name. */
  entries: Array<{ path: string; is_dir: boolean; layer: AgentContentLayer }>;
  /** How many names at this level match the filter, across every page. */
  total: number;
  /** More entries follow this page. */
  truncated: boolean;
  /** The `offset` that asks for the next page, while one follows. */
  next_offset: number | null;
}

/** Whether "Describe it to an agent" opened one, and what to tell the user when not. */
export interface AgentAuthoring {
  opened: boolean;
  message: string;
}

/** Create an agent (no `form.key`) or replace one against `baseRevision`. */
export const agentSave = (form: AgentForm, baseRevision?: number) =>
  invoke<AgentSaveReply>("agent_save", { form, baseRevision: baseRevision ?? null });

/** What an unsaved form would resolve to, against `team` when given. Writes nothing. */
export const agentPreview = (form: AgentForm, team?: AgentTeamRef, cwd?: string) =>
  invoke<AgentPreviewReply>("agent_preview", {
    form,
    team: team ?? null,
    ...(cwd === undefined ? {} : { cwd }),
  });

/** SW-XS-15: one entry of an agent's `context` (or a repo override's `add`). */
export interface AgentContextEntry {
  path: string;
  dest?: string;
}

/** SW-XS-15: the tier a want-set file comes from. */
export type AgentContextTier =
  | { kind: "repo"; org: string; team: string; repo: string }
  | { kind: "personal"; org?: string }
  | { kind: "team"; org: string; team: string };

/** SW-XS-15: one directory want-set files come from, on this Mac. */
export interface AgentContextRoot {
  tier: AgentContextTier;
  path: string;
}

/** SW-XS-15: one file of an agent's want set. `root` indexes `AgentContextView.roots` (its
 *  tier and directory); `null` when its tier has no directory here. */
export interface AgentContextFile {
  source?: string;
  tier?: AgentContextTier;
  destination: string;
  logical: string;
  bytes: number;
  root: number | null;
}

/** SW-XS-15: one stored per-repo override of an agent's context. */
export interface AgentRepoOverride {
  /** `host/owner/repo`. */
  repo: string;
  add: AgentContextEntry[];
  remove: string[];
}

/** SW-XS-15: what the left nav's "Context for ‹Agent› · in ‹Repo›" overlay shows. With no
 *  repo it is what a team-workspace launch gets. */
export interface AgentContextView {
  key: string;
  name: string;
  /** What an edit's `baseRevision` must be. */
  revision: number;
  /** The stored `context`; `null` while the runtime default applies. */
  context: AgentContextEntry[] | null;
  /** What a team-wide edit writes out first while `context` is `null`. */
  runtime_default: AgentContextEntry[];
  startup: string[];
  repo_overrides: AgentRepoOverride[];
  /** `host/owner/repo` when resolved for a repo. */
  repo: string | null;
  defaulted: boolean;
  entries: Array<{
    path: string;
    dest?: string;
    source: "context" | "default" | "startup" | "repo_add";
    files: number;
    bytes: number;
  }>;
  roots: AgentContextRoot[];
  files: AgentContextFile[];
  truncated: boolean;
  totals: { files: number; bytes: number };
  warnings: Array<{ path?: string; code: string; message: string }>;
  /** SW-XS-16: the one-time `projections.yml` suggestion (XS-14) at this revision; absent or
   *  null once accepted, dismissed, or when there is nothing to suggest. */
  context_suggestion?: AgentContextSuggestion | null;
}

/** SW-XS-15: what one nav edit did. `saved` carries the want set re-resolved after the write. */
export type AgentContextEditReply =
  | { outcome: "saved"; key: string; revision: number; view: AgentContextView }
  | { outcome: "conflict"; current_revision: number }
  | { outcome: "blocked"; reason: string }
  | { outcome: "invalid"; message: string };

/** SW-XS-15: an agent's want set for `repo` (`owner/repo` or `host/owner/repo`; omitted = its
 *  team workspace). Writes nothing. */
export const agentContextPreview = (key: string, repo?: string) =>
  invoke<AgentContextView>("agent_context_preview", { key, repo: repo ?? null });

/** One `agent_context_update` operation, applied on the server to the stored definition:
 *  - `add` / `remove` a path (SW-XS-15; `add` may carry a `dest` remap, SW-XS-16);
 *  - `customize`: write the runtime default out as the agent's own list (SW-XS-16);
 *  - `add_entries`: add several entries in one revision, e.g. an accepted `projections.yml`
 *    suggestion (SW-XS-16);
 *  - `drop_remove`: with `repo`, drop that exact `remove` pattern (a glob included) from the
 *    repo's override (SW-XS-16);
 *  - `drop_add`: with `repo`, drop that `add` entry from the repo's override, never writing a
 *    `remove` (SW-XS-16).
 *  With `repo` the edit changes that repo's override ("… for ‹Repo› only"). */
export type AgentContextEdit =
  | { op: "add"; path: string; dest?: string; repo?: string; viewRepo?: string }
  | { op: "remove"; path: string; repo?: string; viewRepo?: string }
  | { op: "customize"; viewRepo?: string }
  | {
      op: "add_entries";
      /** Each entry's other stored keys ride along (sent as `keep`), so an Undo restores it
       *  exactly. */
      entries: AgentContextEntry[];
      /** The suggestion fingerprint this accepts: recorded as dismissed once saved. */
      accepts?: string;
      viewRepo?: string;
    }
  | { op: "drop_remove"; path: string; repo: string; viewRepo?: string }
  | { op: "drop_add"; path: string; repo: string; viewRepo?: string };

/** SW-XS-15: one context edit (the nav's "Add to / Remove from ‹Agent›'s context", the agent
 *  editor's Context section), applied on the server to the stored definition against
 *  `baseRevision`. The reply's view is re-resolved for `viewRepo`, the repo being shown. */
export const agentContextUpdate = (key: string, baseRevision: number, edit: AgentContextEdit) =>
  invoke<AgentContextEditReply>("agent_context_update", {
    key,
    baseRevision,
    op: edit.op,
    path: "path" in edit ? edit.path : null,
    dest: "dest" in edit ? (edit.dest ?? null) : null,
    entries:
      "entries" in edit
        ? edit.entries.map(({ path, dest, ...keep }) => ({
            path,
            ...(dest ? { dest } : {}),
            ...(Object.keys(keep).length > 0 ? { keep } : {}),
          }))
        : null,
    accepts: "accepts" in edit ? (edit.accepts ?? null) : null,
    repo: "repo" in edit ? (edit.repo ?? null) : null,
    viewRepo: edit.viewRepo ?? null,
  });

/** Make an older revision current again, as a new revision made against `baseRevision`. */
export const agentRestoreRevision = (key: string, revision: number, baseRevision: number) =>
  invoke<AgentSaveReply>("agent_restore_revision", { key, revision, baseRevision });

/** One page of one level of `team`'s content under `dir` (the content root when omitted), from
 *  `offset`, keeping names that contain `filter` (ignoring case). Never recurses. */
export const agentStartupCatalog = (
  team: AgentTeamRef,
  dir?: string,
  page?: { offset?: number; filter?: string },
) =>
  invoke<AgentStartupCatalog>("agent_startup_catalog", {
    team,
    dir: dir ?? null,
    offset: page?.offset ?? null,
    filter: page?.filter ?? null,
  });

/** Open a visible agent in the active team's workspace, asked to help create an agent. */
export const agentOpenAuthoringAgent = () => invoke<AgentAuthoring>("agent_open_authoring_agent");

/** What a person asks the launch sheet for: a named tab in a story's workspace (`story`) or its
 *  own team workspace (`team-workspace`), with its manager (`launchedBy`, that session's handle),
 *  role and face. The same open an agent makes over MCP, shown and focused. */
export interface AgentLaunchOpenArgs {
  org?: string;
  team?: string;
  worktree?: string;
  createWorktree?: boolean;
  queueRole?: string | null;
  target: "story" | "team-workspace";
  storyId?: string;
  name?: string;
  agent?: string;
  /** Its starting prompt (a /command works too); absent, the agent's own default prompt. */
  initialPrompt?: string;
  launchedBy?: string;
  role?: string;
  avatar?: string;
  /** The machine a story launch runs on (`"local"`: This Mac). A story's workspace stays
   *  where it was created: this picks where a new one is created, and must match an
   *  existing one. Story launches only. */
  machineId?: string;
}

/** The launch's outcome; `invalid`, `alreadyLive` and `busy` carry a plain sentence. */
export interface AgentLaunchOpenResult {
  worktree?: { id: string; name: string; branch: string; created: boolean };
  status: "opened" | "alreadyLive" | "busy" | "invalid";
  handle: string | null;
  name: string | null;
  message: string;
  warnings?: AgentLaunchWarning[];
}

export const agentLaunchOpen = (args: AgentLaunchOpenArgs) =>
  invoke<AgentLaunchOpenResult>("agent_launch_open", { args });

/** SW-AN-42 → SW-AN-45: the per-session config for an interactive launch, returned as the
 *  launch additions (agent-neutral). `sessionId` MUST be the launcher's PTY session id so
 *  `kill_session` reaps the 0600 scope dir. `mcpAllow = []` is a valid bridge-only scope;
 *  `null` keeps the agent's own servers and adds the bridge (and Claude's status hooks).
 *  Nothing is written into `workdir`. `launch.binary` (the CLI's basename) picks the runtime
 *  whose scope descriptor the projection uses (Codex `env_dir` vs Claude `config_flag`). */
export const projectMcpScopeConfig = (
  sessionId: string,
  workdir: string,
  mcpAllow: string[] | null,
  account?: ResolveAgentAccountInput,
  launchArgs?: string[],
  launch?: McpScopeLaunch,
) =>
  invoke<McpScopeOutcome>("project_mcp_scope_config", {
    sessionId,
    workdir,
    mcpAllow,
    account: account ?? null,
    launchArgs: launchArgs ?? null,
    launch: launch ?? null,
  });

/** What the renderer says about the launch a scope is projected for. */
export interface McpScopeLaunch {
  frameId?: string;
  pagePath?: string;
  teamKey?: string;
  /** The CLI's basename, which picks the runtime (claude or codex). Any other CLI gets
   *  nothing. */
  binary?: string;
  /** A named agent's relaunch: its scope alone. */
  agent?: boolean;
  /** Captured effective CodeSense bridge tool grant for a named-agent relaunch. */
  bridgeTools?: string[] | null;
  /** The launch runs the launcher's default args: the runtime's bridge override (Codex's
   *  `-c mcp_servers.codesense…`) is added. */
  defaultArgs?: boolean;
}

export interface CreateWorkspaceResult {
  org: LoadedOrg;
  operation?: {
    generation: string;
    stage:
      | "journaled"
      | "scaffolded"
      | "cancellingPreRemote"
      | "remoteConfirmed"
      | "pushedAndVerified"
      | "bindingPersisted"
      | "completed";
    remote: { provider: string; host: string; owner: string; repo: string };
  };
}

export interface SharedRemoteIntent {
  generation: string;
  provider: string;
  host: string;
  owner: string;
  repo: string;
  mode: "create" | "select";
  private: boolean | null;
  credentialRef: CredentialRef;
}

export type CreateWorkspaceError =
  | { kind: "failure"; message: string }
  | { kind: "remoteExists" }
  | { kind: "remoteNotFound" }
  | { kind: "remoteReadOnly" }
  | { kind: "remoteNotEmpty" }
  | { kind: "networkUnreachable" }
  | { kind: "boundCredential"; provider: string; account: string; cause: ConnectCredentialCause }
  | {
      kind: "partialSuccess";
      generation: string;
      stage: NonNullable<CreateWorkspaceResult["operation"]>["stage"];
      remote: { provider: string; host: string; owner: string; repo: string };
      message: string;
    };

export interface PendingOrgCreate {
  schema_version: number;
  generation: string;
  remote: { provider: string; host: string; owner: string; repo: string };
  scaffold: string;
  scaffold_fingerprint: string;
  workspace_name: string;
  credential_ref: CredentialRef;
  mode: "create" | "select";
  private: boolean | null;
  stage: NonNullable<CreateWorkspaceResult["operation"]>["stage"];
  created_at: string;
  updated_at: string;
}

type PendingOrgCreates = Record<string, PendingOrgCreate>;

export const listPendingOrgCreatesCmd = (githubUserId: number) =>
  retiredRepositoryAction<PendingOrgCreates>("list_pending_org_creates", { githubUserId });

export const cancelSharedOrgCreateCmd = (
  githubUserId: number,
  owner: string,
  repo: string,
  generation: string,
) =>
  retiredRepositoryAction<{ kind: "queued" | "tooLate" | "notFound" | "invalid" }>(
    "cancel_shared_org_create",
    {
      githubUserId,
      owner,
      repo,
      generation,
    },
  );

export const createWorkspaceCmd = (
  githubUserId: number,
  name: string,
  githubUsername: string,
  githubDisplayName: string,
  avatarUrl: string | null,
  remoteUrl: string | null = null,
  sharedRemote: SharedRemoteIntent | null = null,
) =>
  retiredRepositoryAction<CreateWorkspaceResult>("create_workspace", {
    githubUserId,
    name,
    githubUsername,
    githubDisplayName,
    avatarUrl,
    remoteUrl,
    sharedRemote,
  });

// Field names are snake_case to match the Rust struct — Tauri
// preserves serde-default field names on return types. Arguments
// going the other direction (JS → Rust) do get camelCase ↔ snake_case
// translation.
export interface ConnectOrgResult {
  org_slug: string;
  org_root: string;
  /** Server returns "cloned", "already_existed" or "adopted" (seeded from the user's clone). */
  clone_outcome: string;
  loaded_org: LoadedOrg;
}

export type ConnectCredentialCause = "signedOut" | "noToken" | "unavailable";
export type JoinFailureStage =
  | "clone"
  | "targetOccupied"
  | "notYourClone"
  | "invalidRegistry"
  | "bindingPersist"
  | "bindingVerify"
  | "cleanup";
export type ConnectOrgError =
  | { kind: "boundCredential"; provider: string; account: string; cause: ConnectCredentialCause }
  | { kind: "failure"; message: string }
  | { kind: "joinFailure"; stage: JoinFailureStage; message: string; path?: string };

export type JoinRemoteClassification =
  | {
      kind: "supported";
      provider: string;
      provider_name: string;
      /** Comparison key (`host/owner/repo`, no scheme). Never pass it to a join. */
      canonical_url: string;
      /** The `https://` URL to join with. */
      clone_url: string;
    }
  | { kind: "comingSoon"; provider: string; provider_name: string }
  | { kind: "unavailable"; provider: string }
  | { kind: "unsupported"; message: string };

export const classifyJoinRemote = (remoteUrl: string) =>
  retiredRepositoryAction<JoinRemoteClassification>("classify_join_remote_cmd", { remoteUrl });

export const connectToOrgCmd = (
  remoteUrl: string,
  githubUserId: number,
  credentialRef: CredentialRef | null = null,
) =>
  retiredRepositoryAction<ConnectOrgResult>("connect_to_org_cmd", {
    remoteUrl,
    githubUserId,
    credentialRef,
  });

/** SW-FU-B4 "Use my clone": join from a clone the user already has, no network. */
export const adoptOrgCloneCmd = (
  remoteUrl: string,
  clonePath: string,
  githubUserId: number,
  credentialRef: CredentialRef,
) =>
  retiredRepositoryAction<ConnectOrgResult>("adopt_org_clone_cmd", {
    remoteUrl,
    clonePath,
    githubUserId,
    credentialRef,
  });

/** SW-FU-B4: move what's in the team's folder aside (nothing is deleted). Returns where it went. */
export const setAsideOrgTargetCmd = (remoteUrl: string, githubUserId: number) =>
  retiredRepositoryAction<string>("set_aside_org_target_cmd", { remoteUrl, githubUserId });

export const setOrgConnectionBinding = (
  orgRoot: string,
  remoteUrl: string,
  githubUserId: number,
  credentialRef: CredentialRef | null,
) =>
  retiredRepositoryAction<CredentialRef | null>("set_org_connection_binding_cmd", {
    orgRoot,
    remoteUrl,
    githubUserId,
    credentialRef,
  });

/** SLW-PT-20: whether the user's scratch items apply in `orgSlug`/`team` ("Use my
 *  scratch items in ‹Team›"). Stored in the scratch repo; on by default. */
export const getScratchTeamUse = (orgSlug: string, team: string) =>
  invoke<boolean>("get_scratch_team_use", { orgSlug, team });

/** SLW-PT-20: turn "Use my scratch items in ‹Team›" on or off. Every backend fold
 *  (overlay, agents, routines) reads it for that team. */
export const setScratchTeamUse = (orgSlug: string, team: string, on: boolean) =>
  invoke<void>("set_scratch_team_use", { orgSlug, team, on });

/** SLW-PT-20: one worktree file the scratch overlay touches (mirrors the Rust DTO). */
export interface ScratchOverlayFile {
  /** Worktree-relative path (`/`-separated). */
  path: string;
  /** The content-relative path "Use team's" keys on. */
  logical: string;
  origin: "scratch" | "allOrgs";
  /** The team has its own file here, which the scratch file replaces. */
  replacesTeam: boolean;
  /** "Use team's" is on: the worktree holds the team's file; the scratch copy is set aside. */
  usingTeam: boolean;
  /** SLW-PT-22: the worktree's own org and team (never the one on screen). */
  orgSlug: string;
  team: string;
  /** SLW-PT-22: for an org scratch file, the kind it would apply as in every org; `null`
   *  when it can't go in all orgs. */
  inertKind: ScratchInertKind | null;
}

/** SLW-PT-20: the scratch overlay files materialized into the worktree at `codeRepoPath`
 *  (empty for an untracked worktree) — the feed for the per-file "From scratch" badge. */
export const listScratchOverlayFiles = (codeRepoPath: string) =>
  invoke<ScratchOverlayFile[]>("list_scratch_overlay_files", { codeRepoPath });

/** SLW-PT-20: "Use team's" (`useTeam`) at a scratch file of that worktree's team, or back to
 *  the scratch file. The scratch file itself is never moved or deleted. */
export const setScratchFileUseTeam = (codeRepoPath: string, logical: string, useTeam: boolean) =>
  invoke<void>("set_scratch_file_use_team", { codeRepoPath, logical, useTeam });

/** SLW-PT-21: one file in the org's scratch space (applies over its teams' files). */
export interface ScratchFile {
  replacesTeam?: boolean;
  /** Absolute path, for opening it. */
  path: string;
  /** Path relative to the space's files root — where it applies in the org's teams. */
  logical: string;
  /** SLW-PT-22: the kind it would apply as in every org ("Use in all orgs"); `null` when
   *  it can't go there (by path or content). */
  inertKind: ScratchInertKind | null;
}

/** SLW-PT-21: the inert kinds the all-orgs folder applies. */
export type ScratchInertKind = "skill" | "prompt" | "snippet" | "template";

/** SLW-PT-21: one file in the all-orgs folder; `kind: null` = no org reads it. */
export interface ScratchAllOrgsFile {
  path: string;
  logical: string;
  kind: ScratchInertKind | null;
}

/** SLW-PT-21: the Scratch space tab feed (mirrors the Rust `ScratchSpaceListing`). */
export interface ScratchManagement {
  repo: {
    path: string;
    lastCommitAt: number | null;
    uncommitted: number;
    error: string | null;
  } | null;
  orgs: {
    org: string;
    exists: boolean;
    counts: {
      notes: number;
      overlayFiles: number;
      panes: number;
      routines: number;
      agents: number;
    };
    offTeams: string[];
    settingsDamaged: boolean;
    uncommitted: number;
    tracked: boolean;
  }[];
}
export interface ScratchSpaceListing {
  management?: ScratchManagement | null;
  orgSlug: string | null;
  /** Whether that org's scratch space exists yet. */
  exists: boolean;
  files: ScratchFile[];
  allOrgs: ScratchAllOrgsFile[];
  /** SLW-PT-22: every org synced on this Mac (where "Copy to another org" can go). */
  syncedOrgs: string[];
}

/** SLW-PT-21: list `orgSlug`'s scratch space files and the all-orgs
 *  folder. Read-only: never creates a space. Notes, routines and agents come from
 *  their own commands. */
export const listScratchSpace = (orgSlug: string | null, team?: string) =>
  invoke<ScratchSpaceListing>("list_scratch_space", { orgSlug, team: team ?? null });

/** SLW-PT-21: "Who can use it: Only me" — create an empty file named `name` in the
 *  org's scratch space at the place `dir` (a folder of `team`'s files) has in the
 *  team. A taken name gets a numbered copy. Returns the new file's absolute path. */
export const createScratchFile = (orgSlug: string, team: string, dir: string, name: string) =>
  invoke<string>("create_scratch_file", { orgSlug, team, dir, name });

// ==========================================================
// === Content engine boundary surface (SS-13) =============
// ==========================================================
//
// DRY consumer contract for the `sense_sync_*` MCP tools and the
// frontend (the merge preview panel). The 8 commands below wrap `content_engine`
// APIs at the Tauri boundary. ALL inputs are JSON-safe — no Rust-
// internal types (`ResolverContext`) cross the boundary; callers
// pass `(codesenseHome, userId, orgSlug, teamName)` and the Rust
// side calls `derive_resolver_context` to build the engine context.
//
// Field naming: TS uses camelCase; Tauri converts to snake_case
// before reaching the Rust side. Return types use camelCase via
// `#[serde(rename_all = "camelCase")]` on the Rust response types.

/** Mirrors the Rust `LayerId` enum (`#[serde]` default = variant name).
 *  SW-XS-8 deleted the retired install tiers (Team Repo / Team Workspace);
 *  team content is the one layer. */
export type LayerId = "TeamProjection";

export interface LayerInfo {
  id: LayerId;
  label: string;
  writable: boolean;
  /** False for repo-scoped layers when codeRepoSlug=null, or for any tier
   *  with no on-disk storage yet in the current context (the sidebar greys
   *  those out). */
  backed: boolean;
  /** Null for unbacked layers; otherwise the absolute on-disk source path. */
  sourcePath: string | null;
}

export interface LayerEntry {
  logicalName: string;
}

/** Discriminated union — `kind` mirrors Rust's `#[serde(tag = "kind")]`. */
export type MoveOutcome =
  | { kind: "moved"; count: number }
  /** Subtree move under `skipExisting`: `moved` leaves landed; `skipped`
   *  lists the logical names of the colliding leaves left in place — the
   *  target exists and is not byte-identical to the source (differing
   *  content, or a non-regular/unreadable target). Byte-identical
   *  collisions are auto-promoted and counted in `moved`, never reported
   *  here. The workbench renders these in the per-file conflict dialog
   *  (SS-FU-18). */
  | { kind: "movedPartial"; moved: number; skipped: string[] }
  /** SW-XS-8c: team content is the only layer, so a move between layers is
   *  always this (`move_file_between_layers` has no second layer to move to;
   *  moves within team content are `move_content_file`). */
  | { kind: "rejectedSameLayer" }
  | { kind: "rejectedSourceMissing" }
  | { kind: "rejectedTargetExists" }
  | { kind: "rejectedSourceUnbacked" }
  | { kind: "rejectedTargetUnbacked" }
  | { kind: "rejectedCrossFilesystem" };

export type WriteOutcome =
  | { kind: "written" }
  | { kind: "rejectedTargetUnbacked" }
  /** SS-6 Task 1.3 — atomic 3-way conflict. Server read the current
   *  layer file under the per-repo lock; its hash differed from the
   *  caller's `expectedLayerHash`. `layerCurrentHash` is the layer's
   *  actual blake3 hash (32 bytes as JSON array of u8). Caller can
   *  surface a conflict prompt and re-issue with `force=true` to
   *  override. */
  | { kind: "rejectedConflict"; layerCurrentHash: number[] };

export interface ScanReport {
  dirty: DirtyEntry[];
  untracked: UntrackedEntry[];
}

export interface DirtyEntry {
  logicalName: string;
  /** Absolute path that was actually hashed for this report. When the
   *  lockfile records a `code_repo_path` that differs from the caller's
   *  (e.g. the user moved the repo on disk), this is the path REWRITTEN
   *  to the caller's `codeRepoPath` rather than the stale path recorded
   *  in the lockfile — so the hash mismatch reflects the live file at
   *  the caller's location. Review-iter-5 (F1) introduced the rewrite;
   *  review-iter-7 (Edge-M5) documents the contract here. */
  destPath: string;
  sourceLayer: LayerId;
  /** 32 bytes — JSON array of u8. */
  expectedHash: number[];
  actualHash: number[];
}

export interface UntrackedEntry {
  codeRepoRelativePath: string;
  matchedPattern: string;
}

/** Discriminated union mirroring Rust's `Error` enum. */
export type ContentEngineError =
  | { kind: "invalidLogicalName"; data: string }
  | { kind: "invalidContextSegment"; data: { field: string; reason: string } }
  | { kind: "layerUnbacked"; data: LayerId }
  | { kind: "fileNotFound"; data: { layer: LayerId; logicalName: string } }
  | { kind: "lockfileRead"; data: string }
  /** `sense_sync_scan_cwd` was called with a `codeRepoPath` that is not
   *  a git working tree (no `.git` present). Light defense-in-depth so
   *  malformed Tauri payloads cannot drive the scanner against system
   *  directories. Review-iter-5 (EH-14). */
  | { kind: "notAGitWorkingTree"; data: string }
  /** Engine surfaced a variant not currently mapped through SS-13
   *  (`Notify`, `GitNotFound`, etc.). TS consumers should treat this
   *  like an unexpected internal error and report. Review-iter-5
   *  (EH-21). */
  | { kind: "internal"; data: string }
  | {
      kind: "io";
      data: {
        /** Closed vocabulary mirroring Rust's `Error::Io.op` literal set
         *  (`commands/content_engine.rs`). Review-iter-4 (P-22) tightened
         *  this from `string` to a union literal so TS consumers can
         *  exhaustively switch on the operation. New op labels added on
         *  the Rust side must be appended here to keep the contract in
         *  sync — see also the iter-4 "boundary error op vocabulary"
         *  note in `SS-13-tauri-command-surface.md`. */
        op: "rename" | "read" | "write" | "hash" | "stat" | "mkdir" | "engine" | "state";
        path: string;
        message: string;
      };
    };

// Shared context shape — every command threads these four. Keep this
// definition local so callers can hand-author the literal at call sites
// without importing a shared `ContextArgs` type for read-only commands
// (matches the per-command-params shape of `MoveFileParams` /
// `WriteToLayerParams` on the Rust side without creating a `ContextArgs`
// abstraction on either side).

export interface ListLayersParams {
  codesenseHome: string;
  userId: number;
  orgSlug: string;
  teamName: string | null;
  codeRepoSlug: SourceRepoSlug | null;
  writableOnly: boolean;
}
export const listLayers = (params: ListLayersParams) =>
  invoke<LayerInfo[]>("list_layers", { params });

export interface ListLayerContentsParams {
  layerId: LayerId;
  codesenseHome: string;
  userId: number;
  orgSlug: string;
  teamName: string | null;
  codeRepoSlug: SourceRepoSlug | null;
}
export const listLayerContents = (params: ListLayerContentsParams) =>
  invoke<LayerEntry[]>("list_layer_contents", { params });

// === Canvas wireframe libraries (SW-SC-5, collapsed to one team tier by SW-SC-8) =====
// Repo-agnostic — the team workspace tier only, so no `codeRepoSlug`. The
// native Excalidraw panel does all management; these two commands just load
// and persist the whole library bag.

export interface CanvasLibraryContextArgs {
  codesenseHome: string;
  userId: number;
  orgSlug: string;
  teamName: string | null;
}

/** Read the active team's library bag (the whole v2 `.excalidrawlib`),
 *  seeding the bundled starter into it on first open (idempotent,
 *  delete-sticky). `null` when there's no active team or no bag yet. */
export const readCanvasLibrary = (params: CanvasLibraryContextArgs) =>
  invoke<string | null>("read_canvas_library", { params });

/** Persist the entire library bag for the active team. The Rust side
 *  flattens the context fields, so this stays one flat object. */
export interface WriteCanvasLibraryParams extends CanvasLibraryContextArgs {
  content: string;
}
export const writeCanvasLibrary = (params: WriteCanvasLibraryParams) =>
  invoke<void>("write_canvas_library", { params });

export interface ReadLayerFileParams {
  layerId: LayerId;
  logicalName: string;
  codesenseHome: string;
  userId: number;
  orgSlug: string;
  teamName: string | null;
  codeRepoSlug: SourceRepoSlug | null;
}
export const readLayerFile = (params: ReadLayerFileParams) =>
  invoke<number[]>("read_layer_file", { params });

/** Mirrors Rust `Lockfile` (v3 schema). The engine-side struct
 *  serializes snake_case (intentional — matches the on-disk lockfile
 *  schema) so this interface keeps snake_case field names to match the
 *  actual wire format. Review-iter-2 fix — previously declared
 *  camelCase fields would have arrived as `undefined` over IPC. */
export interface ContentEngineLockfile {
  schema_version: number;
  org_slug: string;
  team_name: string | null;
  code_repo_path: string;
  entries: ContentEngineMaterializedEntry[];
}

export interface ContentEngineMaterializedEntry {
  logical_name: string;
  dest_path: string;
  source_layer: LayerId;
  source_path: string;
  /** 32-byte blake3 hash, serialized as a JSON array of u8. */
  last_materialized_hash: number[];
  /** Nanoseconds since UNIX_EPOCH. The Rust side is u128 — values
   *  above 2^53 lose precision in JS. Acceptable for SS-13's read-only
   *  display surface; consumers doing arithmetic should treat with
   *  caution. */
  last_materialized_mtime_ns: number;
}

export interface ReadLockfileForRepoParams {
  codeRepoSlug: SourceRepoSlug;
  /** Caller-supplied code repo filesystem path. The empty-lockfile
   *  fallback uses this value for `Lockfile.codeRepoPath` so TS
   *  consumers always see a meaningful path. Review-iter-1 contract
   *  change. */
  codeRepoPath: string;
  codesenseHome: string;
  userId: number;
  orgSlug: string;
  teamName: string | null;
}
export const readLockfileForRepo = (params: ReadLockfileForRepoParams) =>
  invoke<ContentEngineLockfile>("read_lockfile_for_repo", { params });

export interface MoveFileBetweenLayersParams {
  fromLayer: LayerId;
  toLayer: LayerId;
  logicalName: string;
  codeRepoSlug: SourceRepoSlug | null;
  codesenseHome: string;
  userId: number;
  orgSlug: string;
  teamName: string | null;
  /** When true, `logicalName` is a directory-subtree prefix moved
   *  leaf-by-leaf as a unit; when omitted/false it is a single file. */
  moveAsSubtree?: boolean;
  /** Conflict resolution for a subtree move (ignored for single-file).
   *  Defaults to `rejectWhole`. */
  conflictPolicy?: "rejectWhole" | "skipExisting";
  /** When true, a single-file move overwrites an existing target instead of
   *  rejecting it (`rejectedTargetExists`) — the workbench "Replace" choice.
   *  Ignored by subtree moves (they use `conflictPolicy`). Defaults to
   *  false (reject on collision). */
  force?: boolean;
}
export const moveFileBetweenLayers = (params: MoveFileBetweenLayersParams) =>
  invoke<MoveOutcome>("move_file_between_layers", { params });

/** SW-LT-4: result of promoting a materialized entry to another team in the
 *  inheritance chain. Mirrors {@link MoveOutcome}'s tagged shape — soft
 *  rejections are expected user outcomes, not errors. */
export type PromoteOutcome =
  | { kind: "promoted"; count: number }
  | { kind: "rejectedSameTeam" }
  | { kind: "rejectedTargetNotInChain" }
  | { kind: "rejectedSourceMissing" }
  | { kind: "rejectedTargetExists" }
  | { kind: "rejectedUnbacked" }
  | { kind: "rejectedCrossFilesystem" };

export interface PromoteContentEntryParams {
  /** The layer KIND being promoted — unchanged by the move (only the owning
   *  team changes). */
  layer: LayerId;
  logicalName: string;
  codeRepoSlug: SourceRepoSlug | null;
  codesenseHome: string;
  userId: number;
  orgSlug: string;
  /** Active viewing team — whose lockfiles hold the entry and whose chain
   *  bounds the valid targets. */
  teamName: string | null;
  /** Team that currently owns the file (from its source path's team segment). */
  fromTeam: string;
  /** Team to promote into — must be in the active team's chain. */
  toTeam: string;
  /** Overwrite an existing target file at the destination (the "Replace"
   *  choice). Defaults to reject-on-collision. */
  force?: boolean;
}
export const promoteContentEntry = (params: PromoteContentEntryParams) =>
  invoke<PromoteOutcome>("promote_content_entry", { params });

/** SLW-PT-12: one end of a cross-context MOVE. Kind-tagged to mirror the FE's
 *  writable `ContextKind`s — a view-only repo is never a move endpoint. */
export type MoveContextEndpoint =
  /** The user's scratch space for `orgSlug`, else the ACTIVE org's. */
  | { kind: "personal"; orgSlug?: string }
  /** The scratch folder of inert items that apply in every org (inert kinds only). */
  | { kind: "scratchAllOrgs" }
  | { kind: "teamAll"; orgSlug: string; teamName: string }
  | { kind: "teamRepo"; orgSlug: string; teamName: string; codeRepoSlug: SourceRepoSlug };

export interface MoveContentBetweenContextsParams {
  /** The source Context (loses the item). */
  from: MoveContextEndpoint;
  /** The target Context (gains the item). */
  to: MoveContextEndpoint;
  /** Logical name of the item to move. */
  logicalName: string;
  /** Target logical name — set to a distinct value for a Keep-both auto-rename;
   *  defaults to `logicalName`. */
  toLogicalName?: string;
  /** Signed-in GitHub user id (the Personal provider's owner). */
  userId: number;
  /** Overwrite an existing target (the Replace choice). Defaults to
   *  reject-on-collision so the FE surfaces the collision chooser first. */
  force?: boolean;
  /** SLW-PT-13 COPY: retain the source (duplicate instead of move). When `true`
   *  the source keeps the item and the outcome is `copied`. Defaults to `false`
   *  (a MOVE). */
  retainSource?: boolean;
}

/** SLW-PT-12/13: result of a cross-context move/copy. Soft rejections are
 *  expected user outcomes the FE renders as gate/toast — only real I/O failures
 *  throw. */
export type MoveBetweenContextsOutcome =
  | { kind: "moved" }
  | { kind: "copied" }
  /** SLW-PT-22: moved, but the original couldn't be removed (`reason`) — it is in
   *  both places; retrying finishes the move. */
  | { kind: "movedSourceKept"; reason: string }
  | { kind: "rejectedSameContext" }
  | { kind: "rejectedSourceMissing" }
  | { kind: "rejectedTargetExists" }
  | { kind: "rejectedUnbacked" };

export const moveContentBetweenContexts = (params: MoveContentBetweenContextsParams) =>
  invoke<MoveBetweenContextsOutcome>("move_content_between_contexts", { params });

/** SLW-PT-14: parameters for a REMOVE — delete one content item from a single
 *  Context. The gate ladder (shared confirm vs the §J personal sole-copy gate)
 *  is decided on the FE; this executes the resolved delete. */
export interface RemoveContentFromContextParams {
  /** The Context the item lives in (and is removed from). */
  context: MoveContextEndpoint;
  /** Logical name (content-root-relative path) of the item to remove. */
  logicalName: string;
  /** Signed-in GitHub user id (the Personal provider's owner). */
  userId: number;
}

/** SLW-PT-14: result of a REMOVE. A soft rejection is an expected user outcome
 *  the FE renders as a toast — only real I/O failures throw. */
export type RemoveFromContextOutcome =
  | { kind: "removed" }
  | { kind: "rejectedSourceMissing" }
  | { kind: "rejectedUnbacked" };

export const removeContentFromContext = (params: RemoveContentFromContextParams) =>
  invoke<RemoveFromContextOutcome>("remove_content_from_context", { params });

export interface SenseSyncWriteToLayerParams {
  layerId: LayerId;
  logicalName: string;
  /** Byte payload as JSON array of u8 (Tauri marshals Vec<u8>). */
  content: number[];
  codeRepoSlug: SourceRepoSlug | null;
  codesenseHome: string;
  userId: number;
  orgSlug: string;
  teamName: string | null;
  /** SS-6 Task 1.3 — when set, server pre-reads the layer file under
   *  the per-repo lock and rejects with
   *  `WriteOutcome.rejectedConflict` if the current blake3 differs
   *  from this value. 32 bytes (matches `DirtyEntry.expectedHash`). */
  expectedLayerHash?: number[];
  /** SS-6 Task 1.3 — bypass the `expectedLayerHash` gate (user picked
   *  `[P] push anyway` in the conflict prompt). Has no effect when
   *  `expectedLayerHash` is unset. */
  force?: boolean;
}
export const senseSyncWriteToLayer = (params: SenseSyncWriteToLayerParams) =>
  invoke<WriteOutcome>("sense_sync_write_to_layer", { params });

export interface SenseSyncScanCwdParams {
  codeRepoSlug: SourceRepoSlug;
  codeRepoPath: string;
  codesenseHome: string;
  userId: number;
  orgSlug: string;
  teamName: string | null;
}
export const senseSyncScanCwd = (params: SenseSyncScanCwdParams) =>
  invoke<ScanReport>("sense_sync_scan_cwd", { params });

// === SLW-MD-1: in-file frontmatter / cs:* metadata editor ===

/**
 * Canonical frontmatter fields as parsed by the Rust `FrontmatterFields`
 * struct. Note the wire shape is snake_case (the struct has no camelCase
 * rename) with `type` serialized as `type` (serde rename of `document_type`).
 * All fields optional; a missing key is `null`/absent.
 */
export interface MetadataFields {
  type?: string | null;
  status?: string | null;
  story_id?: string | null;
  source_repo?: string | null;
  /** SV-CDE-85: the repo whose history holds the work, when not `source_repo`. */
  work_repo?: string | null;
  /** SW-GS-35: story-scoped dev branch — OBSERVED (auto-written every establish). */
  dev_branch?: string | null;
  /** SW-WB-1: story-scoped INTENDED branch — editable; drives the worktree. */
  branch_name?: string | null;
  team?: string | null;
  /** SW-SB-1 / SW-ID-5: the person a story is assigned to (a GitHub login).
   *  Already serialized by `get_metadata_model` (FrontmatterFields) — surfaced
   *  here so the metadata band's Assignee picker can read the current value. */
  assigned_to?: string | null;
  depends_on?: string[] | null;
  supporting_docs?: string[] | null;
  repos?: string[] | null;
  branch?: string | null;
  scope_context?: string[] | null;
  scope_install?: string[] | null;
}

/** Per-field validity state — only `invalid` badges (matches the nav rule). */
export type FieldState = "absent" | "valid" | "invalid" | "warning";

/**
 * One field's validity. `code` carries the stable finding code (same strings
 * as MCP `validate_story` where they overlap: `UNKNOWN_SOURCE_REPO`,
 * `INVALID_TYPE`, `INVALID_STATUS`, `UNKNOWN_TEAM`).
 */
export interface FieldValidity {
  field: string;
  state: FieldState;
  message: string | null;
  code: string | null;
}

/** SW-AU-77I: which registry a {@link SchemaFieldRow} came from — a
 *  provenance/legibility marker (never a permission). */
export type RowSource = "Team" | "Org";

/**
 * SW-AU-77G/77I: one schema-declared field PRESENT on a doc, projected as a
 * first-class band row (`frontmatter_editor.rs` `SchemaFieldRow`). `name` is
 * canonical; `values` the present value(s) (scalar = 1 element; `multi_select` =
 * per item); `type`/`allowedValues` the declared render config. Org tags
 * (`source: "Org"`) additionally carry their `color`/`icon`, resolved once in Rust
 * — the FE reads them off the row, never re-deriving from a separate schema prop.
 * Team rows (`source: "Team"`) carry no color/icon. Validity is NOT here — it rides
 * `perFieldValidity` keyed by `name`.
 */
export interface SchemaFieldRow {
  name: string;
  values: string[];
  type?: string;
  allowedValues?: string[];
  color?: string;
  icon?: string;
  source: RowSource;
}

/**
 * SW-AU-77J: one team-declared custom field DECLARED BUT ABSENT from a doc
 * (`frontmatter_editor.rs` `DeclaredFieldDef`), offered by the band's "+ Add field"
 * menu. Declaration config only (no value); `required` is a "worth adding" hint, never
 * a block. The mirror of `SchemaFieldRow` for absent fields.
 */
export interface DeclaredFieldDef {
  name: string;
  type?: string;
  allowedValues?: string[];
  required: boolean;
}

/** The metadata band's read-model for one doc (`get_metadata_model`). */
export interface MetadataModel {
  format: "markdown" | "html";
  fields: MetadataFields;
  /** Frontmatter keys not in the known schema — `[key, rawValue]`, preserved
   *  verbatim and round-tripped on every save (AC-8). SW-AU-77G/77I: schema-declared
   *  fields present on the doc (team customs AND org tags) are lifted OUT of this
   *  list into `schemaRows` (rendered as typed rows), so they appear once. */
  unknownFields: Array<[string, string]>;
  isEpic: boolean;
  /** `false` when the block is present but malformed → raw-editor fallback (AC-12). */
  blockParseOk: boolean;
  perFieldValidity: FieldValidity[];
  /** SW-AU-77G/77I: present schema-declared fields (team customs + org tags), as
   *  first-class typed rows. */
  schemaRows: SchemaFieldRow[];
  /** SW-AU-77J: team-declared custom fields DECLARED BUT ABSENT from the doc — offered
   *  by the "+ Add field" menu (the complement of `schemaRows`). */
  declarableFields: DeclaredFieldDef[];
  /** The verbatim on-disk metadata block, byte-for-byte (no re-serialization):
   *  the YAML frontmatter body between the `---` fences (markdown) or the
   *  contiguous run of `<meta name="cs:*">` tags (HTML). `null` when the doc has
   *  no metadata block. Populated **even when `blockParseOk === false`** — the
   *  raw editor (AC-12) seeds from this so the user can see/repair a broken
   *  block. It is the exact text a `ReplaceRawBlock` edit swaps, so it
   *  round-trips with `applyMetadataEdit(ReplaceRawBlock)`. */
  rawBlock: string | null;
}

/**
 * Read the metadata model for `path`. Pass the resolved org anchor + active
 * team (the FE already has these from `load_org`) so `source_repo`/`team`
 * validity resolves against the team's CONFIGURED repos (locked decision #5).
 */
export const getMetadataModel = (path: string, orgRoot?: string, orgSlug?: string, team?: string) =>
  invoke<MetadataModel>("get_metadata_model", {
    path,
    orgRoot: orgRoot ?? null,
    orgSlug: orgSlug ?? null,
    team: team ?? null,
  });

// === Create a story from the app (validated by the `validate_story` engine) ===

/** One `validate_story` finding — the same shape the MCP tool returns. */
export interface StoryFinding {
  code: string;
  field: string;
  severity: "blocking" | "advisory";
  message: string;
  hint: string;
}

/** The `validate_story` report (only the fields the app renders). */
export interface StoryValidationReport {
  detectedKind: "plain_doc" | "pipeline_story" | "epic_main_doc";
  format: string;
  valid: boolean;
  findings: StoryFinding[];
}

/** The New-story form's input. `epicDir` is the absolute epic directory. */
export interface StoryDraft {
  epicDir: string;
  title: string;
  storyId: string;
  status: string;
  sourceRepo: string;
  dependsOn: string[];
}

/** Where the story would be written, its exact bytes, and their validation. */
export interface NewStoryPreview {
  path: string;
  content: string;
  validation: StoryValidationReport;
}

/** Render + validate a draft without writing. */
export const previewNewStory = (draft: StoryDraft) =>
  invoke<NewStoryPreview>("preview_new_story", { draft });

/** Render, validate and write a draft; rejects (writing nothing) on a blocking
 *  finding or an existing file. */
export const createStory = (draft: StoryDraft) =>
  invoke<NewStoryPreview>("create_story", { draft });

/** Run `validate_story` on an on-disk file (the metadata band's findings). */
export const validateStoryFile = (path: string) =>
  invoke<StoryValidationReport>("validate_story_file", { path });

/** The repos a story's `source_repo` should come from: the `repos:` of the
 *  nearest enclosing epic that names any (empty when none does). */
export const storyRepoChoices = (path: string) => invoke<string[]>("story_repo_choices", { path });

// === SW-SB-2: per-team sprint definition registry (read side) ===

/** Sprint lifecycle state (lowercase, matches the on-disk `state:` field). */
export type SprintState = "planning" | "active" | "closed";

/** One sprint definition, an entry in the team ledger
 *  `{team}/config/sprints.yaml` (SW-SB-24). Shape fixed by the live `SprintDef`
 *  struct (`sprint_registry.rs`). Descriptive fields default to empty when the
 *  ledger entry omits them. */
export interface SprintDef {
  id: string;
  state: SprintState;
  goal: string;
  focusEpics: string[];
  start: string;
  end: string;
  /** SW-SB-11 AC-5: velocity stamped by the close action (delivered = points on
   *  `done` members; carried = points rolled over). A durable historical fact so
   *  SB-13 shows a closed sprint's velocity without recomputing from mutable story
   *  files. `0` for open / pre-SB-11 sprints (Rust defaults). */
  deliveredPoints?: number;
  carriedPoints?: number;
}

/** The read-side sprint registry for one team (`load_sprint_registry`). SB-3
 *  reads `activeSprint` (nullable — a legitimate planning→active gap) + the
 *  ordered `sprints` list to default and populate its selector; `warnings`
 *  carries soft, non-fatal advisories (multi-active drift, skipped malformed
 *  files) for the drift/empty states. */
export interface SprintRegistry {
  activeSprint: string | null;
  sprints: SprintDef[];
  warnings: string[];
}

/** Load the active team's sprint registry from the ledger
 *  `{orgRoot}/{team}/config/sprints.yaml` (SW-SB-24). Never rejects on malformed
 *  content — a broken entry is skipped and surfaced in `warnings`. */
export const loadSprintRegistry = (orgRoot: string, team: string) =>
  invoke<SprintRegistry>("load_sprint_registry", { orgRoot, team });

/** An open file's honest lifecycle as the viewer store tracks it (snake_case).
 *  `deleted` is terminal; `temporarily_unavailable` invites a retry. */
export type SubjectLifecycle = "active" | "moved" | "deleted" | "temporarily_unavailable";

/** SW-CP-24: mark a content-relative path as structured merge-with-source
 *  (JSON/JSONC/YAML/TOML) so projection deep-merges it with the worktree file
 *  instead of clobbering. `arrayKeys` optionally declares an SMP merge key per
 *  array target (dot-joined path → key field). Rejects a non-structured path.
 *  Filesystem work runs on the Rust blocking pool (never the UI thread). */
export const setContentMergeMark = (
  orgRoot: string,
  team: string,
  logicalPath: string,
  arrayKeys: Record<string, string> = {},
) => invoke<void>("set_content_merge_mark", { orgRoot, team, logicalPath, arrayKeys });

/** SW-CP-24: clear a content path's merge mark (idempotent — clearing an
 *  unmarked path is a no-op success). */
export const clearContentMergeMark = (orgRoot: string, team: string, logicalPath: string) =>
  invoke<void>("clear_content_merge_mark", { orgRoot, team, logicalPath });

/** SW-CP-27: list every merge-marked content-relative path for a team (sorted).
 *  The read seam the right-click toggle loads once per team to render the in-tree
 *  merge badge + pick the Mark/Unmark label. A missing/malformed store yields `[]`
 *  (tolerant by construction). Filesystem read runs on the Rust blocking pool. */
export const listContentMergeMarks = (orgRoot: string, team: string) =>
  invoke<string[]>("list_content_merge_marks", { orgRoot, team });

/** SW-CP-29: one attached code repo a merge-marked file can be previewed against
 *  — a merge-preview selector option. `codeRepoPath` is the on-disk target the
 *  wave writes into (the base the preview merges the fragment onto). */
export type MergePreviewRepo = {
  owner: string;
  repo: string;
  codeRepoPath: string;
};

/** SW-CP-29: list the attached code repos the active team projects into — the
 *  candidates for the repo-scoped merge-preview selector. Same enumeration the
 *  wave fans out over, so the selector can't offer a target the wave wouldn't
 *  write. Read-only; runs on the Rust blocking pool (never the UI thread). */
export const listMergePreviewRepos = (
  codesenseHome: string,
  userId: number,
  orgSlug: string,
  team: string,
) =>
  invoke<MergePreviewRepo[]>("list_merge_preview_repos", {
    codesenseHome,
    userId,
    orgSlug,
    team,
  });

/** SW-XS-8b: what a merge-marked file's preview shows. */
export interface MergePreview {
  /** Exactly the merged file a launch in that repo writes (Step 2's generated entry). */
  text: string;
  /** What Step 2 does with it there. */
  outcome: "write" | "unchanged" | "replace" | "keepEdited" | "keepUnmanaged" | "tracked";
  /** The plain-language line for `outcome`. */
  note: string;
}

/** SW-CP-29 / SW-XS-8b: the read-only, repo-scoped preview of a merge-marked
 *  file — exactly the merged file a launch in `codeRepoPath` writes, and what
 *  Step 2 does with it there (write, replace an unedited copy, keep an edited
 *  one). Purely read/compute, on the Rust blocking pool. */
export const previewMergeMarkedFile = (
  codesenseHome: string,
  userId: number,
  orgSlug: string,
  team: string,
  logicalPath: string,
  codeRepoPath: string,
) =>
  invoke<MergePreview>("preview_merge_marked_file", {
    codesenseHome,
    userId,
    orgSlug,
    team,
    logicalPath,
    codeRepoPath,
  });

/** SW-CP-28: the Settings "Excluded paths" view model — the in-code default
 *  globs (rendered as toggles) plus the team's raw override patterns (added
 *  excludes and gitignore-style `!`-negations). The FE renders `defaults` from
 *  THIS backend data, never a hardcoded literal, so a change to `SECRET_GLOBS`
 *  surfaces automatically. */
export interface ProjectionExcludesView {
  defaults: string[];
  patterns: string[];
}

/** SW-CP-28: read a team's projection-exclude view (defaults + overrides). A
 *  missing/malformed store yields the defaults with an empty override list.
 *  Filesystem read runs on the Rust blocking pool (never the UI thread). */
export const getProjectionExcludes = (orgRoot: string, team: string) =>
  invoke<ProjectionExcludesView>("get_projection_excludes", { orgRoot, team });

/** SW-CP-28: write a team's projection-exclude override patterns (adds +
 *  `!`-negations). Each glob is validated backend-side; an invalid pattern rejects
 *  the whole write with its parse error. The next projection wave re-projects (a
 *  removed exclude hydrates the file; an added one un-projects it). Write runs on
 *  the Rust blocking pool. */
export const setProjectionExcludes = (orgRoot: string, team: string, patterns: string[]) =>
  invoke<void>("set_projection_excludes", { orgRoot, team, patterns });

/** SW-SR-3: the auto-run provenance join key stamped on a dispatched worker
 *  `{ runId, storyId, automationId? }` — what SR-4's monitor rolls up on.
 *  SW-AU-13: a fan-out worker also carries the triggering item (`triggerItem`), the shared poll
 *  timestamp (`pollTs`, epoch ms — the `(runId, pollTs)` fan-out group key), and the trigger-source
 *  category (`triggerSource`) the AU-3 `trigger` facet slices on. All absent for a manual run. */
export interface RunProvenance {
  runId: string;
  storyId: string;
  automationId?: string;
  triggerSource?: string;
  team?: string;
  workspace?: string;
  scheduledAt?: string;
  firedAt?: string;
  /** The named agent the session runs, as `<key>@<revision>`, on every agent launch. */
  agent?: string;
  /** The startup manifest hash of that agent launch. */
  manifestHash?: string;
  /** The routine whose run this is (`<org>/<team>/<id>`), for a routine-started session. */
  routineKey?: string;
}

/** SW-GT-1: the fixed 5-member role set a state carries. Code keys off the role;
 *  teams customize the states (labels/colors/order/count) mapped to each. */
export type StatusRole = "not_started" | "active" | "review" | "done" | "blocked";

/** One resolved state in a team's status taxonomy. Shape fixed by the live
 *  `StatusState` struct (`status_taxonomy.rs`). `order` is concrete after load
 *  (missing values are assigned append-after-max), so consumers never see a gap. */
export interface StatusState {
  id: string;
  label: string;
  color: string;
  /** Lucide icon **name** (e.g. `"CircleDashed"`); the FE resolves it to a
   *  component and falls back for an unknown name. Never empty (absent → role icon). */
  icon: string;
  order: number;
  role: StatusRole;
  /** SW-GT-6: the one manual per-state marker. `false` marks a terminal as
   *  *dropped / won't-do* (velocity counts `role done AND delivered !== false`);
   *  absent (⇒ delivered) or `true` is ordinary. Omitted on write when unset. */
  delivered?: boolean;
}

/** The read-side status taxonomy for one team (`load_status_taxonomy`). GT-3/GT-4
 *  read the ordered `states` to drive switchers, board columns, and rollups;
 *  `warnings` carries soft, non-fatal drift (malformed/duplicate edits) for the
 *  drift banner. Never rejects — a broken file falls back to the shipped defaults. */
export interface StatusTaxonomy {
  states: StatusState[];
  warnings: string[];
}

/** Load the team's status taxonomy from `{orgRoot}/{team}/content/status-taxonomy.yml`.
 *  Never rejects on malformed content — an unreadable/broken file falls back to the
 *  shipped defaults and surfaces the reason in `warnings` (SE-6 foundation). */
export const loadStatusTaxonomy = (orgRoot: string, team: string) =>
  invoke<StatusTaxonomy>("load_status_taxonomy", { orgRoot, team });

// ---------------------------------------------------------------------------
// SW-GT-6: team config editor — field schema. Types mirror the live serde models
// (`field_schema.rs`).
// ---------------------------------------------------------------------------

/** One declared metadata field (`field_schema.rs` `FieldDef`). `official` is derived
 *  (a codesense-baseline field vs a team custom field), never authored. */
export interface FieldDef {
  name: string;
  type?: string;
  /** SW-AU-77E: the ordered allowed-value vocabulary for a `choice` field (absent/empty
   *  for every other type). */
  allowedValues?: string[];
  required: boolean;
  official: boolean;
}

/** The read-side field schema (`load_field_schema`): official ∪ custom + `warnings`. */
export interface FieldSchema {
  fields: FieldDef[];
  warnings: string[];
}

/** An edited team config to persist — tagged by which file it targets. The FE sends
 *  the same shape its `load_*` returned minus the runtime `warnings`; the schema arm
 *  sends official ∪ custom (the command writes only the custom fields). */
export type TeamConfigWrite =
  | { kind: "status_taxonomy"; states: StatusState[] }
  | { kind: "field_schema"; fields: FieldDef[] };

/** Persist an edited team config to the content layer (SW-GT-6 AC-4). Off the webview
 *  thread; the file-watcher → sync wave propagates it — no explicit publish call. */
export const saveTeamConfig = (orgRoot: string, team: string, write: TeamConfigWrite) =>
  invoke<void>("save_team_config", { orgRoot, team, write });

/** SW-AU-77E: the drawer's declare input — a name, one of the five authoring types, an
 *  optional `choice` vocabulary, and (optionally) the value about to be set on the current
 *  story so the backend validates it before any write. */
export type FieldType = "text" | "number" | "yes-no" | "choice" | "date";
export interface NewFieldInput {
  name: string;
  type: FieldType;
  allowedValues?: string[];
  required?: boolean;
  value?: string | null;
}

/** SW-AU-77E (AC-5): whether the current context may declare a team field, and — when not
 *  — a truthful reason the drawer shows on the disabled "+ Add field" action. */
export interface DeclareGate {
  allowed: boolean;
  reason?: string;
}

/** SW-AU-77E (AC-5): pre-flight gate — may this context declare a team field? */
export const canDeclareTeamField = (orgRoot: string, team: string) =>
  invoke<DeclareGate>("can_declare_team_field", { orgRoot, team });

/** SW-AU-77E: declare (or reconcile onto) ONE typed custom field in the team FieldSchema
 *  from the metadata drawer — additive + RMW-safe (never the full-vector overwrite Settings
 *  does). Returns the reloaded schema (official ∪ custom) so the caller re-reads the
 *  authoritative set. Off the webview thread. */
export const declareTeamField = (orgRoot: string, team: string, field: NewFieldInput) =>
  invoke<FieldSchema>("declare_team_field", { orgRoot, team, field });

/**
 * One `source_repo` picker option. `attached` is a SORT hint only (never a
 * filter, never a validity input — locked decision #5); every configured repo
 * is a valid, selectable option regardless.
 */
export interface SourceRepoOption {
  slug: string;
  attached: boolean;
}

/** The team's CONFIGURED source repos (the picker options + validity bar). */
export const listSourceReposForTeam = (orgRoot: string, orgSlug: string, team: string) =>
  invoke<SourceRepoOption[]>("list_source_repos_for_team", { orgRoot, orgSlug, team });

// === SW-DX-1: team tag schema (the Open Dashboard Runtime data layer) =

/** A tag's declared type (mirrors Rust `TagType`; serialized snake_case).
 *  `select`/`ordered`/`status` (single) and `multi_select` (per item) validate
 *  as closed-set membership; `ordered` also exposes an order index; `date`
 *  parses ISO-8601; `text`/`relation` are free. */
export type TagType =
  | "text"
  | "select"
  | "multi_select"
  | "ordered"
  | "status"
  | "date"
  | "relation"
  // Any unrecognized `type:` string (a future DX version's tag) degrades here
  // instead of failing the org config load — mirrors Rust `TagType::Unknown`.
  | "unknown";

/** One tag definition in the team registry (mirrors Rust `TagDef`). */
export interface TagDef {
  name: string;
  type: TagType;
  /** Ordered vocabulary for closed-set / `ordered` tags — the ORDER is the
   *  ladder (never sorted). Empty ⇒ syntactic-only (no badge). Always present
   *  (possibly `[]`). */
  allowedValues: string[];
  /** Optional per-tag color (a `--cs-*` token name or literal). Omitted when
   *  the tag declares none — `tagDisplay` supplies a default. */
  color?: string;
  /** Optional per-tag Lucide icon name. Omitted ⇒ `tagDisplay` default. */
  icon?: string;
}

/** The optional team-defined tag registry (mirrors Rust `TagSchema`). An empty
 *  `tags` array is the unopinionated default. */
export interface TagSchema {
  tags: TagDef[];
}

/**
 * SW-DX-1 read seam: resolve the active team's tag registry. `undefined`/empty
 * anchors (or an unresolved org) return an empty schema — never an error.
 *
 * Memoize this PER TEAM — it is NOT a per-file-open read. The metadata band's
 * per-tag validity already rides `getMetadataModel`; `getTagSchema` supplies the
 * dropdown option lists + `color`/`icon` render config for DX-2 / DX-4 / the band.
 */
export const getTagSchema = (orgRoot?: string, orgSlug?: string, team?: string) =>
  invoke<TagSchema>("get_tag_schema", {
    orgRoot: orgRoot ?? null,
    orgSlug: orgSlug ?? null,
    team: team ?? null,
  });

/**
 * One structured metadata edit. Serde-tagged on `kind`. A list item containing
 * a comma is rejected for BOTH formats (HTML joins lists with commas).
 */
export type MetadataEdit =
  | { kind: "setField"; field: string; value: string }
  | { kind: "setList"; field: string; items: string[] }
  | { kind: "clearField"; field: string }
  | { kind: "replaceRawBlock"; text: string };

/**
 * Apply a batch of metadata edits to `path` in place (byte-preserving, format-
 * aware). Rejected when `path` is a read-only projected/installed view (AC-13).
 * After success, the caller handles refresh (e.g. `bumpFileRefresh`).
 */
export const applyMetadataEdit = (
  path: string,
  edits: MetadataEdit[],
  frameId?: string,
  pagePath?: string,
) =>
  invoke<void>("apply_metadata_edit", {
    path,
    edits,
    ...(frameId ? { frameId } : {}),
    ...(pagePath ? { pagePath } : {}),
  });

/**
 * SW-OR-12/OR-13: the durable terminal outcome for ONE run, keyed by `run_id`, PLUS its
 * validator-aware display state (SW-BA-8) — the `AgentResultPanel` header source (chip + summary +
 * timestamps). Resolves the owning `story_id` trusted-side then folds the run/verdict store off the
 * UI thread; `null` when the run recorded no outcome yet (in-flight) so the panel degrades to its
 * live/chip-only header. `validated` is composed over the SAME records, so the live-completion chip
 * and the durable SW-AU-55 row cannot diverge.
 */
export const readRunOutcome = (runId: string) =>
  invoke<RunOutcomeRead | null>("read_run_outcome", { runId });

/**
 * SW-AU-82A: open one run's evidence aggregate — the immutable audit header + the causal identity
 * spine (`AutomationRun → StepExecution → Attempt`) — for the `RunEvidence` view. Folds the store
 * ONCE off the UI thread and passes the capability∧ownership gate (AC-9); `null` when the run has no
 * recorded outcome (unknown / in-flight) so the view degrades to empty/unavailable. Requires
 * `storyId` too — evidence is never exposed by `runId` alone.
 */
export const readRunEvidence = (storyId: string, runId: string) =>
  invoke<RunEvidence | null>("read_run_evidence", { storyId, runId });

export const readRunWorkProduct = (runId: string) =>
  invoke<RunWorkProduct | null>("read_run_work_product", { runId });

/** Latest durable outcome for every run, folded in one off-thread store read. */
export const listRunOutcomes = () => invoke<RunOutcomeRead[]>("list_run_outcomes");

/**
 * SW-OR-12: the persisted transcript body for ONE run, keyed by `run_id` — the panel's body source
 * (completed case). Reads the bounded artifact off the UI thread; `null` when the run left no
 * output (a side-effect-only success, or a run predating this feature) → "No output captured".
 */
export const readRunArtifact = (runId: string) =>
  invoke<string | null>("read_run_artifact", { runId });

/** Lazily read the optional, redacted terminal-level snapshot. */
export const readRawTerminalSnapshot = (runId: string) =>
  invoke<string | null>("read_raw_terminal_snapshot", { runId });
export const hasRawTerminalSnapshot = (runId: string) =>
  invoke<boolean>("has_raw_terminal_snapshot", { runId });

/**
 * SW-OR-16: the parsed STRUCTURED transcript (neutral turns / tool calls / usage) for ONE run,
 * keyed by `run_id` — the panel's rich-mode source. Reads the already-parsed, already-redacted
 * `{run_id}.turns.json` off the UI thread; `null` when the agent CLI has no structured capture or the
 * source did not parse → the panel stays on the plain-text body (`readRunArtifact`).
 */
export const readRunTranscript = (runId: string) =>
  invoke<SessionTranscript | null>("read_run_transcript", { runId });

export type LegacyRepositoryState = "migrating" | "needs-attention" | "complete";

export interface LegacyRepositoryStatus {
  session_generation: number;
  job_key: string;
  org_slug?: string;
  workspace_root?: string;
  remote_key: string;
  state: LegacyRepositoryState;
  error_category?: string;
  resolved_repository_paths?: Record<string, string>;
}

export const legacyRepositoryStatuses = (orgSlug?: string) =>
  invoke<LegacyRepositoryStatus[]>("legacy_repository_statuses", { orgSlug });

export type LegacyRepositoryLocalState = "ready" | "no-principal";
export const legacyRepositoryLocalState = () =>
  invoke<LegacyRepositoryLocalState>("legacy_repository_local_state");

export const retryLegacyRepositoryJob = (jobKey: string) =>
  retiredRepositoryAction<void>("retry_legacy_repository_job", { jobKey });
export const cancelLegacyRepositoryJob = (jobKey: string) =>
  invoke<void>("cancel_legacy_repository_job", { jobKey });

// ── SW-AU-86B: Build-canvas workflow authoring ──────────────────────────────────────────────
// The WorkflowGraph/Layout model (SW-AU-79A) + file-authoritative persistence (SW-AU-79C), the
// node-kind catalog, blocking validation, and the three-path Save flow. The graph/layout objects
// are the SEMANTIC + VISUAL truth the canvas projects to/from — React Flow's node objects are
// never persisted. Inner field names mirror the Rust serde (snake_case); only top-level command
// argument names are camelCase (Tauri maps them to snake_case parameters).

// === Remote Machines: the machine registry + SSH connector ===

/** The built-in This Mac entry's id — local is a machine, not a special case. */
export const LOCAL_MACHINE_ID = "local";

/** Where a machine's declaration lives: the personal context layer (follows you
 *  across orgs/teams) or ONE team's context layer (published through normal
 *  sync). A team is only unique within its org, so the scope names both. */
export type MachineScope = { kind: "personal" } | { kind: "team"; orgSlug: string; team: string };

/** A machine's shareable, secret-free half. A team-shared declaration NEVER
 *  carries a username, key or passphrase. */
export interface MachineIdentity {
  host: string;
  port: number;
  jumpHost: string | null;
  hostKeyFingerprint: string | null;
  serverId: string | null;
}

export interface MachineIdentityChange {
  accepted: MachineIdentity;
  declared: MachineIdentity;
  revision: string;
  author: string | null;
  date: string | null;
  observedServerId?: string | null;
}

export const resolveMachineIdentity = (machineId: string, revision: string, accept: boolean) =>
  invoke<void>("resolve_machine_identity", { machineId, revision, accept });

export interface MachineDeclaration {
  identityChange?: MachineIdentityChange | null;
  mode?: "legacy" | "private-account";
  host: string;
  port: number;
  jumpHost: string | null;
  defaultBaseDir: string | null;
  /** The pinned host-key fingerprint (anti-MITM for teammates); null until first trust. */
  pinnedHostKeyFp: string | null;
  /** Publisher-verified terminal-server identity shared with teammates. */
  pinnedServerId: string | null;
}

/** A machine's personal half, never synced to a team. `hidden` is the local-only
 *  Hide flag; `credentialRef` is the keychain handle of the key's passphrase
 *  (null = ssh-agent / `~/.ssh/config` / a key without one). */
export interface MachineBinding {
  username: string | null;
  /** The key file this user signs in with (prefills the form); null = agent. */
  keyPath: string | null;
  credentialRef: CredentialRef | null;
  autoConnect: boolean;
  hidden: boolean;
  /** It held open tabs when last seen, so it connects at launch to bring them
   *  back (never for a sign-in that could prompt). Absent on older rows. */
  reconnectAtLaunch?: boolean;
}

/**
 * The state of one machine. `needsCredential` reuses the connections health
 * grammar for a machine this user has not set up yet (rendered "Needs setup").
 * `idle` is a known machine that is simply not connected — nothing auto-connects.
 * The registry reports `connected` (This Mac), `idle` and `needsCredential`; the
 * connection states come from the connection manager.
 */
export type MachineStatus =
  | {
      state: "connected";
      workspaces: number | null;
      /** SW-XS-6: its terminal server is not this app's build (or lacks a
       *  capability this app needs), even at the same version string. */
      terminalServerUpdate?: boolean;
      /** With an update: the sessions the running server holds (it ends them). */
      terminalServerSessions?: number;
    }
  | { state: "idle" }
  | { state: "connecting" }
  | { state: "handshaking" }
  | { state: "disconnected" }
  | { state: "connectionFailed"; lastError: string }
  | { state: "reconnecting"; attempt: number }
  | { state: "unreachable"; lastReachedAtMs: number; attempt: number; retryInSecs: number }
  | { state: "needsCredential" }
  | { state: "authFailed"; reason: string }
  | { state: "hostKeyChanged"; pinnedFp: string; presentedFp: string }
  | { state: "notInstalled" }
  | { state: "versionMismatch"; serverVersion: string; appNeeds: string; sessions: number }
  | { state: "terminalServerFrozen"; count: number | null; replacementSupported: boolean }
  | { state: "replacementReserved"; count: number | null }
  | { state: "replacing"; count: number | null }
  | { state: "readmitting"; count: number | null }
  | { state: "replacementFailed"; oldStopped: boolean; count: number | null; repair: string };

/** One-use replacement authorization, bound by Rust to this Machine's frozen
 * proof, incarnation, and generation. It never contains caller-selected host
 * or process details. */
export interface MachineTerminalServerReplacementPrompt {
  serverToken: string;
  count: number | null;
}

/** SW-AGR-19 PR-4: one tab after an update. */
export interface RestoredTab {
  name: string;
  workspace: string;
  /** The agent that ran there and ended with the old server. */
  agent?: string;
  /** What is not as it was (another folder, a refusal, still opening). */
  note?: string;
  /** SW-AGR-19 PR-6: its Agent was relaunched with a resume brief. */
  resumed?: boolean;
}

/** SW-AGR-19 PR-4: what reopened after a terminal server update. */
export interface TerminalRestoreSummary {
  reopened: RestoredTab[];
  notReopened: RestoredTab[];
  /** Whether the tabs could be recorded before the old server stopped. */
  recorded: boolean;
  /** Whether everything came back as it was. */
  complete: boolean;
  /** The one line the person reads (also posted to the Updates lane). */
  message: string;
}

/** SW-AGR-19 PR-5: This Mac's running terminal server, what its update's
 *  confirmation binds to. */
export interface LocalServerIdentity {
  serverId: string;
  epoch: string;
  pid: number;
  sessionIds: string[];
}

/** SW-AGR-19 PR-5: This Mac's update, when its server is not this app's
 *  build (older, or another CodeSense build's); `null` otherwise. */
export interface LocalServerUpdate {
  expected: LocalServerIdentity;
  terminalServer: TerminalServerInfo;
}

export const localTerminalServerUpdate = () =>
  invoke<LocalServerUpdate | null>("local_terminal_server_update");

/** Rejects with `{ oldStopped, changed, message }`: `changed` when the server
 *  or its sessions changed since review (nothing stopped). */
export interface LocalTerminalServerUpdateError {
  oldStopped: boolean;
  changed: boolean;
  message: string;
}

export interface LocalUpdateResult {
  sessions: number;
  restored: TerminalRestoreSummary | null;
}

/** SW-AGR-19 PR-5: replace This Mac's terminal server with this build's,
 *  ending exactly the `expected` sessions (a changed set is refused). */
export const updateLocalTerminalServer = (expected: LocalServerIdentity) =>
  invoke<LocalUpdateResult>("update_local_terminal_server", { expected });

/** SW-AGR-19 PR-8: what one machine's update binds to: its server, lifetime
 *  and the sessions confirmed ending (This Mac adds its server process). */
export interface ServerExpectation {
  serverId: string;
  epoch: string;
  sessions: number;
  pid?: number;
  sessionIds?: string[];
}

/** SW-AGR-19 PR-8: what came of one machine in an Update all run. */
export interface TerminalServersUpdateOutcome {
  machineId: string;
  label: string;
  kind: "updated" | "skipped" | "failed";
  message: string;
  /** Its old server is gone and the new one is not up. */
  oldStopped?: boolean;
  /** Asked, but its result is unknown. */
  unconfirmed?: boolean;
  restored?: TerminalRestoreSummary;
}

/** SW-AGR-19 PR-8: an Update all run, as the backend runs it. */
export interface TerminalServersUpdateRun {
  opId: string;
  running: boolean;
  machines: string[];
  current?: string;
  outcomes: TerminalServersUpdateOutcome[];
}

export const reviewTerminalServerUpdate = (machineId: string) =>
  invoke<ServerExpectation>("review_terminal_server_update", { machineId });

export const startTerminalServersUpdate = (
  items: { machineId: string; expected: ServerExpectation }[],
) => invoke<TerminalServersUpdateRun>("start_terminal_servers_update", { items });

export const terminalServersUpdateStatus = (opId: string | null) =>
  invoke<TerminalServersUpdateRun | null>("terminal_servers_update_status", { opId });

/** SW-AGR-19 PR-7: how a stray terminal server relates to this app. */
export type StrayKind = "bundled" | "otherBuild" | "ownRoot";

/** SW-AGR-19 PR-7: a terminal server on This Mac besides the one this app uses. */
export interface StrayServer {
  pid: number;
  /** When it started, as the system prints it; with the pid it names this process. */
  started: string;
  /** The same moment, for the person (epoch ms). */
  startedAtMs?: number;
  exe: string;
  root: string;
  kind: StrayKind;
  /** Its sessions, when its socket answered as this very process. */
  sessions: number | null;
}

/** SW-AGR-19 PR-7: the exact stray the person confirmed stopping. */
export interface StrayTarget {
  pid: number;
  started: string;
  root: string;
}

export type StrayStop =
  | { status: "stopped"; killed: boolean }
  | { status: "gone" }
  | { status: "attached" }
  | { status: "failed"; message: string };

export const listStrayTerminalServers = () => invoke<StrayServer[]>("list_stray_terminal_servers");

export const stopStrayTerminalServer = (target: StrayTarget) =>
  invoke<StrayStop>("stop_stray_terminal_server", { target });

/** SW-AGR-19: how many tabs an unfinished update left to reopen (`null`:
 *  nothing to offer). */
export const machinePendingTabs = (machineId: string) =>
  invoke<number | null>("machine_pending_tabs", { machineId });

/** SW-AGR-19: reopen an unfinished update's tabs, at the person's word. */
export const reopenMachinePendingTabs = (machineId: string) =>
  invoke<TerminalRestoreSummary | null>("reopen_machine_pending_tabs", { machineId });

export type MachineTerminalServerReplacementResult =
  | { status: "replaced"; count: number | null; restored?: TerminalRestoreSummary }
  | {
      status: "failed";
      oldStopped: boolean;
      count: number | null;
      repair: string;
    };

export const prepareMachineTerminalServerReplacement = (machineId: string) =>
  invoke<MachineTerminalServerReplacementPrompt>("prepare_machine_terminal_server_replacement", {
    machineId,
  });

/** SW-AGR-19: how a machine's terminal server is updated — the proof-bound
 *  safe replacement, or (for a server too old for it, a legacy install, a frozen
 *  server without proof) the confirmed reinstall. */
export type TerminalServerUpdateMode = "replace" | "reinstall";

/** How the update would run, and the sessions the running server holds now
 *  (`null` when it cannot say) — for the confirmation. */
export interface TerminalServerUpdatePlan {
  mode: TerminalServerUpdateMode;
  sessions: number | null;
}

export const machineTerminalServerUpdateMode = (machineId: string) =>
  invoke<TerminalServerUpdatePlan>("machine_terminal_server_update_mode", { machineId });

export type ReinstallTerminalServerResult =
  | { status: "reinstalled"; sessions: number; restored?: TerminalRestoreSummary }
  /** More sessions run than were confirmed: nothing ran; confirm this count. */
  | { status: "sessionsChanged"; sessions: number }
  | { status: "failed"; oldStopped: boolean; repair: string };

/** SW-AGR-19: stop the old server by its pid over SSH, install this app's
 *  build, reconnect. Ends every session there; refused without the
 *  confirmation. */
export const reinstallMachineTerminalServer = (
  machineId: string,
  confirmEndSessions: boolean,
  /** The session count the confirmation named; more at stop time refuses. */
  expectedSessions: number | null,
) =>
  invoke<ReinstallTerminalServerResult>("reinstall_machine_terminal_server", {
    machineId,
    confirmEndSessions,
    expectedSessions,
  });

export const restartMachineTerminalServer = (machineId: string, serverToken: string) =>
  invoke<MachineTerminalServerReplacementResult>("restart_machine_terminal_server", {
    machineId,
    serverToken,
  });

/** One row of the machine registry as the active team sees it. */
export type RuntimeMemberAddress =
  | { machineId: "local"; slot: "inProcess" | "sidecar" }
  | { machineId: string; slot: "remote" };

/** Canonical decimal u64; compare using BigInt, never Number or string order. */
export type RuntimeCounter = string;

export interface RuntimeNamespace {
  serverId: string;
  epoch: string;
  generation: RuntimeCounter;
}

export interface FrozenRuntimeLayout {
  lastNamespace: RuntimeNamespace | null;
  serverId: string;
  epoch: string;
  snapshot: TerminalLayoutSnapshot;
  reason: "incompatible" | "unreachable";
}

export type RuntimeAdmissionRow = {
  privateAuthorityEpoch?: string;
  member: RuntimeMemberAddress;
  revision: RuntimeCounter;
} & (
  | { state: "live"; namespace: RuntimeNamespace; historical: null }
  | { state: "historical"; namespace: null; historical: FrozenRuntimeLayout }
  | { state: "unavailable" | "removed"; namespace: null; historical: null }
);

export interface RuntimeMemberTarget {
  member: RuntimeMemberAddress;
  admissionRevision: RuntimeCounter;
  namespace: RuntimeNamespace;
}

export interface RuntimeAdmissionBaseline {
  rows: RuntimeAdmissionRow[];
}

export type RuntimeLayoutReply =
  | (RuntimeMemberTarget & {
      mode: "live";
      privateAuthorityEpoch?: string;
      snapshot: TerminalLayoutSnapshot;
    })
  | {
      mode: "historical";
      privateAuthorityEpoch?: string;
      member: RuntimeMemberAddress;
      admissionRevision: RuntimeCounter;
      frozen: FrozenRuntimeLayout;
    }
  | { mode: "unavailable"; member: RuntimeMemberAddress; admissionRevision: RuntimeCounter };

export interface RuntimeSessionTarget extends RuntimeMemberTarget {
  session: string;
}
export type RuntimeSessionReference = RuntimeSessionTarget | string;
export interface RuntimeWorkspaceTarget extends RuntimeMemberTarget {
  workspace: string;
}
export type RuntimeWorkspaceReference = RuntimeWorkspaceTarget | string;

export const terminalLayoutAdmission = () =>
  invoke<RuntimeAdmissionBaseline>("terminal_layout_admission");

/** SW-AGR-19: how a machine's running terminal server stands against this app.
 *  `otherBuild` (This Mac only): another CodeSense build runs it — shown, never
 *  counted as out of date. */
export type TerminalServerRelation = "same" | "older" | "newer" | "otherBuild";

/** SW-AGR-19: a connected machine's running terminal server, next to this app. */
export interface TerminalServerInfo {
  /** The app release the server was built for; absent for an older build that
   *  predates build stamps. */
  serverVersion?: string;
  appVersion: string;
  upToDate: boolean;
  relation: TerminalServerRelation;
  /** What the server announced (`0.1.0`, `0.1.0+{digest}`). */
  announcedVersion: string;
  buildCommit?: string;
  builtAtMs?: number;
  /** An installed build's digest (remote installs). */
  buildDigest?: string;
  /** This Mac: the running server's executable. */
  serverPath?: string;
  /** This Mac: the server executable this app ships. */
  appServerPath?: string;
  /** This Mac: when the running server started. */
  startedAtMs?: number;
  /** This Mac: the running server's pid. */
  pid?: number;
  /** This app is a development build. */
  devBuild: boolean;
}

/** Self-reported terminal-server observation; byte counts, epoch-ms sample time. */
export interface MachineStats {
  load1?: number;
  cpuCount?: number;
  memUsedBytes?: number;
  memTotalBytes?: number;
  diskFreeBytes?: number;
  diskTotalBytes?: number;
  diskPath?: string;
  sampledAt?: number;
  machineStatsSupported?: boolean;
}

export interface MachineSummary extends MachineStats {
  /** Personal private-binding permission; omitted older summaries mean false. */
  remoteMcpControl?: boolean;
  machineId: string;
  kind: "local" | "ssh";
  label: string;
  /** `null` for This Mac. */
  scope: MachineScope | null;
  /** `null` for This Mac. */
  declaration: MachineDeclaration | null;
  /** `null` for This Mac, and for a shared machine this user has not touched. */
  binding: MachineBinding | null;
  /** Whether this user can publish the declaration (Edit/Unshare/Remove). Always
   *  true for personal machines; for team-shared ones it is "can publish to that
   *  team's context" — the governance, no RBAC. */
  canPublish: boolean;
  status: MachineStatus;
  /** Agents on this machine waiting on the user (`⚑ N needs you`). */
  attention: number;
  /** `agentCallbacks: false` → MCP/status hooks can't reach back here; the
   *  capability is hidden, never a failure. */
  capabilities: { agentCallbacks: boolean };
  /** SW-AGR-19: its running terminal server; absent while not connected. */
  terminalServer?: TerminalServerInfo;
}

/** The machines visible to `orgSlug`/`team`: This Mac first, then the team's
 *  shared declarations, then the user's personal ones. */
export const listMachines = (orgSlug: string, team: string | null, freshStats = false) =>
  invoke<MachineSummary[]>("list_machines", { orgSlug, team, freshStats });

/** A saved machine file a team's list skips because it can't be read. */
export interface UnreadableMachineFile {
  machineId: string;
  scope: MachineScope;
  path: string;
  error: string;
}

/** The machine files `orgSlug`/`team`'s list skips as unreadable. */
export const listUnreadableMachineFiles = (orgSlug: string, team: string | null) =>
  invoke<UnreadableMachineFile[]>("list_unreadable_machine_files", { orgSlug, team });

/** Open an unreadable machine file in its default app, to repair it. */
export const openUnreadableMachineFile = (
  orgSlug: string,
  team: string | null,
  machineId: string,
) => invoke<void>("open_unreadable_machine_file", { orgSlug, team, machineId });

/** Delete an unreadable machine file; its machine goes with it. */
export const removeUnreadableMachineFile = (
  orgSlug: string,
  team: string | null,
  machineId: string,
) => invoke<void>("remove_unreadable_machine_file", { orgSlug, team, machineId });

/** A `Host` alias from `~/.ssh/config`, prefilled into the Where step. */
export interface SshConfigHost {
  alias: string;
  hostName: string;
  user: string | null;
  port: number;
  /** The alias's ProxyJump, as `ssh -G` resolved it. */
  proxyJump?: string | null;
  /** The first IdentityFile the alias names beyond ssh's defaults. */
  identityFile?: string | null;
  /** The alias's ProxyCommand — a machine connection can't use one. */
  proxyCommand?: string | null;
}

/** A key the local ssh-agent holds (auto-detected for the Sign in step). */
export interface SshAgentKey {
  keyType: string;
  fingerprint: string;
  comment: string;
  /** The OpenSSH public-key line (`ssh-add -L`) — "Copy public key" copies it. */
  publicKey: string;
  /** A public key file naming exactly this key: signing in with it as the key
   *  file offers the agent only this key. */
  identityFile?: string | null;
}

export interface SshLocalSources {
  configHosts: SshConfigHost[];
  agentKeys: SshAgentKey[];
}

/** What the local SSH setup already knows (config aliases + agent keys). */
export const listSshSources = () => invoke<SshLocalSources>("list_ssh_sources");

/** Ask for `keyPath`'s passphrase in a system dialog and keep it in the OS
 *  keychain. Resolves the keychain HANDLE (null when the user cancelled) — the
 *  passphrase itself never reaches the webview or crosses IPC. */
export const captureMachinePassphrase = (keyPath: string) =>
  invoke<CredentialRef | null>("capture_machine_passphrase", { keyPath });

/** How the user signs in. `agent` (the default) = ssh-agent / config identity;
 *  `keyFile` = a key without a passphrase; `keychainPassphrase` = a key whose
 *  passphrase {@link captureMachinePassphrase} kept, named by its handle. */
export type MachineSignIn =
  | { method: "agent" }
  | { method: "keyFile"; keyPath: string }
  | { method: "keychainPassphrase"; keyPath: string; credentialRef: CredentialRef };

/** The Add-machine form. `trustedHostKeyFp` carries a first-sight Trust click. */
export interface MachineDraftInput {
  mode?: "legacy" | "private-account";
  label: string;
  host: string;
  port: number;
  username: string | null;
  jumpHost: string | null;
  signIn: MachineSignIn;
  trustedHostKeyFp: string | null;
}

export type MachineTestStepId =
  | "private-account"
  | "agentPrerequisite"
  | "reachable"
  | "hostKey"
  | "auth"
  | "platform"
  | "dataDir"
  | "terminalServer";

/** A failed check's specific next step (the Add-machine error table). A missing or stale
 *  terminal server is not a failure: it surfaces as `installPlan`. */
export type MachineNextStep =
  | "enroll-private-account"
  | "install-terminal-server"
  | "editAddress"
  | "useJumpHost"
  | "retry"
  | "chooseKey"
  | "changeUser"
  | "copyPublicKey";

/** `needsTrust` = first sight of a host key (fingerprint + Trust). `blocked` = the
 *  key differs from the pinned one — no bypass. `skipped` = not reached (an
 *  earlier step failed). */
export type MachineTestStepStatus = "ok" | "needsTrust" | "warn" | "fail" | "blocked" | "skipped";

export interface MachineTestStep {
  id: MachineTestStepId;
  status: MachineTestStepStatus;
  detail: string;
  nextSteps: MachineNextStep[];
  /** Host-key step only: the presented fingerprint (for Trust / review). */
  fingerprint?: string;
}

export interface InstallPlan {
  sizeBytes: number;
  targetDir: string;
  /** Exactly the shell commands the install runs, for "Show commands". */
  commands: string[];
}

export interface MachineTestReport {
  steps: MachineTestStep[];
  /** Present when the terminal server is missing or the wrong version — the
   *  Install step renders from it. */
  installPlan: InstallPlan | null;
  /** Unsupported OS: the machine can be saved but only for file view (later). */
  fileViewOnly: boolean;
}

/** Run the live checklist against a draft (nothing is saved). `machineId` names
 *  a saved machine whose pinned host key the check holds it to. */
export const testMachineConnection = (machineId: string | null, input: MachineDraftInput) =>
  invoke<MachineTestReport>("test_machine_connection", { machineId, input });

export const enrollPrivateMachineAccount = (machineId: string | null, input: MachineDraftInput) =>
  invoke<MachineTestReport>("enroll_private_machine_account", { machineId, input });

/** An install's outcome. An `interrupted` one (the upload broke off) is safe to
 *  resume: an install is idempotent and never leaves a partial version in place;
 *  a `failed` one needs its cause fixed first. `logoutWarning` says when the
 *  machine would end the terminal server at logout, and how to stop it. */
export type InstallTerminalServerResult =
  | { ok: true; version: string; logoutWarning: string | null }
  | { ok: false; code: "interrupted" | "failed"; message: string };

/** Upload the terminal server from the app over SSH (no `curl`, no sudo). */
export const installTerminalServer = (machineId: string | null, input: MachineDraftInput) =>
  invoke<InstallTerminalServerResult>("install_terminal_server", { machineId, input });

/** Upsert a machine. `machineId` null = add. `connect: false` saves a draft that
 *  lands as a `Needs setup` row (closing the panel mid-flow); `connect: true`
 *  re-runs the checklist and pins the terminal server's identity first. */
export const saveMachine = (
  machineId: string | null,
  input: MachineDraftInput,
  share: MachineScope,
  connect: boolean,
) => invoke<MachineSummary>("save_machine", { machineId, input, share, connect });

/** The local-only Hide flag (a binding field; never synced). */
export const setMachineHidden = (machineId: string, hidden: boolean) =>
  invoke<void>("set_machine_hidden", { machineId, hidden });

/** Explicit personal preference for reconnecting this machine in the background.
 *  Enrollment and current connection state never enable it implicitly. */
export const setMachineAutoConnect = (machineId: string, autoConnect: boolean) =>
  invoke<void>("set_machine_auto_connect", { machineId, autoConnect });

/** Move a team-shared declaration back to the publisher's personal layer. */
export const unshareMachine = (machineId: string) =>
  invoke<MachineSummary>("unshare_machine", { machineId });

/** Delete a declaration (personal, or team-shared by a publisher). */
export const removeMachine = (machineId: string) => invoke<void>("remove_machine", { machineId });

/** What the shared-account terminal server a legacy machine left running is doing. */
export type LegacyServerRetirement =
  | { status: "notRunning" }
  | { status: "running"; sessions: number }
  | { status: "stopped"; sessions: number };

/** Stop the legacy terminal server `machineId`'s account still runs — at once
 *  when it holds no terminals, else only with `stopWithSessions` (otherwise it
 *  reports `running` for the person to confirm). Rejects when unreachable. */
export const retireLegacyTerminalServer = (machineId: string, stopWithSessions: boolean) =>
  invoke<LegacyServerRetirement>("retire_legacy_terminal_server", {
    machineId,
    stopWithSessions,
  });

/** Connect one saved remote machine in the foreground. */
export const connectMachine = (machineId: string) =>
  invoke<MachineSummary>("connect_machine", { machineId });

/** Disconnect one remote machine without changing its auto-connect setting. */
export const disconnectMachine = (machineId: string) =>
  invoke<MachineSummary>("disconnect_machine", { machineId });

/** Cancel the current generation and retry immediately. */
export const retryMachine = (machineId: string) =>
  invoke<MachineSummary>("retry_machine", { machineId });

export type RemoteShellLaunch = "shell" | AgentRuntime;
export interface RemoteMcpInventoryEntry {
  id: string;
  logicalName: string;
  executionLocation: "desktop" | "remoteWorkspace";
  availability: "selected";
}

export type RemoteShellTabRequest =
  | {
      operation: "create";
      launch?: RemoteShellLaunch;
      agentKey?: string;
      preparedWorkspaceHandle?: string;
      requestId: string;
      machineId: string;
      teamKey: string;
      workspaceId: string;
      /** Omitted: the remote account's home, resolved on the host. */
      cwd?: string;
      name?: string;
      headed: boolean;
      /** A saved Agent's first prompt, in place of its own: it starts with it. */
      initialPrompt?: string;
    }
  | {
      operation: "attach";
      machineId: string;
      teamKey: string;
      reference: { handle: string } | { requestId: string };
      headed: boolean;
    };
export interface RemoteTabOutcome {
  retryable?: boolean;
  agent?: {
    key: string;
    revision: number;
    manifestHash: string;
    preparedWorkspaceHandle: string;
  } | null;
  mcpInventory?: RemoteMcpInventoryEntry[];
  mcpProfileRevision?: string | null;
  status: "opened" | "alreadyLive" | "pending" | "unknown" | "refused" | "ended";
  handle?: string | null;
  requestId?: string | null;
  code?: string | null;
  message?: string | null;
  headed: boolean;
  workspaceId?: string | null;
  cwd?: string | null;
  target?: RuntimeSessionTarget | null;
  workspaceKey?: string | null;
}
export const remoteShellTab = (
  request: RemoteShellTabRequest,
  caller?: { frameId?: string; pagePath?: string },
) => invoke<RemoteTabOutcome>("terminal_remote_shell_tab", { request, ...(caller ?? {}) });

export interface PreparedRemoteWorkspaceChoice {
  handle: string;
  cwd: string;
  sourceRepo: SourceRepoSlug;
  branch: string;
}
export const listPreparedRemoteWorkspaces = (
  machineId: string,
  teamKey: string,
  workspaceId: string,
) =>
  invoke<PreparedRemoteWorkspaceChoice[]>("list_prepared_remote_workspaces", {
    machineId,
    teamKey,
    workspaceId,
  });

/** Create (or reconcile) a story's workspace on a machine. The paths are
 *  derived on the machine; progress arrives as `REMOTE_WORKSPACE_PROGRESS_EVENT`. */
export interface PrepareRemoteStoryWorkspaceRequest {
  requestId: string;
  machineId: string;
  teamKey: string;
  workspaceId: string;
  storyId: string;
  /** `owner/repo` as the team configures it. */
  sourceRepo: string;
  baseRef: string;
  branch: string;
}

/** The machine's answer: `prepared` / `alreadyPrepared` carry the managed
 *  handle and the worktree; `refused` / `unknown` carry why. */
export interface PreparedRemoteWorkspace {
  status: "prepared" | "alreadyPrepared" | "refused" | "unknown" | (string & {});
  handle?: string | null;
  cwd?: string | null;
  repoPath?: string | null;
  repositoryKey?: string | null;
  resolvedCommit?: string | null;
  branch?: string | null;
  message?: string | null;
}

/** The machine's answer with the preparation identity it was made under: a
 *  prior preparation of this workspace's worktree is replayed as it was made,
 *  so these can differ from the request. */
export interface PreparedRemoteStoryWorkspace {
  requestId: string;
  baseRef: string;
  branch: string;
  result: PreparedRemoteWorkspace;
  /** SW-XS-1: once the worktree exists — `pending` until an agent launch gave
   *  the workspace its runtime context, `ready` after. */
  agentContext?: "pending" | "ready";
}

export const prepareRemoteStoryWorkspace = (request: PrepareRemoteStoryWorkspaceRequest) =>
  invoke<PreparedRemoteStoryWorkspace>("prepare_remote_story_workspace", { request });

/** SW-XS-9: which workspace on a machine to move to its base's newest commit. */
export interface UpdateWorkspaceBaseRequest {
  machineId?: string;
  workspaceHandle?: string;
  storyId?: string;
  workspaceId?: string;
  teamKey?: string;
  /** Remove managed copies the new base tracks instead of refusing. */
  removeManagedCopies?: boolean;
}

export interface WorkspaceBaseUpdateReport {
  machineId: string;
  workspaceHandle: string;
  baseRef: string;
  status:
    | "updated"
    | "upToDate"
    | "refusedDirty"
    | "refusedDiverged"
    | "refusedManagedOverlap"
    | "refused"
    | "unknown"
    | (string & {});
  previousCommit: string | null;
  commit: string | null;
  /** Bundle bytes sent: only the commits the machine lacked. */
  bundleBytes: number;
  /** Unedited CodeSense copies the new base tracks (removed only with
   *  `removeManagedCopies`). */
  managedOverlap: string[];
  /** Edited CodeSense copies the new base tracks: never removed. */
  modifiedManaged: string[];
  /** Other ignored files the new base tracks: never overwritten. */
  unmanagedOverlap: string[];
  dirty: string[];
  message: string | null;
}

/** Move a machine workspace to its base's newest commit: incremental bundle,
 *  fast-forward only when clean. Progress arrives as `WORKSPACE_BASE_PROGRESS_EVENT`. */
export const updateWorkspaceBase = (request: UpdateWorkspaceBaseRequest) =>
  invoke<WorkspaceBaseUpdateReport>("update_workspace_base", { request });

/** Personal permission; enabling requires explicit confirmation in the UI. */
export const setMachineRemoteMcpControl = (machineId: string, enabled: boolean) =>
  invoke<void>("set_machine_remote_mcp_control", { machineId, enabled });

// ── Routines ─────────────────────────────────────────────────────────────────
// Thin wrappers over the routine service. A routine key is the rendered,
// account-scoped string the backend serializes (`org/team/id`, or
// `_personal/<account>/personal/<id>`); every time is epoch milliseconds.

/** Who wrote a routine revision. */
export interface RoutineAuthor {
  kind: "user" | "agent";
  name: string;
}

export interface RoutineMetadataCondition {
  selection: ContextQuery;
  target: ContextFilter;
  previous?: ContextFilter;
  list?: boolean;
}

export type RoutineEventDef = {
  kind: string;
  from_agent?: string;
  metadata?: RoutineMetadataCondition;
};

export type RoutineTriggerDef =
  | { schedule: { cron: string; tz: string } }
  | { event: RoutineEventDef };

/** A routine definition as stored (`routine.yml`, under its agent's dir). */
export type RoutineAction = "agent" | "command";
export interface RoutineAfter {
  routine: string;
  on: "succeeded" | "finished";
}
export interface RoutineCommandEvidence {
  action?: RoutineAction;
  max_runtime_ms?: number;
  argv?: string[];
  exit_code?: number | null;
  duration_ms?: number | null;
  stderr_tail?: string | null;
  cwd?: string | null;
  causation?: { routine_key: string; routine_name: string; run_id: string; phase: string } | null;
}

export interface RoutineDef {
  action?: RoutineAction;
  command?: string[];
  after?: RoutineAfter;
  id: string;
  name: string;
  owner: string;
  /** The agent that runs it: an agent id in the routine's own space, or an agent key. Read
   *  from the routine's dir (it lives under its agent), so always present here. */
  agent?: string;
  /** `fresh` starts a new instance per fire; `wake` delivers into a live one (SW-AGR-2).
   *  Absent only on a routine written before modes, which runs fresh. */
  mode?: RoutineMode;
  task?: string;
  /** `machine`: `local`, `any` or a team-shared machine id (kept, not yet used). */
  where: { kind: string; machine?: string };
  triggers?: RoutineTriggerDef[];
  policy: {
    overlap: "coalesce" | "skip" | "queue";
    missed: "skip" | "run_once";
    max_runtime?: string;
    visible: boolean;
    /** How long a wake waits for its session to be idle (`30m`, `2h`); 30m when absent. */
    max_wait?: string;
    /** SW-AGR-13, each optional: wakes in all (reached, the routine pauses itself). */
    max_wakes?: number;
    /** Wakes in any rolling 24 h. */
    max_wakes_per_day?: number;
    /** The least time between two wakes into one session (`30m`, `2h`, `1d`). */
    min_gap?: string;
  };
  variables?: Record<string, { type: "text" | "number" | "boolean"; default?: string }>;
  revision: number;
  armed_host: string;
  /** A team routine's mode: run once for the team on the `armed_host` Mac
   *  (`one_mac`, also when absent), or on each Mac that turns it on (`each_mac`). */
  runs_on?: RoutineRunsOn;
  /** The machine whose session server arms it (kept, not yet used). */
  armed_machine?: string;
  archived_at?: string;
  /** Archived because its agent was: unarchiving the agent offers it back. */
  archived_with_agent?: boolean;
  /** Where a migrated routine came from; `note` says why it was renamed on the way. */
  migrated?: {
    from: string;
    note?: string;
    /** The revision the migration wrote; a later revision means it has been saved since. */
    revision?: number;
  };
  result_schema?: unknown;
}

/** What a routine fire does. */
export type RoutineMode = "wake" | "fresh";

/** How a team routine runs. */
export type RoutineRunsOn = "one_mac" | "each_mac";

/** A routine's latest run as its list row shows it. */
export interface RoutineLastRun {
  run_id: string;
  phase: RoutineRunPhase;
  reason: string | null;
  /** When it ended, or started while still running. */
  at: number;
  result_text: string | null;
  /** Why the run ended (a launch refusal, an unfilled task, a signal-only CLI's exit summary). Wake, machine and admission explanations still use `result_text` until SV-ENG-114. */
  detail?: string | null;
  /** SW-AGR-9: when a wake run's task was typed into its live session (epoch ms). */
  delivered_at?: number;
  /** SW-AGR-2: the machine of the live session a wake run asked to wake. */
  wake_machine?: string;
  /** SW-AGR-2b: the machine a remote fresh run (or a wake's launch) started its agent on. */
  launch_machine?: string;
  /** The session the run launched or woke, for "Show ‹agent›'s tab". */
  session_id?: string | null;
}

/** One listed routine (mirror of Rust `RoutineRow`). */
export interface RoutineRow {
  action?: RoutineAction;
  command?: string[];
  key: string;
  scope: "personal" | "team";
  /** This Mac runs it: a shared job armed here, or any recipe each Mac runs. */
  armed_here: boolean;
  owner_display: string;
  /** This Mac's switch: not paused here, or (each_mac) turned on here. */
  enabled: boolean;
  /** Revision 1's author, when its snapshot is present. */
  created_by: RoutineAuthor | null;
  schedule_in_words: string;
  next_run_at: number | null;
  last_run: RoutineLastRun | null;
  needs_you: boolean;
  needs_you_run: RoutineLastRun | null;
  /** The routine's agent by name, resolved by the backend. */
  agent_display: RoutineAgentDisplay;
  /** The key of the agent it belongs to, when its reference reads as one. */
  agent_key: string | null;
  /** Its agent is nowhere on this Mac: listed under Unassigned, never fired, until an agent
   *  is picked. */
  unassigned: boolean;
  /** Another routine of its space has its id (a sync brought two): never fired. */
  duplicate: boolean;
  /** What a fire does: its stored mode, or its agent's default (`fresh` on a starter). */
  effective_mode: RoutineMode;
  def: RoutineDef;
  /** The caller created it. Only credits and groups it: anyone can change any routine. */
  mine: boolean;
  /** SW-AGR-5: its wake queued on a busy session (`⏰ due`), else null; the golden
   *  `agent_get.routines[].queued` shape. */
  queued?: QueuedWake | null;
  /** SW-XS-25: its arm on its machine is behind what it and its agent are now; the
   *  machine fires with what was last prepared until Refresh on machine. Null otherwise. */
  content_stale?: RoutineContentStale | null;
}

/** A routine's agent as the tab shows it (mirror of Rust `AgentDisplay`). */
export interface RoutineAgentDisplay {
  /** The agent's name, or the reference as written when it does not resolve. */
  name: string;
  archived: boolean;
  /** False when no agent you can see answers to it: its runs fail `agent_unavailable`. */
  resolvable: boolean;
}

export type RoutineRunPhase =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "needs_you"
  | "ended"
  | "timed_out"
  | "cancelled"
  | "skipped"
  | "coalesced"
  /** SW-AGR-2: a wake typed into its agent's live session. */
  | "delivered"
  /** A wake that was not typed (`reason` says why). */
  | "undelivered"
  /** A wake that may have been typed; never re-sent. */
  | "unknown";

/** One `routine_runs` row. */
export interface RoutineRun extends RoutineCommandEvidence {
  trigger_payload?: Record<string, unknown>;
  run_id: string;
  routine_key: string;
  revision: number;
  source: string;
  occurrence_key: string | null;
  phase: RoutineRunPhase;
  reason: string | null;
  linked_run_id: string | null;
  session_id: string | null;
  created_at: number;
  started_at: number | null;
  ended_at: number | null;
  result_text: string | null;
  detail?: string | null;
  /** The agent the run launched, `<key>@<revision>`. */
  agent: string | null;
  /** SW-AGR-9: when a wake run's task was typed into its live session (epoch ms). */
  delivered_at?: number;
  /** SW-AGR-2: the machine of the live session a wake run asked to wake (`local` or
   *  a machine id); set only on a wake run. */
  wake_machine?: string;
  /** SW-AGR-2b: the machine a remote fresh run (or a wake's launch) started its agent on. */
  launch_machine?: string;
}

export interface RoutineRunHistory {
  runs: RoutineRun[];
  /** Pass as `before` for the next page; null on the last. */
  next_before: string | null;
}

export interface RoutineRunDetail extends RoutineRun {
  routine_name: string;
}

export interface RoutineRevision {
  revision: number;
  author: RoutineAuthor;
  at: string;
  routine: RoutineDef;
}

export interface RoutineDetail {
  key: string;
  scope: "personal" | "team";
  armed_here: boolean;
  owner_display: string;
  enabled: boolean;
  agent_display: RoutineAgentDisplay;
  agent_key: string | null;
  unassigned: boolean;
  effective_mode: RoutineMode;
  def: RoutineDef;
  revisions: RoutineRevision[];
}

export interface SavedRoutine {
  key: string;
  def: RoutineDef;
  warnings: AgentLaunchWarning[];
}

/** What admission decided for a manual fire. */
export type RoutineAdmission = { run_id: string } & (
  | { outcome: "queued" }
  | { outcome: "held" }
  | { outcome: "coalesced"; active_run_id: string }
  | { outcome: "skipped"; reason: string }
  | { outcome: "duplicate" }
);

export interface RoutineSchedulerStatus {
  /** Occurrences skipped while the app was closed or asleep, since acknowledged. */
  missed_count: number;
}

export const routineList = () => invoke<RoutineRow[]>("routine_list");

export const routineGet = (key: string) => invoke<RoutineDetail>("routine_get", { key });

/** Flip this Mac's switch: pause or resume a shared job, or turn a recipe each
 *  Mac runs on or off here. Local state, never an edit. */
/** SW-XS-25: resolves to what to tell the person when a resume could not prepare the
 *  routine on its machine, else null. */
export const routineSetEnabled = (key: string, enabled: boolean) =>
  invoke<string | null>("routine_set_enabled", { key, enabled });

export const routineRunNow = (
  key: string,
  variables?: Record<string, string>,
  frameId?: string,
  pagePath?: string,
) =>
  invoke<RoutineAdmission>("routine_run_now", {
    key,
    variables: variables ?? null,
    frameId: frameId ?? null,
    pagePath: pagePath ?? null,
  });

export const routineRuns = (key: string, limit?: number, before?: string) =>
  invoke<RoutineRunHistory>("routine_runs", { key, limit: limit ?? null, before: before ?? null });

/** Every routine's runs since `sinceMs` (epoch ms), newest first, in one bounded read: at
 *  most `perRoutine` of each routine's runs and as many of its merged and skipped fires
 *  (the Agents tab Schedule's Recent, SW-AGR-5). */
export const routineRunsSince = (sinceMs: number, perRoutine?: number) =>
  invoke<RoutineRun[]>("routine_runs_since", { sinceMs, perRoutine: perRoutine ?? null });

/** End a run `cancelled`; false when it had already ended. A queued wake is withdrawn from
 *  its host first; an unreachable host keeps it queued and rejects. */
export const routineStopRun = (runId: string) => invoke<boolean>("routine_stop_run", { runId });

export const routineArchive = (key: string) => invoke<SavedRoutine>("routine_archive", { key });

export const routineCopyToMine = (key: string) =>
  invoke<SavedRoutine>("routine_copy_to_mine", { key });

export const routineArmHere = (key: string) => invoke<SavedRoutine>("routine_arm_here", { key });

/** SW-AGR-8: move where a routine fires: this Mac (`machine` omitted) or a machine's
 *  terminal server, which then fires it on its own clock with no Mac open. */
export const routineArmOn = (key: string, machine?: string) =>
  invoke<SavedRoutine>("routine_arm_on", { key, machine: machine ?? null });

/** SW-AGR-8: stop waiting for a machine that is gone to confirm it stopped firing a
 *  routine moved off it. */
export const routineHandoverRelease = (key: string, machine: string) =>
  invoke<string | null>("routine_handover_release", { key, machine });

/** SW-XS-25: a machine-armed routine whose prepared content is behind. */
export interface RoutineContentStale {
  machine: string;
  since_ms: number;
  why: string;
}

/** SW-XS-25: "Refresh on machine": prepare a machine-armed routine there from what it is
 *  now and push it. Resolves to the machine. */
export const routineRefreshOnMachine = (key: string) =>
  invoke<string>("routine_refresh_on_machine", { key });

export const routineRunDetail = (runId: string) =>
  invoke<RoutineRunDetail>("routine_run_detail", { runId });

// ── The page bridge's routine and run reads: each is its MCP twin's shape
// (`routine_list`/`routine_get`/`routine_runs`/`routine_run` data, `list_runs`,
// `read_run`). Times are RFC 3339 strings.

/** One `routine_list` row. */
export interface RoutineListItem {
  id: string;
  scope: "personal" | "team";
  org: string;
  team: string;
  name: string;
  schedule_in_words: string;
  agent: string;
  where: string;
  runs_on: "one_mac" | "each_mac";
  enabled: boolean;
  next_run_at: string | null;
  last_run: { status: RoutineRunPhase; reason?: string; at: string } | null;
  owner: string;
  owner_name: string;
  created_by: RoutineAuthor | null;
  armed_here: boolean;
  needs_you: boolean;
  revision: number;
}

/** One `routine_runs` row as the MCP tool returns it. */
export interface RoutineRunItem extends RoutineCommandEvidence {
  run_id: string;
  revision: number;
  source: string;
  phase: RoutineRunPhase;
  reason: string | null;
  linked_run_id: string | null;
  created_at: string;
  started_at: string | null;
  ended_at: string | null;
  result_text: string | null;
  detail?: string | null;
  agent: string | null;
}

/** `routine_get` data. */
export interface RoutineGot {
  id: string;
  scope: "personal" | "team";
  owner: string;
  owner_name: string;
  armed_here: boolean;
  runs_on: "one_mac" | "each_mac";
  enabled: boolean;
  revision: number;
  schedule_in_words: string;
  routine: RoutineDef;
  runs: RoutineRunItem[];
}

/** `routine_runs` data. */
export interface RoutineRunsPage {
  runs: RoutineRunItem[];
  next_before: string | null;
}

/** `routine_run` data: the run with the routine it belongs to. */
export interface RoutineRunGot extends RoutineRunItem {
  routine_id: string;
  routine_name: string;
}

export const routineReadList = (frameId?: string, page?: string) =>
  invoke<RoutineListItem[]>("routine_read_list", { frameId, page });
export const routineReadGet = (key: string, frameId?: string, page?: string) =>
  invoke<RoutineGot>("routine_read_get", { key, frameId, page });
export const routineReadRuns = (
  key: string,
  limit?: number,
  before?: string,
  frameId?: string,
  page?: string,
) =>
  invoke<RoutineRunsPage>("routine_read_runs", {
    key,
    limit: limit ?? null,
    before: before ?? null,
    frameId,
    page,
  });
export const routineReadRun = (runId: string, frameId?: string, page?: string) =>
  invoke<RoutineRunGot>("routine_read_run", { runId, frameId, page });

/** `read_run`: one agent run in full. */
export interface RunRead {
  /** Its `list_runs` row; null while it runs or when it recorded none. */
  outcome: RunOutcomeRead | null;
  transcript: SessionTranscript | null;
  /** The tail of its captured output (redacted), or null. */
  artifact: string | null;
  artifactTruncated: boolean;
}

export const runReadList = (limit?: number) =>
  invoke<RunOutcomeRead[]>("run_read_list", { limit: limit ?? null });
export const runRead = (runId: string, maxArtifactBytes?: number) =>
  invoke<RunRead>("run_read", { runId, maxArtifactBytes: maxArtifactBytes ?? null });
/** `read_run_evidence`: the run's evidence, scoped to `storyId` or else its own story. */
export const runReadEvidence = (runId: string, storyId?: string) =>
  invoke<RunEvidence | null>("run_read_evidence", { runId, storyId: storyId ?? null });
/** `read_run_work_product`: the branch the run produced, or null. */
export const runReadWorkProduct = (runId: string) =>
  invoke<RunWorkProduct | null>("run_read_work_product", { runId });

export const routineNeedsYouCount = () => invoke<number>("routine_needs_you_count");

export const routineSchedulerStatus = () =>
  invoke<RoutineSchedulerStatus>("routine_scheduler_status");

export const routineAcknowledgeMissed = () => invoke<void>("routine_acknowledge_missed");

/** The routine sentence as the editor sends it. A create needs name and task;
 *  an edit sends only changed fields and preserves other triggers. */
export interface RoutineForm {
  action?: RoutineAction;
  command?: string[];
  after?: RoutineAfter | null;
  /** Replace metadata triggers only; omitted preserves them and other events. */
  metadataTriggers?: RoutineMetadataCondition[];
  name?: string;
  task?: string;
  /** A preset (`every 5m`, `daily 09:00`, `weekdays 09:00`, `weekly mon 09:00`,
   *  `hourly`) or a 5-field cron, read in `tz`. */
  when?: string;
  tz?: string;
  /** The agent that runs it: an agent id in the routine's own space, or an agent key.
   *  A create needs one. */
  agent?: string;
  where?: string;
  overlap?: RoutineDef["policy"]["overlap"];
  missed?: RoutineDef["policy"]["missed"];
  maxRuntime?: string;
  visible?: boolean;
  resultSchema?: unknown;
  /** A team routine's mode. */
  runsOn?: RoutineRunsOn;
  /** `fresh` or `wake`; a create without one gets its agent's default. */
  mode?: RoutineMode;
  /** SW-AGR-13's wake limits: a value sets one, `null` clears it on an edit, absent keeps
   *  it. A count that is not a whole number is sent as typed, so the save refuses it. */
  maxWakes?: number | string | null;
  maxWakesPerDay?: number | string | null;
  minGap?: string | null;
  /** Where a wake looks for the agent (SW-AGR-2's `target.machine`): `local`,
   *  a machine id from `list_machines`, or `any`. "Keep this…" sends the timer's
   *  tab's machine. */
  machine?: string;
}

/** What a save or restore did; a stale base revision is a conflict, not an error. */
export type RoutineSaveReply =
  | {
      outcome: "saved";
      key: string;
      revision: number;
      /** What a run would be warned about (e.g. a `/command` its agent does not start
       *  with). Never blocks the save. */
      warnings: AgentLaunchWarning[];
    }
  | { outcome: "conflict"; current_revision: number };

export interface RoutineSchedulePreview {
  cron: string;
  tz: string;
  in_words: string;
  /** Epoch ms. */
  next_fires: number[];
}

export interface RoutineAuthoringAgent {
  opened: boolean;
  message: string;
}

/** Create a routine (no `key`; `enabled: false` saves a draft) or edit one against
 *  `baseRevision`. A create goes to `team` ("Who can use it: ‹Team›"), else to your
 *  scratch space for the open org ("Only me"). */
export const routineSave = (
  form: RoutineForm,
  edit?: { key: string; baseRevision: number },
  enabled?: boolean,
  team?: { org: string; team: string },
) =>
  invoke<RoutineSaveReply>("routine_save", {
    key: edit?.key ?? null,
    baseRevision: edit?.baseRevision ?? null,
    form,
    enabled: enabled ?? null,
    team: team ?? null,
  });

export const routinePreviewSchedule = (when: string, tz?: string) =>
  invoke<RoutineSchedulePreview>("routine_preview_schedule", { when, tz: tz ?? null });

export const routineRestoreRevision = (key: string, revision: number, baseRevision: number) =>
  invoke<RoutineSaveReply>("routine_restore_revision", { key, revision, baseRevision });

/** Open a visible agent in the active team workspace, asked to help create a routine. */
export const routineOpenAuthoringAgent = () =>
  invoke<RoutineAuthoringAgent>("routine_open_authoring_agent");

/** Create canonical local context without a provider account or Git transport. */
/** Create a new context repo at `destinationPath` (its parent is created when missing)
 *  and connect it. `fromExample` seeds it with an editable copy of the CodeSense docs
 *  repo's setup. */
export const createLocalOrganization = async (
  displayName: string,
  destinationPath: string,
  fromExample = false,
) =>
  invoke<ConnectOrgResult>("create_local_organization", {
    displayName,
    destinationPath,
    expectedPrincipal: await rendererPrincipal(),
    fromExample,
  });

/** The bundled CodeSense docs context repo, placed and connected for this profile. */
export interface CodesenseDocsResult {
  org_slug: string;
  org_root: string;
  team: string;
  readme_path: string;
  placement: "placed" | "updated" | "kept";
}

/** Place the bundled CodeSense docs repo under the CodeSense home (first launch) and
 *  connect it. Idempotent; runs no git beyond a local checkout of the bundled commit. */
export const ensureCodesenseDocs = () => invoke<CodesenseDocsResult>("ensure_codesense_docs");

/** SW-ODC-11 result shape for the sample source repo placement call. */
export interface SampleSourceRepoResult {
  root: string;
  bundledCommit: string;
  /** `placed`, `updated`, `kept`, or `refused`. `refused` fires AC-11 (non-destructive
   *  posture): a non-matching existing directory at the target path is kept untouched and
   *  dashboards fall back to seed-data-only mode. */
  placement: "placed" | "updated" | "kept" | "refused";
  available: boolean;
  refusedReason: string | null;
}

/** Place the bundled sample source repo under the CodeSense home (SW-ODC-11). Mirrors
 *  `ensureCodesenseDocs`; idempotent and non-destructive. */
export const ensureSampleRepo = () => invoke<SampleSourceRepoResult>("ensure_sample_repo");

/** Register an existing checkout in place, without forge authentication. */
export const attachContextRepository = (clonePath: string) =>
  invoke<ConnectOrgResult>("attach_context_repository", { clonePath });
export const listContextAttachments = () => invoke<OrgSummary[]>("list_context_attachments");
export const detachContextRepository = (orgSlug: string) =>
  invoke<void>("detach_context_repository", { orgSlug });

/** Compatibility for older UI consumers: never dispatch retired repository mutations. */
function retiredRepositoryAction<T = void>(action: string, ..._args: unknown[]): Promise<T> {
  return Promise.reject(
    new Error(
      `${action} has been retired. Prepare and synchronize repositories with your agent or CLI, then attach the local folder.`,
    ),
  );
}

export interface RoutineResultStatus {
  routine_key: string;
  revision: number;
  schema_compatible: boolean;
  snapshot: { run_id: string; revision: number; succeeded_at: number; payload: unknown } | null;
  latest_attempt: RoutineRun | null;
  active_attempt: RoutineRun | null;
}
export const routineResult = (key: string, frameId?: string, page?: string) =>
  invoke<RoutineResultStatus>("routine_result", { key, frameId, page });
export const routineCancelResult = (key: string) =>
  invoke<number>("routine_cancel_result", { key });

/** SW-OC-26: find context files by metadata across the whole footprint (the one
 *  Rust engine — see `@/shared/lib/context-query/types`). `page`: answering for a
 *  page (dashboard, csquery block, pane) at that path — private items then come
 *  only from the page's own org (`""` = none). Required: the app always answers
 *  for a page. */
export const queryContext = (
  query: ContextQuery,
  page: string,
  frameId?: string,
  callerPage?: string,
  callerTeam?: string,
) => invoke<ContextQueryResult>("query_context", { query, page, frameId, callerPage, callerTeam });

/** Where a page or file sits in the user's footprint. */
export interface ContentLocation {
  /** `null` for all-orgs content. */
  org: string | null;
  /** `null` for private and all-orgs content. */
  team: string | null;
  layer: "team" | "private" | "all-orgs";
  /** The content root's absolute directory. */
  contentRoot: string;
}

/** The page bridge's `context()`: the content root the page at `page` lives in
 *  (`null` outside every known root). `page` comes from the trusted host. */
export const bridgeDocContext = (page: string) =>
  invoke<ContentLocation | null>("bridge_doc_context", { page });

/** A file a page may open, resolved inside a team's content root. */
export interface BridgeOpenTarget {
  path: string;
  org: string;
  team: string;
  contentRoot: string;
}

/** The page bridge's `openFile`: validate a page-supplied path against the known
 *  team content roots and resolve it (rejects `refused:` / `not-found:`). */
export const bridgeResolveOpenTarget = (
  page: string,
  path: string,
  team?: string,
  frameId?: string,
) =>
  invoke<BridgeOpenTarget>("bridge_resolve_open_target", {
    page,
    path,
    team: team ?? null,
    frameId,
  });

/** A frontmatter value `updateFrontmatter` can write; `null` removes the key. */
export type FrontmatterValue = string | number | boolean | null | Array<string | number | boolean>;

/** Set (or with `null`, remove) top-level frontmatter keys of a content file
 *  inside a team or private content root, in one atomic, byte-preserving write. */
export const updateFrontmatter = (
  path: string,
  fields: Record<string, FrontmatterValue>,
  frameId?: string,
  pagePath?: string,
) =>
  invoke<void>("update_frontmatter", {
    path,
    fields,
    ...(frameId ? { frameId } : {}),
    ...(pagePath ? { pagePath } : {}),
  });

// === Workspace move bridges ===

/** Inputs to the Rust move-workspace pipeline; local targets require a worktree path. */
export interface MoveWorkspaceParams {
  storyId: string;
  /** Machine ID, or "local" for This Mac. */
  targetMachine: string;
  /** Defaults to false; dirty source worktrees are refused unless explicitly allowed. */
  discardUncommitted?: boolean;
  /** Required for local targets; remote targets derive their path and ignore this field. */
  targetWorktreePath?: string | null;
}

export interface MoveWorkspaceResult {
  moved: boolean;
  /** Null machine IDs represent This Mac. */
  from: string | null;
  to: string | null;
  handle: string | null;
  cwd: string | null;
  /** Omitted when source cleanup completed. */
  cleanupPending?: boolean;
}

/** Rust `MoveIntentView`, also emitted by `move-workspace-intent-hydrate`. */
export interface MoveIntentView {
  teamKey: string;
  workspaceId: string;
  from: {
    /** Null represents This Mac. */
    machineId: string | null;
    requestId: string;
    handle: string | null;
    cwd: string | null;
    repoPath: string;
    repositoryKey: string;
  };
  to: {
    /** Null represents This Mac. */
    machineId: string | null;
    requestId: string;
  };
  step: "preparing-target" | "swapping-location" | "cleaning-source";
  dirtyAllowed: boolean;
  startedAtMs: number;
  lastProgressAtMs: number;
}

/** Move a workspace in the active team to another machine. */
export const moveWorkspaceBridge = (params: MoveWorkspaceParams) =>
  invoke<MoveWorkspaceResult>("move_workspace_bridge", { params });

/** Read open move intents to restore the viewer's move projections on startup. */
export const listMoveIntentsBridge = () => invoke<MoveIntentView[]>("list_move_intents_bridge");

// === SW-WC-1: per-workspace archive flag ===

/** One workspace's archive state as the Rust registry reports it.
 *
 *  `archivedAtMs` is epoch-ms when the operator marked the workspace archived,
 *  or `null` for an active workspace. The entry is keyed by the viewer
 *  workspace id string (`root:<owner>/<repo>` / `story:<id>` / `generic:<path>` —
 *  the same form `OpenTerminalTabParams.workspaceId` carries).
 *
 *  See `_bmad-output/21-workspace-chat/stories/SW-WC-1-workspace-archive.md`.
 */
export interface WorkspaceArchiveState {
  workspaceId: string;
  archivedAtMs: number | null;
}

/** Stamp a workspace as archived now. Writes `~/.codesense/workspace-registry.json`
 *  atomically and emits `codesense:workspace-archive-changed`.
 *  SW-WC-2 reads this flag to lock the workspace-chat compose box. */
export const archiveWorkspace = (workspaceId: string) =>
  invoke<WorkspaceArchiveState>("archive_workspace", { workspaceId });

/** Clear a workspace's archive flag. Keeps the registry entry so the FE can
 *  distinguish "explicitly unarchived" from "never archived". */
export const unarchiveWorkspace = (workspaceId: string) =>
  invoke<WorkspaceArchiveState>("unarchive_workspace", { workspaceId });

/** Read one workspace's archive state. An absent entry returns
 *  `{ workspaceId, archivedAtMs: null }` — the FE renders the "active" look. */
export const getWorkspaceArchiveState = (workspaceId: string) =>
  invoke<WorkspaceArchiveState>("get_workspace_archive_state", { workspaceId });

/** List every workspace with a persisted archive entry (archived or
 *  explicitly unarchived). Merge with the visible workspace set. */
export const listWorkspaceArchiveStates = () =>
  invoke<WorkspaceArchiveState[]>("list_workspace_archive_states");

// ─────────────────────────────────────────────────────────────────────────────
// SW-WC-2: workspace chat engine.
//
// One primitive, two surfaces: this Tauri surface for the inbox group chat,
// plus an MCP tool pair (`workspace_chat_send` / `workspace_chat_read`) for
// agents. Storage: append-only JSONL per workspace under
// `~/.codesense/workspace-chat/chats/<slug>.jsonl` + `index.json` registry.
// Message shape is a strict superset of `InboxMessageRow` — same renderer
// (`InboxMessageContent`) in the chat pane.
// ─────────────────────────────────────────────────────────────────────────────

/** One line in a workspace chat log. Strict superset of `InboxMessageRow`;
 *  the renderer (`InboxMessageContent`) consumes it with no transform. */
export interface WorkspaceChatMessage {
  messageId: number;
  from: string;
  fromName: string;
  createdAtMs: number;
  inReplyTo?: number | null;
  text: string;
  primaryFile?: string | null;
  attachments?: InboxMediaAttachment[];
  onClick?: MessageOnClick | null;
  contentType?: MessageContentType | null;
  workspaceId: string;
  kind: "chat" | "system";
}

/** One workspace's row in the chat index — carries the sidebar's badge data
 *  without having to scan every log file. */
export interface WorkspaceChatIndexEntry {
  workspaceId: string;
  slug: string;
  displayName?: string | null;
  createdAtMs: number;
  lastMessageAtMs?: number | null;
  headId: number;
  archivedAtMs?: number | null;
}

/** Result of a successful `workspace_chat_send`. */
export interface WorkspaceChatSendResult {
  messageId: number;
  createdAtMs: number;
  headId: number;
}

/** Result of a `workspace_chat_read`. Messages are OLDEST-FIRST regardless of
 *  `since`. `archived` reflects SW-WC-1's registry — a chat can be read even
 *  when archived, but new sends are refused. */
export interface WorkspaceChatReadResult {
  humanReadId?: number;
  workspaceId: string;
  messages: WorkspaceChatMessage[];
  headId: number;
  archived: boolean;
}

/** Payload for the `codesense:workspace-chat-updated` Tauri event — nudges
 *  the dashboard to re-fetch with `since = lastSeenHeadId`. */
export interface WorkspaceChatUpdatedPayload {
  workspaceId: string;
  headId: number;
}

/** Human compose. The invoke path stamps `from = "human"` and `fromName` from
 *  the active profile's display name (fallback chain in `commands.rs`). */
export const workspaceChatSend = (
  workspaceId: string,
  text: string,
  options?: {
    contentType?: MessageContentType;
    onClick?: MessageOnClick;
    primaryFile?: string;
    attachments?: InboxMediaAttachment[];
  },
) =>
  invoke<WorkspaceChatSendResult>("workspace_chat_send", {
    workspaceId,
    text,
    contentType: options?.contentType ?? null,
    onClick: options?.onClick ?? null,
    primaryFile: options?.primaryFile ?? null,
  });

/** Read a window of chat messages. `since = null` returns the tail (last
 *  `limit` messages). `since = k` returns messages with id > k up to `limit`. */
export const workspaceChatRead = (
  workspaceId: string,
  since?: number | null,
  limit?: number | null,
) =>
  invoke<WorkspaceChatReadResult>("workspace_chat_read", {
    workspaceId,
    since: since ?? null,
    limit: limit ?? null,
  });

export const workspaceChatMarkViewed = (workspaceId: string, throughId: number) =>
  invoke<void>("workspace_chat_mark_viewed", { workspaceId, throughId });
export interface HumanChatRoom {
  workspaceId: string;
  unread: number;
  agents: number;
}
export const workspaceChatHumanRooms = () => invoke<HumanChatRoom[]>("workspace_chat_human_rooms");

/** List every workspace with a chat log (sorted by last-message time desc). */
export const workspaceChatListWorkspaces = () =>
  invoke<WorkspaceChatIndexEntry[]>("workspace_chat_list_workspaces");

/** Stamp a chat-local archive marker + emit a system row. The authoritative
 *  archive flag lives in SW-WC-1's workspace registry; this is for audit. */
export const workspaceChatArchive = (workspaceId: string) =>
  invoke<void>("workspace_chat_archive", { workspaceId });

/** Old declarations may remain readable, but service operations never cross IPC. */
function retiredServiceAction<T = unknown>(_command: string, _args?: unknown): Promise<T> {
  return Promise.reject(
    new Error(
      "Built-in service connections are retired. Use your agent or CLI for service access and routine result snapshots for dashboards.",
    ),
  );
}

export interface PrivateNavSource {
  repoRoot: string;
  contentPath: string;
  prefix: string;
  baselineStarted: boolean;
  baselineError: string | null;
}
export const resolvePrivateNavSource = (orgSlug: string) =>
  invoke<PrivateNavSource>("resolve_private_nav_source", { orgSlug });

export const openPanelsMenu = () => invoke<void>("open_panels_menu");

export interface DashboardPublisherKeyStatus {
  status: "missing" | "configured" | "refused" | "unavailable";
  hint: string | null;
}
export const dashboardPublisherKeyStatus = () =>
  invoke<DashboardPublisherKeyStatus>("dashboard_publisher_key_status");
export const dashboardPublisherKeySave = (key: string) =>
  invoke<DashboardPublisherKeyStatus>("dashboard_publisher_key_save", { key });
export const dashboardPublisherKeyRemove = () =>
  invoke<DashboardPublisherKeyStatus>("dashboard_publisher_key_remove");

export const dashboardInventory = (folder?: string, localOnly = false) =>
  invoke<import("@/features/dashboard-publishing/types").DashboardInventory>(
    "dashboard_inventory",
    { folder: folder ?? null, localOnly },
  );
export const dashboardDetail = (slug: string) =>
  invoke<import("@/features/dashboard-publishing/types").DashboardDetail>("dashboard_detail", {
    slug,
  });
export const dashboardMoveLink = (slug: string) => invoke<string>("dashboard_move_link", { slug });
export const dashboardPublish = (folder: string) =>
  invoke<import("@/features/dashboard-publishing/types").PublicationStatus>("dashboard_publish", {
    folder,
  });
export const dashboardUnpublish = (slug: string, folder: string | null) =>
  invoke<import("@/features/dashboard-publishing/types").PublicationStatus>("dashboard_unpublish", {
    slug,
    folder,
  });
export const dashboardViewerSignOut = (slug: string, shareId: string) =>
  invoke<void>("dashboard_viewer_sign_out", { slug, shareId });
export const dashboardShare = (slug: string, add: string[], remove: string[]) =>
  invoke<import("@/features/dashboard-publishing/types").PublicationStatus>("dashboard_share", {
    slug,
    add,
    remove,
  });
export interface DashboardLiveRequest {
  slug: string;
  title: string;
  by: string;
  requestedAt: string;
  liveOn: boolean;
  waiting: boolean;
  terminals: boolean;
  control: boolean;
  widerAudience: boolean;
  viewersToAdd: number;
  declined: boolean;
  /** What the row shows; the confirm sends it back so only this is applied. */
  shown: DashboardLiveShown | null;
}
export interface DashboardLiveShown {
  sha: string | null;
  audience: string;
  shareAdd: string[];
}
export const dashboardLiveRequests = () =>
  invoke<DashboardLiveRequest[]>("dashboard_live_requests");
export const dashboardLiveConfirm = (slug: string, shown: DashboardLiveShown) =>
  invoke<import("@/features/dashboard-publishing/types").PublicationStatus>(
    "dashboard_live_confirm",
    { slug, shown },
  );
export const dashboardLiveDecline = (slug: string) =>
  invoke<void>("dashboard_live_decline", { slug });
export const dashboardLiveTurnOff = (slug: string) =>
  invoke<void>("dashboard_live_turn_off", { slug });
export const dashboardControlLinks = (slug: string) =>
  invoke<import("@/features/dashboard-publishing/types").ControlLinks>("dashboard_control_links", {
    slug,
  });

export interface RoleConfig {
  id: string;
  name: string;
  icon: string | null;
  defaultHandler: string | null;
}
export const rolesList = (teamKey: string) => invoke<RoleConfig[]>("roles_list", { teamKey });
export const roleSave = (teamKey: string, config: RoleConfig) =>
  invoke<RoleConfig>("role_save", { teamKey, config });

export function liveHubSubscribe(
  topic: string,
  viewKey: string,
  sinceRev?: number,
  options?: { subscriptionId?: string; window?: [number, number] },
) {
  return invoke<import("@/features/dashboard/lib/live-hub").HubReply>("live_hub_subscribe", {
    topic,
    viewKey,
    sinceRev,
    ...options,
  });
}

export interface FrameTeam {
  org: string;
  team: string;
}
export interface FrameGrant {
  frameId: string;
  principal: string;
  dashboard: string;
  slot: string;
  team: FrameTeam | null;
  scope: "open" | "team" | "private";
}
export interface FramePolicy {
  ownFolderOnly?: boolean;
  blockServiceWorkers?: boolean;
  noOutsideNetwork?: boolean;
  allowedHosts?: string[];
  teamSafe?: boolean;
}
export type FrameCallerKind = "message" | "team" | "private" | "builtin";
export interface FrameRecord {
  parentFrame?: string | null;
  effectiveGrant?: string[];
  kind: FrameCallerKind;
  frameId: string;
  origin: string;
  token: string;
  grant: FrameGrant;
  contentRoot: string;
  documentPath: string;
  assetFolder: string;
  policy: FramePolicy;
  message: { org: string; team: string; messageId: number } | null;
}
export interface MintFrameArgs {
  parentFrame?: string;
  /** Host-selected team, validated against this principal's bundled descriptor. */
  builtinTeam?: FrameTeam;
  documentPath: string;
  contentRoot: string;
  assetFolder?: string;
  slot: string;
  message?: { org: string; team: string; messageId: number };
  policy?: FramePolicy;
}
export const mintFrame = (args: MintFrameArgs) => invoke<FrameRecord>("mint_frame", { args });
export const releaseFrame = (frameId: string) => invoke<void>("release_frame", { frameId });
/** Re-binds a follows-context frame to a team: same origin, a new token, the old one revoked. */
export const rebindFrameTeam = (frameId: string, token: string, org: string, team: string) =>
  invoke<FrameRecord>("rebind_frame_team", { frameId, token, org, team });
export const resolveFrameGrant = (origin: string, token: string, messageIds?: number[]) =>
  invoke<FrameGrant>("resolve_frame_grant", {
    origin,
    token,
    ...(messageIds ? { messageIds } : {}),
  });
/** Set which of the calling frame's declared commands are active; rejects an undeclared id. */
export const commandsRegister = (origin: string, token: string, ids: string[]) =>
  invoke<{ dashboard: string; active: string[] }>("commands_register", { origin, token, ids });

/** A slot dashboard's command as the host registry holds it for list and run. */
export interface SlotCommand {
  /** Namespaced `<dashboardKey>/<id>`. */
  id: string;
  dashboard: string;
  dashboardTitle: string;
  dashboardScope: DashboardRef["scope"];
  slot: string;
  capabilities: string[];
  /** The id the dashboard declared. */
  command: string;
  label: string;
  scope: DashboardCommandScope;
  /** The effective chord, empty when it has none. */
  binding: string;
  /** The manifest's suggestion. */
  humanOnly: boolean;
}

/** A `commands.list` / `command_list` row. */
export interface CommandRow {
  id: string;
  label: string;
  scope: DashboardCommandScope;
  binding: string;
  dashboard: string;
  active: boolean;
  humanOnly: boolean;
}

/** Push the slot dashboards' commands, with their effective chords, to the host. */
export const commandsSync = (commands: SlotCommand[]) =>
  invoke<void>("commands_sync", { commands });
/** The person's list of slot commands (the `commands.list` page verb). */
export const commandsList = () => invoke<CommandRow[]>("commands_list");
/** Run a slot command for the person (the `commands.run` page verb). */
export const commandsRun = (id: string, args?: unknown) =>
  invoke<void>("commands_run", { id, args: args ?? null });
export const liveHubPage = (subscriptionId: string, cursor: string) =>
  invoke<import("@/features/dashboard/lib/live-hub").HubSnapshot>("live_hub_page", {
    subscriptionId,
    cursor,
  });
export const liveHubWindow = (subscriptionId: string, window: [number, number]) =>
  invoke<import("@/features/dashboard/lib/live-hub").HubDelta>("live_hub_window", {
    subscriptionId,
    window,
  });
export const liveHubUnsubscribe = (subscriptionId: string, paused?: boolean) =>
  invoke<void>("live_hub_unsubscribe", { subscriptionId, paused });

export const liveHubViewKey = (origin: string, token: string) =>
  invoke<string>("live_hub_view_key", { origin, token });

/** Acknowledges one projected crash mark. */
export function dismissAgentMark(
  sessionId: string,
  crashId: string,
  requestId?: string,
  frame?: { origin: string; token: string },
): Promise<{ sessionId: string; crashId: string; dismissed: boolean; requestId: string | null }> {
  return invoke("dismiss_agent_mark", { sessionId, crashId, requestId, ...frame });
}

export const workspaceGroupEdit = async (
  teamKey: string,
  edit: import("@/features/viewer/lib/workspace-groups").GroupEdit,
) =>
  invoke<import("@/features/viewer/lib/workspace-groups").GroupResult>("workspace_group_edit", {
    teamKey,
    edit,
    expectedPrincipal: await rendererPrincipal(),
  });

export interface RoutineCommandPreview {
  argv: string[];
  error: string | null;
  warnings: string[];
}
export const routinePreviewCommand = (
  command: string[],
  org: string,
  team: string,
  after?: RoutineAfter,
  key?: string,
) =>
  invoke<RoutineCommandPreview>("routine_preview_command", {
    command,
    org,
    team,
    after: after ?? null,
    key: key ?? null,
  });
export const recordsCall = (
  frameId: string,
  operation: string,
  args: Record<string, unknown> = {},
) => invoke<unknown>("records_call", { frameId, operation, args });
export const recordsPrepareView = (frameId: string, collection: string) =>
  invoke<string>("records_prepare_view", { frameId, collection });
/** The host's view of one of the person's private collections in `orgSlug`, for
 *  `liveHub.watch("records", viewKey)` (the native Notes list). */
export const recordsPrepareNativeView = (orgSlug: string, collection: string) =>
  invoke<string>("records_prepare_native_view", { orgSlug, collection });

export interface BudgetUsd {
  perDay?: number | null;
  perWeek?: number | null;
}
export interface SpendCapView {
  today: number;
  week: number;
  cap: BudgetUsd | null;
  /** `unrecorded`: managed Claude settings send this scope's usage elsewhere, so the spend shown is not the real spend. */
  state: "ok" | "warn" | "reached" | "unpriced" | "unrecorded";
  pending: boolean;
  requestedCap: BudgetUsd | null;
  pendingAt?: number | null;
  unpricedModels: string[];
  pendingRates?: PendingModelRate[];
}
/** A person's native Save both writes and accepts the cap. */
export const spendCapSet = (
  scopeKind: "agent" | "routine",
  key: string,
  budgetUsd: BudgetUsd | null,
) => invoke<void>("spend_cap_set", { scopeKind, key, budgetUsd });

/** Accept exactly the reviewed cap request from the native window. No dashboard bridge verb exists. */
export const spendCapAccept = (
  scopeKind: "agent" | "routine",
  key: string,
  budgetUsd: BudgetUsd | null,
  keep = false,
) => invoke<void>("spend_cap_accept", { scopeKind, key, budgetUsd, keep });

/** Apply exactly the reviewed pending rate request from the native window. */
export interface PendingModelRate {
  modelPrefix: string;
  effectiveFrom: string;
  inputPerMtok: number;
  outputPerMtok: number;
  cacheReadPerMtok?: number | null;
  long?: LongRateArgs | null;
}
export const spendRateAccept = (request: PendingModelRate) =>
  invoke<number>("spend_rate_accept", { request });

export function fileSurfaceCall(
  operation: string,
  args: unknown,
  frameId?: string,
): Promise<unknown> {
  return invoke("file_surface_call", { operation, args, frameId });
}

export function fileSurfacePrepareView(frameId: string, args: unknown): Promise<string> {
  return invoke("file_surface_prepare_view", { frameId, args });
}

/** The native scan/mark owner publishes navigation data; dashboards only subscribe. */
export const navModelPublish = (teamKey: string, rows: unknown[], files: unknown) =>
  invoke<void>("nav_model_publish", { teamKey, rows, files });

/** The arrays `only` narrows a story read to (the Rust `story_changes::Only`). */
export const STORY_CHANGES_ONLY = [
  "tasks",
  "sessions",
  "commits",
  "prs",
  "branches",
  "worktrees",
  "actual",
  "reviews",
] as const;
export type StoryChangesOnly = (typeof STORY_CHANGES_ONLY)[number];
/** Story sessions, tasks and totals shared with MCP story_changes. */
export interface StoryChangesParams {
  storyId: string;
  limit?: number;
  cursor?: string;
  include?: "ranges"[];
  only?: StoryChangesOnly[];
}
export const storyChanges = (params: StoryChangesParams, frameId?: string, page?: string) =>
  invoke<unknown>("story_changes", {
    params,
    ...(frameId !== undefined ? { frameId } : {}),
    ...(page !== undefined ? { page } : {}),
  });

/** Many stories' cached totals, shared with MCP story_totals. */
export interface StoryTotalsParams {
  storyIds: string[];
}
export const storyTotals = (params: StoryTotalsParams, frameId?: string, page?: string) =>
  invoke<unknown>("story_totals", {
    params,
    ...(frameId !== undefined ? { frameId } : {}),
    ...(page !== undefined ? { page } : {}),
  });

/** Read-only diff target, resolved afresh from a configured source slug. */
export const openDiffRead = (
  params: import("@/features/git-sync/lib/open-diff-types").OpenDiffParams,
  frameId?: string,
  page?: string,
) =>
  invoke<import("@/features/git-sync/lib/open-diff-types").OpenDiffRead>("open_diff_read", {
    params,
    ...(frameId ? { frameId } : {}),
    ...(page ? { page } : {}),
  });
