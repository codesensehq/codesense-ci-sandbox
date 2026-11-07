// SPDX-License-Identifier: MIT
/**
 * SW-DX-5 / SW-OC-26 — the pure `csquery` DSL parser. No I/O, no React, no Tauri:
 * `parseCsQuery(src)` is deterministic and NEVER throws — it returns a
 * `CsQuerySpec` (forgiving: unknown keys → warnings, empty body → every
 * pipeline item) or a `CsQueryError` (`{ error, line }`) for the few hard cases.
 * The `query` it produces is a `ContextQuery` for the one Rust engine, so the
 * block is a view of the engine's answer, not a re-implementation.
 *
 * Grammar (one directive per line, `key: value`; blank + `#` comment lines
 * ignored; keys case-insensitive; whitespace tolerant):
 *   tag: <value>            → `tags` contains value         (conventional `tags:` frontmatter)
 *   tag: <key>=<value>      → <key> equals value             (any frontmatter key)
 *   status: <a>, <b>        → status in [a, b]
 *   epic: <id>              → parent epic                    (`parent_epic`/`epic`, or a folder)
 *   depends: <id>           → depends on                     (path, story_id or stem)
 *   related: <pathOrId>     → lists it in supporting_docs
 *   backlink: <pathOrId>    → is listed in its supporting_docs
 *   kind: <a>, <b>          → kinds   (default story, bug, spike)
 *   org: all | <a>, <b>     → orgs    (default: the block's own org)
 *   team: <a>, <b>          → teams
 *   layer: team|private|all-orgs
 *   text: <words>           → body full-text match
 *   scope: <nameOrPath>     → sub-epic name or abs path prefix (connectivity view)
 *   view: table|list|board|connectivity  (default table; unknown → default + warning)
 *   group: <field>          → board lane / list section
 *   columns: <a>, <b>       → table columns (default name,status)
 *   limit: <n>              → rows shown (default 200)
 *   sort: <field> [asc|desc]→ any field (`name` = file name, `id` = story id)
 *
 * READ-ONLY invariant (AC-11): the grammar exposes ONLY data selectors + view
 * hints. Any action-shaped key (`setStatus`/`fire`/`open`/`draft`/`route`, …) is
 * warned-and-dropped — an inline block runs in the app's trusted context, so it
 * MUST NOT be able to name a mutation.
 */

import type {
  ContextFilter,
  ContextKind,
  ContextLayer,
  ContextQuery,
  ContextRelationshipFilter,
  ContextSort,
} from "../context-query/types";
import {
  DEFAULT_CSQUERY_COLUMNS,
  DEFAULT_CSQUERY_KINDS,
  DEFAULT_CSQUERY_LIMIT,
  type CsQueryParseResult,
  type CsQuerySort,
  type CsQueryViewKind,
} from "./types";

/** Action-shaped keys that are explicitly refused (AC-11) with a pointed warning. */
const ACTION_KEYS = new Set([
  "setstatus",
  "set_status",
  "fire",
  "open",
  "draft",
  "route",
  "run",
  "exec",
  "mutate",
  "write",
  "delete",
]);

const VIEW_KINDS: ReadonlySet<string> = new Set(["table", "list", "board", "connectivity"]);

const KINDS: ReadonlySet<string> = new Set([
  "story",
  "bug",
  "spike",
  "epic",
  "doc",
  "dashboard",
  "skill",
  "other",
]);

const LAYERS: ReadonlySet<string> = new Set(["team", "private", "all-orgs"]);

/** User-facing sort tokens mapped onto engine field names. */
const SORT_FIELD_ALIASES: Record<string, string> = {
  name: "fileName",
  filename: "fileName",
  file: "fileName",
  id: "story_id",
  storyid: "story_id",
};

/** The status order a block uses when it names no sort (status, then story id). */
const DEFAULT_SORT: ContextSort[] = ["status", { key: "story_id", missing: "first" }];

function normalizeKey(key: string): string {
  return key.trim().toLowerCase();
}

function splitCommaList(value: string): string[] {
  return value
    .split(",")
    .map((t) => t.trim())
    .filter((t) => t.length > 0);
}

