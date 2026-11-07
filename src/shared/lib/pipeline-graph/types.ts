// SPDX-License-Identifier: MIT
import type { Node } from "@xyflow/react";

export const PIPELINE_STATUSES = [
  "planning",
  "todo",
  "in-progress",
  "blocked",
  "review",
  "qa",
  "done",
] as const;
/** The default taxonomy's status ids — the compile-time seed vocabulary. Live
 *  status ids are team-configurable (arbitrary strings); see {@link PipelineStatus}. */
export type DefaultStatusId = (typeof PIPELINE_STATUSES)[number];
/** SW-GT-3: a status id. The taxonomy is team-replaceable, so an id is any string;
 *  the fixed default union survives only as {@link DefaultStatusId} / the
 *  `PIPELINE_STATUSES` seed. Widened from the old 7-literal union so custom states
 *  flow through every consumer without a per-site cast. */
export type PipelineStatus = string;

export type StoryNodeData = {
  title: string;
  status: PipelineStatus;
  storyFile?: string;
  /** Authoritative display label from `story_id` frontmatter (e.g. "W-5"). */
  storyId?: string;
  /** Content of the `## Story` section from the story file. */
  storyText?: string;
  /**
   * Document type from YAML frontmatter `type` field. Always populated by the
   * transform — defaults to `"story"` when the scanner returns `null`.
   * Actionable types (`"story"`, `"bug"`, `"spike"`) support status changes and
   * worktree controls. Non-actionable types (`"doc"`, `"retro"`) show visual
   * differentiation only: background tint and type icon.
   */
  documentType: string;
  /**
   * Source repo slug from YAML frontmatter (e.g. `"alderco/codesense"`).
   * In workspace mode, used to resolve the per-repo store key via
   * `syntheticRepoKey`. Also rendered as a badge on story nodes.
   */
  sourceRepo?: string;
  /** SW-ID-6: assignee login from `assigned_to` frontmatter — drives the node's
   *  assignee avatar. Undefined/null = unassigned. */
  assignedTo?: string | null;
  isBlocked?: boolean;
  /**
   * True when the story is ready-eligible (`planning` or `todo`, see
   * `isReadyEligibleStatus`) and every dependency is met — i.e. ready to be
   * worked next (planned or picked up). Resolved against the FULL graph
   * (scope-independent, matches the nav-row glow), so it doesn't flip on drill.
   * Undefined when readiness wasn't computed (no full index supplied to the
   * transform). Note: the board's Ready column applies its own stricter
   * bucketing (planning stays in Backlog by design), so this flag is not a
   * 1:1 map to a board column.
   */
  isReady?: boolean;
  /** P.2: True when cross-epic deps were dropped due to scope. */
  hasExternalDeps?: boolean;
  /** P.2: All resolved dependencies (intra + cross-epic) for footer badge rendering. */
  resolvedDeps?: ResolvedDep[];
  /**
   * Supporting documents resolved from the story's `supporting_docs`
   * frontmatter. Populated by `derivedTreeToReactFlow` for story-type
   * documents only. Each entry carries enough metadata for the badge
   * popover to render a type icon and clickable filename.
   */
  resolvedSupportingDocs?: Array<{
    fileName: string;
    filePath: string;
    documentType: string;
  }>;
};

// ---------------------------------------------------------------------------
// DerivedPipelineTree — the scanner's output (30.7)
// ---------------------------------------------------------------------------

/**
 * Scanner output for a single story file. R-14 dropped `title` and
 * `storyText` — they're body-extracted (H1 line + `## Story` section), not
 * frontmatter, and consumers fetch them on-demand via the
 * `get_story_details` Tauri command + `useStoryDetails` hook. Keep this
 * type in sync with the Rust `StoryRef` in `pipeline_scanner.rs`.
 */
