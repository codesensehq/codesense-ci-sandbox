// SPDX-License-Identifier: MIT
/**
 * SW-GS-1 — Git Sync Workbench typed view-model contracts.
 *
 * This file is the SINGLE SOURCE OF TRUTH for the shapes the Git Sync
 * Workbench renders. SW-GS-2 (the `get_publish_set` / `gs_status` verb
 * surface) and SW-GS-2a (the dedicated-session spawn + liveness) IMPORT
 * these types and serialize their returns to match — they do NOT redefine
 * them. SW-GS-1 ships fixtures conforming to these shapes via the
 * `publish-set-source.ts` seam; no component imports `invoke` directly.
 *
 * Locked constraints (architecture §2.4/§10): all result/structural labels
 * are agent-AGNOSTIC — "Finalized", "Agent", never a vendor string. The
 * live chat may show a real agent name; these view-model types never carry
 * one.
 */

/** Workspace kind used to label/glyph a publish bucket. "general" is the
 *  synthetic unattributed bucket (not a real workspace). */
export type PublishBucketKind = "story" | "epic" | "general";

/**
 * A workspace reference used to label a publish bucket. A bucket is a
 * *publish parameter* (which files get staged), NOT a different workspace
 * home (architecture Rev-2026-06-23 §A).
 */
export interface WorkspaceRef {
  /** Stable id (workspace id / slug). Unique within a `PublishSet`. */
  id: string;
  /** Agent-agnostic human label, e.g. "story-X · Persistent Terminals". */
  label: string;
  kind: PublishBucketKind;
}

/**
 * Per-file change classification badges (architecture G8/G9/G12),
 * surfaced read-only in V2. Curation is conversational (chat), never a V2
 * control.
 */
export type PublishFileBadge =
  | "eol-only" // G8 — whitespace/EOL-only change
  | "mode" // G9 — mode or symlink change
  | "rename"; // G12 — rename

/** Per-file Add/Modify/Delete status vs the publish baseline (SW-GS-2). */
export type PublishFileStatus = "added" | "modified" | "deleted";

/** A single pending file change within a bucket. */
export interface PublishFile {
  /** Repo-relative path (post-rename for a rename). */
  path: string;
  /** SW-GS-2: Add/Modify/Delete vs the publish baseline. */
  status: PublishFileStatus;
  /** Zero or more classification badges. Absent ⇒ a plain content change. */
  badges?: PublishFileBadge[];
  /**
   * SW-GS-14: pre-rename repo-relative path; present only for a rename (mirrors
   * the Rust `renamedFrom`). Drives the rename chip + the redline's baseline
   * lookup (the content lives at the old path).
   */
  renamedFrom?: string;
  /**
   * SLW-DV-6: added/removed line counts vs the baseline (git line-stats).
   * Absent for a binary file or a synthesized delete/add — distinguish
   * "not applicable" (undefined) from "no line change" (0). The working-tree
   * change-set and the per-commit (History-mode) change-set both populate these
   * (shared backend spine); the PR tree-to-tree path leaves them absent.
   */
  added?: number;
  removed?: number;
}

/** One change-bearing workspace bucket — only workspaces WITH pending
 *  changes appear (AC-3). */
export interface PublishBucket {
  workspaceRef: WorkspaceRef;
  /** Pre-computed count (== `files.length`; carried so the picker can render
   *  a count without walking files). */
  count: number;
  files: PublishFile[];
}

/** The General / unattributed bucket (planning, wip, anything with no
 *  provenance stamp). Always present; may be empty. */
export interface GeneralBucket {
  count: number;
  files: PublishFile[];
}

/**
 * Grouped, counted publish set — the V1 picker (AC-3) and V2 totality
 * (AC-5) data contract. Returned by `get_publish_set` (SW-GS-2; mocked
 * here via the data-source seam). Real provenance grouping rides SW-CP
 * CP-4 (until then real data groups to General); fixtures exercise the
 * grouped shape regardless.
 */
export interface PublishSet {
  buckets: PublishBucket[];
  general: GeneralBucket;
  /** Why the diff is against HEAD: no publish baseline (origin's default branch, local
   *  `main`, the branch's upstream) resolved. Absent when one did. */
  baselineReason?: string;
}

