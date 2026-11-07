// SPDX-License-Identifier: MIT
import { useState } from "react";
import { AlarmClock, Plus } from "lucide-react";

import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { AppButton as Button } from "@/shared/components/app-button";
import { relativeTime } from "@/features/routines/lib/routine-display";
import { queuedCopy, runLine, runMachine, runStatus } from "@/features/routines/lib/run-status";
import { machineLabel, useRoutineMachines } from "@/features/routines/lib/use-routine-machines";
import { RunDetails, RunStatusMark } from "@/features/routines/components/run-status";
import { StaleOnMachine } from "@/features/routines/components/stale-on-machine";
import { runContext } from "@/features/routines/components/routine-detail";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import type { AgentRow, RoutineRow } from "@/tauri/commands";

import {
  afterWords,
  lastRunAsRun,
  chipTime,
  clockTime,
  routineSentence,
  routinesOf,
} from "../lib/agent-routines";

/** Routines shown before the list folds behind "Show all N" (design decision 2). */
const FOLD_AFTER = 5;

/**
 * An agent's Routines (SW-AGR-5, design §4.4): one row per routine, read as a sentence,
 * with this Mac's switch, its next run and how the last one went. Add routine opens New
 * routine with the agent fixed; a row opens the routine's page. Routines belong to
 * agents, so this is where they are made and managed.
 */
export function AgentRoutines({ agent, now }: { agent: AgentRow; now: number }) {
  const rows = useRoutinesStore((s) => s.rows);
  const listError = useRoutinesStore((s) => s.listError);
  const loadList = useRoutinesStore((s) => s.loadList);
  const startCreateFor = useRoutinesStore((s) => s.startCreateFor);
  const [showAll, setShowAll] = useState(false);
  const archived = agent.archived;

  if (archived) {
    const names = agent.archived_routines ?? [];
    if (names.length === 0) return null;
    return (
      <section
        aria-labelledby="agent-routines-title"
        data-testid="agent-routines"
        className="@container"
      >
        <h3 id="agent-routines-title" className="mb-2 text-sm font-semibold">
          Routines{" "}
          <span className="font-normal text-muted-foreground">(archived with {agent.name})</span>
        </h3>
        <ul className="divide-y divide-border/70 border-t border-border/70">
          {names.map((r) => (
            <li key={r.key} className="px-3 py-2 text-sm text-muted-foreground">
              {r.name}
            </li>
          ))}
        </ul>
      </section>
    );
  }

  const header = (count: number, add: boolean) => (
    <div className="mb-2 flex items-center justify-between gap-2">
      <h3 id="agent-routines-title" className="text-sm font-semibold">
        Routines
        {count >= 2 && (
          <span className="ml-1.5 font-normal text-muted-foreground tabular-nums">{count}</span>
        )}
      </h3>
      {add && (
        <Button
          size="sm"
          variant="outline"
          data-testid="agent-routine-add"
          onClick={() => startCreateFor(agent.key)}
        >
          <Plus />
          Add routine
        </Button>
      )}
    </div>
  );

  if (rows === null) {
    return (
      <section
        aria-labelledby="agent-routines-title"
        data-testid="agent-routines"
        className="@container"
      >
        {header(0, false)}
        {listError ? (
          <p className="text-sm text-destructive">
            Couldn&apos;t load {agent.name}&apos;s routines: {listError}{" "}
            <button
              type="button"
              className="underline underline-offset-2"
              onClick={() => void loadList()}
            >
              Try again
            </button>
          </p>
        ) : (
          <p className="text-sm text-muted-foreground">Loading routines…</p>
        )}
      </section>
    );
  }

  const mine = routinesOf(rows, agent.key);
  if (mine.length === 0) {
    return (
      <section
        aria-labelledby="agent-routines-title"
        data-testid="agent-routines"
        className="@container"
      >
        {header(0, false)}
        <div className="rounded-md border border-dashed border-border p-3 text-sm">
          <p className="font-medium">No routines yet.</p>
          <p className="mt-0.5 mb-2.5 text-xs text-muted-foreground">
            Add one to have {agent.name} start or wake on a schedule.
          </p>
          <Button
            size="sm"
            variant="outline"
            data-testid="agent-routine-add"
            onClick={() => startCreateFor(agent.key)}
          >
            <Plus />
            Add routine
          </Button>
        </div>
      </section>
    );
  }

  const shown = showAll ? mine : mine.slice(0, FOLD_AFTER);
  return (
    <section
      aria-labelledby="agent-routines-title"
      data-testid="agent-routines"
      className="@container"
    >
      {header(mine.length, true)}
      <ul className="divide-y divide-border/70 border-t border-border/70">
        {shown.map((row) => (
          <AgentRoutineRow key={row.key} row={row} agent={agent} now={now} />
        ))}
      </ul>
      {mine.length > FOLD_AFTER && !showAll && (
        <button
          type="button"
          onClick={() => setShowAll(true)}
          className="mt-1.5 text-xs text-muted-foreground underline decoration-dotted underline-offset-2 hover:text-foreground"
        >
          Show all {mine.length}
        </button>
      )}
    </section>
  );
}