export type StoryRef = {
  /** e.g. "30-5-pipeline-interactivity.md" */
  fileName: string;
  /** ABSOLUTE path under contextRepoPath, ready for openFile / Read tool calls. */
  filePath: string;
  /**
   * `depends_on` array from the story's YAML frontmatter (30.9). `null` when:
   * frontmatter is absent, the field is missing, malformed YAML, or the field
   * is the wrong shape. Empty array means the field was present but explicitly
   * empty (still distinct from `null`). Each entry is either a bare-stem
   * reference to a sibling story in the same epic directory, or a relative
   * path containing `/` for cross-epic references (e.g., `../workspaces/W-5-foo`).
   * Cross-epic refs resolve to the target epic's group node (P.2).
   */
  dependsOn: string[] | null;
  /** Raw `status` string from YAML frontmatter. Validated on the TS side. */
  status: string | null;
  /**
   * @deprecated R-14: title is no longer produced by the scanner — it's
   * body-extracted (H1 line) and fetched on-demand via the
   * `get_story_details` Tauri command. Field retained as optional+deprecated
   * to keep historical test fixtures compiling; the Rust scanner does not
   * populate it. New code should call `useStoryDetails(filePath)` instead.
   */
  title?: string | null;
  /**
   * @deprecated R-14: storyText is no longer produced by the scanner —
   * it's body-extracted (`## Story` section) and fetched on-demand via
   * `get_story_details`. Field retained as optional+deprecated to keep
   * historical test fixtures compiling. New code should call
   * `useStoryDetails(filePath)` instead.
   */
  storyText?: string | null;
  /**
   * Raw `type` string from YAML frontmatter (e.g. `"bug"`, `"spike"`).
   * `null` when absent — the transform layer defaults to `"story"`.
   */
  documentType: string | null;
  /**
   * Relative paths to supporting documents from the `supporting_docs`
   * YAML frontmatter field (e.g. `["./bug-report.md"]`). `null` when
   * absent. Resolution and filtering happen in `derivedTreeToReactFlow`.
   */
  supportingDocs: string[] | null;
  /** Authoritative display label from `story_id` frontmatter field (e.g. "W-5"). */
  storyId?: string;
  /**
   * Raw `source_repo` slug from YAML frontmatter (e.g. `"alderco/codesense"`).
   * `null` when absent. Validation against `teams.yml.source_repos` happens
   * at usage time on the TS side. Consumers split on `/` for owner/repo.
   */
  sourceRepo: string | null;
  /** Team name from frontmatter (e.g. `"default"`). `null` when absent. */
  team: string | null;
  /**
   * SW-SB-1: the sprint tag (e.g. `"2026-08-A"`) — genuine board metadata
   * (unlike epic/sub-epic, which is folder position). The board's sprint filter
   * (SB-3) reads it HERE; `DocSummary` does not reach the board. `null` when
   * absent; `undefined` on older payloads. Keep in sync with the Rust `StoryRef`.
   */
  sprint?: string | null;
  /**
   * SW-SB-1: story estimate in points. `null` when absent/malformed. Raw — the
   * off-Fibonacci lint lives in `validate_story` (SB-9), not here. Consumed by
   * SB-6 (render/estimate). Keep in sync with the Rust `StoryRef.points`.
   */
  points?: number | null;
  /**
   * SW-SB-1: assignee username (e.g. `"octocat-dev"`). `null` when absent.
   * Feeds SB-12 `getReadyUnassigned` + SE-7 avatars. Keep in sync with Rust.
   */
  assignedTo?: string | null;
  /**
   * SW-SB-22: opaque board-ordering rank — a string fractional index (see
   * `rank.ts`). Sorts a card within its column via `sortColumnStories`; compared
   * lexically only, never parsed. `null`/absent ⇒ unranked (sorts after ranked
   * cards). Keep in sync with the Rust `StoryRef.rank`.
   */
  rank?: string | null;
  /**
   * SLW-MD-1: `false` when this doc's metadata is present-invalid (a
   * constrained field like `source_repo`/`type`/`team` has a bad value) OR
   * unparseable; `true` for absent/warning/unknown-only (absent = valid).
   * Drives the nav's amber invalid-metadata badge (AC-11). Defaults `true`
   * on older payloads (serde default).
   */
  metadataValid?: boolean;
  /** SLW-MD-1: first offending field's reason (badge tooltip/aria-label). */
  metadataInvalidReason?: string | null;
  /**
   * SW-DX-2 (AC-13): open/arbitrary frontmatter tag values — every frontmatter
   * key NOT in the hardcoded set above, mapped to its structured value list (a
   * scalar → 1-item array, a YAML list → N items). Includes SW-DX-1 typed-schema
   * tags (e.g. `context_level`), which live in open frontmatter. Populated by the
   * scanner from the SAME single parse that fills the named fields (reuses the
   * `open_tag_values` extractor) — so the query layer filters/returns tags with
   * zero per-doc filesystem I/O. `null`/absent on older payloads (serde default)
   * and non-story docs; treat as `{}`. Keep in sync with the Rust `StoryRef`.
   */
  openTags?: Record<string, string[]> | null;
  /**
   * SW-DX-9 (query v2): the doc file's mtime in epoch **nanoseconds** — the
   * cached entry's mtime (warm scan) or an on-scan stat (inline / `/api/v1/query`
   * path). `undefined`/`null` when the scan didn't record one. Surfaced onto
   * `DocSummary` for the Freshness dashboard. Keep in sync with the Rust
   * `StoryRef.mtime`.
   */
  mtime?: number | null;
};

