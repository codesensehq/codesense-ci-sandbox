// SPDX-License-Identifier: MIT
/**
 * SW-OC-26 — present an engine row in the `DocSummary` shape the existing
 * renderers (csquery views, the connectivity builder, panes) read. A pure
 * reshaping: every frontmatter key becomes a `tags` value list.
 */

import type { DocSummary } from "../doc-query/types";
import type { ContextRow } from "./types";

function strings(v: unknown): string[] {
  if (v === null || v === undefined || v === "") return [];
  if (Array.isArray(v)) return v.flatMap(strings);
  if (typeof v === "object") return [JSON.stringify(v)];
  return [String(v)];
}

function str(v: unknown): string | null {
  return typeof v === "string" && v !== "" ? v : null;
}

function num(v: unknown): number | null {
  const n = typeof v === "number" ? v : typeof v === "string" ? Number(v) : NaN;
  return Number.isFinite(n) ? n : null;
}

export function rowToDocSummary(row: ContextRow): DocSummary {
  const tags: Record<string, string[]> = {};
  for (const [k, v] of Object.entries(row.fields)) tags[k] = strings(v);
  const f = row.fields;
  return {
    id: row.id,
    path: row.path,
    fileName: row.fileName,
    tags,
    status: str(f.status),
    documentType: str(f.type),
    mtime: row.mtime,
    sourceRepo: str(f.source_repo),
    sprint: str(f.sprint),
    points: num(f.points),
    assignedTo: str(f.assigned_to),
    relationships: {
      supportingDocs: row.relationships?.supportingDocs ?? [],
      backlinks: row.relationships?.backlinks ?? [],
      dependsOn: row.relationships?.dependsOn ?? [],
    },
  };
}
