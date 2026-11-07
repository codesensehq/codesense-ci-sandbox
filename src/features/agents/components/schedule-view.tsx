// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { CalendarClock, ChevronDown, Laptop, Plus, Server, TriangleAlert, X } from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Badge } from "@/components/ui/badge";
import { InlineAlert } from "@/components/ui/inline-alert";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/shared/components/empty-state";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { AppButton as Button } from "@/shared/components/app-button";
import { useMachineColor } from "@/features/machines/lib/machine-color";
import { agentRefFor, offeredTo, routineSpace } from "@/features/routines/lib/routine-agent-ref";
import { queuedCopy, runLine, runStatus } from "@/features/routines/lib/run-status";
import { useRoutineMachines } from "@/features/routines/lib/use-routine-machines";
import { RunDetails, RunStatusMark } from "@/features/routines/components/run-status";
import { ArgvChips } from "@/features/routines/components/routine-command-fields";
import { runContext, RoutineDetailPane } from "@/features/routines/components/routine-detail";
import { RoutinePage } from "@/features/routines/components/routine-page";
import { RoutinesStatusStrip } from "@/features/routines/components/routines-status-strip";
import { useRoutinesStore, watchRecentRuns } from "@/features/routines/stores/routines-store";
import { StaleOnMachine } from "@/features/routines/components/stale-on-machine";
import { toast } from "sonner";
import type { AgentRow, RoutineRow, RoutineRun } from "@/tauri/commands";

import {
  afterWords,
  chipTime,
  clockTime,
  lastRunAsRun,
  modeWord,
  taskWords,
} from "../lib/agent-routines";
import {
  attentionCount,
  recentGroups,
  runAt,
  scheduleGroups,
  type AgentGroup,
  type RecentItem,
} from "../lib/schedule-model";
import { useAgentsStore } from "../stores/agents-store";
import { AgentRowFace } from "@/features/notes/components/agent-row-face";
import { RuntimeBadge } from "./agent-bits";
import { LineTab, LineTabs } from "./line-tabs";
import { newScheduleRoutine, openScheduleRoutine, showAgent } from "./agents-tab-nav";

/**
 * The Agents tab's Schedule (SW-AGR-5, design §4.8): when every agent's routines run and
 * how the last ones went, grouped by agent and then by machine, Unassigned first when it
 * isn't empty. The routine page opens on the right, the same page as under the agent.
 */
export function ScheduleView({ now }: { now: number }) {
  const agents = useAgentsStore((s) => s.rows);
  const owner = useRoutinesStore((s) => s.routineOwner);
  const creating = useRoutinesStore((s) => s.creating);
  // The routine's own agent (a move re-files it): its row's `agent_key`, not what the
  // page was opened with.
  const rowAgent = useRoutinesStore(
    (s) => s.rows?.find((r) => r.key === s.selectedKey)?.agent_key ?? null,
  );
  const command = useRoutinesStore(
    (s) => s.rows?.find((r) => r.key === s.selectedKey)?.def.action === "command",
  );
  const selected = useRoutinesStore((s) => s.selectedKey !== null);
  const agentKey = owner === null ? null : creating ? owner : selected ? (rowAgent ?? owner) : null;
  const pageAgent = !command && agentKey ? (agents?.find((a) => a.key === agentKey) ?? null) : null;
  return (
    <div data-testid="schedule-view" className="flex min-h-0 flex-1 flex-col">
      <RoutinesStatusStrip />
      <div className="flex min-h-0 flex-1">
        <div className="w-[min(25rem,45%)] shrink-0 overflow-y-auto border-r border-border">
          <ScheduleList now={now} />
        </div>
        <div className="min-w-0 flex-1 overflow-y-auto">
          {pageAgent ? (
            <RoutinePage agent={pageAgent} now={now} />
          ) : creating || selected ? (
            <RoutineDetailPane now={now} />
          ) : (
            <EmptyState
              icon={<CalendarClock />}
              title="Pick a routine to see its runs."
              className="pt-16"
            />
          )}
        </div>
      </div>
    </div>
  );
}

