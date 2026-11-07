// SPDX-License-Identifier: MIT
/**
 * SW-GS-2a — the single source of truth for per-workspace-`kind` behavior.
 *
 * Workspace `kind` was discriminated at ~41 scattered `switch(kind)` / `kind === …`
 * sites (nav, tray, terminal aggregation, persistence, soft-close, display). Adding
 * a kind meant hunting all of them. This registry centralizes the BEHAVIORAL
 * decisions so a new kind is a single row here, not an N-site edit, and the
 * scattered sites read these capabilities instead of inlining a kind check.
 *
 * Scope: behavioral booleans + enums (the source of the 41-site pain). Genuinely
 * per-kind *rendering* (label text, icon component) stays at its call site but is
 * gated by the flags here (e.g. `resolvesDocTitle`, `statusBadge`).
 *
 * 4-kind model (Trevor 2026-06-24): the old zombie `kind:"epic"` (never created,
 * always dropped on load) was removed and the live `integration` kind was renamed
 * to `epic`. So `epic` HERE carries the old-integration semantics (terminal,
 * README/HUD, doc title, cwd = the epic dir). The Rust `ensure_epic_workspace`
 * dir-provisioner is a separate, live thing and is unaffected.
 */

/** The workspace kind union — the single source of truth (viewer-store's
 *  `Workspace.kind` imports this). Adding a kind = a union member here + a row in
 *  {@link WORKSPACE_KIND_CAPABILITIES} (TS Record exhaustiveness forces the row). */
export type WorkspaceKind = "root" | "story" | "epic" | "generic" | "context";

/** Where a workspace's chip sorts in the bottom tray (group-workspaces-by-epic.ts):
 *  `root` = the leading ROOTS rail (SW-EW-38); `epic-member` = grouped under its
 *  epic dir (falls to the trailing "Ungrouped" bucket when it has no epic dir);
 *  `ungrouped` = always trailing (e.g. a user-rooted `generic` dir). */
export type TrayGroup = "root" | "epic-member" | "ungrouped";

/** Load-time migration policy (viewer-store load sweep): `keep` always restores the
 *  record; `drop` discards it on load (server-minted / terminal-less); `keep-if-valid`
 *  keeps it only when its identity fields resolve (epic needs epicDirPath +
 *  (sourceRepoSlug | epicDocPath)). */
export type LoadMigrationPolicy = "keep" | "drop" | "keep-if-valid";

/** Which `Workspace` field backs the double-click "open the backing doc" gesture
 *  (story-doc-gesture.ts), or `null` for kinds with no backing doc. */
export type DocPathSource = "storyFilePath" | "epicDocPath" | null;

