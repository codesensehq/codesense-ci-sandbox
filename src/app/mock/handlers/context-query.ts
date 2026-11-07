// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { MockEventBus } from "../mock-events";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME } from "../fixtures";
import type {
  ContextFilter,
  ContextQuery,
  ContextRow,
  ContextSort,
} from "@/shared/lib/context-query/types";
import { CONTEXT_INDEX_CHANGED_EVENT } from "@/tauri/events";

/**
 * The context index in mock mode: a small, realistic team board (two epic
 * folders, stories across statuses, points, assignees, sprints and ranks) that
 * `queryContext` / `subscribeContext` panes query, plus the page bridge's
 * `context()` / `openFile` resolvers and its frontmatter writes, which edit the
 * rows in memory and emit `context-index-changed` as the real index does.
 *
 * The evaluator covers the query model a board uses (scope, filters, `match`,
 * `parentEpic`, sort, `groupBy`, `select`, `limit`); the real engine is Rust.
 */

/** Where mock team content lives; panes sit beside it (`handlers/panes.ts`). */
export const MOCK_CONTEXT_ROOT = "/mock-ctx";
const CONTENT = `${MOCK_CONTEXT_ROOT}/${MOCK_TEAM_NAME}/content`;

interface Seed {
  id: string;
  dir: string;
  title: string;
  fields: Record<string, unknown>;
}

const EPIC_PANES = "20-panes";
const EPIC_BOARD = "21-board";

const SEEDS: Seed[] = [
  {
    id: "README",
    dir: EPIC_PANES,
    title: "Panes",
    fields: { type: "epic", status: "in-progress", story_id: "SW-PN" },
  },
  {
    id: "README",
    dir: EPIC_BOARD,
    title: "Board",
    fields: { type: "epic", status: "todo", story_id: "SW-BD" },
  },
  ...(
    [
      ["SW-PN-1", EPIC_PANES, "Panes read the theme", "done", 3, "alice", "2026-09-B", 1],
      ["SW-PN-2", EPIC_PANES, "Badge on the panel title", "review", 2, "bob", "2026-10-A", 2],
      [
        "SW-PN-3",
        EPIC_PANES,
        "Pause polling while hidden",
        "in-progress",
        5,
        "alice",
        "2026-10-A",
        3,
      ],
      ["SW-PN-4", EPIC_PANES, "Stop a session from a pane", "todo", 3, null, "2026-10-A", 4],
      ["SW-PN-5", EPIC_PANES, "Pane errors in place", "blocked", 2, "carol", "2026-10-A", 5],
      ["SW-BD-1", EPIC_BOARD, "Columns from the taxonomy", "in-progress", 5, "bob", "2026-10-A", 1],
      ["SW-BD-2", EPIC_BOARD, "Drag a card to change status", "todo", 8, null, "2026-10-A", 2],
      ["SW-BD-3", EPIC_BOARD, "Estimate in place", "todo", null, null, null, 3],
      ["SW-BD-4", EPIC_BOARD, "Sprint filter", "planning", 3, "carol", null, 4],
      ["SW-BD-5", EPIC_BOARD, "Open a card's story", "qa", 1, "alice", "2026-10-A", 5],
    ] as const
  ).map(([id, dir, title, status, points, assignee, sprint, rank]) => ({
    id,
    dir,
    title,
    fields: {
      type: "story",
      story_id: id,
      status,
      parent_epic: dir === EPIC_PANES ? "SW-PN" : "SW-BD",
      rank,
      ...(points === null ? {} : { points }),
      ...(assignee === null ? {} : { assigned_to: assignee }),
      ...(sprint === null ? {} : { sprint }),
    },
  })),
];