function ScheduleList({ now }: { now: number }) {
  const rows = useRoutinesStore((s) => s.rows);
  const listError = useRoutinesStore((s) => s.listError);
  const loadList = useRoutinesStore((s) => s.loadList);
  const recentRuns = useRoutinesStore((s) => s.recent);
  const recentError = useRoutinesStore((s) => s.recentError);
  const agents = useAgentsStore((s) => s.rows);
  const filter = useAgentsStore((s) => s.scheduleFilter);
  const list = useAgentsStore((s) => s.scheduleList);
  const { setScheduleFilter, setScheduleList } = useAgentsStore.getState();
  const { labels, options: machines } = useRoutineMachines();

  // Recent is one bounded read of every enabled routine's runs, kept live while shown.
  useEffect(() => (list === "recent" ? watchRecentRuns() : undefined), [list]);
  const runsByKey = useMemo(() => {
    const map: Record<string, RoutineRun[]> = {};
    for (const run of recentRuns ?? []) (map[run.routine_key] ??= []).push(run);
    return map;
  }, [recentRuns]);
  const offline = useMemo(
    () => new Set(machines.filter((m) => m.offline).map((m) => m.id)),
    [machines],
  );

  const agentList = useMemo(() => agents ?? [], [agents]);
  const seen = useAgentsStore((s) => s.seenRuns);
  const attention = useMemo(() => (rows ? attentionCount(rows, seen) : 0), [rows, seen]);
  const upcoming = useMemo(
    () => (rows ? scheduleGroups(rows, agentList, labels, filter) : null),
    [rows, agentList, labels, filter],
  );
  const recent = useMemo(
    () =>
      rows && list === "recent" && recentRuns
        ? recentGroups(rows, runsByKey, agentList, labels, filter, now)
        : null,
    [rows, runsByKey, recentRuns, agentList, labels, filter, now, list],
  );

  const header = (
    <div className="sticky top-0 z-10 flex flex-wrap items-center gap-2 border-b border-border/70 bg-background px-3 pt-2 pb-1.5">
      <LineTabs
        value={list}
        onValueChange={(v) => setScheduleList(v as "upcoming" | "recent")}
        label="Schedule list"
        className="h-8"
      >
        <LineTab value="upcoming" className="px-2.5 text-[13px]">
          Upcoming
        </LineTab>
        <LineTab value="recent" className="px-2.5 text-[13px]">
          Recent
        </LineTab>
      </LineTabs>
      <Button size="xs" variant="outline" onClick={newScheduleRoutine}>
        <Plus />
        New routine
      </Button>
      <span className="flex-1" />
      {/* The one place the needs-attention filter lives: the chip while it is on, the offer
          while there is something to show. */}
      {filter && (
        <span
          data-testid="schedule-filter-chip"
          className="inline-flex h-[26px] items-center gap-1.5 rounded-full border border-warning bg-warning-wash pr-1 pl-2.5 text-[13px] font-medium text-warning-ink"
        >
          {filter.kind === "agent" ? filter.name : "Needs attention"}
          <button
            type="button"
            aria-label="Show every agent's routines"
            onClick={() => setScheduleFilter(null)}
            className="rounded-full p-0.5 outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
          >
            <X aria-hidden="true" className="size-3" />
          </button>
        </span>
      )}
      {!filter && attention > 0 && (
        <button
          type="button"
          onClick={() => setScheduleFilter({ kind: "attention" })}
          className="rounded-sm text-xs text-muted-foreground underline decoration-dotted underline-offset-2 outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
        >
          {attention === 1
            ? "Show the 1 that needs attention"
            : `Show the ${attention} that need attention`}
        </button>
      )}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button size="xs" variant="ghost">
            {filter ? "Filter" : "All agents"}
            <ChevronDown />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-52">
          <DropdownMenuItem onSelect={() => setScheduleFilter({ kind: "attention" })}>
            Needs attention
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setScheduleFilter(null)}>All agents</DropdownMenuItem>
          <DropdownMenuSeparator />
          {[...agentList]
            .filter((a) => !a.archived && (rows ?? []).some((r) => r.agent_key === a.key))
            .sort((a, b) => a.name.localeCompare(b.name))
            .map((a) => (
              <DropdownMenuItem
                key={a.key}
                onSelect={() => setScheduleFilter({ kind: "agent", agentKey: a.key, name: a.name })}
              >
                {a.name}
              </DropdownMenuItem>
            ))}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );

  if (!rows || !upcoming) {
    return (
      <>
        {header}
        {listError ? (
          <InlineAlert
            severity="error"
            className="m-3"
            title="Couldn't load the schedule"
            error={listError}
            action={{ label: "Try again", onClick: () => void loadList() }}
          />
        ) : (
          <div aria-busy="true" aria-label="Loading the schedule" className="p-2">
            <span className="sr-only">Loading the schedule…</span>
            {[0, 1, 2, 3].map((i) => (
              <div key={i} className="flex h-12 items-center gap-3 px-2.5">
                <Skeleton className="h-3 w-14" />
                <div className="grid flex-1 gap-1.5">
                  <Skeleton className="h-3 w-40" />
                  <Skeleton className="h-2.5 w-28" />
                </div>
              </div>
            ))}
          </div>
        )}
      </>
    );
  }

  const empty = upcoming.unassigned.length === 0 && upcoming.groups.length === 0;
  if (empty) {
    return (
      <>
        {header}
        <div className="px-4 py-6 text-sm">
          {filter?.kind === "agent" ? (
            <>
              <p className="font-medium">{filter.name} has no routines.</p>
              <Button
                className="mt-2.5"
                size="sm"
                variant="outline"
                onClick={() => showAgent(filter.agentKey, true)}
              >
                Add routine for {filter.name}
              </Button>
            </>
          ) : filter?.kind === "attention" ? (
            <p className="text-muted-foreground">Nothing needs attention.</p>
          ) : (
            <>
              <p className="font-medium">Nothing scheduled yet.</p>
              <p className="mt-0.5 text-muted-foreground">
                Create a routine to run an agent or a command.
              </p>
              <Button className="mt-2.5" size="sm" variant="outline" onClick={newScheduleRoutine}>
                <Plus />
                New routine
              </Button>
            </>
          )}
        </div>
      </>
    );
  }

  const allOff = rows.length > 0 && rows.every((r) => !r.enabled);
  return (
    <>
      {header}
      <div id="schedule-list-panel" role="tabpanel" className="flex flex-col gap-3 px-3 pb-4">
        {allOff && <p className="px-1 text-xs text-muted-foreground">All routines are off.</p>}
        {list === "upcoming" && upcoming.unassigned.length > 0 && (
          <Unassigned rows={upcoming.unassigned} agents={agentList} />
        )}
        {list === "upcoming" &&
          upcoming.groups.map((g) => (
            <Group key={g.agentKey} group={g} unit="routine" offline={offline}>
              {(row) => <UpcomingRow key={row.key} row={row} group={g} now={now} />}
            </Group>
          ))}
        {list === "recent" && !recent && (
          <p className="px-1 py-2 text-sm text-muted-foreground">
            {recentError ? `Couldn't load recent runs: ${recentError}` : "Loading the schedule…"}
          </p>
        )}
        {list === "recent" &&
          recent &&
          (recent.groups.length > 0 ? (
            recent.groups.map((g) => (
              <Group key={g.agentKey} group={g} unit="run" offline={offline}>
                {(item) => <RecentRow key={item.run.run_id} item={item} group={g} now={now} />}
              </Group>
            ))
          ) : (
            <p className="px-1 py-2 text-sm text-muted-foreground">No runs in the last 7 days.</p>
          ))}
      </div>
    </>
  );
}

