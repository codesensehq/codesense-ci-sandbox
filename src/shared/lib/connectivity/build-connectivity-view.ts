// SPDX-License-Identifier: MIT
import type { DocSummary, RelRef } from "../doc-query/types";
import { classifyDocType } from "./classify-doc-type";
import { classifyHealth } from "./classify-health";
import {
  THIN_THRESHOLD,
  type BuildConnectivityOpts,
  type ConnectivityView,
  type ContextLink,
  type OrphanDoc,
  type StoryConnectivity,
} from "./types";

/**
 * SW-DX-8 — the pure connectivity builder (AC-1/2/4/5/6). Consumes the SW-DX-2
 * `DocSummary[]` query result (edges ALREADY resolved full-file by
 * the Rust context engine) and a `scopeRoot` path prefix. Does ZERO path
 * resolution of its own — it reads resolved `RelRef.path`s — so a resolver fork
 * is structurally impossible (AC-3). Scope bounds BOTH the row set and the
 * backlink fan by a pure in-scope-path intersection (AC-6). Total + never throws.
 *
 * Both surfaces feed this the same resolved edges: the inline island via the
 * context engine (`subscribeContext`), the standalone dashboard via
 * `window.codesense.query({ scope })` — one resolver, identical result.
 */
export function buildConnectivityView(
  docs: DocSummary[],
  scopeRoot: string,
  opts: BuildConnectivityOpts = {},
): ConnectivityView {
  const thinThreshold = opts.thinThreshold ?? THIN_THRESHOLD;

  const inScope = docs.filter((d) => isUnderScope(d.path, scopeRoot));
  // All docs by path (not just in-scope) so a forward target that is a STORY
  // gets its real documentType for the chip, even if it lives out of scope.
  const byPath = new Map<string, DocSummary>(docs.map((d) => [d.path, d]));

  const toLink = (r: RelRef, direction: ContextLink["direction"]): ContextLink => ({
    filePath: r.path,
    fileName: r.fileName,
    groupKey: classifyDocType(r.fileName, byPath.get(r.path)?.documentType ?? null),
    direction,
    resolved: r.resolved,
  });

  let contextDesertCount = 0;
  let thinCount = 0;
  let healthyCount = 0;
  let totalContextLinks = 0;
  let unresolvedCount = 0;

  const rows: StoryConnectivity[] = inScope.map((story) => {
    const sd = story.relationships.supportingDocs;
    const backlinksInScope = story.relationships.backlinks.filter((r) =>
      isUnderScope(r.path, scopeRoot),
    );

    const forwardPaths = new Set(sd.map((r) => r.path));
    const backlinkPaths = new Set(backlinksInScope.map((r) => r.path));

    // Forward links carry `both` when the target also backlinks this story
    // (a mutual link) — so a doc renders ONCE, under `forward`.
    const forward = sd.map((r) => toLink(r, backlinkPaths.has(r.path) ? "both" : "forward"));
    // Backlinks exclude the `both` edges (already shown under forward).
    const backlinks = backlinksInScope
      .filter((r) => !forwardPaths.has(r.path))
      .map((r) => toLink(r, "backlink"));

    // Health keys off RESOLVED forward context (a dead ref isn't real context).
    const forwardCount = forward.reduce((n, l) => n + (l.resolved ? 1 : 0), 0);
    const health = classifyHealth(forwardCount, thinThreshold);
    if (health === "context-desert") contextDesertCount++;
    else if (health === "thin") thinCount++;
    else healthyCount++;

    totalContextLinks += forward.length + backlinks.length;
    unresolvedCount += forward.reduce((n, l) => n + (l.resolved ? 0 : 1), 0);

    return {
      story,
      forward,
      backlinks,
      groups: groupByKey(forward),
      health,
      forwardCount,
      backlinkCount: backlinks.length,
    };
  });

  const orphans: OrphanDoc[] = inScope
    .filter((d) => {
      // An island in the scope subgraph: no in-scope story links it, and it
      // links no in-scope (resolved) forward target of its own (AC-5).
      const linkedIn = d.relationships.backlinks.some((r) => isUnderScope(r.path, scopeRoot));
      const linksIn = d.relationships.supportingDocs.some(
        (r) => r.resolved && isUnderScope(r.path, scopeRoot),
      );
      return !linkedIn && !linksIn;
    })
    .map((d) => ({
      filePath: d.path,
      fileName: d.fileName,
      groupKey: classifyDocType(d.fileName, d.documentType),
    }));

  return {
    scope: scopeRoot,
    rows,
    orphans,
    summary: {
      storyCount: inScope.length,
      contextDesertCount,
      thinCount,
      healthyCount,
      orphanCount: orphans.length,
      totalContextLinks,
      unresolvedCount,
    },
  };
}

/** True when `path` is `scopeRoot` itself or nested under it (separator-guarded).
 *  An empty scope bounds NOTHING (never "everything") — a foot-gun guard for the
 *  shared/vanilla contract, which has no upstream non-empty check. */
function isUnderScope(path: string, scopeRoot: string): boolean {
  const prefix = scopeRoot.endsWith("/") ? scopeRoot.slice(0, -1) : scopeRoot;
  if (prefix === "") return false;
  return path === prefix || path.startsWith(`${prefix}/`);
}

/** Bucket links by `groupKey`, preserving encounter order within a bucket. */
function groupByKey(links: ContextLink[]): Record<string, ContextLink[]> {
  const groups: Record<string, ContextLink[]> = {};
  for (const link of links) {
    (groups[link.groupKey] ??= []).push(link);
  }
  return groups;
}