/** SW-DNS-2: what each seeded story's `## Purpose` says (the index's `summary`). */
const MOCK_SUMMARIES: Record<string, string> = {
  "SW-PN":
    "Custom right panes become first-class: themed, badged, live, and able to drive the app.",
  "SW-BD": "A board pane that drives the sprint from the team's own taxonomy.",
  "SW-PN-1": "Panes read the app theme as CSS tokens so a pane looks native in light and dark.",
  "SW-PN-2": "A pane can show a count on its panel title, even while its section is collapsed.",
  "SW-PN-3":
    "Pause a pane's polling and animations while it is off screen, so hidden panes cost nothing and resume instantly when shown again.",
  "SW-PN-4": "Stop an agent session from a pane without leaving it.",
  "SW-PN-5": "A pane's errors show in place, naming the file and the reason.",
  "SW-BD-1": "Board columns come from the team taxonomy, in order, with their colours.",
  "SW-BD-2": "Drag a card between columns to change its status; the file's frontmatter updates.",
  "SW-BD-3": "Estimate a story's points in place on its card.",
  "SW-BD-4": "Filter the board to one sprint, the backlog, or everything.",
  "SW-BD-5": "Open a card's story file in the viewer with one click.",
};

function seedRows(): ContextRow[] {
  return SEEDS.map((s, i) => {
    const fileName = `${s.id}.md`;
    const relPath = `${s.dir}/${fileName}`;
    return {
      id: s.id === "README" ? String(s.fields.story_id) : s.id,
      path: `${CONTENT}/${relPath}`,
      relPath,
      fileName,
      org: MOCK_ORG_SLUG,
      team: MOCK_TEAM_NAME,
      layer: "team",
      kind: s.fields.type === "epic" ? "epic" : "story",
      mtime: 1_759_000_000_000 + i * 60_000,
      title: s.title,
      fields: { ...s.fields },
      relationships: {
        dependsOn: [],
        dependents: [],
        supportingDocs: [],
        backlinks: [],
        parentEpic: (s.fields.parent_epic as string | undefined) ?? null,
        folderEpic: s.dir,
      },
    };
  });
}

let rows: ContextRow[] = seedRows();

/** Back to the seed rows (tests). */
export function resetMockContextRows(): void {
  rows = seedRows();
}

const norm = (key: string) =>
  key
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/-/g, "_")
    .toLowerCase();

function lookup(r: ContextRow, key: string): unknown {
  const k = norm(key);
  switch (k) {
    case "path":
    case "id":
    case "org":
    case "mtime":
      return r[k];
    case "rel_path":
      return r.relPath;
    case "file_name":
      return r.fileName;
    case "team":
    case "title":
    case "layer":
    case "kind":
      return r.fields[k] ?? r[k];
    case "dir":
      return r.fields.dir ?? r.relPath.split("/").slice(0, -1).join("/");
  }
  const want = k === "epic" ? "parent_epic" : k;
  const hit = Object.entries(r.fields).find(([f]) => {
    const n = norm(f);
    return (n === "epic" ? "parent_epic" : n) === want;
  });
  return hit?.[1];
}

const items = (v: unknown): string[] =>
  v === null || v === undefined || v === ""
    ? []
    : Array.isArray(v)
      ? v.flatMap(items)
      : [String(v)];

function matches(r: ContextRow, f: ContextFilter): boolean {
  const have = items(lookup(r, f.key));
  const want = items(f.value).map((w) => (w === "@me" ? "mock-dev" : w));
  switch (f.op) {
    case "eq":
    case "in":
      return have.some((h) => want.includes(h));
    case "not":
      return !have.some((h) => want.includes(h));
    case "exists":
      return have.length > 0;
    case "missing":
      return have.length === 0;
    case "contains":
      return have.some((h) => want.some((w) => h.toLowerCase().includes(w.toLowerCase())));
    case "prefix":
      return have.some((h) => want.some((w) => h.startsWith(w)));
    case "range": {
      const n = have.map(Number).filter((x) => !Number.isNaN(x));
      return n.some(
        (x) =>
          (f.min === undefined || x >= Number(f.min)) &&
          (f.max === undefined || x <= Number(f.max)),
      );
    }
  }
}