function Group<T>({
  group,
  unit,
  offline,
  children,
}: {
  group: AgentGroup<T>;
  unit: "routine" | "run";
  offline: ReadonlySet<string>;
  children: (item: T) => ReactNode;
}) {
  const count = `${group.count} ${unit}${group.count === 1 ? "" : "s"}`;
  const groupAgent = useAgentsStore((s) => s.rows?.find((r) => r.key === group.agentKey) ?? null);
  return (
    <section
      data-testid="schedule-group"
      data-agent={group.agentKey}
      aria-label={group.name}
      className="border-t border-border pt-3 first:border-t-0 first:pt-0"
    >
      <h3 className="flex items-center gap-2 px-1 py-1 text-sm font-semibold">
        {groupAgent && <AgentRowFace agent={groupAgent} size={20} />}
        <span className="truncate">{group.name}</span>
        {group.runtime && <RuntimeBadge runtime={group.runtime} />}
        <span className="text-xs font-normal text-muted-foreground tabular-nums">{count}</span>
        {group.agentKey !== null && (
          <button
            type="button"
            onClick={() => showAgent(group.agentKey!)}
            className="ml-auto text-xs font-normal text-muted-foreground underline decoration-dotted underline-offset-2 hover:text-foreground"
          >
            Open {group.name}
          </button>
        )}
      </h3>
      {group.team && <p className="px-1 text-xs text-muted-foreground">{group.team}</p>}
      {group.machines.map((m) => (
        <div key={m.machine}>
          <MachineHead machine={m.machine} label={m.label} offline={offline.has(m.machine)} />
          <div className="flex flex-col">{m.rows.map(children)}</div>
        </div>
      ))}
    </section>
  );
}