/** Metadata extracted from an epic's main doc (e.g. README.md). */
export type MainDoc = {
  metadataValid?: boolean;
  metadataInvalidReason?: string | null;
  titleMeta?: string | null;
  storyId?: string | null;
  status?: string | null;
  sprint?: string | null;
  /** Absolute filesystem path to the main doc file. */
  filePath: string;
  /** Raw `supporting_docs` from YAML frontmatter, or null. */
  supportingDocs: string[] | null;
  /** Content of the `## Purpose` section, capped at 500 chars. Null if absent. */
  purposeText: string | null;
  /**
   * SW-AU-77K: an author-declared automation `subject_id` on the epic main doc,
   * mirroring `DocRef.subjectId`. Absent ⇒ not an addressable subject (no badge);
   * indexed path→id by the viewer store exactly like a doc's. The main doc's
   * directory remains a separate `epic`-kind subject. Keep in sync with Rust
   * `MainDoc.subject_id`.
   */
  subjectId?: string;
};

/**
 * PTS-2 follow-up: companion to `StoryRef` for md/html files that pass
 * the scanner's filename/extension filter (`is_pipeline_document`) but
 * fail the pipeline-type predicate — no frontmatter, no `type:` field,
 * or a non-pipeline `type:` value (e.g. `doc`, `retro`, `llm-distillate`).
 * Rendered in the nav as plain file rows; NOT in the pipeline view.
 *
 * Deliberately minimal — only the fields a plain file row needs. If a
 * future consumer wants frontmatter metadata for docs, add fields here.
 */
export type DocRef = {
  /** e.g. "prd.md", "architecture.md" */
  fileName: string;
  /** ABSOLUTE path, ready for openFile / Read tool calls. */
  filePath: string;
  /**
   * SW-XS-19/20: the PARENT team (in the active team's chain, self →
   * `parent_team` → …) this doc is inherited from. The backend nav scan sets it
   * only for a file the team does not have itself; the nav shows the inherited
   * marker naming that team, and `filePath` is the file's real path in the
   * parent, so an edit here updates the parent's file. ABSENT ⇒ the active
   * team's own file (no marker). Keep in sync with Rust `DocRef.source_team`.
   */
  sourceTeam?: string | null;
  /**
   * SW-XS-20: on the active team's own doc only — the nearest parent team that
   * has a file at the same path, which this doc overrides for the team. Deleting
   * it brings that parent's version back. Rust `DocRef.overrides_team`.
   */
  overridesTeam?: string;
  /**
   * SLW-NC-1 Phase 3 (flat unified merge): the `owner/repo` slug when this doc
   * came from a **source repo** overlaid onto the team workspace. Present ⇒ the
   * row is a source-repo layer file: **browse-only** (the layered view is NC-2) and
   * **tinted** with the repo's provenance color. Absent ⇒ a team-workspace
   * (context) doc with full affordances. Set only by the nav path-merge
   * (`mergeInstalledIntoNav`), never by the context scanner.
   */
  installedSourceRepo?: string;
  /**
   * SLW-NC-1 Phase 3 (P3-AC-3b): true when this source-repo doc **wins** a path
   * that also exists in the team workspace — i.e. it shadows a team-workspace
   * version. Drives the "shadowed" hint badge. A *hint only*: the shadowed
   * content / per-layer view is NC-2. Never true for a team-workspace doc or a
   * source doc with no collision.
   */
  shadowsTeamFile?: boolean;
  /**
   * SLW-MD-1: same as `StoryRef.metadataValid` — a *plain* doc can be
   * badge-worthy too (AC-16: e.g. an invalid `source_repo` on a `type: doc`).
   * `false` ⇒ render the amber invalid-metadata badge. Defaults `true`.
   */
  metadataValid?: boolean;
  /** SLW-MD-1: first offending field's reason (badge tooltip/aria-label). */
  metadataInvalidReason?: string | null;
  /**
   * SW-AU-77H: an author-declared automation `subject_id` (frontmatter, or the HTML
   * `cs:subject-id` twin) that makes this non-story doc a durable, move-safe
   * automation subject. Absent ⇒ not an addressable subject (no badge). Indexed
   * path→id by the viewer store exactly like a story's `story_id`. Keep in sync with
   * Rust `DocRef.subject_id`.
   */
  subjectId?: string;
};

