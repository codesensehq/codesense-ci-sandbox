// SPDX-License-Identifier: MIT
/**
 * SW-DX-8 — the inline connectivity surface (AC-8/12). A thin, read-only shell
 * over the pure `buildConnectivityView`: for each in-scope story it renders a
 * health badge, its context chips grouped by type, and forward/backlink counts;
 * flags the context-desert ("will be built blind") rows; and lists orphan
 * islands. Clicking a story or chip opens the file via the viewer's `onNavigate`.
 *
 * `buildConnectivityView` is memoized on `[docs, scopeRoot, thinThreshold]`
 * (AC-10) so unrelated store churn does not re-run the fan.
 */

import { useMemo, useState } from "react";
import type { DocSummary } from "@/shared/lib/doc-query/types";
import { buildConnectivityView } from "@/shared/lib/connectivity/build-connectivity-view";
import type { ContextLink, StoryConnectivity } from "@/shared/lib/connectivity/types";
import { DOC_TYPE_ICON } from "@/shared/lib/pipeline-graph/doc-type-icon";
import { cn } from "@/lib/utils";
import { HEALTH_CONFIG } from "./health-config";

type ConnectivityViewProps = {
  /** In-scope query result (scope-bounded upstream by the island's filter). */
  docs: DocSummary[];
  /** Resolved absolute scope root, or `null` when no `scope:` was authored. */
  scopeRoot: string | null;
  /** The raw `scope:` value, for display in messages. */
  scopeLabel: string | null;
  /** Whether the scope name resolved to a real sub-epic node. */
  scopeFound: boolean;
  thinThreshold?: number;
  onNavigate?: (filePath: string) => void;
};