function AgentRoutineRow({ row, agent, now }: { row: RoutineRow; agent: AgentRow; now: number }) {
  const open = useRoutinesStore((s) => s.selectedKey === row.key && s.routineOwner === agent.key);
  const toggling = useRoutinesStore((s) => s.pending[`enable:${row.key}`] === true);
  const requested = useRoutinesStore((s) => s.pendingEnabled[row.key]);
  const { setEnabled, openRoutine } = useRoutinesStore.getState();
  const { labels } = useRoutineMachines();
  const s = routineSentence(row, labels, true);
  const on = requested ?? row.enabled;
  return (
    <li
      data-testid="agent-routine-row"
      data-routine-key={row.key}
      data-off={!on || undefined}
      aria-current={open || undefined}
      className={cn(
        "grid grid-cols-[28px_minmax(0,1fr)] gap-x-2 px-3 py-2 text-sm transition-colors @[420px]:grid-cols-[28px_minmax(0,1fr)_auto]",
        open ? "bg-muted" : "hover:bg-muted/50",
      )}
    >
      <div className="pt-0.5">
        {row.armed_here ? (
          <Switch
            size="sm"
            aria-label={`${row.def.name} on`}
            checked={on}
            disabled={toggling}
            onCheckedChange={(next) => void setEnabled(row.key, next)}
          />
        ) : null}
      </div>
      <div className="min-w-0">
        <button
          type="button"
          data-routine-open={row.key}
          onClick={() => openRoutine(agent.key, row.key)}
          className="block w-full min-w-0 rounded-sm text-left focus-visible:outline-2 focus-visible:outline-ring"
        >
          <span
            data-testid="agent-routine-name"
            className={cn("block truncate font-medium", !on && "text-muted-foreground")}
          >
            {row.def.name}
          </span>
          <span
            data-testid="agent-routine-sentence"
            className="mt-px block text-xs leading-[17px] text-muted-foreground"
          >
            {s.when} · <span className="font-medium text-foreground">{s.mode}</span> · {s.task} ·{" "}
            <span className="whitespace-nowrap">{s.machine}</span>
          </span>
        </button>
        {!row.armed_here && (
          <p className="mt-0.5 text-xs text-muted-foreground">Runs on another Mac</p>
        )}
        <StaleOnMachine row={row} />
        <LastRun row={row} agent={agent} now={now} />
      </div>
      <NextColumn row={row} now={now} on={on} />
    </li>
  );
}

/** The next column: when it fires, `due` while a wake waits, or Off. */
function NextColumn({ row, now, on }: { row: RoutineRow; now: number; on: boolean }) {
  const base =
    "col-start-2 pt-0.5 text-xs leading-[17px] whitespace-nowrap text-muted-foreground tabular-nums @[420px]:col-start-3 @[420px]:text-right";
  if (!on) {
    return (
      <div data-testid="agent-routine-next" className={base}>
        Off
      </div>
    );
  }
  if (row.queued) {
    const until = row.queued.deadlineMs ? clockTime(row.queued.deadlineMs) : null;
    return (
      <div
        data-testid="agent-routine-next"
        data-state="due"
        className={base}
        title={queuedCopy(row.agent_display.name, until)}
      >
        <span className="inline-flex items-center gap-1 font-medium text-foreground">
          <AlarmClock aria-hidden="true" className="size-3 text-[var(--cs-status-busy)]" />
          due
        </span>
      </div>
    );
  }
  if (row.next_run_at !== null) {
    return (
      <div data-testid="agent-routine-next" className={base}>
        next {chipTime(row.next_run_at, now)} · {relativeTime(row.next_run_at, now)}
      </div>
    );
  }
  return (
    <div data-testid="agent-routine-next" className={base}>
      {afterWords(row.def) ?? ""}
    </div>
  );
}

/** How the last run went: dot, word and time, then why when it did not land. An unknown
 *  wake always shows the full binding sentence on its own line (R5). */
function LastRun({ row, agent, now }: { row: RoutineRow; agent: AgentRow; now: number }) {
  const { labels } = useRoutineMachines();
  const last = row.last_run;
  if (!last) {
    return (
      <p data-testid="agent-routine-last" className="mt-0.5 text-xs text-muted-foreground">
        No runs yet
      </p>
    );
  }
  const view = runStatus(last);
  const ctx = runContext(row, labels, runMachine(last) ?? row.def.where.machine);
  const ctxName = { ...ctx, agent: agent.name };
  const until = row.queued?.deadlineMs ? clockTime(row.queued.deadlineMs) : null;
  const line = view.status === "queued" && row.queued ? queuedCopy(agent.name, until) : null;
  const reason = last.phase === "undelivered" ? runLine(last, ctxName) : null;
  const unknown = last.phase === "unknown";
  const run = lastRunAsRun(row)!;

  return (
    <div
      data-testid="agent-routine-last"
      className="mt-0.5 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-xs leading-4"
    >
      <span className="inline-flex items-center gap-1.5">
        <RunStatusMark view={view} />
        <span className="text-foreground">{view.word}</span>
      </span>
      <span className="text-muted-foreground tabular-nums">· {chipTime(last.at, now)}</span>
      {line && <span className="text-muted-foreground">— {line}</span>}
      {reason && <span className="text-muted-foreground">— {reason}.</span>}
      {reason && <RunDetails run={run} ctx={ctxName} />}
      {unknown && (
        <span className="basis-full pl-3.5 text-foreground">
          {runLine(last, ctxName)} <RunDetails run={run} ctx={ctxName} />
        </span>
      )}
      {!unknown && !reason && last.wake_machine && view.status === "delivered" && (
        <span className="text-muted-foreground">
          into {agent.name} {`on ${machineLabel(last.wake_machine, labels)}`}
        </span>
      )}
      {!last.wake_machine && last.launch_machine && last.launch_machine !== "local" && (
        <span data-testid="agent-routine-launch-machine" className="text-muted-foreground">
          on {machineLabel(last.launch_machine, labels)}
        </span>
      )}
    </div>
  );
}
