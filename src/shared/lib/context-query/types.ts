// SPDX-License-Identifier: MIT
/**
 * SW-OC-26 — the context query model, mirroring the Rust `context_query::model`
 * (the one engine). Types only: every query runs in Rust via the
 * `query_context` command (`queryContext` in `@/tauri/commands`).
 *
 * Example — stories in an epic, open, assigned to me, by priority, grouped by status:
 *
 *   {
 *     where: { kinds: ["story"] },
 *     relationships: { parentEpic: "SW-OC" },
 *     filters: [
 *       { key: "status", op: "in", value: ["todo", "in-progress"] },
 *       { key: "assigned_to", op: "eq", value: "@me" },
 *     ],
 *     sort: ["priority"],
 *     groupBy: "status",
 *   }
 */

export const CONTEXT_QUERY_SCHEMA_VERSION = 1;

export type ContextLayer = "team" | "private" | "all-orgs";

export type ContextKind =
  | "story"
  | "bug"
  | "spike"
  | "epic"
  | "doc"
  | "dashboard"
  | "skill"
  | "other";

export type ContextWhere = {
  /** `"all"` (default: every synced org) or explicit org slugs. */
  orgs?: "all" | string[];
  /** Limit to the org of the file at this absolute path (a UI default). */
  sameOrgAs?: string;
  /** Limit to the one content root (team repo, private space, all-orgs) this path is under. */
  sameRootAs?: string;
  teams?: string[];
  layers?: ContextLayer[];
  kinds?: ContextKind[];
  /** Absolute path prefix, or a prefix of the content-root-relative path. */
  pathPrefix?: string;
  /** Glob over the content-root-relative path. */
  pathGlob?: string;
};

export type ContextFilterOp =
  | "eq"
  | "in"
  | "not"
  | "exists"
  | "missing"
  | "contains"
  | "range"
  | "prefix";

export type ContextScalar = string | number | boolean;

/** Clauses on the same key OR together; different keys AND. `"@me"` = the signed-in user. */
export type ContextFilter = {
  key: string;
  op: ContextFilterOp;
  value?: ContextScalar | ContextScalar[];
  min?: ContextScalar;
  max?: ContextScalar;
};

export type ContextRelationshipFilter = {
  dependsOn?: string;
  dependedOnBy?: string;
  hasSupportingDoc?: string;
  backlinkedBy?: string;
  parentEpic?: string;
  hasDeps?: boolean;
  isBacklinked?: boolean;
};

/** `"status"`, `"-mtime"`, or the object form; `order` ranks values explicitly
 *  (unlisted values sort after) and orders `groupBy` buckets on the same key. */
export type ContextSort =
  | string
  | { key: string; dir?: "asc" | "desc"; missing?: "first" | "last"; order?: string[] };

export type ContextQuery = {
  where?: ContextWhere;
  filters?: ContextFilter[];
  /** Shorthand: a scalar is `eq`, an array is `in`. */
  match?: Record<string, ContextScalar | ContextScalar[]>;
  relationships?: ContextRelationshipFilter;
  /** Token-aware title, heading and body search (bounded scan); explicit sort overrides relevance. */
  text?: string;
  /** Frontmatter keys to return; include `"relationships"` for refs. */
  select?: string[];
  sort?: ContextSort[];
  limit?: number;
  cursor?: string;
  groupBy?: string;
};

export type ContextRelRef = {
  id: string;
  path: string;
  fileName: string;
  resolved: boolean;
};

export type ContextRowRelationships = {
  dependsOn: ContextRelRef[];
  dependents: ContextRelRef[];
  supportingDocs: ContextRelRef[];
  backlinks: ContextRelRef[];
  /** `parent_epic` / `epic` frontmatter. */
  parentEpic: string | null;
  /** The nearest folder holding a README/index main doc — the epic as the left
   *  nav derives it, whatever the frontmatter says. */
  folderEpic: string | null;
};

export type ContextRow = {
  textMatches?: { title: string; anchor: string }[];
  id: string;
  path: string;
  relPath: string;
  fileName: string;
  org: string | null;
  team: string | null;
  layer: ContextLayer;
  kind: ContextKind;
  /** Epoch milliseconds. */
  mtime: number | null;
  title: string | null;
  fields: Record<string, unknown>;
  relationships?: ContextRowRelationships;
  /** True when the file's metadata ran past what the index reads (keys may be missing). */
  metadataTruncated?: boolean;
  /** SW-DNS-2: present only when `select` names `"summary"`: a story/bug/spike/epic's
   *  first configured summary section (capped), or null. */
  summary?: string | null;
};

export type ContextGroupCount = { value: string | null; count: number };

export type ContextQueryResult = {
  /** Default exclusions applied before text scanning; explicit paths opt in. */
  excluded?: string[];
  schemaVersion: number;
  total: number;
  rows: ContextRow[];
  groups?: ContextGroupCount[];
  nextCursor?: string;
  textScanTruncated: boolean;
  /** True when `total` may undercount (the `text` scan stopped early). */
  totalApproximate: boolean;
  /** The orgs the query was narrowed to; absent = every org. */
  scopeOrgs?: string[];
  /** True while the first index build is still running. */
  indexing: boolean;
  /** The index snapshot this answer came from. */
  generation: number;
};
