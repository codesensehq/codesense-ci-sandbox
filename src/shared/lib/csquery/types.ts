// SPDX-License-Identifier: MIT
/**
 * SW-DX-5 / SW-OC-26 — the inline `csquery` block contract. A `csquery` fence is
 * parsed by the pure `parseCsQuery` into a `CsQuerySpec` (a `ContextQuery` for
 * the one Rust engine + view-only hints) or a `CsQueryError`. The view island
 * (`cs-query-view.tsx`) runs the query through `subscribeContext`, so the block
 * is live and never filters, sorts or pages on the client.
 *
 * `query` is the SINGLE membership source. View concerns (`view`/`columns`/
 * `group`/`limit`) live BESIDE it. UI defaults that depend on where the block
 * sits (its own org, relative refs) are applied by the island, not the parser.
 */

import type { ContextQuery } from "../context-query/types";

export type { ContextQuery };

/** The inline render shapes. `connectivity` (SW-DX-8) renders the scoped
 *  story×context view; it requires a `scope:` directive. */
export type CsQueryViewKind = "table" | "list" | "board" | "connectivity";

/** A parsed sort directive: any field plus a direction. */
export type CsQuerySort = { field: string; dir: "asc" | "desc" };

/** The parsed, ready-to-render spec. */
export type CsQuerySpec = {
  /** Membership + order for the Rust engine. No `limit` (the view's cap is `limit`). */
  query: ContextQuery;
  /** The raw `scope:` directive (a sub-epic name or absolute path), resolved by the island. */
  scope: string | null;
  /** True when the block named its orgs (`org:`); otherwise the island scopes to the block's own org. */
  orgExplicit: boolean;
  /** Chosen view (default `table`). */
  view: CsQueryViewKind;
  /** Table columns, in order (default `["name", "status"]`). */
  columns: string[];
  /** Board lane / list section key (a field name); `null` ⇒ view default (board → `status`). */
  group: string | null;
  /** Rows shown at most (default `DEFAULT_CSQUERY_LIMIT`). */
  limit: number;
  /** The `sort:` directive (`null` ⇒ status order). */
  sort: CsQuerySort | null;
  /** Non-fatal notices (unknown keys, action verbs dropped, bad values defaulted). */
  warnings: string[];
};

/** A hard parse error — the only non-forgiving outcome. Never thrown; returned. */
export type CsQueryError = { error: string; line?: number };

export type CsQueryParseResult = CsQuerySpec | CsQueryError;

/** Narrow a parse result to its error arm. */
export function isCsQueryError(r: CsQueryParseResult): r is CsQueryError {
  return typeof (r as CsQueryError).error === "string";
}

/** Default render cap — an unbounded block is a footgun. */
export const DEFAULT_CSQUERY_LIMIT = 200;

/** Default table columns when `columns:` is omitted. */
export const DEFAULT_CSQUERY_COLUMNS = ["name", "status"] as const;

/** Kinds a block lists when it names none: pipeline work items, as before. */
export const DEFAULT_CSQUERY_KINDS = ["story", "bug", "spike"] as const;
