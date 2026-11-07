// SPDX-License-Identifier: MIT
/**
 * SW-DX-2 — the per-doc summary shape the in-app renderers (csquery views, the
 * connectivity builder, panes) read, mirroring the Rust `doc_query` types. Rows
 * from the context engine are reshaped into it (`context-query/to-doc-summary`).
 *
 * `schemaVersion` is load-bearing: bump `DOC_QUERY_SCHEMA_VERSION` on ANY
 * breaking field change so downstream consumers fail fast (AC-6).
 */

/**
 * Current stable shape version. Bump on any breaking `DocSummary` change.
 *
 * v2 (SW-DX-9): `DocSummary` gains `mtime` + `sourceRepo` (freshness data).
 * v3 (SW-SB-1): `DocSummary` gains `sprint` + `points` + `assignedTo` (board
 * metadata).
 */
export const DOC_QUERY_SCHEMA_VERSION = 3;

/**
 * A resolved (or unresolved) reference to another doc, used for every
 * relationship edge (supporting docs, backlinks, depends-on).
 * `resolved: false` means the target isn't present in the scanned tree
 * (e.g. a binary leaf, or a cross-epic dep outside `scope`) — it is surfaced,
 * NOT dropped (parity with SW-CV-1 leaf handling).
 */
export type RelRef = {
  /** `storyId ?? fileName` of the target (or, when unresolved, the ref's stem). */
  id: string;
  /** Absolute `filePath` of the target (or the best-effort resolved path). */
  path: string;
  /** Basename of the target. */
  fileName: string;
  /** True iff the target resolved to a doc present in the scanned tree. */
  resolved: boolean;
};

/** One doc in a query result. The stable per-doc contract. */
export type DocSummary = {
  /** `storyId ?? fileName`. */
  id: string;
  /** Absolute `filePath`. */
  path: string;
  fileName: string;
  /** Open/arbitrary frontmatter tags (incl. SW-DX-1 schema tags), value lists. */
  tags: Record<string, string[]>;
  /** Raw status string (unknown values retained literally). */
  status: string | null;
  /** Raw `type` frontmatter (`null` ⇒ treated as `"story"` by consumers). */
  documentType: string | null;
  /**
   * SW-DX-9 (v2): the doc file's mtime in epoch **milliseconds** (converted from
   * the scan's internal nanoseconds). `null` when unrecorded. Milliseconds so the
   * value stays within JS's safe-integer range. Powers the Freshness dashboard
   * (doc mtime vs the mtime of the source it documents).
   */
  mtime: number | null;
  /**
   * SW-DX-9 (v2): the doc's raw `source_repo` slug (e.g. `"owner/repo"`). `null`
   * when absent. The Freshness dashboard resolves this to the source it documents.
   */
  sourceRepo: string | null;
  /** SW-SB-1 (v3): sprint tag (e.g. `"2026-08-A"`). `null` when absent. */
  sprint: string | null;
  /** SW-SB-1 (v3): story estimate in points. `null` when absent/malformed. */
  points: number | null;
  /** SW-SB-1 (v3): assignee username (e.g. `"octocat-dev"`). `null` when absent. */
  assignedTo: string | null;
  relationships: {
    /** Docs this doc forward-links via `supporting_docs`. */
    supportingDocs: RelRef[];
    /** Docs that list THIS doc in their `supporting_docs`. */
    backlinks: RelRef[];
    /** Docs this doc `depends_on`. */
    dependsOn: RelRef[];
  };
};

/** The versioned result envelope. */
export type DocQueryResult = {
  schemaVersion: number;
  docs: DocSummary[];
};

/**
 * Relationship predicates. `hasSupportingDoc` / `backlinkedBy` / `dependsOn`
 * take a path-or-id matched against the resolved edge targets; the two boolean
 * predicates test edge presence. Multiple predicates AND together.
 */
export type DocRelationshipFilter = {
  /** Doc forward-links this path/id/stem via `supporting_docs`. */
  hasSupportingDoc?: string;
  /** Doc is backlinked BY this path/id/stem. */
  backlinkedBy?: string;
  /** Doc `depends_on` this id/stem/path. */
  dependsOn?: string;
  /** Doc has ≥1 backlink. */
  isBacklinked?: boolean;
  /** Doc has ≥1 resolved-or-unresolved dependency. */
  hasDeps?: boolean;
};

/** Declared sort keys. Default is `status`→`storyId`→`path` (deterministic). */
export type DocQuerySort = "status" | "storyId" | "path" | "fileName";

/** The filter. All dimensions optional; omitted ⇒ no constraint on that axis. */
export type DocQuery = {
  /**
   * field → required value(s). Reads open frontmatter tags (incl. SW-DX-1
   * schema tags) AND the well-known scalar `StoryRef` fields (`status`,
   * `team`, `source_repo`/`sourceRepo`, `document_type`/`documentType`,
   * `story_id`/`storyId`). Multiple fields AND; an array value is any-of (`in`).
   * Unknown/off-vocabulary values still match literally (never dropped/thrown).
   */
  tag?: Record<string, string | string[]>;
  relationship?: DocRelationshipFilter;
  /** Single status or any-of array, matched against raw `status`. */
  status?: string | string[];
  /** Absolute path prefix bounding which docs are RETURNED (backlinks still resolve against the full tree). */
  scope?: string;
  sort?: DocQuerySort;
  /** Caps result length AFTER sort. */
  limit?: number;
};