export interface WorkspaceKindCapabilities {
  /** User may close it (× / requestCloseWorkspace).
   *  Sites: viewer-store closeWorkspace root-guard, requestCloseWorkspace early-exit. */
  closableByUser: boolean;
  /** Eligible to become a minimized soft-close shadow (keeps PTYs alive).
   *  Site: viewer-store replaceFocusedLeafWorkspace `kind !== "root"` guard. */
  canSoftClose: boolean;
  /** Participates in the foreground tile/zoom set AND appears in the tray.
   *  Sites: foreground-tiles foregroundTilesFor, cli-bottom-chrome tile count,
   *  viewer-store zoom-cleanup count, group-workspaces-by-epic filter. */
  foregroundTile: boolean;
  /** Tray sort/group bucket. Site: group-workspaces-by-epic trayWorkspaceOrder. */
  trayGroup: TrayGroup;
  /** Load-migration restore policy. Site: viewer-store load sweep. */
  loadMigration: LoadMigrationPolicy;
  /** Shows the story-status badge/dropdown. Sites: workspace-pill, terminal-tile-chrome. */
  statusBadge: boolean;
  /** Shows the story/epic-HUD chrome toggle. Site: terminal-tile-chrome. */
  hudToggle: boolean;
  /** Per-tab-primitives: this kind is rooted at a USER-POKABLE folder — a dir
   *  the viewer can list and read files from (the per-tab Source icon lights
   *  on `sourceCapable && folderResolved`). TRUE for every folder-rooted kind,
   *  so `root` / `story` / `epic` / `generic` all qualify;
   *  the icon's click opens a file-browse + readonly viewer overlay regardless
   *  of git state. Replaces the single `sourceDiffCapable` from #1904 so a
   *  plain CLI tab in a non-git dir still gets the Source affordance (its
   *  cwd IS a browsable folder) while Diff stays gated separately. */
  sourceCapable: boolean;
  /** Per-tab-primitives: this kind is eligible for the git-diff affordance
   *  (the Diff icon). TRUE on the same subset as the old `sourceDiffCapable`
   *  — `story` (worktree by construction), `root` and `generic` (needs the
   *  live `resolveTabSourceRepo` to confirm a working tree before the icon
   *  lights, AD-3) — AND the live resolver still has to confirm a git working
   *  tree. `epic` whose cwd is a provisioned non-git dir stays `false` so the
   *  resolver never runs on it. */
  diffCapable: boolean;
  /** Resolves an async doc H1 title for its label. Site: use-workspace-label. */
  resolvesDocTitle: boolean;
  /** Backing-doc field for the open-doc gesture. Site: story-doc-gesture. */
  docPathSource: DocPathSource;
  /** Frontmatter (storyStatus/storyId/sourceRepoSlug) is reconciled into it.
   *  Site: viewer-store reconcile `kind === "story"`. */
  frontmatterReconcile: boolean;
  /** Re-opens its surface watch on launch. Site: viewer-store load `kind === "story"`. */
  surfaceWatchOnLoad: boolean;
  /** Its on-disk worktree is checked for staleness on launch. Site: viewer-store load. */
  worktreeStalenessCheck: boolean;
  /** Its terminal cwd is a REAL on-disk directory (epic dir, org_root, repo root),
   *  so `send-to-terminal` may skip the synthetic-clone attach gate. `false` only for
   *  `story`, whose cwd is a synthetic-clone that must be attached first (the gate
   *  exists FOR story). Site: send-to-terminal.ts. (SW-AW-5 AC-2 — re-adds the
   *  cwd/clone-gate capability cut in SW-AW-1 review, now with a real consumer.) */
  skipsCloneGate: boolean;
}

/**
 * The registry. Each row encodes the EXACT current behavior of its kind (verified
 * against the cited sites @ 8210c540) so migrating the sites onto these reads is a
 * zero-behavior-change refactor — EXCEPT the two Trevor-approved changes: `root` is
 * now closable/soft-closeable + ungrouped. `epic` carries the old-integration
 * semantics (the kind was renamed).
 */
