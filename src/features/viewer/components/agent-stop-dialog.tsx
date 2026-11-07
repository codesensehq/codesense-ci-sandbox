// SPDX-License-Identifier: MIT
import { endAgentSession } from "@/features/viewer/lib/agent-stop-action";
import { HAND_STARTED_KIND } from "@/features/viewer/lib/hand-agents";
import { useCallback, useRef, useState, type ReactNode } from "react";

import { toast } from "sonner";

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
import { useAgentSessions } from "@/features/viewer/hooks/use-agent-sessions";
import { agentRowNames } from "@/features/viewer/lib/agent-row-presentation";
import { logger } from "@/shared/lib/logger";
import { type AgentSessionRow, wvRosterForSession, wvWorkerForceClose } from "@/tauri/commands";

export interface StopApproval {
  id: string;
  requester: string;
  details?: ReactNode;
  allowAlways?: boolean;
  onOpen?: () => void;
  canConfirm: () => boolean;
  onAnswer: (allow: boolean, always?: boolean) => void;
}
interface StopTarget {
  approval?: StopApproval;
  row: AgentSessionRow;
  /** A Weft worker still in dispatch: it has not declared done. */
  undeclared: boolean;
}

/**
 * Stopping an agent, asked first: `requestStop` opens the confirmation, `dialog` renders it.
 * A Weft worker still in dispatch has not declared done: its Stop says so and, on confirm,
 * marks it needs-attention before stopping it. The dialog opens only once that lookup
 * settles, so its wording never changes under the cursor.
 */
export function useAgentStop(): {
  requestStop: (row: AgentSessionRow, approval?: StopApproval) => void;
  dialog: React.JSX.Element;
} {
  const { setSessions, reload } = useAgentSessions();
  const [target, setTarget] = useState<StopTarget | null>(null);
  const [always, setAlways] = useState(false);

  const stop = useCallback(
    (row: AgentSessionRow) => {
      const { handle } = row;
      setSessions((cur) => (cur ? cur.filter((r) => r.handle !== handle) : cur));
      const end = endAgentSession(row);
      Promise.resolve(end)
        .catch((e) => {
          logger.warn("agents-list", `stop failed: ${String(e)}`);
          toast("Couldn't stop this agent.");
        })
        .finally(
          () =>
            void reload().catch((error: unknown) =>
              logger.warn("agents-list", `refresh after stop failed: ${String(error)}`),
            ),
        );
    },
    [setSessions, reload],
  );

  const request = useRef(0);
  const requestStop = useCallback((row: AgentSessionRow, approval?: StopApproval) => {
    const mine = ++request.current;
    const lookup = row.sessionId
      ? wvRosterForSession(row.sessionId).catch(() => null)
      : Promise.resolve(null);
    void lookup.then((roster) => {
      if (mine !== request.current) return;
      setAlways(false);
      approval?.onOpen?.();
      setTarget({ row, undeclared: roster?.status === "dispatch", approval });
    });
  }, []);

  const confirm = (t: StopTarget) => {
    const { handle } = t.row;
    if (!t.undeclared) return stop(t.row);
    void wvWorkerForceClose(handle)
      .catch(() => {})
      .finally(() => stop(t.row));
  };

  const names = target ? agentRowNames(target.row) : null;
  const noteIsTitle = names?.source === "progress";
  const stableName = noteIsTitle ? names.secondary : names?.full;

  const dialog = (
    <AlertDialog
      key={target?.approval?.id}
      open={target !== null}
      onOpenChange={(open) => {
        if (!open) {
          target?.approval?.onAnswer(false);
          setTarget(null);
        }
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            {target?.undeclared && !noteIsTitle
              ? "Worker has not declared done"
              : `Stop ${stableName ?? "agent"}?`}
          </AlertDialogTitle>
          <AlertDialogDescription>
            {noteIsTitle && <span className="mb-2 block">Currently: {names.primary}</span>}
            {target?.approval && <span className="mb-2 block">{target.approval.requester}</span>}
            {target?.undeclared
              ? "This worker is still running and has not declared a done-state. Stop it anyway and mark it needs-attention?"
              : target?.row.kind === HAND_STARTED_KIND
                ? "This ends the agent and its shell and closes this terminal tab. Its workspace and saved agent definition stay available."
                : "This ends only this agent's session. Its terminal output, workspace and saved agent definition stay available."}
          </AlertDialogDescription>
        </AlertDialogHeader>
        {target?.approval?.details}
        {target?.approval?.allowAlways && (
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={always}
              onChange={(event) => setAlways(event.target.checked)}
            />
            Always allow for this dashboard
          </label>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel autoFocus onClick={() => target?.approval?.onAnswer(false)}>
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction
            onClick={(event) => {
              if (target?.approval && !target.approval.canConfirm()) {
                event.preventDefault();
                return;
              }
              if (target?.approval) target.approval.onAnswer(true, always);
              else if (target) confirm(target);
              setTarget(null);
            }}
          >
            {target?.undeclared ? "Stop anyway" : "Stop"}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
  return { requestStop, dialog };
}
