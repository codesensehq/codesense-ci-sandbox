// SPDX-License-Identifier: MIT
import type { MetadataFields } from "@/tauri/commands";

// ---------------------------------------------------------------------------
// SLW-MD-1: canonical field schema for the in-file metadata band.
//
// Source of truth mirrors the Rust `FrontmatterFields`. The user ALWAYS sees
// these canonical snake_case names — never the `cs:*`/dash HTML form (AC-3).
// ---------------------------------------------------------------------------

/** Scalar text/select fields (non-list, non-nested). */
export type ScalarField =
  | "type"
  | "status"
  | "story_id"
  | "source_repo"
  | "branch_name"
  | "dev_branch"
  | "team"
  | "assigned_to"
  | "branch";

/** List (chip-editor) fields. */
export type ListField =
  | "depends_on"
  | "supporting_docs"
  | "repos"
  | "scope_context"
  | "scope_install";

/** Every known field key. */
export type KnownField = ScalarField | ListField;

/** Constrained `type` options (mirrors Rust `PIPELINE_DOCUMENT_TYPES`). */
export const TYPE_OPTIONS = ["story", "bug", "spike"] as const;

/** Fields offered only when the doc is an epic (AC-7).
 *
 * SW-OR-1b: `branch` is NO LONGER epic-only — it is the unified canonical branch
 * for a STORY (superseding `branch_name`/`dev_branch`) and remains the integration
 * branch for an EPIC. A doc is one or the other, so the same key serves both; the
 * band therefore offers `branch` on stories too. */
export const EPIC_ONLY_FIELDS: ReadonlySet<KnownField> = new Set<KnownField>(["repos"]);

/** SW-XS-5: retired fields — never offered by "+ Add field", but still rendered
 *  (and removable) when an existing doc carries them. `scope_context` /
 *  `scope_install` were the epic's directory designation; directory ownership is
 *  gone and nothing reads them. */
export const RETIRED_FIELDS: ReadonlySet<KnownField> = new Set<KnownField>([
  "scope_context",
  "scope_install",
]);

/** Fields that only apply to a story, never an epic (SW-ID-5): assignment is
 *  per-story — an epic has no assignee, so the band hides the Assignee control
 *  (and its "+ Add field" entry) on epic docs. Mirror of {@link EPIC_ONLY_FIELDS}. */
export const STORY_ONLY_FIELDS: ReadonlySet<KnownField> = new Set<KnownField>(["assigned_to"]);

export const LIST_FIELDS: readonly ListField[] = [
  "depends_on",
  "supporting_docs",
  "repos",
  "scope_context",
  "scope_install",
];

/** Ordered list of every known field for "+ Add field" + present-field render order. */
export const KNOWN_FIELD_ORDER: readonly KnownField[] = [
  "type",
  "status",
  "story_id",
  "source_repo",
  "branch_name",
  "dev_branch",
  "team",
  "assigned_to",
  "depends_on",
  "supporting_docs",
  "repos",
  "branch",
  "scope_context",
  "scope_install",
];

export function isListField(field: string): field is ListField {
  return (LIST_FIELDS as readonly string[]).includes(field);
}

/** Read a field's current value off the wire model. Lists return arrays, scalars strings. */
export function fieldScalarValue(fields: MetadataFields, field: ScalarField): string | null {
  const v = fields[field];
  return typeof v === "string" ? v : null;
}

export function fieldListValue(fields: MetadataFields, field: ListField): string[] {
  const v = fields[field];
  return Array.isArray(v) ? v : [];
}

/** Is a known field present (has a value) in the model? */
export function isFieldPresent(fields: MetadataFields, field: KnownField): boolean {
  if (isListField(field)) return fieldListValue(fields, field).length > 0;
  return fieldScalarValue(fields, field as ScalarField) != null;
}

/**
 * Reject a list-chip value containing a comma (AC-3 parity, Write-path gap #2).
 * HTML joins lists with `,` (`split_csv`), so an unescaped comma would split one
 * item into two on round-trip — silent data divergence between md and HTML.
 * Returns an error message string when invalid, or `null` when the value is OK.
 */
export function validateListChipValue(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed === "") return "Value can't be empty";
  if (trimmed.includes(",")) return "Value can't contain a comma";
  return null;
}