function MachineHead({
  machine,
  label,
  offline,
}: {
  machine: string;
  label: string;
  offline: boolean;
}) {
  const color = useMachineColor(machine);
  const remote = machine !== "local" && machine !== "any" && machine !== "each";
  const Icon = remote ? Server : Laptop;
  return (
    <div
      data-testid="schedule-machine"
      className="flex items-center gap-1.5 px-1 pt-2 pb-1 text-[11px] font-medium whitespace-nowrap text-muted-foreground"
    >
      <span className="relative inline-flex">
        <Icon aria-hidden="true" className="size-3" style={remote ? { color } : undefined} />
        {offline && (
          <span
            aria-hidden="true"
            className="absolute -top-0.5 -right-0.5 size-1.5 rounded-full bg-warning-fg"
          />
        )}
      </span>
      on {label}
      {offline && " · offline"}
    </div>
  );
}

/** The time column: when it fires, or why it won't. */
function TimeColumn({ top, bottom }: { top: string; bottom: string }) {
  return (
    <div className="w-[4.75rem] shrink-0 text-right whitespace-nowrap tabular-nums">
      <div className="text-sm font-medium">{top}</div>
      <div className="text-[11px] text-muted-foreground">{bottom}</div>
    </div>
  );
}

/** The day under a time: today, tomorrow, yesterday, a weekday within the week, else the
 *  date (design §6). */
function dayWord(at: number, now: number): string {
  const day = (t: number) => new Date(t).setHours(0, 0, 0, 0);
  const days = Math.round((day(at) - day(now)) / 86_400_000);
  if (days === 0) return "today";
  if (days === 1) return "tomorrow";
  if (days === -1) return "yesterday";
  if (Math.abs(days) < 7) {
    return new Intl.DateTimeFormat(undefined, { weekday: "short" }).format(at);
  }
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(at);
}

function UpcomingRow({
  row,
  group,
  now,
}: {
  row: RoutineRow;
  group: AgentGroup<RoutineRow>;
  now: number;
}) {
  const selected = useRoutinesStore((s) => s.selectedKey === row.key);
  const toggling = useRoutinesStore((s) => s.pending[`enable:${row.key}`] === true);
  const requested = useRoutinesStore((s) => s.pendingEnabled[row.key]);
  const setEnabled = useRoutinesStore((s) => s.setEnabled);
  const { labels } = useRoutineMachines();
  const on = requested ?? row.enabled;
  const until = row.queued?.deadlineMs ? clockTime(row.queued.deadlineMs) : null;
  const after = row.def.after ? row.schedule_in_words : afterWords(row.def);
  const time = !on
    ? { top: "—", bottom: "off" }
    : row.queued
      ? { top: "due", bottom: until ? `until ${until}` : "" }
      : row.next_run_at !== null
        ? { top: clockTime(row.next_run_at), bottom: dayWord(row.next_run_at, now) }
        : after
          ? { top: after.split(" ")[0], bottom: after.split(" ").slice(1).join(" ") }
          : { top: "—", bottom: "" };
  return (
    <div
      data-testid="schedule-row"
      aria-current={selected || undefined}
      className={cn(
        "flex items-start gap-3 rounded-lg px-2 py-2 text-sm transition-colors",
        selected ? "bg-muted" : "hover:bg-muted/50",
      )}
    >
      <TimeColumn {...time} />
      <div className="min-w-0 flex-1">
        <button
          type="button"
          onClick={() => openScheduleRoutine(group.agentKey, row.key)}
          className="block w-full min-w-0 rounded-sm text-left focus-visible:outline-2 focus-visible:outline-ring"
        >
          <span data-testid="schedule-row-name" className="block truncate font-medium">
            {row.def.name}
          </span>
          {group.showSpace && (
            <span className="block text-xs text-muted-foreground">
              {row.key.slice(0, row.key.lastIndexOf("/"))}
            </span>
          )}
          <span className="block text-xs text-muted-foreground">
            {row.def.action === "command" ? (
              <>
                Runs <ArgvChips argv={row.def.command ?? []} />
              </>
            ) : (
              <>
                <span className="font-medium text-foreground">{modeWord(row.effective_mode)}</span>{" "}
                · {taskWords(row.def.task ?? "")}
              </>
            )}
          </span>
        </button>
        <LastLine row={row} agent={group.name} labels={labels} now={now} />
        <StaleOnMachine row={row} />
      </div>
      <div className="pt-0.5">
        {row.armed_here && (
          <Switch
            size="sm"
            aria-label={`${row.def.name} on`}
            checked={on}
            disabled={toggling}
            onCheckedChange={(next) => void setEnabled(row.key, next)}
          />
        )}
      </div>
    </div>
  );
}

