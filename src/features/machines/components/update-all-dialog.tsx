// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { buttonVariants } from "@/components/ui/button";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { toast } from "sonner";

import { getErrorMessage } from "@/shared/lib/error-message";

import { type MachineSummary, type TerminalWorkspaceLayout } from "@/tauri/commands";

import { TONE_CLASS, plural } from "../lib/machine-status";
import {
  clearUpdateAllRun,
  confirmedCount,
  reviewUpdateAll,
  outcomeCounts,
  resumeUpdateAllRun,
  startUpdateAll,
  updateAllEntries,
  useUpdateAllRun,
  type UpdateAllEntry,
  type UpdateAllReview,
} from "../lib/update-all";
import { reloadMachines } from "../stores/machines-store";

/** The live workspaces a machine's members hold, by layout key. */
function useWorkspacesOf(): (machineId: string) => [string, TerminalWorkspaceLayout][] {
  const members = useViewerStore((s) => s.terminalLayout.members);
  return (machineId) =>
    Object.values(members)
      // This Mac's in-app tabs keep running: only its server's end.
      .filter(
        (member) =>
          member.target.member.machineId === machineId && member.target.member.slot !== "inProcess",
      )
      .flatMap((member) => Object.entries(member.workspaces));
}

/**
 * SW-AGR-19 PR-5 — the "Update all terminal servers" confirmation: every
 * terminal and agent that ends, grouped by machine and workspace, with agents
 * at work flagged; then each machine in turn (This Mac last) and what came of
 * it. The run itself lives in the app ({@link startUpdateAll}): closing this
 * dialog never stops it, and reopening it shows where it is.
 */
export function UpdateAllDialog({
  machines,
  teamKey,
  open,
  onOpenChange,
}: {
  machines: readonly MachineSummary[];
  teamKey: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const workspacesOf = useWorkspacesOf();
  const bySession = useAgentStateStore((s) => s.bySession);
  const activeRun = useUpdateAllRun();
  const reviewEntries = useMemo(
    () => updateAllEntries(machines, workspacesOf, (session) => bySession[session]?.state),
    // The list is read when the dialog opens; it does not shift under the person.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [open],
  );
  // A run under way (or just finished) is shown as it ran.
  const showingRun = activeRun.running || activeRun.outcomes.length > 0;
  const entries = showingRun ? activeRun.entries : reviewEntries;
  // What each server holds now: what the person confirms ending. `null`
  // until every machine has answered.
  const [reviews, setReviews] = useState<Record<string, UpdateAllReview> | null>(null);
  // A run going on in the app (another window's, or from before a reload)
  // shows here.
  useEffect(() => {
    if (open) void resumeUpdateAllRun();
  }, [open]);
  useEffect(() => {
    if (!open || activeRun.running) return;
    clearUpdateAllRun();
    setReviews(null);
    let live = true;
    void reviewUpdateAll(reviewEntries).then((read) => {
      if (live) setReviews(read);
    });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, reviewEntries]);

  const reviewOf = (entry: UpdateAllEntry) => reviews?.[entry.machine.machineId];
  const working = entries.flatMap((entry) => entry.terminals.filter((t) => t.working));
  const done = showingRun && !activeRun.running;
  const updatable = entries.filter((entry) => !reviewOf(entry)?.problem);
  const run = () => {
    if (!reviews) return;
    startUpdateAll(reviewEntries, reviewOf).then(
      (started) => {
        if (!started) {
          toast.info("A terminal server update is open; finish or close it, then try again.");
          return;
        }
        void reloadMachines(teamKey);
      },
      (error: unknown) => toast.error(`Couldn't start the update: ${getErrorMessage(error)}`),
    );
  };

  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent data-testid="update-all-dialog" className="sm:max-w-lg">
        <AlertDialogHeader>
          <AlertDialogTitle>
            {activeRun.running
              ? "Updating terminal servers"
              : done
                ? `Update all: ${outcomeCounts(activeRun.outcomes)}`
                : entries.length === 0
                  ? "Nothing to update"
                  : `Update ${plural(entries.length, "terminal server")}?`}
          </AlertDialogTitle>
          <AlertDialogDescription role="status" aria-live="polite">
            {entries.length === 0
              ? "Every connected terminal server is this app's build."
              : activeRun.running
                ? `${activeRun.outcomes.length} of ${entries.length} done. It keeps going if you close this.`
                : `Each one restarts with this app's terminal server, one after another, This Mac last. Their terminals and agents end; tabs reopen afterwards as shells.${
                    entries.some((entry) => entry.machine.kind === "local")
                      ? " Updating This Mac closes every terminal in this app, including agents connected to CodeSense and the agent you may be talking to."
                      : ""
                  }`}
          </AlertDialogDescription>
        </AlertDialogHeader>
        {working.length > 0 && !showingRun && (
          <p data-testid="update-all-working" className={`text-xs ${TONE_CLASS.warn}`}>
            {plural(working.length, "agent")} at work now will stop:{" "}
            {working.map((t) => t.name).join(", ")}.
          </p>
        )}
        <ul className="flex max-h-72 flex-col gap-3 overflow-y-auto text-xs">
          {entries.map((entry) => {
            const id = entry.machine.machineId;
            const outcome = activeRun.outcomes.find((o) => o.machineId === id);
            const review = reviewOf(entry);
            const count = confirmedCount(entry, review);
            const byWorkspace = new Map<string, typeof entry.terminals>();
            for (const t of entry.terminals)
              byWorkspace.set(t.workspaceKey, [...(byWorkspace.get(t.workspaceKey) ?? []), t]);
            return (
              <li key={id} data-testid={`update-all-machine-${id}`} className="flex flex-col gap-1">
                <p className="font-medium">
                  {entry.machine.label}{" "}
                  <span className="font-normal text-muted-foreground">
                    ·{" "}
                    {activeRun.current === id
                      ? "updating…"
                      : !reviews && !showingRun
                        ? "reading…"
                        : review?.problem && !showingRun
                          ? `not updated: ${review.problem}`
                          : count === 0
                            ? "nothing running"
                            : `${plural(count, "session")} end`}
                  </span>
                </p>
                {[...byWorkspace].map(([workspaceKey, terminals]) => (
                  <p key={workspaceKey} className="text-muted-foreground">
                    {terminals[0].workspace}:{" "}
                    {terminals
                      .map((t) =>
                        t.agent ? `${t.name} (agent${t.working ? ", working" : ""})` : t.name,
                      )
                      .join(", ")}
                  </p>
                ))}
                {outcome && (
                  <p
                    data-testid={`update-all-outcome-${id}`}
                    className={outcome.kind === "updated" ? TONE_CLASS.ok : TONE_CLASS.warn}
                  >
                    {outcome.kind === "updated"
                      ? "Updated"
                      : outcome.kind === "skipped"
                        ? "Skipped"
                        : "Failed"}
                    : {outcome.message}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
        <AlertDialogFooter>
          <AlertDialogCancel>
            {showingRun || entries.length === 0 ? "Close" : "Not now"}
          </AlertDialogCancel>
          {!done && entries.length > 0 && (
            <AlertDialogAction
              data-testid="update-all-confirm"
              className={buttonVariants({ variant: "destructive" })}
              disabled={activeRun.running || !reviews || updatable.length === 0}
              onClick={(event) => {
                event.preventDefault();
                run();
              }}
            >
              {activeRun.running ? "Updating…" : "Update all"}
            </AlertDialogAction>
          )}
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
