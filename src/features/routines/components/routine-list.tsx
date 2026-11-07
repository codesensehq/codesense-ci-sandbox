// SPDX-License-Identifier: MIT
import { Loader2, Play, Plus, X } from "lucide-react";

import { AppButton as Button } from "@/shared/components/app-button";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { RoutineRow } from "@/tauri/commands";

import { UnresolvableAgentChip } from "./routine-agent-chip";
import {
  agentName,
  createdByLabel,
  groupRoutines,
  lastRunSummary,
  machineLabel,
  phaseLabel,
  phaseTone,
  relativeTime,
  runsOn,
  routineListLabel,
  switchLabel,
  timeAgo,
  whereLabel,
} from "../lib/routine-display";
import { useRoutinesStore } from "../stores/routines-store";

import { needsUpstream, upstreamRunReason } from "../lib/routine-sentence";

const TONE_DOT: Record<ReturnType<typeof phaseTone>, string> = {
  ok: "bg-[var(--cs-status-done)]",
  bad: "bg-destructive",
  busy: "bg-[var(--cs-status-busy)]",
  attention: "bg-warning-fg",
  quiet: "bg-muted-foreground/40",
};

/** The routines, grouped Mine then Team, each group in a fixed order by name. */
export function RoutineList({ now }: { now: number }) {
  const rows = useRoutinesStore((s) => s.rows);
  const listError = useRoutinesStore((s) => s.listError);
  const agentFilter = useRoutinesStore((s) => s.agentFilter);
  const setAgentFilter = useRoutinesStore((s) => s.setAgentFilter);
  if (rows === null) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        {listError ? `Couldn't load routines: ${listError}` : "Loading routines…"}
      </p>
    );
  }
  const shown = agentFilter ? rows.filter((r) => agentFilter.routineKeys.includes(r.key)) : rows;
  const { unassigned, mine, team } = groupRoutines(shown);
  return (
    <div data-testid="routine-list" className="flex flex-col gap-4 p-3">
      <NewRoutineButton />
      {agentFilter && (
        <span
          data-testid="routine-agent-filter"
          className="inline-flex items-center gap-1 self-start rounded-full bg-muted px-2 py-0.5 text-xs font-medium text-foreground"
        >
          Using {agentFilter.name}
          <button
            type="button"
            aria-label={`Show every routine, not only those using ${agentFilter.name}`}
            onClick={() => setAgentFilter(null)}
            className="rounded-full text-muted-foreground hover:text-foreground"
          >
            <X aria-hidden="true" className="size-3" />
          </button>
        </span>
      )}
      {unassigned.length > 0 && (
        <RoutineGroup
          title="Unassigned"
          note="These never run: their agent is gone from this Mac (open one and pick an agent), or another routine already has their id."
          rows={unassigned}
          now={now}
        />
      )}
      <RoutineGroup title="Mine" rows={mine} now={now} empty="No routines yet." />
      {team.length > 0 && <RoutineGroup title="Team" rows={team} now={now} />}
    </div>
  );
}

function NewRoutineButton() {
  const creating = useRoutinesStore((s) => s.creating);
  const startCreate = useRoutinesStore((s) => s.startCreate);
  return (
    <Button
      size="sm"
      variant={creating ? "secondary" : "outline"}
      className="self-start"
      aria-pressed={creating}
      onClick={() => startCreate()}
    >
      <Plus />
      New routine
    </Button>
  );
}

function RoutineGroup({
  title,
  note,
  rows,
  now,
  empty,
}: {
  title: string;
  /** A line under the title. */
  note?: string;
  rows: RoutineRow[];
  now: number;
  empty?: string;
}) {
  return (
    <section aria-label={title}>
      <h2 className="mb-1 px-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {title}
      </h2>
      {note && (
        <p data-testid="routine-group-note" className="mb-1 px-1 text-xs text-muted-foreground">
          {note}
        </p>
      )}
      {rows.length === 0 && empty ? (
        <p className="px-1 text-sm text-muted-foreground">{empty}</p>
      ) : (
        <ul className="flex flex-col gap-1">
          {rows.map((row) => (
            <RoutineListRow
              key={row.duplicate ? `${row.key}#duplicate:${row.agent_key}` : row.key}
              row={row}
              now={now}
            />
          ))}
        </ul>
      )}
    </section>
  );
}