/** The canonical empty publish set — nothing pending. */
export const EMPTY_PUBLISH_SET: PublishSet = { buckets: [], general: { count: 0, files: [] } };

/**
 * SW-GS-3 — before/after content for one changed file, the input to the
 * rendered-markdown redline. `before` is the publish-baseline blob, `after`
 * is the working-tree content — the SAME baseline the changed-files list diffs
 * against, so list and redline never disagree. `binary` true ⇒ `before`/`after`
 * are empty and the FE renders the "open both / pick-a-side" affordance.
 */
export interface FileRedline {
  beforeLabel?: string;
  afterLabel?: string;
  baselineSha?: string;
  status: PublishFileStatus;
  binary: boolean;
  before: string;
  after: string;
}

/**
 * SLW-WD-8 — the kind of ref decorating a commit in the source commit graph
 * (mirrors the Rust `RefKind`). `head` is the synthetic decoration for the
 * worktree's own symbolic HEAD, so WD-9 marks "where I am" off the ref labels.
 */
export type RefKind = "local" | "remote" | "tag" | "head";

/**
 * SLW-WD-8 — one branch/remote/tag/HEAD decoration pointing at a commit (mirrors
 * the Rust `RefLabel`). A plain projection of git refs; `name` is the shorthand
 * (`main`, `origin/master`, `v1.0`, or `HEAD`).
 */
export interface RefLabel {
  name: string;
  kind: RefKind;
}

/**
 * SLW-WD-8 — one commit in the source worktree's all-refs commit graph (mirrors
 * the Rust `SourceCommitGraphEntry`). Metadata + `parents` (the parent shas that
 * make edges/lanes drawable — ALL parents for a merge) + `refs` (the decorations
 * pointing at this commit, incl. the synthetic `HEAD`). Deliberately carries NO
 * `paths`: the graph walk never diffs (the per-commit change-set is fetched lazily
 * via {@link sourceCommitChangeset}). `committedAt` is Unix SECONDS.
 */
export interface SourceCommitGraphEntry {
  sha: string;
  shortSha: string;
  parents: string[];
  refs: RefLabel[];
  subject: string;
  author: string;
  committedAt: number;
}

/**
 * SW-GS-19 (A3) — an enriched open-PR summary for the in-UI review panel.
 * Everything the panel + Approve&sync need EXCEPT a mergeable hint (R8 — the
 * GitHub list endpoint doesn't return it). `authorLogin` drives own-PR
 * detection (skip the approve step on your own PR).
 */
export interface PrReviewSummary {
  number: number;
  title: string;
  authorLogin: string;
  headRef: string;
  baseRef: string;
  htmlUrl: string;
}

/**
 * SW-GS-19 (A2) — result of Approve&sync's server legs (approve + merge).
 * `merged` ⇒ the FE proceeds to pull down; `blocked` carries GitHub's reason
 * and the PR stays open. Approve/auth/network failures reject instead.
 */
export type ApproveMergeOutcome = { status: "merged" } | { status: "blocked"; reason: string };

/**
 * PR sub-state for V3 (AC-7). `waiting-on-checks` appears ONLY when the
 * repo has required checks (merge-when-green, architecture §4.4/G16b) — the
 * common path is `opened` → `merged` with no checks state at all.
 */
export type GsPrPhase =
  | "opened"
  | "waiting-on-checks"
  | "checks-failed"
  | "merged"
  | "re-integrating"; // origin moved; re-integrating before retry

/** PR state within `GsStatus`. */
export interface GsPrState {
  number: number;
  phase: GsPrPhase;
}

/**
 * Snapshot → push → PR → merge → absorb status for V3 (AC-6) — one
 * glanceable line, NOT a CI dashboard. Every field optional so the line
 * renders progressively as the flow advances. Driven by `gs_status` typed
 * JSON (SW-GS-2; fixture here).
 */
export interface GsStatus {
  /** Files captured in the in-memory snapshot. */
  snapshotted?: number;
  /** Integration outcome. `conflict` routes V4. */
  integrated?: "clean" | "conflict";
  /** The pushed `gs/*` branch (deterministic name, e.g. `gs/story-abc1234`),
   *  or null before push. */
  pushedBranch?: string | null;
  /** PR state, or null before a PR exists. */
  pr?: GsPrState | null;
  /** True once the merged result is absorbed back into the dest. */
  absorbed?: boolean;
}

