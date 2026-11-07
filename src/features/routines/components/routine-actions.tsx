// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { Loader2, MoreHorizontal, Play } from "lucide-react";
import { toast } from "sonner";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";
import { AppButton as Button } from "@/shared/components/app-button";
import { ConfirmActionDialog } from "@/features/settings/components/confirm-action-dialog";
import { openAgentResult } from "@/features/viewer/components/agent-result-panel";
import type { AgentRow, RoutineRow } from "@/tauri/commands";
import { agentRefFor, offeredTo, routineSpace } from "../lib/routine-agent-ref";
import { needsUpstream, upstreamRunReason } from "../lib/routine-sentence";
import { useRoutinesStore } from "../stores/routines-store";
import { fireTime } from "./run-status";

/** Run now (held back after an unknown wake) and the ⋯ menu. */
export function RoutineActions({ row, agent }: { row: RoutineRow; agent?: AgentRow }) {
  const running = useRoutinesStore((s) => s.pending[`run:${row.key}`] === true);
  const archiving = useRoutinesStore((s) => s.pending[`archive:${row.key}`] === true);
  const runs = useRoutinesStore((s) => s.runs[row.key]);
  const { runNow, archiveRoutine, armHere, copyToMine } = useRoutinesStore.getState();
  const [guard, setGuard] = useState(false);
  const [confirmArchive, setConfirmArchive] = useState(false);
  const [moving, setMoving] = useState(false);
  const last = row.last_run;
  const unknown = !!agent && last?.phase === "unknown";
  const sessionId = runs?.find((r) => r.run_id === last?.run_id)?.session_id ?? null;
  const fire = () => {
    setGuard(false);
    void runNow(row.key);
  };
  const menu = (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button size="sm" variant="outline" aria-label={`More for ${row.def.name}`}>
          <MoreHorizontal />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-52">
        {agent && (
          <DropdownMenuItem onSelect={() => setMoving(true)}>
            Move to another agent…
          </DropdownMenuItem>
        )}
        {!row.armed_here && (
          <DropdownMenuItem onSelect={() => void armHere(row.key)}>
            Run on this Mac
          </DropdownMenuItem>
        )}
        {agent && !row.mine && (
          <DropdownMenuItem onSelect={() => void copyToMine(row.key)}>
            Copy to my routines
          </DropdownMenuItem>
        )}
        <DropdownMenuSeparator />
        <DropdownMenuItem
          className="text-destructive focus:text-destructive"
          onSelect={() => setConfirmArchive(true)}
        >
          Archive routine
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
  return (
    <div className="flex shrink-0 items-center gap-1.5">
      <Popover open={guard} onOpenChange={setGuard}>
        <PopoverAnchor asChild>
          <Button
            size="sm"
            variant="outline"
            disabled={running || needsUpstream(row.def)}
            title={upstreamRunReason(row, useRoutinesStore.getState().rows)}
            aria-haspopup={unknown ? "dialog" : undefined}
            onClick={() => (unknown ? setGuard(true) : fire())}
          >
            {running ? <Loader2 className="animate-spin" /> : <Play />}
            Run now
          </Button>
        </PopoverAnchor>
        <PopoverContent
          role="dialog"
          aria-labelledby="routine-run-guard-title"
          align="end"
          data-testid="routine-run-guard"
          className="w-80 max-w-[calc(100vw-16px)] gap-2 p-3 shadow-none"
        >
          <h3 id="routine-run-guard-title" className="text-sm font-semibold">
            Check {agent?.name}&apos;s tab first
          </h3>
          <p className="text-sm text-muted-foreground">
            {row.def.name}&apos;s {last ? fireTime(last.at) : ""} wake may have been delivered.
            Running it now could paste it twice.
          </p>
          <div className="flex justify-end gap-1.5">
            <Button size="sm" variant="outline" onClick={fire}>
              Run anyway
            </Button>
            <Button
              size="sm"
              disabled={!last}
              onClick={() => {
                setGuard(false);
                if (last) openAgentResult({ run_id: last.run_id, sessionId });
              }}
            >
              Show {agent?.name}&apos;s tab
            </Button>
          </div>
        </PopoverContent>
      </Popover>
      {agent ? (
        <MoveToAgent row={row} agent={agent} open={moving} onOpenChange={setMoving}>
          {menu}
        </MoveToAgent>
      ) : (
        menu
      )}
      <ConfirmActionDialog
        open={confirmArchive}
        onOpenChange={setConfirmArchive}
        title={`Archive ${row.def.name}?`}
        description="It stops running. You can restore it from History."
        confirmLabel="Archive"
        loadingLabel="Archiving…"
        loading={archiving}
        onConfirm={() => void archiveRoutine(row.key).then(() => setConfirmArchive(false))}
        testId="routine-archive-confirm"
      />
    </div>
  );
}

/** Move to another agent…: the routine's own team's agents (and the starters, for a
 *  routine that starts fresh), anchored to the ⋯ menu. */
function MoveToAgent({
  row,
  agent,
  open,
  onOpenChange,
  children,
}: {
  row: RoutineRow;
  agent: AgentRow;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  children: React.ReactNode;
}) {
  const agents = useRoutinesStore((s) => s.agents);
  const moving = useRoutinesStore((s) => s.pending[`move:${row.key}`] === true);
  const moveRoutine = useRoutinesStore((s) => s.moveRoutine);
  const space = routineSpace(row.scope, row.key);
  const teamKey = row.scope === "team" ? `${agent.org}/${agent.team}` : null;
  const offered = (agents ?? []).filter(
    (a) =>
      a.key !== agent.key &&
      !a.archived &&
      offeredTo(a, space, teamKey) &&
      (a.scope !== "builtin" || row.effective_mode === "fresh"),
  );
  const [pick, setPick] = useState("");
  const selectRef = useRef<HTMLSelectElement>(null);
  useEffect(() => {
    if (open) requestAnimationFrame(() => selectRef.current?.focus());
  }, [open]);
  const move = async () => {
    const to = offered.find((a) => a.key === pick);
    if (!to) return;
    if (await moveRoutine(row.key, agentRefFor(to, space), row.def.revision)) {
      onOpenChange(false);
      toast(`${row.def.name} now belongs to ${to.name}.`);
    }
  };
  return (
    <Popover open={open} onOpenChange={onOpenChange}>
      <PopoverAnchor asChild>
        <span>{children}</span>
      </PopoverAnchor>
      <PopoverContent
        role="dialog"
        aria-labelledby="routine-move-title"
        align="end"
        className="w-80 max-w-[calc(100vw-16px)] gap-2 p-3 shadow-none"
      >
        <h3 id="routine-move-title" className="text-sm font-semibold">
          Move {row.def.name} to another agent
        </h3>
        {offered.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No other agent can run it. Routines stay within their team.
          </p>
        ) : (
          <select
            ref={selectRef}
            aria-label="Agent"
            value={pick}
            onChange={(e) => setPick(e.target.value)}
            className="h-8 rounded-lg border border-input bg-background px-2 text-sm text-foreground outline-none focus-visible:border-ring"
          >
            <option value="" disabled>
              Pick an agent
            </option>
            {offered.map((a) => (
              <option key={a.key} value={a.key}>
                {a.name}
              </option>
            ))}
          </select>
        )}
        <div className="flex justify-end gap-1.5">
          <Button size="sm" variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button size="sm" disabled={!pick || moving} onClick={() => void move()}>
            {moving && <Loader2 className="animate-spin" />}
            Move
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  );
}
