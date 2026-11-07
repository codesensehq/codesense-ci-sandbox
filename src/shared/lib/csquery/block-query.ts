// SPDX-License-Identifier: MIT
/**
 * SW-OC-26 — the query a mounted `csquery` block actually runs: the parsed
 * spec's `ContextQuery` plus the UI defaults that depend on where the block
 * sits. Pure; no filtering happens here (the Rust engine does all of it).
 *
 * - Its own org by default (`sameOrgAs: basePath`) unless the block names orgs
 *   (`org: all` / `org: a, b`) — a UI default, never an engine default. Private
 *   items are only ever its own org's ([`blockPage`]).
 * - Relative `related:` / `backlink:` / `depends:` refs resolve against the
 *   block's directory so they match a target's absolute path unambiguously.
 * - A resolved `scope:` root bounds the query by path.
 * - `limit` is the view's cap (the connectivity view asks for the full set).
 */

import { dirname } from "../basename";
import { resolveRelativePath } from "../paths/resolve-relative-path";
import type { ContextQuery, ContextRelationshipFilter, ContextSort } from "../context-query/types";
import type { CsQuerySpec } from "./types";

/** Rows the connectivity view asks for (it renders the full in-scope set). */
export const CONNECTIVITY_LIMIT = 10_000;

function looksRelative(token: string): boolean {
  return token.includes("/") && !token.startsWith("/");
}

function resolveRefs(
  rel: ContextRelationshipFilter | undefined,
  basePath: string | undefined,
): ContextRelationshipFilter | undefined {
  if (!rel || !basePath) return rel;
  const base = dirname(basePath);
  const out = { ...rel };
  for (const k of ["hasSupportingDoc", "backlinkedBy", "dependsOn"] as const) {
    const v = out[k];
    if (v && looksRelative(v)) out[k] = resolveRelativePath(base, v);
  }
  return out;
}

export type BlockContext = {
  /** Absolute path of the doc holding the block. */
  basePath?: string;
  /** The resolved `scope:` root (absolute), when the block has one. */
  scopeRoot?: string | null;
  /** The team's status ids in taxonomy order: status sorts (and board lanes) follow it. */
  statusOrder?: string[];
};

/** Status sort keys take the team's taxonomy order when one is known. */
function withStatusOrder(sort: ContextSort[] | undefined, order: string[] | undefined) {
  if (!sort || !order || order.length === 0) return sort;
  return sort.map((s) => {
    if (s === "status" || s === "-status") {
      return { key: "status", dir: s === "-status" ? "desc" : "asc", order } as ContextSort;
    }
    if (typeof s === "object" && s.key === "status" && !s.order) return { ...s, order };
    return s;
  });
}

/**
 * The page a block's query runs for: the doc holding it. Private items then
 * come only from that doc's org, whatever the block says (`org: all`,
 * `layer: private`, …). A block with no known doc runs as an unknown page: no
 * private items at all (fail closed).
 */
export function blockPage(basePath: string | undefined): string {
  return basePath ?? "";
}

/** The final query for a block. Never mutates `spec`. */
export function blockQuery(spec: CsQuerySpec, ctx: BlockContext): ContextQuery {
  const where = { ...spec.query.where };
  if (!spec.orgExplicit && ctx.basePath) where.sameOrgAs = ctx.basePath;
  const scopeRoot = ctx.scopeRoot ?? spec.scope;
  if (scopeRoot) where.pathPrefix = scopeRoot;
  const relationships = resolveRefs(spec.query.relationships, ctx.basePath);
  const sort = withStatusOrder(spec.query.sort, ctx.statusOrder);
  return {
    ...spec.query,
    where,
    ...(sort ? { sort } : {}),
    ...(relationships ? { relationships } : {}),
    limit: spec.view === "connectivity" ? CONNECTIVITY_LIMIT : spec.limit,
    ...(spec.group || spec.view === "board" ? { groupBy: spec.group ?? "status" } : {}),
  };
}