function inScope(r: ContextRow, q: ContextQuery): boolean {
  const w = q.where ?? {};
  if (Array.isArray(w.orgs) && r.org && !w.orgs.includes(r.org)) return false;
  if (w.teams && (!r.team || !w.teams.includes(r.team))) return false;
  if (w.layers && !w.layers.includes(r.layer)) return false;
  if (w.kinds && !w.kinds.includes(r.kind)) return false;
  if (w.sameRootAs && !w.sameRootAs.startsWith(`${CONTENT}/`)) return false;
  if (w.sameOrgAs && !w.sameOrgAs.startsWith(`${MOCK_CONTEXT_ROOT}/`)) return false;
  if (w.pathPrefix) {
    const p = w.pathPrefix.replace(/\/+$/, "");
    const on = p.startsWith("/") ? r.path : r.relPath;
    if (on !== p && !on.startsWith(`${p}/`)) return false;
  }
  return true;
}

function inEpic(r: ContextRow, token: string): boolean {
  const epic = lookup(r, "parent_epic");
  if (typeof epic === "string" && epic.toLowerCase() === token.toLowerCase()) return true;
  return r.relPath.split("/").slice(0, -1).includes(token);
}

function compare(a: ContextRow, b: ContextRow, sort: ContextSort): number {
  const spec: Exclude<ContextSort, string> =
    typeof sort === "string"
      ? { key: sort.replace(/^-/, ""), dir: sort.startsWith("-") ? "desc" : "asc" }
      : sort;
  const rank = (r: ContextRow): [number, string | number] => {
    const v = items(lookup(r, spec.key))[0];
    if (v === undefined) return [spec.missing === "first" ? -1 : 1, ""];
    if (typeof sort !== "string" && sort.order) {
      const i = sort.order.indexOf(v);
      return [0, i < 0 ? sort.order.length : i];
    }
    const n = Number(v);
    return [0, Number.isNaN(n) ? v : n];
  };
  const [ma, va] = rank(a);
  const [mb, vb] = rank(b);
  if (ma !== mb) return ma - mb;
  const c = va < vb ? -1 : va > vb ? 1 : 0;
  return spec.dir === "desc" ? -c : c;
}

/** Answer a `query_context` call against the mock rows. */
export function queryMockContext(q: ContextQuery = {}): unknown {
  const match: ContextFilter[] = Object.entries(q.match ?? {}).map(([key, value]) => ({
    key,
    op: Array.isArray(value) ? "in" : "eq",
    value,
  }));
  const filters = [...(q.filters ?? []), ...match];
  // Clauses on one key OR together; different keys AND.
  const byKey = new Map<string, ContextFilter[]>();
  for (const f of filters) byKey.set(norm(f.key), [...(byKey.get(norm(f.key)) ?? []), f]);
  let hits = rows.filter(
    (r) =>
      inScope(r, q) &&
      [...byKey.values()].every((clauses) => clauses.some((f) => matches(r, f))) &&
      (!q.relationships?.parentEpic || inEpic(r, q.relationships.parentEpic)) &&
      (!q.text || `${r.title ?? ""}`.toLowerCase().includes(q.text.toLowerCase())),
  );
  for (const s of [...(q.sort ?? [])].reverse()) hits = [...hits].sort((a, b) => compare(a, b, s));
  const groups = q.groupBy
    ? [
        ...hits.reduce((m, r) => {
          const v = items(lookup(r, q.groupBy!))[0] ?? null;
          return m.set(v, (m.get(v) ?? 0) + 1);
        }, new Map<string | null, number>()),
      ].map(([value, count]) => ({ value, count }))
    : undefined;
  const wantRel = !q.select || q.select.includes("relationships");
  // SW-DNS-2: `summary` rides only when selected, as the real engine does.
  const wantSummary = !!q.select?.some((s) => norm(s) === "summary");
  const out = hits.slice(0, q.limit ?? 200).map((r) => ({
    ...r,
    ...(wantSummary ? { summary: MOCK_SUMMARIES[r.id] ?? null } : {}),
    fields: q.select
      ? Object.fromEntries(
          Object.entries(r.fields).filter(([k]) => q.select!.some((s) => norm(s) === norm(k))),
        )
      : { ...r.fields },
    ...(wantRel ? {} : { relationships: undefined }),
  }));
  return {
    schemaVersion: 1,
    total: hits.length,
    rows: out,
    ...(groups ? { groups } : {}),
    textScanTruncated: false,
    totalApproximate: false,
    scopeOrgs: [MOCK_ORG_SLUG],
    indexing: false,
    generation: 1,
  };
}

