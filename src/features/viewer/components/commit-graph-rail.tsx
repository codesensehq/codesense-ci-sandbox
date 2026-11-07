// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef } from "react";

import { cn } from "@/lib/utils";
import { VirtualList, type VirtualListHandle } from "@/shared/components/virtual-list";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useAsyncGraphLayout } from "@/shared/lib/commit-graph/use-async-graph-layout";
import type { CommitGraphRow, CommitLaneInput } from "@/shared/lib/commit-graph/layout";
import { commitRowDraw } from "@/shared/lib/commit-graph/row-geometry";
import type { RefLabel, SourceCommitGraphEntry } from "@/features/git-sync/lib/types";

import { useSourceCommitGraph } from "../lib/use-source-diff";

const ROW_H = 46;
const LANE_W = 14;
const NODE_R = 4;

/** The pinned Working-tree node (index 0), the commits, then a load-more sentinel. */
type RailItem =
  | { kind: "working" }
  | { kind: "commit"; entry: SourceCommitGraphEntry; row: CommitGraphRow | null; index: number }
  | { kind: "sentinel" };

/** A linear fallback row (single lane) — used per-row while the async lane layout
 *  is still computing, or if it failed (AC-10: never a blank/hung rail). */
function linearRow(sha: string, hasParent: boolean): CommitGraphRow {
  return { sha, lane: 0, parentLanes: hasParent ? [0] : [], incoming: [sha] };
}

function RefPill({ label }: { label: RefLabel }) {
  if (label.kind === "head") {
    return (
      <span className="rounded-sm bg-[var(--primary)] px-1 text-[0.6rem] font-semibold uppercase leading-4 text-[var(--primary-foreground)]">
        HEAD
      </span>
    );
  }
  const tone =
    label.kind === "remote"
      ? "text-[var(--node-text-muted)] border-[var(--border)]"
      : label.kind === "tag"
        ? "text-[var(--warning,#b58900)] border-[var(--border)]"
        : "text-[var(--foreground)] border-[var(--border)]";
  return (
    <span
      className={cn("truncate rounded-sm border px-1 text-[0.6rem] leading-4", tone)}
      title={label.name}
    >
      {label.name}
    </span>
  );
}

/** One commit row: the per-row SVG lanes + node dot, then sha / subject / refs. */
function CommitRow({
  entry,
  row,
  laneCount,
  isHead,
  selected,
  onSelect,
}: {
  entry: SourceCommitGraphEntry;
  row: CommitGraphRow;
  laneCount: number;
  isHead: boolean;
  selected: boolean;
  onSelect: () => void;
}) {
  const draw = commitRowDraw(row);
  const width = Math.max(1, laneCount) * LANE_W;
  const cx = (lane: number) => (lane + 0.5) * LANE_W;
  const cy = (y: number) => y * ROW_H;
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      data-testid={`commit-row-${entry.sha}`}
      className={cn(
        "flex h-full w-full items-stretch gap-2 rounded px-1 text-left transition-colors",
        selected ? "bg-[var(--muted)]" : "hover:bg-[var(--muted)]/50",
      )}
    >
      <svg
        width={width}
        height={ROW_H}
        className="shrink-0"
        aria-hidden
        style={{ overflow: "visible" }}
      >
        {draw.segments.map((s, i) => (
          <line
            key={i}
            x1={cx(s.x1)}
            y1={cy(s.y1)}
            x2={cx(s.x2)}
            y2={cy(s.y2)}
            stroke="var(--border)"
            strokeWidth={1.5}
          />
        ))}
        <circle
          cx={cx(draw.nodeLane)}
          cy={cy(0.5)}
          r={NODE_R}
          fill={isHead ? "var(--primary)" : "var(--foreground)"}
          stroke={isHead ? "var(--primary)" : "var(--background)"}
          strokeWidth={isHead ? 2 : 1}
        />
      </svg>
      {/* One line so the row height stays a fixed ROW_H — the per-row SVG lane
          geometry (`cy = y * ROW_H`) assumes it, and variable height would clip
          the content + misalign the node dot on ref-decorated rows (review r1). */}
      <span className="flex min-w-0 items-center gap-1.5">
        <code className="shrink-0 text-[0.65rem] text-[var(--node-text-muted)]">
          {entry.shortSha}
        </code>
        {entry.refs.map((r) => (
          <RefPill key={`${r.kind}:${r.name}`} label={r} />
        ))}
        <span className="truncate text-xs text-[var(--foreground)]" title={entry.subject}>
          {entry.subject}
        </span>
      </span>
    </button>
  );
}

/**
 * SLW-WD-9 — the commit-graph History rail. A virtualized list of a pinned
 * Working-tree node + the all-refs commit graph (per-row SVG lanes, ref labels, a
 * HEAD marker), served by `useSourceCommitGraph` (paged + cached + nonce-live) and
 * laid out off-thread by `useAsyncGraphLayout`. Selection is by sha in the store
 * so the pane the overlay renders survives paging / virtualization (RD-11).
 */
