// SPDX-License-Identifier: MIT
// SW-DX-1: the ONE tag render-config source (mirrors the SW-CV-1 FILE_TYPE_CONFIG
// / pipeline STATUS_CONFIG discipline — no forked color maps). Every consumer —
// the metadata band chips, the DX-2 query result rendering, DX-4 dashboards, any
// legend — reads a tag's color/icon/label from `tagDisplay`.
//
// `icon` is a Lucide icon NAME (a string), not a component: DX-4 dashboards render
// in a sandboxed HTML iframe that can't use React components, so the contract is a
// name both React and HTML consumers resolve. `color` is always a `--cs-*` design
// token (project convention: no hex literals in tag display).

import type { TagSchema, TagType } from "@/tauri/commands";

/** One tag's resolved display config. Pure, string-only (iframe-safe). */
export interface TagDisplay {
  /** A CSS color value — the tag's declared `color`, else a per-type default
   *  `--cs-*` token wrapped in `var(...)`. Never a hardcoded hex. */
  color: string;
  /** A Lucide icon NAME — the tag's declared `icon`, else a per-type default. */
  icon: string;
  /** A human label derived from the tag name. */
  label: string;
}

/** Per-type default color token + icon — ONE source. Colors are `--cs-tag-*`
 *  design tokens (defined in `index.css`), never hex literals. */
const TAG_TYPE_DEFAULTS: Record<TagType, { colorToken: string; icon: string }> = {
  text: { colorToken: "--cs-tag-text", icon: "Tag" },
  select: { colorToken: "--cs-tag-select", icon: "List" },
  multi_select: { colorToken: "--cs-tag-multi-select", icon: "ListChecks" },
  ordered: { colorToken: "--cs-tag-ordered", icon: "ArrowUpNarrowWide" },
  status: { colorToken: "--cs-tag-status", icon: "CircleDot" },
  date: { colorToken: "--cs-tag-date", icon: "Calendar" },
  relation: { colorToken: "--cs-tag-relation", icon: "Link" },
  // Unknown/future type → neutral text-tag defaults (never a hex, never a throw).
  unknown: { colorToken: "--cs-tag-text", icon: "Tag" },
};

/** Fallback for an unknown tag (no `TagDef`) — sensible neutral text-tag config. */
const FALLBACK = { colorToken: "--cs-tag-text", icon: "Tag" } as const;

/** Title-case a tag name for display (`context_level` → `Context level`). */
function humanize(name: string): string {
  const t = name.trim().replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim();
  return t.length ? t.charAt(0).toUpperCase() + t.slice(1) : name;
}

/** A declared color may be a bare custom-property (`--cs-x` → `var(--cs-x)`) or
 *  an already-usable CSS value (a `var(...)`, `hsl(...)`, or named color) — wrap
 *  a bare token, pass anything else through unchanged. */
function cssColor(value: string): string {
  return value.startsWith("--") ? `var(${value})` : value;
}

/** Canonical form of a tag key for matching — MUST mirror Rust
 *  `TagSchema::tag`'s `canonical_tag_key` (trim + lowercase + hyphen→underscore)
 *  so the FE render config resolves the same `TagDef` the Rust validity does. */
function canonicalKey(name: string): string {
  return name.trim().toLowerCase().replace(/-/g, "_");
}

/**
 * Resolve one tag's display config from the schema — the single source every
 * tag renderer reads. A declared `color`/`icon` wins; otherwise a per-type
 * default `--cs-*` token + Lucide name. An unknown tag (no matching `TagDef`)
 * falls back to neutral text-tag defaults. Pure and total.
 */
export function tagDisplay(schema: TagSchema, tagName: string): TagDisplay {
  const key = canonicalKey(tagName);
  const def = schema.tags.find((t) => canonicalKey(t.name) === key);
  return resolveDisplay(tagName, def?.type, def?.color, def?.icon);
}

/**
 * SW-AU-77I: resolve a display config from an ALREADY-projected row (the metadata
 * band's `SchemaFieldRow`) — its `color`/`icon`/`type` were resolved in Rust, so
 * this needs no schema lookup (the FE-side key match 77G deliberately rejected). A
 * declared `color`/`icon` wins; otherwise the per-type default keyed off the row's
 * own `type`. Same resolution as {@link tagDisplay}, just sourced from the row.
 */
export function rowDisplay(row: {
  name: string;
  type?: string;
  color?: string;
  icon?: string;
}): TagDisplay {
  return resolveDisplay(row.name, row.type as TagType | undefined, row.color, row.icon);
}

/** Shared color/icon/label resolution — one home for {@link tagDisplay} and
 *  {@link rowDisplay} so schema-sourced and row-sourced rendering never drift. */
function resolveDisplay(
  name: string,
  type: TagType | undefined,
  color: string | undefined,
  icon: string | undefined,
): TagDisplay {
  const defaults = (type && TAG_TYPE_DEFAULTS[type]) || FALLBACK;
  const declaredColor = color?.trim();
  const declaredIcon = icon?.trim();
  return {
    color: declaredColor ? cssColor(declaredColor) : `var(${defaults.colorToken})`,
    icon: declaredIcon || defaults.icon,
    label: humanize(name),
  };
}
