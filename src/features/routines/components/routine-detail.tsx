// SPDX-License-Identifier: MIT
import { RoutineSpendCap } from "@/features/agents/components/spend-cap";
import { Loader2, Play, Square, X } from "lucide-react";

import { AppButton as Button } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";
import { openAgentResult } from "@/features/viewer/components/agent-result-panel";
import type { RoutineRow, RoutineRun } from "@/tauri/commands";

import { UnresolvableAgentChip } from "./routine-agent-chip";
import {
  agentName,
  commandRunLine,
  phaseLabel,
  reasonCopy,
  runsOn,
  scheduleLabel,
  timeAgo,
  whereLabel,
} from "../lib/routine-display";
import {
  canRetry,
  queuedCopy,
  runLine,
  runMachine,
  runStatus,
  type RunContext,
} from "../lib/run-status";
import { clockTime } from "@/features/agents/lib/agent-routines";
import { openLaunchSheet } from "@/features/agents/lib/launch-sheet";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { machineLabel, waitWords } from "../lib/use-routine-machines";
import { ArgvChips } from "./routine-command-fields";
import { needsUpstream, upstreamRunReason } from "../lib/routine-sentence";
import { CommandRunDetails, RunDetails, RunStatusWord } from "./run-status";
import { useRoutinesStore, type RoutineView } from "../stores/routines-store";
import { RoutineCreateForm, RoutineOverview } from "./routine-editor";
import { RoutineActions } from "./routine-actions";
import { showRoutine } from "@/features/agents/components/agents-tab-nav";
import { RoutineHistory } from "./routine-history";

const VIEWS: { id: RoutineView; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "runs", label: "Runs" },
  { id: "history", label: "History" },
];

/**
 * The selected routine: its sentence (editable by anyone), every run it
 * recorded including the fires that did not run, and its revision history. With
 * nothing selected and New routine open, the create form.
 */
export function RoutineDetailPane({ now }: { now: number }) {
  const creating = useRoutinesStore((s) => s.creating);
  const createSeq = useRoutinesStore((s) => s.createSeq);
  const selectedKey = useRoutinesStore((s) => s.selectedKey);
  const row = useRoutinesStore((s) => s.rows?.find((r) => r.key === s.selectedKey) ?? null);
  const detail = useRoutinesStore((s) => (s.selectedKey ? s.details[s.selectedKey] : undefined));
  const detailError = useRoutinesStore((s) =>
    s.selectedKey ? s.detailError[s.selectedKey] : undefined,
  );
  const view = useRoutinesStore((s) => s.view);
  const setView = useRoutinesStore((s) => s.setView);
  if (creating)
    return (
      <div className="p-4">
        <RoutineCreateForm key={createSeq} />
      </div>
    );
  if (!selectedKey || !row) {
    return <p className="p-4 text-sm text-muted-foreground">Select a routine to see its runs.</p>;
  }
  return (
    <div data-testid="routine-detail" className="flex flex-col gap-4 p-4">
      <header>
        <div className="flex items-start justify-between gap-3">
          <h2 className="text-base font-semibold">{row.def.name}</h2>
          {row.def.action === "command" && <RoutineActions row={row} />}
        </div>
        <p data-testid="routine-sentence" className="mt-1 text-sm text-muted-foreground">
          {row.def.action === "command" ? (
            <>
              {scheduleLabel(row)}, runs <ArgvChips argv={row.def.command ?? []} /> in{" "}
              {whereLabel(row.def.where.kind)}.
            </>
          ) : (
            sentence(row)
          )}
        </p>
        <UnresolvableAgentChip row={row} />
        <p data-testid="routine-mode-line" className="mt-1 text-xs text-muted-foreground">
          {row.def.action === "command"
            ? "Each run starts the command. No agent, no model."
            : row.effective_mode === "wake"
              ? "Each run wakes the agent's live session."
              : "Each run starts a fresh session of the agent."}
        </p>
        <Downstream row={row} />
        {row.duplicate && (
          <p data-testid="routine-duplicate" className="mt-1 text-xs text-warning-ink">
            Another routine already has the id {row.def.id}, so this one never runs. Rename or
            remove one of the two folders.
          </p>
        )}
        {row.def.migrated?.note && (
          <p data-testid="routine-migration-note" className="mt-1 text-xs text-muted-foreground">
            {row.def.migrated.note}
          </p>
        )}
      </header>
      <div role="tablist" aria-label="Routine views" className="flex gap-1 border-b border-border">
        {VIEWS.map(({ id, label }) => (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={view === id}
            onClick={() => setView(id)}
            className={cn(
              "-mb-px border-b-2 px-3 py-1.5 text-sm",
              view === id
                ? "border-foreground font-medium text-foreground"
                : "border-transparent text-muted-foreground hover:text-foreground",
            )}
          >
            {label}
          </button>
        ))}
      </div>
      {/* Kept mounted while another view shows, so an unsaved edit survives a
          look at Runs or History. */}
      <div hidden={view !== "overview"}>
        <RoutineSpendCap row={row} />
        <RoutineOverview key={row.key} row={row} />
      </div>
      {view === "runs" && <RunsView row={row} now={now} labels={new Map()} />}
      {view === "history" && detailError && (
        <p className="text-sm text-destructive">Couldn't load the history: {detailError}</p>
      )}
      {view === "history" && !detail && !detailError && (
        <p className="text-sm text-muted-foreground">Loading…</p>
      )}
      {view === "history" && detail && <RoutineHistory row={row} detail={detail} now={now} />}
    </div>
  );
}