export function CommitGraphRail({
  ws,
  worktreeNonce,
  activeTabSessionId,
}: {
  ws: { id: string; cwd: string };
  worktreeNonce: number;
  /** SW-TP CAP-3: the active tab's sessionId — selected-sha state is per-tab now. */
  activeTabSessionId: string | undefined;
}) {
  const { entries, loading, loadingMore, error, hasMore, loadMore } = useSourceCommitGraph(
    ws.id,
    ws.cwd,
    worktreeNonce,
    true,
  );

  const laneInput = useMemo<CommitLaneInput[]>(
    () => entries.map((e) => ({ sha: e.sha, parents: e.parents })),
    [entries],
  );
  const { layout, layoutFailed } = useAsyncGraphLayout(laneInput);

  const selectedSha = useViewerStore((s) =>
    activeTabSessionId ? (s.sourceDiffSelectedSha[activeTabSessionId] ?? null) : null,
  );
  const setSelectedSha = useViewerStore((s) => s.setSourceDiffSelectedSha);

  const headIndex = useMemo(
    () => entries.findIndex((e) => e.refs.some((r) => r.kind === "head")),
    [entries],
  );

  const listRef = useRef<VirtualListHandle>(null);
  // RD-7: jump to HEAD ONCE on open (after the first layout settles), not on every
  // re-layout / page append — those must not yank the user's scroll.
  const jumpedRef = useRef(false);
  useEffect(() => {
    if (jumpedRef.current || loading || entries.length === 0) return;
    // Only LATCH once we actually jumped — if HEAD isn't in the loaded pages yet
    // (HEAD on a non-tip branch), leave it un-latched so a later page that brings
    // HEAD in still recenters (review r1). +1 for the pinned Working node at row 0.
    if (headIndex >= 0) {
      listRef.current?.scrollToIndex(headIndex + 1, { align: "center" });
      jumpedRef.current = true;
    }
  }, [loading, entries.length, headIndex]);

  const items = useMemo<RailItem[]>(() => {
    const rows: RailItem[] = [{ kind: "working" }];
    entries.forEach((entry, index) =>
      rows.push({ kind: "commit", entry, row: layout.rows[index] ?? null, index }),
    );
    if (hasMore) rows.push({ kind: "sentinel" });
    return rows;
  }, [entries, layout, hasMore]);

  const laneCount = Math.max(1, layout.width);

  const renderItem = useCallback(
    (item: RailItem) => {
      if (item.kind === "working") {
        return (
          <button
            type="button"
            onClick={() => setSelectedSha(activeTabSessionId ?? "", null)}
            aria-pressed={selectedSha === null}
            data-testid="commit-row-working"
            className={cn(
              "flex h-full w-full items-center gap-2 rounded px-1 text-left text-xs transition-colors",
              selectedSha === null ? "bg-[var(--muted)]" : "hover:bg-[var(--muted)]/50",
            )}
          >
            <span className="shrink-0" style={{ width: LANE_W }} aria-hidden>
              <svg width={LANE_W} height={ROW_H}>
                <circle
                  cx={LANE_W / 2}
                  cy={ROW_H / 2}
                  r={NODE_R}
                  fill="none"
                  stroke="var(--foreground)"
                  strokeWidth={1.5}
                  strokeDasharray="2 2"
                />
              </svg>
            </span>
            <span className="font-medium text-[var(--foreground)]">Working tree</span>
          </button>
        );
      }
      if (item.kind === "sentinel") {
        return <LoadMoreSentinel onReach={loadMore} count={entries.length} loading={loadingMore} />;
      }
      const row = item.row ?? linearRow(item.entry.sha, item.entry.parents.length > 0);
      return (
        <CommitRow
          entry={item.entry}
          row={row}
          laneCount={item.row ? laneCount : 1}
          isHead={item.index === headIndex}
          selected={selectedSha === item.entry.sha}
          onSelect={() => setSelectedSha(activeTabSessionId ?? "", item.entry.sha)}
        />
      );
    },
    [
      activeTabSessionId,
      selectedSha,
      setSelectedSha,
      laneCount,
      headIndex,
      loadMore,
      loadingMore,
      entries.length,
    ],
  );

  if (loading) {
    return (
      <div className="space-y-1.5 p-1" aria-hidden data-testid="commit-graph-skeleton">
        {[0, 1, 2, 3].map((i) => (
          <div key={i} className="h-8 animate-pulse rounded bg-[var(--muted)]/50" />
        ))}
      </div>
    );
  }

  // Only the COLD (first-page) error blanks the rail. A load-older (page-N) failure
  // must NOT discard the graph already on screen (AC-10 "never a blank rail",
  // review r1): entries are present, so keep them — the failed page just stops
  // appending (the sentinel's key is stable on error, so it won't retry-loop).
  if (error && entries.length === 0) {
    return (
      <p className="px-1 py-1 text-xs text-[var(--destructive)]" data-testid="commit-graph-error">
        Couldn’t load the commit graph.
      </p>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {layoutFailed && (
        <p className="px-1 pb-1 text-[0.65rem] text-[var(--node-text-muted)]">
          Showing a linear view.
        </p>
      )}
      <VirtualList<RailItem>
        ref={listRef}
        items={items}
        estimateSize={ROW_H}
        overscan={8}
        getKey={(item) =>
          item.kind === "commit"
            ? item.entry.sha
            : item.kind === "sentinel"
              ? `sentinel-${entries.length}`
              : "working"
        }
        className="min-h-0 flex-1 overflow-y-auto"
        data-testid={`commit-graph-rail-${ws.id}`}
        renderItem={renderItem}
      />
    </div>
  );
}

/** The load-older trigger: mounts when scrolled near the bottom (virtualized), so
 *  its mount effect appends the next page. Keyed by `count` so it re-mounts after
 *  each append and can fire again while still in view (AC-7). */
function LoadMoreSentinel({
  onReach,
  loading,
}: {
  onReach: () => void;
  count: number;
  loading: boolean;
}) {
  useEffect(() => {
    onReach();
    // Fire once per mount; the parent re-keys this by `count` so a new page
    // remounts it. `onReach` itself guards hasMore/in-flight.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <div className="flex h-full items-center px-2 text-[0.65rem] text-[var(--node-text-muted)]">
      {loading ? "Loading older commits…" : "…"}
    </div>
  );
}
