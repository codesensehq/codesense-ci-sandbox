// SPDX-License-Identifier: MIT
/**
 * SW-DX-5 / SW-OC-26 — the inline `csquery` view island. Mounted at a
 * `data-island-id` placeholder by markdown-viewer's existing `createRoot` loop
 * (the same seam as mermaid/d2), so it is DETACHED from React context — it takes
 * everything (`tree`, `basePath`, `onNavigate`) as explicit props.
 *
 * Data flow: the parsed block becomes a `ContextQuery` (`blockQuery`, adding the
 * block's UI defaults) and runs through `subscribeContext` — the one Rust engine,
 * re-run on every index change. The engine filters, sorts, pages and groups; the
 * island only renders. `tree` is used for one thing: resolving a `scope:` sub-epic
 * name to its folder.
 *
 * READ-ONLY: rows navigate via the viewer's `onNavigate`; there is no write path
 * (the parser already refuses action verbs, AC-11).
 */

import { useEffect, useMemo, useState } from "react";

import type { DocSummary } from "@/shared/lib/doc-query/types";
import type { ContextGroupCount } from "@/shared/lib/context-query/types";
import { subscribeContext } from "@/shared/lib/context-query/subscribe-context";
import { rowToDocSummary } from "@/shared/lib/context-query/to-doc-summary";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { parseCsQuery } from "@/shared/lib/csquery/parse-csquery";
import { blockPage, blockQuery } from "@/shared/lib/csquery/block-query";
import { findSubtreeByScope } from "@/shared/lib/connectivity/find-subtree-by-scope";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import {
  isCsQueryError,
  type CsQuerySpec,
  type CsQueryParseResult,
} from "@/shared/lib/csquery/types";
import { ErrorBoundary } from "@/shared/components/error-boundary";
import { CsQueryErrorCard } from "./cs-query-error-card";
import { ConnectivityView } from "./connectivity-view";

type CsQueryIslandProps = {
  /** The raw `csquery` fence body. */
  code: string;
  /** The scanned tree, used to resolve a `scope:` sub-epic name; `null` while scanning. */
  tree: DerivedPipelineTree | null;
  /** Absolute path of the containing doc — its org is the default scope; anchors relative refs. */
  basePath?: string;
  callerPage?: string;
  callerTeam?: string;
  /** The viewer's navigate handler — a row click opens the doc in the main viewer. */
  onNavigate?: (filePath: string) => void;
};

/**
 * The island entry point (used by BOTH the worker-island mount and the
 * jsx/sync `code` override). Parses the fence, renders a friendly error card on
 * a hard parse error, and otherwise wraps the live view in an `ErrorBoundary`
 * so a runtime throw degrades to a card instead of white-screening the doc
 * (AC-6). Keyed by `code` so an edited block resets the boundary.
 */
export function CsQueryIsland({
  code,
  tree,
  basePath,
  callerPage,
  callerTeam,
  onNavigate,
}: CsQueryIslandProps) {
  const parsed: CsQueryParseResult = useMemo(() => parseCsQuery(code), [code]);

  if (isCsQueryError(parsed)) {
    return (
      <CsQueryErrorCard
        message="Couldn't parse this csquery block"
        detail={parsed.error}
        line={parsed.line}
      />
    );
  }

  return (
    <ErrorBoundary
      key={code}
      scope="csquery"
      fallback={
        <CsQueryErrorCard
          message="This csquery block hit a runtime error"
          detail="The query couldn't be rendered. The rest of the document is unaffected."
        />
      }
    >
      <CsQueryView
        spec={parsed}
        tree={tree}
        basePath={basePath}
        callerPage={callerPage}
        callerTeam={callerTeam}
        onNavigate={onNavigate}
      />
    </ErrorBoundary>
  );
}

type ViewState =
  | { phase: "loading" }
  | {
      phase: "ready";
      docs: DocSummary[];
      total: number;
      groups: ContextGroupCount[] | undefined;
      fresh: boolean;
    }
  | { phase: "error"; message: string };