export type DerivedPipelineTree = {
  /**
   * Path relative to pipelineRoot. At the scope root, equals the scope path
   * (e.g. "in-progress"), or "" when scoped at pipelineRoot itself.
   */
  dirPath: string;
  /** Leaf name of this directory, or "" for pipelineRoot itself. */
  dirName: string;
  /** Absolute path under contexts_dir. */
  absoluteDirPath: string;
  /** .md/.html files with `type: story|bug|spike` (md) or cs:* meta (html). */
  stories: StoryRef[];
  /**
   * PTS-2 follow-up: md/html files in this dir that passed the
   * filename/extension filter but failed the pipeline-type predicate.
   * Optional with `[]` default for backward-compat with older Rust binaries
   * that don't emit the field. Rendered as plain file rows in the nav.
   */
  docs?: DocRef[];
  /** Recursive sub-epic directories. */
  subEpics: DerivedPipelineTree[];
  /** Parse/schema/validation error if pipeline.json is invalid, else null. */
  orderError: string | null;
  /** Epic main doc metadata (README.md / index.md / configured). Null if absent. */
  mainDoc: MainDoc | null;
};

// ---------------------------------------------------------------------------
// React Flow node data types
// ---------------------------------------------------------------------------

/** Resolved dependency metadata for footer badge rendering. */
export type ResolvedDep = {
  /** Abbreviated story ID, e.g. "W.5", "F.12", or bare stem for non-matching names. */
  label: string;
  /** Full story title for tooltip. */
  title: string;
  /** Epic stroke color for cross-epic deps (matches edge). Undefined for intra-epic. */
  epicColor?: string;
  /** Epic badge background tint for cross-epic deps. Undefined for intra-epic. */
  epicBadgeBg?: string;
  /** True when the depended-on story's status is "done". Undefined for out-of-scope deps (status unknown). */
  met?: boolean;
  /** Absolute path to the dependency story file. Used by clickable dep badges. */
  filePath?: string;
  /** True when the dep target is outside the current drill scope. Badge click needs drill-to-root navigation. */
  outOfScope?: boolean;
};

/** Resolved supporting doc metadata for popover rendering. */
export type ResolvedSupportingDoc = {
  fileName: string;
  filePath: string;
  documentType: string;
};

export type GroupNodeData = {
  title: string;
  /** Small subtitle rendered below the title in muted-foreground text. */
  subtitle?: string;
  /** Non-null when pipeline.json parsing failed — shown as a warning badge. */
  orderError?: string;
  /**
   * Absolute filesystem path to this epic's directory. Populated by
   * `derivedTreeToReactFlow` from the scanner's `DerivedPipelineTree`.
   * Used by the drill-in affordance: clicking the drill button on a
   * group node hands this path to `PipelineNodeCallbacksContext.onDrillIn`,
   * which routes it through the same file-tree/pipeline-scope sync path
   * that the file tree's drill arrow uses.
   */
  absoluteDirPath?: string;
  /**
   * Pipeline-root-relative directory path (e.g. `"in-progress/epic-a"`).
   * Stable across drill scopes; passed to the pipeline viewer context menu.
   */
  dirPath?: string;
  /**
   * Depth in the emitted group tree — 0 for top-level epics under the
   * scope root, 1 for their sub-epics, 2 for sub-sub-epics, etc. Used
   * by `PipelineGroupNode` to alternate background colors between
   * `--cs-pipeline-group-bg` (even depth) and `--cs-pipeline-group-alt-bg`
   * (odd depth) so a nested epic is visually distinct from its parent.
   * Populated by `derivedTreeToReactFlow`.
   */
  depth?: number;
  /** Content of the `## Purpose` section from the main doc, or undefined. */
  purposeText?: string;
  /** Absolute path to the main doc file. Used by the "open doc" button. */
  mainDocFilePath?: string;
  /** Resolved supporting docs for the group node's popover. */
  resolvedSupportingDocs?: ResolvedSupportingDoc[];
  /** Aggregated progress for the epic progress strip. Undefined = no actionable stories. */
  statusCounts?: {
    total: number;
    done: number;
    active: number; // in-progress + review + qa
    blocked: number;
    remaining: number; // planning + todo
  };
};

export type StoryNode = Node<StoryNodeData, "story">;
export type GroupNode = Node<GroupNodeData, "group">;
export type PipelineNode = StoryNode | GroupNode;
