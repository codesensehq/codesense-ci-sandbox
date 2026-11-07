// SPDX-License-Identifier: MIT
import { RoutineSpendCap } from "@/features/agents/components/spend-cap";
import { useRef, type KeyboardEvent } from "react";
import { ChevronLeft } from "lucide-react";
import { cn } from "@/lib/utils";
import { queryShown } from "@/shared/lib/warm-scopes";
import { routineSentence } from "@/features/agents/lib/agent-routines";
import type { AgentRow, RoutineRow } from "@/tauri/commands";
import { machineTeamOf, useRoutineMachines } from "../lib/use-routine-machines";
import { useRoutinesStore, type RoutineView } from "../stores/routines-store";
import { RoutineCreateForm, RoutineOverview } from "./routine-editor";
import { RoutineHistory } from "./routine-history";
import { Downstream, RunsView } from "./routine-detail";
import { RoutineActions } from "./routine-actions";

const VIEWS: { id: RoutineView; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "runs", label: "Runs" },
  { id: "history", label: "History" },
];

/**
 * A routine's page under its agent (SW-AGR-5, design §4.5): the breadcrumb always names
 * the agent (ownership is the point), the read-only sentence, Run now and the ⋯ menu,
 * then Overview (the sentence form) · Runs · History. New routine is the same page with
 * the agent fixed and no tabs. Escape or ‹ Agent returns to the agent.
 */
export function RoutinePage({ agent, now }: { agent: AgentRow; now: number }) {
  const creating = useRoutinesStore((s) => s.creating);
  const createSeq = useRoutinesStore((s) => s.createSeq);
  const row = useRoutinesStore((s) => s.rows?.find((r) => r.key === s.selectedKey) ?? null);
  const closeRoutine = useRoutinesStore((s) => s.closeRoutine);
  const rowsLoaded = useRoutinesStore((s) => s.rows !== null);
  const selectedKey = useRoutinesStore((s) => s.selectedKey);
  const ref = useRef<HTMLDivElement>(null);
  // Back (or Escape) returns focus to the row that opened the page (design §7).
  const back = () => {
    const opener = selectedKey;
    closeRoutine();
    requestAnimationFrame(() => {
      if (useRoutinesStore.getState().routineOwner !== null) return;
      const target = opener
        ? queryShown(`[data-routine-open="${CSS.escape(opener)}"]`)
        : queryShown("[data-testid=agent-routine-add]");
      target?.focus();
    });
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Escape" || e.defaultPrevented) return;
    const target = e.target as HTMLElement;
    // Escape in a popover, menu or open select closes that first.
    if (target.closest("[role=dialog],[role=menu],select")) return;
    e.preventDefault();
    back();
  };
  return (
    <div
      ref={ref}
      data-testid="routine-page"
      onKeyDown={onKeyDown}
      className="flex flex-col gap-4 p-4"
    >
      <header className="flex flex-nowrap items-start justify-between gap-3">
        <div className="min-w-0">
          <nav aria-label="Breadcrumb" className="flex min-w-0 items-center gap-1.5">
            <button
              type="button"
              data-testid="routine-breadcrumb-back"
              onClick={back}
              className="inline-flex shrink-0 items-center gap-0.5 rounded-md text-sm text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring"
            >
              <ChevronLeft aria-hidden="true" className="size-4" />
              {agent.name}
            </button>
            <span aria-hidden="true" className="text-muted-foreground">
              /
            </span>
            <h2 className="truncate text-base font-semibold">
              {creating ? "New routine" : (row?.def.name ?? "Routine")}
            </h2>
          </nav>
          {creating ? (
            <p className="mt-1 text-xs text-muted-foreground">
              For {agent.name}. Routines belong to the agent they run.
            </p>
          ) : row ? (
            <ReadOnlySentence row={row} agent={agent} />
          ) : null}
          {!creating && row && <Downstream row={row} />}
        </div>
        {!creating && row && <RoutineActions row={row} agent={agent} />}
      </header>
      {creating ? (
        <RoutineCreateForm key={createSeq} fixedAgent={agent} onCancel={back} />
      ) : row ? (
        <RoutineTabs row={row} agent={agent} now={now} />
      ) : (
        <p data-testid="routine-page-missing" className="text-sm text-muted-foreground">
          {rowsLoaded ? "This routine was archived or moved." : "Loading…"}
        </p>
      )}
    </div>
  );
}

/** The routine in one read-only line, the mode word in the foreground. */
function ReadOnlySentence({ row, agent }: { row: RoutineRow; agent: AgentRow }) {
  const { labels } = useRoutineMachines(machineTeamOf(agent), agent?.scope === "personal");
  const s = routineSentence(row, labels);
  return (
    <p data-testid="routine-sentence" className="mt-1 text-sm text-muted-foreground">
      {s.when} · <span className="font-medium text-foreground">{s.mode}</span> · {s.task} ·{" "}
      <span className="whitespace-nowrap">{s.machine}</span>
    </p>
  );
}

function RoutineTabs({ row, agent, now }: { row: RoutineRow; agent: AgentRow; now: number }) {
  const view = useRoutinesStore((s) => s.view);
  const setView = useRoutinesStore((s) => s.setView);
  const detail = useRoutinesStore((s) => s.details[row.key]);
  const detailError = useRoutinesStore((s) => s.detailError[row.key]);
  const { labels } = useRoutineMachines(machineTeamOf(agent), agent?.scope === "personal");
  return (
    <>
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
      {/* Kept mounted while another view shows, so an unsaved edit survives a look at
          Runs or History. */}
      <div hidden={view !== "overview"}>
        <RoutineSpendCap row={row} />
        <RoutineOverview key={row.key} row={row} fixedAgent={agent} />
      </div>
      {view === "runs" && (
        <RunsView
          row={row}
          now={now}
          labels={labels}
          onPickMachine={() => {
            setView("overview");
            requestAnimationFrame(() =>
              queryShown<HTMLSelectElement>("[data-testid=routine-machine]")?.focus(),
            );
          }}
        />
      )}
      {view === "history" && detailError && (
        <p className="text-sm text-destructive">Couldn&apos;t load the history: {detailError}</p>
      )}
      {view === "history" && !detail && !detailError && (
        <p className="text-sm text-muted-foreground">Loading…</p>
      )}
      {view === "history" && detail && <RoutineHistory row={row} detail={detail} now={now} />}
    </>
  );
}
