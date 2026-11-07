// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useRef, useState } from "react";

import { Search, X } from "lucide-react";

import { openAgentResult } from "@/features/viewer/components/agent-result-panel";
import { useNowClock } from "@/features/viewer/hooks/use-now-clock";
import { matchesAgentSearch } from "@/features/viewer/lib/agent-list-view";
import { useRunOutcomes } from "@/features/viewer/hooks/use-run-outcomes";
import { runKind, runLabel, selectRecentRuns } from "@/features/viewer/lib/recent-runs-model";
import { AGENT_STATUS_WORDS } from "@/shared/lib/agent-status-config";
import { markOfAgentStatus } from "@/shared/lib/status-mark";
import { StatusMark } from "@/shared/components/status-mark";
import { formatAgo } from "@/shared/lib/format-updated-ago";
import { isTerminalManagedState } from "@/shared/lib/managed-session";
import { validatedOutcomeChip, type RunOutcomeRead } from "@/shared/lib/run-outcome";
import { useAgentResultStore } from "@/shared/stores/use-agent-result-store";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";

/** A run's durable outcome lands just after its session settles, so a refetch fired on the
 *  settle alone can miss it. */
const SETTLE_REFETCH_DELAY_MS = 750;

/**
 * Finished runs that left output, newest first. Fetches only while `active` (the Runs view is
 * showing), and refetches shortly after any session settles so a just-finished run appears.
 */
export function useRunHistory(active: boolean): RunOutcomeRead[] | null {
  const { outcomes, reload } = useRunOutcomes(active);
  // Keyed on WHICH sessions have settled, not how many, so one ending while another is dropped
  // from the store still refetches. A string keeps the selector's result stable between renders.
  const settled = useManagedSessionStore((s) =>
    Object.entries(s.states)
      .filter(([, state]) => isTerminalManagedState(state))
      .map(([id]) => id)
      .sort()
      .join("\n"),
  );
  const lastSettled = useRef(settled);
  useEffect(() => {
    if (!active || settled === lastSettled.current) return;
    lastSettled.current = settled;
    const timer = setTimeout(() => void reload(), SETTLE_REFETCH_DELAY_MS);
    return () => clearTimeout(timer);
  }, [active, settled, reload]);
  return useMemo(() => (outcomes ? selectRecentRuns(outcomes) : null), [outcomes]);
}

/**
 * The Agents pane's run history: finished runs with output, newest first, each with its
 * outcome, kind and when it ended. Clicking one opens its output in the agent area.
 */
export function RunsView({
  teamKey,
  runs,
}: {
  teamKey: string;
  runs: readonly RunOutcomeRead[] | null;
}): React.JSX.Element {
  const [query, setQuery] = useState("");
  // Only the team whose agent area is showing the output marks its row.
  const openRunId = useAgentResultStore((s) => (s.teamKey === teamKey ? s.runId : null));
  const nowMs = useNowClock(30_000, (runs?.length ?? 0) > 0);
  const shown = useMemo(
    () => (runs ?? []).filter((read) => matchesAgentSearch(runSearchText(read), query)),
    [runs, query],
  );

  return (
    <section aria-label="Run history" className="flex min-h-0 flex-1 flex-col">
      <div className="shrink-0 px-2.5 pb-2">
        <label className="ag-search">
          <Search className="size-3.5 shrink-0" aria-hidden />
          <input
            type="search"
            placeholder="Find runs…"
            aria-label="Find runs"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          {query && (
            <button
              type="button"
              className="ag-search-clear"
              aria-label="Clear search"
              onClick={() => setQuery("")}
            >
              <X className="size-3" />
            </button>
          )}
        </label>
      </div>
      <div className="ag-scroll flex-1 min-h-0 overflow-y-auto pb-3">
        {runs === null ? (
          <p className="px-3.5 py-3 text-[12px] text-[var(--ag-faint)]">Loading runs…</p>
        ) : runs.length === 0 ? (
          <div className="px-3.5 py-3">
            <p className="text-[12px] font-medium text-[var(--ag-muted)]">No finished runs yet</p>
            <p className="mt-1 text-[11px] leading-[15px] text-[var(--ag-faint)]">
              Runs that leave output, from routines and agents, collect here.
            </p>
          </div>
        ) : shown.length === 0 ? (
          <div className="px-3.5 py-3">
            <p className="text-[12px] text-[var(--ag-muted)]">No matching runs</p>
            <button type="button" className="ag-link mt-1" onClick={() => setQuery("")}>
              Clear search
            </button>
          </div>
        ) : (
          <ul>
            {shown.map((read) => (
              <RunRow
                key={read.outcome.run_id}
                read={read}
                nowMs={nowMs}
                open={read.outcome.run_id === openRunId}
              />
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

function runSearchText(read: RunOutcomeRead): string {
  const { outcome } = read;
  return [
    runLabel(outcome.story_id) ?? outcome.run_id,
    outcome.summary,
    runKind(read),
    AGENT_STATUS_WORDS[validatedOutcomeChip(read.validated)],
    outcome.workspace,
  ]
    .filter(Boolean)
    .join(" ");
}

function RunRow({
  read,
  nowMs,
  open,
}: {
  read: RunOutcomeRead;
  nowMs: number;
  open: boolean;
}): React.JSX.Element {
  const { outcome } = read;
  const status = validatedOutcomeChip(read.validated);
  const words = AGENT_STATUS_WORDS[status];
  const label = runLabel(outcome.story_id) ?? outcome.run_id;
  const endedMs = Date.parse(outcome.ended_at);
  const detail = [words, runKind(read), Number.isFinite(endedMs) && formatAgo(endedMs, nowMs)]
    .filter(Boolean)
    .join(" · ");
  return (
    <li>
      <div className="ag-row" data-current={open || undefined}>
        <button
          type="button"
          aria-current={open || undefined}
          className="ag-main flex min-w-0 flex-1 items-start gap-2.5 py-[6px] pl-3 pr-2 text-left"
          title={outcome.summary ?? label}
          onClick={() => openAgentResult({ run_id: outcome.run_id, story_id: outcome.story_id })}
        >
          <StatusMark mark={markOfAgentStatus(status)} label={words} className="mt-[5px]" />
          <span className="flex min-w-0 flex-1 flex-col">
            <span className="ag-title truncate">
              <span className="ag-name">{label}</span>
              {outcome.summary && <span className="ag-task"> · {outcome.summary}</span>}
            </span>
            <span className="ag-sub truncate">{detail}</span>
          </span>
        </button>
      </div>
    </li>
  );
}