function location(page: string) {
  const m = new RegExp(`^${MOCK_CONTEXT_ROOT}/([^/]+)/content/`).exec(page);
  if (!m) return null;
  const priv = m[1] === "private";
  return {
    org: MOCK_ORG_SLUG,
    team: priv ? null : m[1],
    layer: priv ? "private" : "team",
    contentRoot: `${MOCK_CONTEXT_ROOT}/${m[1]}/content`,
  };
}

function resolveOpen(page: string, path: string, team: string | null) {
  const own = location(page);
  const root = team
    ? `${MOCK_CONTEXT_ROOT}/${team}/content`
    : path.startsWith("/")
      ? location(path)?.contentRoot
      : own?.contentRoot;
  if (!root || root.includes("/private/")) {
    throw new Error(`refused: ${path} is not inside a known team content root`);
  }
  const abs = path.startsWith("/") ? path : `${root}/${path}`;
  if (abs.split("/").includes("..") || !abs.startsWith(`${root}/`)) {
    throw new Error(`refused: ${path} leaves its content root`);
  }
  if (!rows.some((r) => r.path === abs)) throw new Error(`not-found: ${abs}`);
  return { path: abs, org: MOCK_ORG_SLUG, team: root.split("/")[2], contentRoot: root };
}

export function contextQueryHandlers(events: MockEventBus): HandlerRegistry {
  const changed = (path: string) =>
    setTimeout(() =>
      events.emit(CONTEXT_INDEX_CHANGED_EVENT, {
        generation: Date.now(),
        paths: [path],
        orgs: [MOCK_ORG_SLUG],
        allOrgs: false,
        truncated: false,
      }),
    );
  const write = (path: string, apply: (row: ContextRow) => void) => {
    const row = rows.find((r) => r.path === path);
    if (!row) {
      throw new Error(
        `refusing to edit metadata: ${path} is not inside a team or private content root`,
      );
    }
    apply(row);
    row.mtime = Date.now();
    if (row.relationships)
      row.relationships.parentEpic = (lookup(row, "parent_epic") as string) ?? null;
    changed(path);
  };
  return {
    query_context: (args) => queryMockContext((args.query ?? {}) as ContextQuery),
    bridge_doc_context: (args) => location(String(args.page ?? "")),
    bridge_resolve_open_target: (args) =>
      resolveOpen(
        String(args.page ?? ""),
        String(args.path ?? ""),
        typeof args.team === "string" ? args.team : null,
      ),
    update_frontmatter: (args) => {
      write(String(args.path), (row) => {
        for (const [k, v] of Object.entries((args.fields ?? {}) as Record<string, unknown>)) {
          if (v === null) delete row.fields[k];
          else row.fields[k] = v;
        }
      });
      return null;
    },
    apply_metadata_edit: (args) => {
      write(String(args.path), (row) => {
        for (const e of (args.edits ?? []) as Array<Record<string, unknown>>) {
          const field = String(e.field ?? "");
          if (e.kind === "setField") row.fields[field] = e.value;
          else if (e.kind === "setList") row.fields[field] = e.items;
          else if (e.kind === "clearField") delete row.fields[field];
        }
      });
      return null;
    },
  };
}