/**
 * A single conflicted file region for V4 (AC-8/9). Canonical TS field
 * names are `base`/`ours`/`theirs`; SW-GS-2's Rust returns the descriptive
 * `*_blob` form and its `commands.ts` wrapper normalizes to these names
 * (verified vs SW-GS-2 AC-4 — a one-line map, no SW-GS-1 rework).
 */
export interface ConflictRegion {
  path: string;
  /** Common-ancestor content; null when added on both sides (no base). */
  base: string | null;
  ours: string;
  theirs: string;
  /** True ⇒ render as a binary pick-a-side (images/`.excalidraw`), not 3-way text. */
  binary: boolean;
}

// Ownership/connection types are OWNED by the generic supervised-agent-session
// primitive (it is kind-agnostic and reused beyond git-sync — AC-17). Git-sync
// re-exports them so its store/view-model read them from one place; the chip
// renders "Agent driving" ONLY when `owner === "agent" && connection === "live"`.
// The real liveness backing them is SW-GS-2a's process-tree `claude`-child
// detection (Winston V-12); never inferred from `aiStatus`.
export type {
  SessionOwner,
  ConnectionPhase,
  AgentSessionState,
} from "@/shared/components/supervised-agent-session/types";

/**
 * SW-GS-7 incoming summary — the quiet, workspace-grouped "ready to sync —
 * N incoming" surface (AC-19). Rendered as a top-right Sonner toast/badge;
 * click routes into the overlay at the bucket. The poller is SW-GS-7;
 * mocked here from a typed prop.
 */
export interface IncomingGroup {
  workspaceRef: WorkspaceRef;
  count: number;
}

export interface IncomingSummary {
  groups: IncomingGroup[];
  /** Total incoming across all groups (== sum of `groups[].count`). */
  total: number;
}

/**
 * SW-GS-8 — the typed result of a node-level publish (`publish_node`). Mirrors
 * the Rust `PublishOutcome` (serde `tag = "status"`, camelCase variants). The UI
 * branches on `status`; hard/unexpected failures throw instead (caught → toast).
 */
export type PublishOutcome =
  | { status: "pushed" }
  /** Slice already committed (shared file self-healed out) — benign no-op. */
  | { status: "nothingToCommit" }
  /** Behind origin's default branch — blocked before committing. Route to "pull first". */
  | { status: "behind"; incoming: number }
  /** Committed locally but push rejected non-ff (TOCTOU). Never force-pushed. */
  | { status: "nonFastForward" }
  /** No usable auth token — re-authenticate. Nothing committed. */
  | { status: "authRequired" }
  /** SW-GS-17 (PR mode) — the slice was committed to a `gs/*` branch and a PR was
   *  opened (or re-attached to the already-open PR for this slice). `main` is
   *  untouched; the node stays in the list with a "PR #N open" chip. */
  | { status: "prOpened"; number: number; htmlUrl: string; branch: string };

/**
 * SW-GS-6 — the cheap out-of-sync probe result: how many commits origin's default
 * branch is ahead of local (0 ⇒ up to date). Repo-global (a commit-graph fact about
 * the single context repo), NOT team-scoped — mirrors the Rust `IncomingStatus`
 * field-for-field. A struct (not a bare number) leaves room for an optional
 * ahead-count later without a signature break.
 */
export interface IncomingStatus {
  incoming: number;
  /**
   * SW-GS-31 — is a *pure* fast-forward currently possible (behind + not-diverged
   * + clean working tree)? The auto-sync-down driver fires only when this is true,
   * so a dirty/diverged repo is skipped on the cheap read-only probe alone. Mirrors
   * the Rust `IncomingStatus::can_fast_forward`. Optional for wire/back-compat;
   * treat a missing value as `false`.
   */
  canFastForward?: boolean;
  /** Why `incoming` is 0 and `canFastForward` false: origin's default branch is not
   *  known locally. Absent when it is. */
  defaultBranchReason?: string;
}