/** Parse a `csquery` block body. Pure + total: never throws, never does I/O. */
export function parseCsQuery(src: string): CsQueryParseResult {
  const warnings: string[] = [];
  const filters: ContextFilter[] = [];
  const statuses: string[] = [];
  const relationships: ContextRelationshipFilter = {};
  let kinds: ContextKind[] | null = null;
  let orgs: "all" | string[] | null = null;
  let teams: string[] | null = null;
  let layers: ContextLayer[] | null = null;
  let text: string | null = null;

  let view: CsQueryViewKind = "table";
  let columns: string[] = [...DEFAULT_CSQUERY_COLUMNS];
  let group: string | null = null;
  let limit = DEFAULT_CSQUERY_LIMIT;
  let sort: CsQuerySort | null = null;
  let scope: string | null = null;

  // Overwrite (last-wins) directives warn on a second occurrence. `tag`/`status`
  // accumulate on purpose.
  const SINGLE_VALUE_KEYS = new Set([
    "related",
    "backlink",
    "epic",
    "depends",
    "scope",
    "view",
    "group",
    "limit",
    "sort",
    "columns",
    "kind",
    "org",
    "team",
    "layer",
    "text",
  ]);
  const seen = new Set<string>();

  const lines = src.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const lineNo = i + 1;
    const raw = lines[i].trim();
    if (raw === "" || raw.startsWith("#")) continue;

    const colon = raw.indexOf(":");
    if (colon < 0) {
      warnings.push(`Line ${lineNo}: ignored "${raw}" — expected "key: value".`);
      continue;
    }

    const key = normalizeKey(raw.slice(0, colon));
    const value = raw.slice(colon + 1).trim();

    if (ACTION_KEYS.has(key.replace(/-/g, "_"))) {
      warnings.push(
        `Line ${lineNo}: "${key}" is an action verb and is not allowed in a read-only csquery block — ignored.`,
      );
      continue;
    }

    if (SINGLE_VALUE_KEYS.has(key) && value !== "") {
      if (seen.has(key)) {
        warnings.push(`Line ${lineNo}: "${key}" set more than once — using the last value.`);
      }
      seen.add(key);
    }

    const needsValue = (name: string): boolean => {
      if (value !== "") return true;
      warnings.push(`Line ${lineNo}: "${name}:" with no value — ignored.`);
      return false;
    };

    switch (key) {
      case "tag": {
        if (!needsValue("tag")) break;
        const eq = value.indexOf("=");
        if (eq >= 0) {
          const field = value.slice(0, eq).trim();
          const v = value.slice(eq + 1).trim();
          if (field === "" || v === "") {
            warnings.push(`Line ${lineNo}: malformed "tag: ${value}" — expected "key=value".`);
            break;
          }
          filters.push({ key: field, op: "eq", value: v });
        } else {
          // Bare `tag: x` matches the conventional `tags:` frontmatter list.
          filters.push({ key: "tags", op: "eq", value });
        }
        break;
      }
      case "status": {
        statuses.push(...splitCommaList(value));
        break;
      }
      case "related": {
        if (needsValue("related")) relationships.hasSupportingDoc = value;
        break;
      }
      case "backlink": {
        if (needsValue("backlink")) relationships.backlinkedBy = value;
        break;
      }
      case "epic": {
        if (needsValue("epic")) relationships.parentEpic = value;
        break;
      }
      case "depends": {
        if (needsValue("depends")) relationships.dependsOn = value;
        break;
      }
      case "kind": {
        const list = splitCommaList(value).map((k) => k.toLowerCase());
        const bad = list.filter((k) => !KINDS.has(k));
        if (bad.length > 0)
          warnings.push(`Line ${lineNo}: unknown kind "${bad.join(", ")}" — ignored.`);
        const good = list.filter((k) => KINDS.has(k)) as ContextKind[];
        if (good.length > 0) kinds = good;
        break;
      }
      case "org": {
        if (!needsValue("org")) break;
        orgs = value.toLowerCase() === "all" ? "all" : splitCommaList(value);
        break;
      }
      case "team": {
        const list = splitCommaList(value);
        if (list.length > 0) teams = list;
        break;
      }
      case "layer": {
        const list = splitCommaList(value).map((l) => l.toLowerCase());
        const good = list.filter((l) => LAYERS.has(l)) as ContextLayer[];
        if (good.length < list.length) {
          warnings.push(
            `Line ${lineNo}: unknown layer in "${value}" — use team, private or all-orgs.`,
          );
        }
        if (good.length > 0) layers = good;
        break;
      }
      case "text": {
        if (needsValue("text")) text = value;
        break;
      }
      case "scope": {
        if (needsValue("scope")) scope = value;
        break;
      }
      case "view": {
        const v = value.toLowerCase();
        if (VIEW_KINDS.has(v)) {
          view = v as CsQueryViewKind;
        } else {
          warnings.push(`Line ${lineNo}: unknown view "${value}" — using "table".`);
        }
        break;
      }
      case "group": {
        group = value === "" ? null : value;
        break;
      }
      case "columns": {
        const cols = splitCommaList(value);
        if (cols.length > 0) columns = cols;
        break;
      }
      case "limit": {
        const n = Number.parseInt(value, 10);
        // Require ≥ 1: `limit: 0` is almost always a typo.
        if (Number.isFinite(n) && n >= 1) {
          limit = n;
        } else {
          warnings.push(
            `Line ${lineNo}: invalid limit "${value}" — using ${DEFAULT_CSQUERY_LIMIT}.`,
          );
        }
        break;
      }
      case "sort": {
        const parsed = parseSort(value);
        if (parsed === null) {
          warnings.push(`Line ${lineNo}: empty sort — using default order.`);
        } else {
          sort = parsed;
        }
        break;
      }
      default: {
        warnings.push(`Line ${lineNo}: unknown key "${key}" — ignored.`);
        break;
      }
    }
  }

  if (statuses.length > 0) filters.push({ key: "status", op: "in", value: statuses });

  const query: ContextQuery = {
    where: {
      kinds: kinds ?? [...DEFAULT_CSQUERY_KINDS],
      ...(orgs !== null ? { orgs } : {}),
      ...(teams !== null ? { teams } : {}),
      ...(layers !== null ? { layers } : {}),
    },
    sort: sort ? [{ key: sort.field, dir: sort.dir }] : DEFAULT_SORT,
  };
  if (filters.length > 0) query.filters = filters;
  if (Object.keys(relationships).length > 0) query.relationships = relationships;
  if (text !== null) query.text = text;

  return {
    query,
    scope,
    orgExplicit: orgs !== null,
    view,
    columns,
    group,
    limit,
    sort,
    warnings,
  };
}

/** Parse a `sort` value (`<field> [asc|desc]`). `null` when empty. */
function parseSort(value: string): CsQuerySort | null {
  const parts = value.trim().split(/\s+/);
  const token = (parts[0] ?? "").trim();
  if (token === "") return null;
  const alias = SORT_FIELD_ALIASES[token.toLowerCase().replace(/-/g, "_").replace(/_/g, "")];
  const field = alias ?? token;
  const dir = (parts[1] ?? "").toLowerCase() === "desc" ? "desc" : "asc";
  return { field, dir };
}
