// SPDX-License-Identifier: MIT
/**
 * SW-DX-8 — the context-connectivity view contract. Pure data shapes over the
 * SW-DX-2 `DocSummary[]` query result (edges already resolved by
 * the Rust context engine's one relationship resolver). The builder does ZERO
 * path resolution — it consumes resolved `RelRef`s — so a resolver fork is
 * structurally impossible (AC-3).
 */

import type { DocSummary } from "../doc-query/types";

/** Below this many resolved forward supporting-docs a story is "thin". */
export const THIN_THRESHOLD = 2;

/** A story's connectivity health. Keys off FORWARD (own declared) context. */
export type HealthVerdict = "context-desert" | "thin" | "healthy";

/** Which way an edge points relative to the row story. `both` = mutual link. */
export type LinkDirection = "forward" | "backlink" | "both";

/** One resolved (or dead) context edge on a story row. */
export type ContextLink = {
  /** Absolute path of the linked file. */
  filePath: string;
  /** Basename of the linked file. */
  fileName: string;
  /** `DOC_TYPE_ICON` bucket key (story·bug·spike·doc·retro·canvas·image). */
  groupKey: string;
  /** Forward (this story lists it), backlink (it lists this story), or both. */
  direction: LinkDirection;
  /** True iff the target exists as a real file (full-file resolution). */
  resolved: boolean;
};

/** One story row: its context, grouped, plus a health verdict. */
export type StoryConnectivity = {
  /** The in-scope story this row is about. */
  story: DocSummary;
  /** Forward links (this story's `supporting_docs`); `both` edges live here. */
  forward: ContextLink[];
  /** In-scope backlinks (stories that list this one), excluding `both` edges. */
  backlinks: ContextLink[];
  /** Forward links bucketed by `groupKey`. */
  groups: Record<string, ContextLink[]>;
  /** Coverage verdict (keys off resolved forward count). */
  health: HealthVerdict;
  /** Count of RESOLVED forward links (the health input). */
  forwardCount: number;
  /** Count of in-scope backlink-only links. */
  backlinkCount: number;
};

/** An in-scope story that is an island in the scope subgraph. */
export type OrphanDoc = {
  filePath: string;
  fileName: string;
  groupKey: string;
};

/** Scope-level roll-up. */
export type ConnectivitySummary = {
  storyCount: number;
  contextDesertCount: number;
  thinCount: number;
  healthyCount: number;
  orphanCount: number;
  /** Total context links shown across all rows (forward + backlink). */
  totalContextLinks: number;
  /** Forward links that don't resolve to a real file (dead refs). */
  unresolvedCount: number;
};

/** The full connectivity view for one scope. */
export type ConnectivityView = {
  /** The scope root (absolute path prefix) this view was built for. */
  scope: string;
  rows: StoryConnectivity[];
  orphans: OrphanDoc[];
  summary: ConnectivitySummary;
};

/** Builder options. */
export type BuildConnectivityOpts = {
  /** Override the "thin" threshold (default `THIN_THRESHOLD`). */
  thinThreshold?: number;
};