export function ConnectivityView({
  docs,
  scopeRoot,
  scopeLabel,
  scopeFound,
  thinThreshold,
  onNavigate,
}: ConnectivityViewProps) {
  const [matrix, setMatrix] = useState(false);

  const view = useMemo(
    () => (scopeRoot ? buildConnectivityView(docs, scopeRoot, { thinThreshold }) : null),
    [docs, scopeRoot, thinThreshold],
  );

  if (!scopeRoot || !view) {
    return <Notice>This connectivity view needs a `scope:` (a sub-epic name).</Notice>;
  }

  if (view.rows.length === 0) {
    return scopeFound ? (
      <Notice>No stories in this scope.</Notice>
    ) : (
      <Notice>Scope not found: {scopeLabel}</Notice>
    );
  }

  const allDesert = view.summary.contextDesertCount === view.summary.storyCount;

  return (
    <div className="text-sm">
      <div className="flex items-center justify-between gap-2 px-3 py-1.5 text-xs text-muted-foreground">
        <span>
          {view.summary.storyCount} {view.summary.storyCount === 1 ? "story" : "stories"} ·{" "}
          <span className="text-red-600 dark:text-red-400">
            {view.summary.contextDesertCount} blind
          </span>{" "}
          · {view.summary.thinCount} thin · {view.summary.healthyCount} healthy
        </span>
        <button
          type="button"
          onClick={() => setMatrix((m) => !m)}
          className="rounded border px-1.5 py-0.5 hover:bg-muted/40"
        >
          {matrix ? "List" : "Matrix"}
        </button>
      </div>

      {allDesert && (
        <div className="mx-3 mb-2 rounded border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-600 dark:text-red-400">
          This sub-epic has no context wired up yet.
        </div>
      )}

      {matrix ? (
        <MatrixView rows={view.rows} onNavigate={onNavigate} />
      ) : (
        <ul className="divide-y">
          {view.rows.map((row) => (
            <StoryRow key={row.story.path} row={row} onNavigate={onNavigate} />
          ))}
        </ul>
      )}

      {view.orphans.length > 0 && (
        <div className="border-t px-3 py-2">
          <div className="mb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Orphan docs ({view.orphans.length})
          </div>
          <div className="flex flex-wrap gap-1.5">
            {view.orphans.map((o) => (
              <Chip
                key={o.filePath}
                groupKey={o.groupKey}
                fileName={o.fileName}
                resolved
                onClick={() => onNavigate?.(o.filePath)}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function StoryRow({
  row,
  onNavigate,
}: {
  row: StoryConnectivity;
  onNavigate?: (filePath: string) => void;
}) {
  const health = HEALTH_CONFIG[row.health];
  const HealthIcon = health.icon;
  const chips = [...row.forward, ...row.backlinks];
  return (
    <li className={cn("px-3 py-2", health.rowTintClass)}>
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => onNavigate?.(row.story.path)}
          className="truncate text-left text-primary underline-offset-2 hover:underline"
        >
          {row.story.fileName}
        </button>
        {row.story.status && (
          <span className="text-xs text-muted-foreground">{row.story.status}</span>
        )}
        <span
          className={cn(
            "ml-auto flex shrink-0 items-center gap-1 rounded border px-1.5 py-0.5 text-[11px]",
            health.badgeClass,
          )}
          title={`${row.forwardCount} forward · ${row.backlinkCount} backlinks`}
        >
          <HealthIcon size={11} />
          {health.label}
        </span>
      </div>
      {chips.length > 0 && (
        <div className="mt-1 flex flex-wrap gap-1.5 pl-1">
          {chips.map((link) => (
            <Chip
              key={`${link.direction}:${link.filePath}`}
              groupKey={link.groupKey}
              fileName={link.fileName}
              resolved={link.resolved}
              direction={link.direction}
              onClick={() => onNavigate?.(link.filePath)}
            />
          ))}
        </div>
      )}
    </li>
  );
}

function Chip({
  groupKey,
  fileName,
  resolved,
  direction,
  onClick,
}: {
  groupKey: string;
  fileName: string;
  resolved: boolean;
  direction?: ContextLink["direction"];
  onClick: () => void;
}) {
  const config = DOC_TYPE_ICON[groupKey] ?? DOC_TYPE_ICON.doc;
  const Icon = config.icon;
  return (
    <button
      type="button"
      onClick={onClick}
      title={
        resolved
          ? direction === "backlink"
            ? `${fileName} (backlink)`
            : fileName
          : `${fileName} — unresolved (link target not found)`
      }
      className={cn(
        "flex max-w-[14rem] items-center gap-1 rounded border px-1.5 py-0.5 text-xs hover:bg-muted/40",
        !resolved && "opacity-50 line-through",
        direction === "backlink" && "border-dashed",
      )}
    >
      <Icon size={11} style={{ color: config.color }} className="shrink-0" />
      <span className="truncate">{fileName}</span>
    </button>
  );
}

/** A compact story×type matrix (rows = stories, columns = types, cell = count). */
function MatrixView({
  rows,
  onNavigate,
}: {
  rows: StoryConnectivity[];
  onNavigate?: (filePath: string) => void;
}) {
  const types = useMemo(() => {
    const set = new Set<string>();
    for (const row of rows) for (const k of Object.keys(row.groups)) set.add(k);
    return [...set].sort();
  }, [rows]);

  return (
    <table className="w-full border-collapse text-xs">
      <thead>
        <tr className="text-left text-muted-foreground">
          <th className="px-3 py-1 font-medium">Story</th>
          {types.map((t) => (
            <th key={t} className="px-2 py-1 text-center font-medium capitalize">
              {DOC_TYPE_ICON[t]?.label ?? t}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.story.path} className="border-t hover:bg-muted/40">
            <td className="px-3 py-1">
              <button
                type="button"
                onClick={() => onNavigate?.(row.story.path)}
                className="text-primary underline-offset-2 hover:underline"
              >
                {row.story.fileName}
              </button>
            </td>
            {types.map((t) => (
              <td key={t} className="px-2 py-1 text-center tabular-nums">
                {row.groups[t]?.length ?? <span className="text-muted-foreground/40">·</span>}
              </td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function Notice({ children }: { children: React.ReactNode }) {
  return <p className="px-3 py-4 text-xs text-muted-foreground">{children}</p>;
}