export const WORKSPACE_KIND_CAPABILITIES: Record<WorkspaceKind, WorkspaceKindCapabilities> = {
  root: {
    // SW-GS-2a (Trevor 2026-06-24): root made user-closable + soft-closeable. It is
    // dormant/retired in the UI today; this removes the old hard "root invariant"
    // close guards so root behaves like a normal workspace if it's ever surfaced.
    // trayGroup stays "root" (its own ROOTS rail) — SW-EW-38 re-established that.
    closableByUser: true,
    canSoftClose: true,
    foregroundTile: true,
    trayGroup: "root",
    loadMigration: "drop", // server-minted on activation; dropped on load
    statusBadge: false,
    hudToggle: false,
    sourceCapable: true,
    diffCapable: true,
    resolvesDocTitle: false,
    docPathSource: null,
    frontmatterReconcile: false,
    surfaceWatchOnLoad: false,
    worktreeStalenessCheck: false,
    // root resolves a real repo cwd, so the synthetic-clone gate was already a
    // no-op for it — skipping it is behavior-preserving (SW-AW-5, Trevor 2026-07-02).
    skipsCloneGate: true,
  },
  story: {
    closableByUser: true,
    canSoftClose: true,
    foregroundTile: true,
    trayGroup: "epic-member",
    loadMigration: "keep",
    statusBadge: true,
    hudToggle: true,
    sourceCapable: true,
    diffCapable: true,
    resolvesDocTitle: false,
    docPathSource: "storyFilePath",
    frontmatterReconcile: true,
    surfaceWatchOnLoad: true,
    worktreeStalenessCheck: true,
    // story's terminal cwd is a synthetic-clone that must be attached first — the
    // no-clone gate exists FOR story, so it must NOT skip it.
    skipsCloneGate: false,
  },
  epic: {
    // Renamed from `integration` (Trevor 2026-06-24) — the live epic terminal
    // workspace: README/HUD, epic doc title, cwd = the provisioned epic dir.
    closableByUser: true,
    canSoftClose: true,
    foregroundTile: true,
    trayGroup: "epic-member",
    loadMigration: "keep-if-valid",
    statusBadge: false,
    hudToggle: true,
    sourceCapable: true,
    diffCapable: false,
    resolvesDocTitle: true,
    docPathSource: "epicDocPath",
    frontmatterReconcile: false,
    surfaceWatchOnLoad: false,
    worktreeStalenessCheck: false,
    // epic dir is always provisioned (cwd = the epic dir) — slug-less, no source
    // clone. This is the behavior the old `kind === "epic"` literal encoded.
    skipsCloneGate: true,
  },
  context: {
    closableByUser: true,
    canSoftClose: true,
    foregroundTile: true,
    trayGroup: "ungrouped",
    loadMigration: "keep-if-valid",
    statusBadge: false,
    hudToggle: false,
    sourceCapable: true,
    diffCapable: true,
    resolvesDocTitle: false,
    docPathSource: null,
    frontmatterReconcile: false,
    surfaceWatchOnLoad: false,
    worktreeStalenessCheck: true,
    skipsCloneGate: true,
  },
  generic: {
    // SW-GW-1: a plain CLI terminal rooted at ANY directory the user picks — the
    // DEGENERATE workspace kind: same tile/tab/spawn machine, every artifact
    // behavior OFF, cwd supplied directly by the user (not computed from a backing
    // artifact). Installs/hydrates nothing.
    closableByUser: true,
    canSoftClose: true,
    foregroundTile: true,
    // OQ-1 (Trevor 2026-07-31): a deliberately-opened workspace survives restart.
    // Validates its cwd-on-disk on load (see worktreeStalenessCheck). Its
    // terminals are the host's: whichever still run come back with it.
    // OQ-3: visible, ungrouped rail (routes through the ungrouped tail in
    // group-workspaces-by-epic — NOT the epic-member branch, so no grouping-code arm).
    trayGroup: "ungrouped",
    loadMigration: "keep",
    statusBadge: false,
    hudToggle: false,
    sourceCapable: true,
    diffCapable: true,
    resolvesDocTitle: false, // no backing doc → label = the dir leaf (workspaceDisplayLabel)
    docPathSource: null,
    frontmatterReconcile: false,
    surfaceWatchOnLoad: false,
    // OQ-1: the record is kept across reload, so we DO validate its cwd on disk —
    // a deleted/moved dir is marked status:"error" by the existing load-time
    // staleness sweep (no crash, no broken tile). This is the one non-story/epic
    // kind that opts into the sweep, precisely because it also persists.
    worktreeStalenessCheck: true,
    // cwd = a real user-chosen on-disk dir that provably exists (the folder picker
    // returned it; the staleness sweep re-validates it), so the synthetic-clone
    // attach gate is a no-op — skip it. (Same OUTCOME as epic/root, though their
    // cwds are artifact-provisioned rather than user-picked.)
    skipsCloneGate: true,
  },
};

/** Resolve the capability row for a kind. */
export function getKindCapabilities(kind: WorkspaceKind): WorkspaceKindCapabilities {
  return WORKSPACE_KIND_CAPABILITIES[kind];
}

/**
 * SLW-FX-18: a workspace surfaces its story-lifecycle status control only when
 * its kind owns a settable story identity (`statusBadge` — story only) AND it is
 * a `ready` workspace with a resolved `storyStatus`. This is the single source
 * for that "shows a status control?" question — previously the same three-term
 * predicate was inlined verbatim in the tile chrome (the dropdown) and the
 * workspace pill (the tab glyph), so the two could silently drift.
 *
 * A generic type-guard so callers that use it inline (`showsStoryStatus(ws) &&
 * …` / `? …`) get `storyStatus` narrowed to a non-null `PipelineStatus` without
 * a `!` assertion, while preserving the caller's full workspace type.
 *
 * NOTE: distinct from — and coarser than — the `hideIdentity` gate in
 * `workspace-terminal-panels.tsx`, which keys on `statusBadge` ALONE: a story
 * tile must still show its title even before it's `ready`/has a status, so the
 * identity block can't fold in the `ready`/`storyStatus` terms.
 */
export function showsStoryStatus<
  T extends { kind: WorkspaceKind; status: "ready" | "creating" | "error"; storyStatus?: unknown },
>(ws: T): ws is T & { storyStatus: NonNullable<T["storyStatus"]> } {
  return (
    getKindCapabilities(ws.kind).statusBadge && ws.status === "ready" && ws.storyStatus != null
  );
}