/** The routine in one read-only sentence. */
function sentence(row: RoutineRow): string {
  const runner = agentName(row.agent_display);
  const where = whereLabel(row.def.where.kind);
  const off = runsOn(row.def) === "each_mac" ? " Off on this Mac." : " Paused on this Mac.";
  const state = row.armed_here && !row.enabled ? off : "";
  return `${scheduleLabel(row)}, ${runner} runs in ${where}.${state}`;
}

/** Who and where a routine's run copy names. */
export function runContext(
  row: RoutineRow,
  labels: ReadonlyMap<string, string>,
  machine: string | undefined = row.def.where.machine,
): RunContext {
  return {
    agent: row.agent_display.name,
    machine: machineLabel(machine, labels),
    maxWait: waitWords(row.def.policy.max_wait),
  };
}

export function RunsView({
  row,
  now,
  labels,
  onPickMachine,
}: {
  row: RoutineRow;
  now: number;
  labels: ReadonlyMap<string, string>;
  /** "Pick a machine" on a run whose machine isn't shared: back to the form's slot. */
  onPickMachine?: () => void;
}) {
  const routineKey = row.key;
  const runs = useRoutinesStore((s) => s.runs[routineKey]);
  const error = useRoutinesStore((s) => s.runsError[routineKey]);
  const more = useRoutinesStore((s) => s.nextBefore[routineKey] ?? null);
  const loadingMore = useRoutinesStore((s) => s.pending[`more:${routineKey}`] === true);
  const loadMoreRuns = useRoutinesStore((s) => s.loadMoreRuns);

  return (
    <section aria-label="Runs" data-testid="routine-runs">
      <OpenRun now={now} />
      {error && <p className="text-sm text-destructive">Couldn't load runs: {error}</p>}
      {!runs && !error && <p className="text-sm text-muted-foreground">Loading runs…</p>}
      {runs?.length === 0 && <p className="text-sm text-muted-foreground">No runs yet.</p>}
      {runs && runs.length > 0 && (
        <ul className="flex flex-col divide-y divide-border rounded-md border border-border">
          {runs.map((run) => (
            <RunRow
              key={run.run_id}
              run={run}
              row={row}
              now={now}
              labels={labels}
              onPickMachine={onPickMachine}
            />
          ))}
        </ul>
      )}
      {more && (
        <Button
          className="mt-2"
          size="sm"
          variant="outline"
          disabled={loadingMore}
          onClick={() => void loadMoreRuns(routineKey)}
        >
          Show older runs
        </Button>
      )}
    </section>
  );
}

function RunRow({
  run,
  row,
  now,
  labels,
  onPickMachine,
}: {
  run: RoutineRun;
  row: RoutineRow;
  now: number;
  labels: ReadonlyMap<string, string>;
  onPickMachine?: () => void;
}) {
  const stopping = useRoutinesStore((s) => s.pending[`stop:${run.run_id}`] === true);
  const running = useRoutinesStore((s) => s.pending[`run:${row.key}`] === true);
  const open = useRoutinesStore((s) => s.openRun?.run_id === run.run_id);
  const { stopRun, openRunDetail, runNow } = useRoutinesStore.getState();
  const teamKey = useActiveTeamKey();
  const at = run.ended_at ?? run.started_at ?? run.created_at;
  const status = runStatus(run);
  const wake = run.wake_machine !== undefined;
  // Each run names the machine its wake went to, by name.
  const ctx = runContext(row, labels, runMachine(run) ?? row.def.where.machine);
  const queuedHere = run.phase === "queued" && wake;
  const until =
    queuedHere && row.queued?.runId === run.run_id && row.queued.deadlineMs
      ? clockTime(row.queued.deadlineMs)
      : null;
  const copy =
    run.action === "command"
      ? commandRunLine(run, now)
      : status.status === "delivered"
        ? `Delivered into ${ctx.agent} on ${ctx.machine}`
        : queuedHere
          ? queuedCopy(ctx.agent, until)
          : run.launch_machine && !wake && run.started_at !== null && !runLine(run, ctx)
            ? `Started a new ${ctx.agent} session on ${ctx.machine}`
            : run.phase === "running" && !wake
              ? null
              : runLine(run, ctx);
  const unknown = run.phase === "unknown";
  const result =
    run.action === "command" && run.phase === "failed"
      ? (run.stderr_tail || run.result_text)?.trim().split("\n").pop()
      : (run.result_text ?? run.detail);
  const mergedInto = wake || row.effective_mode === "wake" ? "wake" : "run";
  const reason = (run.reason ?? "").split(":")[0].trim();
  const show = () => openAgentResult({ run_id: run.run_id, sessionId: run.session_id });

  // The one action each status offers (design §4.7). Unknown: Show only, never Run now.
  let action: React.ReactNode = null;
  if ((run.phase === "running" && !wake) || (run.phase === "queued" && !wake)) {
    action = (
      <Button
        size="sm"
        variant="outline"
        disabled={stopping}
        onClick={() => void stopRun(run.run_id)}
      >
        {stopping ? <Loader2 className="animate-spin" /> : <Square />}
        Stop
      </Button>
    );
  } else if (queuedHere) {
    action = (
      <Button
        size="sm"
        variant="outline"
        disabled={stopping}
        onClick={() => void stopRun(run.run_id)}
      >
        {stopping ? <Loader2 className="animate-spin" /> : <Square />}
        Cancel wake
      </Button>
    );
  } else if (status.status === "delivered" || unknown || reason === "agent_failed") {
    action = run.session_id ? (
      <Button size="sm" variant="outline" onClick={show}>
        Show
      </Button>
    ) : null;
  } else if (run.phase === "undelivered" && reason === "needs_preparation") {
    action = (
      <Button
        size="sm"
        variant="outline"
        onClick={() => row.agent_key && openLaunchSheet({ teamKey, agentKey: row.agent_key })}
      >
        Prepare…
      </Button>
    );
  } else if (run.phase === "undelivered" && reason === "machine_unknown") {
    action = onPickMachine ? (
      <Button size="sm" variant="outline" onClick={onPickMachine}>
        Pick a machine
      </Button>
    ) : null;
  } else if (canRetry(run) && run.run_id === row.last_run?.run_id) {
    // Only the latest run offers Run now: an older one would skip the guard the routine
    // page keeps for an unknown latest wake (R5).
    action = (
      <Button
        size="sm"
        variant="outline"
        disabled={running || needsUpstream(row.def)}
        title={upstreamRunReason(row, useRoutinesStore.getState().rows)}
        onClick={() => void runNow(row.key)}
      >
        {running ? <Loader2 className="animate-spin" /> : <Play />}
        Run now
      </Button>
    );
  }

  return (
    <li
      data-testid="routine-run"
      data-run-id={run.run_id}
      data-phase={run.phase}
      className={cn("flex items-start gap-2 px-3 py-2 text-sm", open && "bg-muted")}
    >
      <div className="min-w-0 flex-1">
        <button
          type="button"
          className="flex items-center gap-2 text-left"
          onClick={() => void openRunDetail(run.run_id)}
          aria-label={`Open run ${run.run_id}`}
        >
          <RunStatusWord run={run} className="font-medium" />
          <span className="text-xs text-muted-foreground tabular-nums">
            {run.causation
              ? `after ${run.causation.routine_name}`
              : run.trigger_payload?.kind === "metadata_change"
                ? "field change"
                : run.source}{" "}
            · {timeAgo(at, now)}
          </span>
        </button>
        {run.phase === "coalesced" ? (
          <div data-testid="routine-run-reason" className="pl-3.5 text-xs text-muted-foreground">
            {run.linked_run_id ? (
              <button
                type="button"
                className="underline decoration-dotted underline-offset-2 hover:text-foreground"
                onClick={() => void openRunDetail(run.linked_run_id!)}
              >
                Merged into the {mergedInto} already waiting
              </button>
            ) : (
              `Merged into the ${mergedInto} already waiting`
            )}
          </div>
        ) : (
          copy && (
            <div
              data-testid="routine-run-reason"
              className={cn(
                "pl-3.5 text-xs",
                unknown ? "text-foreground" : "text-muted-foreground",
              )}
            >
              {copy}
            </div>
          )
        )}
        {run.causation && (
          <button
            type="button"
            className="pl-3.5 text-xs text-muted-foreground underline decoration-dotted"
            onClick={() => void openRunDetail(run.causation!.run_id)}
          >
            After {run.causation.routine_name}{" "}
            {run.causation.phase === "succeeded" ? "succeeded" : "finished"}
          </button>
        )}
        {run.trigger_payload?.kind === "metadata_change" && (
          <p data-testid="routine-run-metadata" className="pl-3.5 text-xs text-muted-foreground">
            {String(
              run.trigger_payload.story_id ||
                run.trigger_payload.rel_path ||
                run.trigger_payload.path,
            )}{" "}
            · {String(run.trigger_payload.field)}{" "}
            {run.trigger_payload.previous_present
              ? metadataValue(run.trigger_payload.previous_value)
              : "(unset)"}{" "}
            → {metadataValue(run.trigger_payload.new_value)}
          </p>
        )}
        {(unknown || run.phase === "undelivered") && (
          <div className="pl-3.5">
            <RunDetails run={run} ctx={ctx} />
          </div>
        )}
        {result && !unknown && !(run.action === "command" && run.started_at === null) && (
          <button
            type="button"
            data-testid="routine-run-result"
            className="line-clamp-2 block pl-3.5 text-left text-xs"
            onClick={() => void openRunDetail(run.run_id)}
          >
            {result}
          </button>
        )}
      </div>
      {action && <div data-testid="routine-run-action">{action}</div>}
    </li>
  );
}

/** The run opened from the list: its result and a way into its session or
 *  transcript. */
function OpenRun({ now }: { now: number }) {
  const run = useRoutinesStore((s) => s.openRun);
  const error = useRoutinesStore((s) => s.openRunError);
  const close = useRoutinesStore((s) => s.closeRunDetail);
  if (error) {
    return <p className="mb-3 text-sm text-destructive">Couldn't open the run: {error}</p>;
  }
  if (!run) return null;
  const started = run.started_at ?? run.created_at;
  // Only a run that launched has a session or transcript to open.
  const startable = run.started_at !== null;
  return (
    <div
      data-testid="routine-open-run"
      className="mb-3 flex flex-col gap-2 rounded-md border border-border p-3 text-sm"
    >
      <div className="flex items-center gap-2">
        <span className="font-medium">
          {run.routine_name} · {phaseLabel(run.phase)}
        </span>
        <span className="text-xs text-muted-foreground">started {timeAgo(started, now)}</span>
        <button
          type="button"
          aria-label="Close run"
          onClick={close}
          className="ml-auto inline-flex h-6 w-6 items-center justify-center rounded-md hover:bg-accent"
        >
          <X size={14} aria-hidden="true" />
        </button>
      </div>
      {reasonCopy(run) && <p className="text-xs text-muted-foreground">{reasonCopy(run)}</p>}
      {run.causation && (
        <button
          type="button"
          className="self-start text-xs underline decoration-dotted"
          onClick={() => void useRoutinesStore.getState().openRunDetail(run.causation!.run_id)}
        >
          After {run.causation.routine_name}{" "}
          {run.causation.phase === "succeeded" ? "succeeded" : "finished"}
        </button>
      )}
      {run.action === "command" ? (
        <CommandRunDetails run={run} now={now} />
      ) : (
        <>
          {run.result_text && <p className="whitespace-pre-wrap text-xs">{run.result_text}</p>}
          {run.detail && (
            <p className="whitespace-pre-wrap text-xs text-muted-foreground">{run.detail}</p>
          )}
        </>
      )}
      {startable && run.action !== "command" && (
        <div>
          <Button
            size="sm"
            variant="outline"
            onClick={() => openAgentResult({ run_id: run.run_id, sessionId: run.session_id })}
          >
            {run.phase === "running" ? "Open live session" : "Open transcript"}
          </Button>
        </div>
      )}
    </div>
  );
}

function metadataValue(value: unknown): string {
  return typeof value === "string" ? value : (JSON.stringify(value) ?? "(unset)");
}

export function Downstream({ row }: { row: RoutineRow }) {
  const rows = useRoutinesStore((s) => s.rows);
  const space = row.key.slice(0, row.key.lastIndexOf("/") + 1);
  const downstream =
    rows?.filter((r) => r.key.startsWith(space) && r.def.after?.routine === row.def.id) ?? [];
  if (!downstream.length) return null;
  return (
    <p className="mt-1 text-xs text-muted-foreground">
      Then:{" "}
      {downstream.map((r, i) => (
        <span key={r.key}>
          {i > 0 && "; "}
          <button
            type="button"
            className="underline decoration-dotted"
            onClick={() => showRoutine(r)}
          >
            {r.def.name}
          </button>
          , when this {r.def.after?.on === "succeeded" ? "succeeds" : "finishes"}
        </span>
      ))}
      .
    </p>
  );
}