function RoutineListRow({ row, now }: { row: RoutineRow; now: number }) {
  const selected = useRoutinesStore((s) => s.selectedKey === row.key);
  const select = useRoutinesStore((s) => s.select);
  const toggling = useRoutinesStore((s) => s.pending[`enable:${row.key}`] === true);
  const requested = useRoutinesStore((s) => s.pendingEnabled[row.key]);
  const running = useRoutinesStore((s) => s.pending[`run:${row.key}`] === true);
  const copying = useRoutinesStore((s) => s.pending[`copy:${row.key}`] === true);
  const arming = useRoutinesStore((s) => s.pending[`arm:${row.key}`] === true);
  const { setEnabled, runNow, copyToMine, armHere } = useRoutinesStore.getState();
  const machine = machineLabel(row);
  const recipe = runsOn(row.def) === "each_mac";
  const needsYou = row.needs_you_run;
  const last = row.last_run;

  return (
    <li
      data-testid="routine-row"
      data-routine-key={row.key}
      data-selected={selected}
      aria-current={selected || undefined}
      className={cn(
        "rounded-md border px-2 py-2 text-sm transition-colors",
        selected ? "border-border bg-muted" : "border-transparent hover:bg-muted/50",
      )}
    >
      {/* The actions wrap under the text when a row has more of them than
          fit beside it, so the name and schedule never squeeze to a sliver. */}
      <div className="flex flex-wrap items-start gap-2">
        <button
          type="button"
          onClick={() => select(row.key)}
          className="min-w-56 flex-1 text-left"
          aria-label={`Open ${row.def.name}`}
        >
          <div className="flex items-center gap-2">
            <span className="truncate font-medium">{row.def.name}</span>
            {needsYou && (
              <span
                data-testid="routine-needs-you"
                title={needsYou.result_text ?? undefined}
                className="shrink-0 rounded-full bg-warning-wash px-1.5 text-[11px] font-semibold text-warning-ink"
              >
                Needs you
              </span>
            )}
          </div>
          <div className="truncate text-xs text-muted-foreground">
            {routineListLabel(row)} ·{" "}
            {row.def.action === "command"
              ? row.def.command?.join(" ")
              : agentName(row.agent_display)}{" "}
            · {whereLabel(row.def.where.kind)}
          </div>
          <UnresolvableAgentChip row={row} />
          {needsYou?.result_text && (
            <div data-testid="routine-needs-you-text" className="truncate text-xs text-warning-ink">
              {needsYou.result_text}
            </div>
          )}
          <div className="mt-0.5 flex flex-wrap items-center gap-x-3 text-xs text-muted-foreground">
            {row.next_run_at !== null && (
              <span data-testid="routine-next-run">Next {relativeTime(row.next_run_at, now)}</span>
            )}
            {last && (
              <span
                data-testid="routine-last-run"
                className="inline-flex items-center gap-1"
                title={lastRunSummary(last)}
              >
                <span
                  aria-hidden="true"
                  className={cn(
                    "inline-block h-2 w-2 rounded-full",
                    TONE_DOT[phaseTone(last.phase)],
                  )}
                />
                {phaseLabel(last.phase)} {timeAgo(last.at, now)}
              </span>
            )}
            <span data-testid="routine-created-by">Created by {createdByLabel(row)}</span>
          </div>
          {machine && (
            <div data-testid="routine-machine" className="text-xs text-muted-foreground">
              {machine}
            </div>
          )}
        </button>
        <div className="flex shrink-0 items-center gap-2">
          {/* The switch is this Mac's state, so it is offered only where the
              routine runs: a shared job armed here (pause), or a recipe each
              Mac runs ("Run on my Mac"). A run by hand runs here wherever it
              is armed. */}
          {row.armed_here && (
            <label className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
              <Switch
                size="sm"
                aria-label={switchLabel(row)}
                checked={requested ?? row.enabled}
                disabled={toggling}
                onCheckedChange={(next) => void setEnabled(row.key, next)}
              />
              {recipe && switchLabel(row)}
            </label>
          )}
          <Button
            size="sm"
            variant="outline"
            disabled={running || needsUpstream(row.def)}
            title={
              upstreamRunReason(row, useRoutinesStore.getState().rows) ??
              (recipe && !row.enabled ? "Turn on for this Mac to run here" : undefined)
            }
            onClick={() => void runNow(row.key)}
          >
            {running ? <Loader2 className="animate-spin" /> : <Play />}
            Run now
          </Button>
          {!row.armed_here && (
            <Button
              size="sm"
              variant="outline"
              disabled={arming}
              onClick={() => void armHere(row.key)}
            >
              Run on this Mac
            </Button>
          )}
          {!row.mine && row.def.action !== "command" && (
            <Button
              size="sm"
              variant="outline"
              disabled={copying}
              onClick={() => void copyToMine(row.key)}
            >
              Copy to my routines
            </Button>
          )}
        </div>
      </div>
    </li>
  );
}