function CsQueryView({
  spec,
  tree,
  basePath,
  callerPage,
  callerTeam,
  onNavigate,
}: {
  spec: CsQuerySpec;
  tree: DerivedPipelineTree | null;
  basePath?: string;
  callerPage?: string;
  callerTeam?: string;
  onNavigate?: (filePath: string) => void;
}) {
  // A `scope:` sub-epic name (or abs path) resolves to its folder, which bounds
  // the query (and the connectivity fan).
  // A sub-epic name that names no folder is an error, never a silent path
  // prefix; an absolute path is used as written. Until the tree arrives a named
  // scope can't be resolved, so the block waits.
  const scopeInfo = useMemo(() => {
    const raw = spec.scope;
    if (!raw)
      return {
        root: null as string | null,
        label: null as string | null,
        found: false,
        pending: false,
        unknown: false,
      };
    const node = tree ? findSubtreeByScope(tree, raw) : null;
    const absolute = raw.startsWith("/");
    return {
      root: node?.absoluteDirPath ?? raw,
      label: raw,
      found: node != null,
      pending: !tree && !absolute,
      unknown: !!tree && !node && !absolute,
    };
  }, [spec.scope, tree]);

  // Status sorts and board lanes follow the team's own status order (SW-GT-20).
  const states = useStatusTaxonomyStore((s) => s.states);
  const statusOrder = useMemo(() => states.map((s) => s.id), [states]);
  const query = useMemo(
    () => blockQuery(spec, { basePath, scopeRoot: scopeInfo.root, statusOrder }),
    [spec, basePath, scopeInfo.root, statusOrder],
  );
  // Re-subscribe only when the effective query actually changes.
  const queryKey = useMemo(() => JSON.stringify(query), [query]);
  const [state, setState] = useState<ViewState>({ phase: "loading" });

  const blocked = scopeInfo.pending || scopeInfo.unknown;
  useEffect(() => {
    if (blocked) return;
    return subscribeContext(
      query,
      (update) => {
        if (update.ok) {
          setState({
            phase: "ready",
            docs: update.result.rows.map(rowToDocSummary),
            total: update.result.total,
            groups: update.result.groups,
            fresh: true,
          });
        } else {
          // Keep the last rows on a failed refresh, cued stale; nothing to show → error.
          setState((prev) =>
            prev.phase === "ready"
              ? { ...prev, fresh: false }
              : { phase: "error", message: update.error },
          );
        }
      },
      blockPage(basePath),
      callerPage,
      callerTeam,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps -- queryKey is the stable trigger; `query` identity is derived from it
  }, [queryKey, blocked, basePath, callerPage, callerTeam]);

  if (scopeInfo.unknown) {
    return (
      <CsQueryErrorCard
        message="This csquery block's scope names no folder"
        detail={`No sub-epic called "${scopeInfo.label}" here. Use a folder name from this epic, or an absolute path.`}
      />
    );
  }

  if (state.phase === "error") {
    return (
      <CsQueryErrorCard
        message="This csquery block couldn't run its query"
        detail={state.message}
      />
    );
  }

  // Connectivity (SW-DX-8) renders the FULL in-scope set via the shared builder.
  if (spec.view === "connectivity") {
    return (
      <div className="my-4 rounded-md border bg-card/40 text-sm">
        <div className="flex items-center justify-end border-b px-3 py-1.5">
          <FreshBadge fresh={state.phase === "ready" && state.fresh} />
        </div>
        {state.phase === "loading" ? (
          <p className="px-3 py-4 text-xs text-muted-foreground">Loading…</p>
        ) : (
          <ConnectivityView
            docs={state.docs}
            scopeRoot={scopeInfo.root}
            scopeLabel={scopeInfo.label}
            scopeFound={scopeInfo.found}
            onNavigate={onNavigate}
          />
        )}
      </div>
    );
  }

  const rows = state.phase === "ready" ? state.docs : [];
  return (
    <div className="my-4 rounded-md border bg-card/40 text-sm">
      <Header
        fresh={state.phase === "ready" && state.fresh}
        count={rows.length}
        total={state.phase === "ready" ? state.total : 0}
        limit={spec.limit}
      />
      {state.phase === "loading" ? (
        <p className="px-3 py-4 text-xs text-muted-foreground">Loading…</p>
      ) : rows.length === 0 ? (
        <EmptyState spec={spec} />
      ) : (
        <Body spec={spec} rows={rows} groups={state.groups} onNavigate={onNavigate} />
      )}
    </div>
  );
}

/** The live/stale connection badge (AC-3), shared by the table Header and the
 *  connectivity view. */
function FreshBadge({ fresh }: { fresh: boolean }) {
  return (
    <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
      <span
        aria-hidden
        className={
          "inline-block h-1.5 w-1.5 rounded-full " + (fresh ? "bg-green-500" : "bg-amber-500")
        }
      />
      {fresh ? "live" : "stale"}
    </span>
  );
}

/** Header with the live/stale badge (AC-3) and a truncation hint when capped. */
function Header({
  fresh,
  count,
  total,
  limit,
}: {
  fresh: boolean;
  count: number;
  total: number;
  limit: number;
}) {
  const truncated = total > limit;
  return (
    <div className="flex items-center justify-between border-b px-3 py-1.5">
      <span className="text-xs text-muted-foreground">
        {count} {count === 1 ? "doc" : "docs"}
        {truncated && (
          <span className="ml-1">
            (of {total}, capped at {limit})
          </span>
        )}
      </span>
      <FreshBadge fresh={fresh} />
    </div>
  );
}

/** The quiet empty placeholder with the effective filter summarized (AC-7). */
function EmptyState({ spec }: { spec: CsQuerySpec }) {
  return (
    <p className="px-3 py-4 text-xs text-muted-foreground">
      No matching docs
      <span className="ml-1 font-mono opacity-70">({summarizeFilter(spec)})</span>
    </p>
  );
}

function Body({
  spec,
  rows,
  groups,
  onNavigate,
}: {
  spec: CsQuerySpec;
  rows: DocSummary[];
  groups: ContextGroupCount[] | undefined;
  onNavigate?: (filePath: string) => void;
}) {
  const nav = (path: string) => onNavigate?.(path);
  if (spec.view === "list")
    return <ListView rows={rows} group={spec.group} groups={groups} nav={nav} />;
  if (spec.view === "board")
    return <BoardView rows={rows} group={spec.group ?? "status"} groups={groups} nav={nav} />;
  return <TableView rows={rows} columns={spec.columns} nav={nav} />;
}

/* ------------------------------ view shapes ------------------------------ */

function TableView({
  rows,
  columns,
  nav,
}: {
  rows: DocSummary[];
  columns: string[];
  nav: (path: string) => void;
}) {
  return (
    <table className="w-full border-collapse text-sm">
      <thead>
        <tr className="text-left text-xs text-muted-foreground">
          {columns.map((c) => (
            <th key={c} className="px-3 py-1.5 font-medium capitalize">
              {c}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {rows.map((doc) => (
          <tr
            key={doc.path}
            className="cursor-pointer border-t hover:bg-muted/40"
            onClick={() => nav(doc.path)}
          >
            {columns.map((c) => (
              <td key={c} className="px-3 py-1.5 align-top">
                {c === "name" || c.toLowerCase() === "name" ? (
                  <span className="text-primary underline-offset-2 hover:underline">
                    {fieldValue(doc, c)}
                  </span>
                ) : (
                  fieldValue(doc, c)
                )}
              </td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function ListView({
  rows,
  group,
  groups,
  nav,
}: {
  rows: DocSummary[];
  group: string | null;
  groups: ContextGroupCount[] | undefined;
  nav: (path: string) => void;
}) {
  if (!group) {
    return (
      <ul className="py-1">
        {rows.map((doc) => (
          <ListItem key={doc.path} doc={doc} nav={nav} />
        ))}
      </ul>
    );
  }
  const lanes = groupRows(rows, group, groups);
  return (
    <div className="py-1">
      {lanes.map(([laneKey, laneRows]) => (
        <div key={laneKey} className="mb-1">
          <div className="px-3 py-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
            {laneKey}
          </div>
          <ul>
            {laneRows.map((doc) => (
              <ListItem key={doc.path} doc={doc} nav={nav} />
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}

function ListItem({ doc, nav }: { doc: DocSummary; nav: (path: string) => void }) {
  return (
    <li className="cursor-pointer px-3 py-1 hover:bg-muted/40" onClick={() => nav(doc.path)}>
      <span className="text-primary underline-offset-2 hover:underline">{doc.fileName}</span>
      {doc.status && <span className="ml-2 text-xs text-muted-foreground">{doc.status}</span>}
    </li>
  );
}

function BoardView({
  rows,
  group,
  groups,
  nav,
}: {
  rows: DocSummary[];
  group: string;
  groups: ContextGroupCount[] | undefined;
  nav: (path: string) => void;
}) {
  const lanes = groupRows(rows, group, groups);
  // Lane counts are the engine's (every match, not just the rows shown).
  const countOf = (lane: string, shown: number) =>
    groups?.find((g) => (g.value ?? "—") === lane)?.count ?? shown;
  return (
    <div className="flex gap-3 overflow-x-auto p-3">
      {lanes.map(([laneKey, laneRows]) => (
        <div key={laneKey} className="min-w-[10rem] flex-1">
          <div className="mb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
            {laneKey} <span className="opacity-60">({countOf(laneKey, laneRows.length)})</span>
          </div>
          <div className="flex flex-col gap-1">
            {laneRows.map((doc) => (
              <button
                key={doc.path}
                type="button"
                onClick={() => nav(doc.path)}
                className="rounded border bg-background px-2 py-1 text-left text-xs hover:bg-muted/40"
              >
                {doc.fileName}
              </button>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

/* ------------------------------- helpers -------------------------------- */

/** Resolve a column / group field name to its value on a doc. */
function fieldValue(doc: DocSummary, field: string): string {
  switch (field.trim().toLowerCase()) {
    case "name":
    case "filename":
    case "file":
      return doc.fileName;
    case "status":
      return doc.status ?? "—";
    case "id":
    case "storyid":
    case "story_id":
      return doc.id;
    case "type":
    case "documenttype":
    case "document_type":
      return doc.documentType ?? "—";
    case "path":
      return doc.path;
    default: {
      // Match a doc's open tag by NORMALIZED key (lowercase, hyphens→underscores)
      // — as the query engine matches keys — so `columns: Team` or
      // `group: doc-type` resolve the tag they also filter on.
      const norm = field.trim().toLowerCase().replace(/-/g, "_");
      let vals = doc.tags[field];
      if (!vals) {
        for (const k of Object.keys(doc.tags)) {
          if (k.toLowerCase().replace(/-/g, "_") === norm) {
            vals = doc.tags[k];
            break;
          }
        }
      }
      return vals && vals.length > 0 ? vals.join(", ") : "—";
    }
  }
}

/** Group the shown rows by a field into lanes, ordered as the engine ordered its
 *  groups (status by workflow order, others by value); unknown lanes last. */
function groupRows(
  rows: DocSummary[],
  field: string,
  groups: ContextGroupCount[] | undefined,
): Array<[string, DocSummary[]]> {
  const lanes = new Map<string, DocSummary[]>();
  for (const doc of rows) {
    const key = fieldValue(doc, field);
    const bucket = lanes.get(key);
    if (bucket) bucket.push(doc);
    else lanes.set(key, [doc]);
  }
  const order = (groups ?? []).map((g) => g.value ?? "—");
  const rank = (k: string) => {
    const i = order.indexOf(k);
    return i < 0 ? order.length : i;
  };
  return Array.from(lanes.entries()).sort(
    (a, b) => rank(a[0]) - rank(b[0]) || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0),
  );
}

/** A short, human summary of the effective query for the empty state. */
function summarizeFilter(spec: CsQuerySpec): string {
  const parts: string[] = [];
  for (const f of spec.query.filters ?? []) {
    const v = Array.isArray(f.value) ? f.value.join("|") : String(f.value ?? "");
    parts.push(f.op === "eq" || f.op === "in" ? `${f.key}=${v}` : `${f.key} ${f.op} ${v}`);
  }
  const rel = spec.query.relationships;
  if (rel?.parentEpic) parts.push(`epic: ${rel.parentEpic}`);
  if (rel?.dependsOn) parts.push(`depends: ${rel.dependsOn}`);
  if (rel?.hasSupportingDoc) parts.push(`related: ${rel.hasSupportingDoc}`);
  if (rel?.backlinkedBy) parts.push(`backlink: ${rel.backlinkedBy}`);
  if (spec.query.text) parts.push(`text: ${spec.query.text}`);
  return parts.length > 0 ? parts.join(", ") : "no filter";
}