function LastLine({
  row,
  agent,
  labels,
  now,
}: {
  row: RoutineRow;
  agent: string;
  labels: ReadonlyMap<string, string>;
  now: number;
}) {
  const last = row.last_run;
  if (!last) return <p className="mt-0.5 text-xs text-muted-foreground">No runs yet</p>;
  const view = runStatus(last);
  const ctx = { ...runContext(row, labels, last.wake_machine ?? row.def.where.machine), agent };
  const line = last.phase === "unknown" || last.phase === "undelivered" ? runLine(last, ctx) : null;
  return (
    <div className="mt-0.5 flex flex-wrap items-center gap-x-1.5 text-xs">
      <RunStatusMark view={view} />
      <span className="text-foreground">{view.word}</span>
      <span className="text-muted-foreground tabular-nums">· {chipTime(last.at, now)}</span>
      {line && (
        <span
          className={cn(
            last.phase === "unknown"
              ? "basis-full pl-3.5 text-foreground"
              : "text-muted-foreground",
          )}
        >
          {last.phase === "unknown" ? line : `— ${line}`}{" "}
          <RunDetails run={lastRunAsRun(row)!} ctx={ctx} />
        </span>
      )}
    </div>
  );
}

function RecentRow({
  item,
  group,
  now,
}: {
  item: RecentItem;
  group: AgentGroup<RecentItem>;
  now: number;
}) {
  const [unfolded, setUnfolded] = useState(false);
  const { labels } = useRoutineMachines();
  const markRunsSeen = useAgentsStore((s) => s.markRunsSeen);
  const { row, run } = item;
  const view = runStatus(run);
  const ctx = {
    ...runContext(row, labels, run.wake_machine ?? row.def.where.machine),
    agent: group.name,
  };
  const at = runAt(run);
  const reason =
    run.phase === "queued" && run.wake_machine ? queuedCopy(group.name, null) : runLine(run, ctx);
  const failed = run.phase === "unknown" || run.phase === "undelivered";
  return (
    <div
      data-testid="schedule-recent-row"
      className="rounded-lg px-2 py-2 text-sm hover:bg-muted/50"
    >
      <div className="flex items-start gap-3">
        <TimeColumn top={clockTime(at)} bottom={dayWord(at, now)} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-2">
            <button
              type="button"
              onClick={() => openScheduleRoutine(group.agentKey, row.key)}
              className="truncate font-medium hover:underline"
            >
              {row.def.name}
            </button>
            <span className="inline-flex items-center gap-1.5 text-xs">
              <RunStatusMark view={view} />
              <span className="text-foreground">{view.word}</span>
            </span>
            {failed && (
              <span onClickCapture={() => markRunsSeen([run.run_id])}>
                <RunDetails run={run} ctx={ctx} />
              </span>
            )}
          </div>
          {group.showSpace && (
            <p className="text-xs text-muted-foreground">
              {row.key.slice(0, row.key.lastIndexOf("/"))}
            </p>
          )}
          {reason && (
            <p
              className={cn(
                "text-xs",
                run.phase === "unknown" ? "text-foreground" : "text-muted-foreground",
              )}
            >
              {reason}
            </p>
          )}
          {item.folded > 0 && (
            <button
              type="button"
              aria-expanded={unfolded}
              onClick={() => setUnfolded((u) => !u)}
              className="mt-0.5 text-xs text-muted-foreground hover:text-foreground"
            >
              +{item.folded} merged or skipped
            </button>
          )}
          {unfolded && (
            <p className="text-xs text-muted-foreground">
              Fires that arrived while this one was waiting or running were merged into it or
              skipped. The routine&apos;s Runs list each one.
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

/** Routines whose agent was deleted or never found: they don't run until one is picked
 *  (design §4.8.1). */
function Unassigned({ rows, agents }: { rows: RoutineRow[]; agents: readonly AgentRow[] }) {
  const n = rows.length;
  return (
    <section data-testid="schedule-unassigned" aria-label="Unassigned">
      <h3 className="flex items-center gap-2 px-1 py-1 text-sm font-semibold">
        <TriangleAlert aria-hidden="true" className="size-3.5 text-warning-fg" />
        Unassigned
        <Badge tone="warn" className="font-medium">
          {n} {n === 1 ? "needs" : "need"} an agent
        </Badge>
      </h3>
      <p className="mb-1.5 px-1 text-xs text-muted-foreground">
        Their agent was deleted or couldn&apos;t be found when routines moved under agents. They
        won&apos;t run until you pick one.
      </p>
      <div className="flex flex-col">
        {rows.map((row) => (
          <div
            key={row.duplicate ? `${row.key}#dup` : row.key}
            data-testid="schedule-row"
            className="flex items-start gap-3 rounded-lg px-2 py-2 text-sm"
          >
            <TimeColumn top="—" bottom="no agent" />
            <div className="min-w-0 flex-1">
              <span data-testid="schedule-row-name" className="block truncate font-medium">
                {row.def.name}
              </span>
              <span className="block text-xs text-muted-foreground">
                <span className="font-medium text-foreground">{modeWord(row.effective_mode)}</span>{" "}
                · {taskWords(row.def.task ?? "")}
              </span>
              <span className="text-xs text-muted-foreground line-through">
                was: {row.def.agent}
              </span>
            </div>
            {row.duplicate ? (
              <span className="max-w-40 text-xs text-muted-foreground">
                Another routine has the id {row.def.id}. Rename or remove one.
              </span>
            ) : (
              <PickAnAgent row={row} agents={agents} />
            )}
          </div>
        ))}
      </div>
    </section>
  );
}

function PickAnAgent({ row, agents }: { row: RoutineRow; agents: readonly AgentRow[] }) {
  const [open, setOpen] = useState(false);
  const moving = useRoutinesStore((s) => s.pending[`move:${row.key}`] === true);
  const moveRoutine = useRoutinesStore((s) => s.moveRoutine);
  const space = routineSpace(row.scope, row.key);
  const team = row.scope === "team" ? row.key.slice(0, row.key.lastIndexOf("/")) : null;
  // The routine's own team's saved agents; a starter only for a routine that starts a
  // new session (§4.8.1).
  const offered = agents
    .filter(
      (a) =>
        !a.archived &&
        offeredTo(a, space, team) &&
        (a.scope !== "builtin" || row.effective_mode === "fresh"),
    )
    .sort(
      (a, b) =>
        Number(a.scope === "builtin") - Number(b.scope === "builtin") ||
        a.name.localeCompare(b.name),
    );
  const move = async (to: AgentRow) => {
    if (await moveRoutine(row.key, agentRefFor(to, space), row.def.revision)) {
      setOpen(false);
      toast(`${row.def.name} now belongs to ${to.name}.`);
    }
  };
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button size="sm" variant="outline" data-testid="schedule-pick-agent">
          Pick an agent
        </Button>
      </PopoverTrigger>
      <PopoverContent
        role="dialog"
        aria-label={`Pick an agent for ${row.def.name}`}
        align="end"
        data-testid="schedule-pick-agent-popover"
        className="w-72 max-w-[calc(100vw-16px)] gap-1 p-1.5 shadow-none"
      >
        {offered.length === 0 ? (
          <p className="px-2 py-1.5 text-sm text-muted-foreground">No other agent can run it.</p>
        ) : (
          <ul aria-label="Agents" className="flex flex-col">
            {offered.map((a) => (
              <li key={a.key}>
                <button
                  type="button"
                  disabled={moving}
                  onClick={() => void move(a)}
                  className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm hover:bg-muted focus-visible:bg-muted focus-visible:outline-none disabled:opacity-60"
                >
                  <RuntimeBadge runtime={a.runtime} />
                  <span className="truncate">{a.name}</span>
                  {a.scope === "team" && (
                    <span className="ml-auto truncate text-xs text-muted-foreground">{a.team}</span>
                  )}
                </button>
              </li>
            ))}
          </ul>
        )}
      </PopoverContent>
    </Popover>
  );
}
