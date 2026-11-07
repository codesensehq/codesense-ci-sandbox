// SPDX-License-Identifier: MIT
import type { MachineSummary } from "@/tauri/commands";

import {
  PRIVATE_ACCOUNT_FOR_TABS,
  describeMachineStatus,
  needsPrivateAccount,
} from "./machine-status";

/** One of the team's machines a story workspace can be created on, as a picker lists it. */
export interface WorkspaceMachineChoice {
  machineId: string;
  label: string;
  /** Why it can't take the workspace now; null when it can (connecting first if needed). */
  blocked: string | null;
  /** What is shown beside a machine that is not connected: why it is blocked, or that
   *  choosing it connects it first. */
  note: string | null;
}

/** The team's machines a story workspace can be created on, each with whether it can
 *  take one now: a connected machine, or one that connects when chosen. */
export function workspaceMachineChoices(
  machines: readonly MachineSummary[] | null,
  nowMs: number,
): WorkspaceMachineChoice[] {
  return (machines ?? [])
    .filter((m) => m.kind === "ssh" && !m.binding?.hidden)
    .map((machine) => {
      if (needsPrivateAccount(machine))
        return {
          machineId: machine.machineId,
          label: machine.label,
          blocked: PRIVATE_ACCOUNT_FOR_TABS,
          note: PRIVATE_ACCOUNT_FOR_TABS,
        };
      const view = describeMachineStatus(machine, nowMs);
      const connected = machine.status.state === "connected";
      // Idle or already connecting: choosing it connects first. Unreachable, or needing
      // a fix (set up, sign-in, host key, install), it can't take a workspace now.
      const canConnect = view.actions.some((a) => a.id === "connect") || view.tone === "progress";
      const blocked = connected || canConnect ? null : (view.blockedReason ?? view.word);
      return {
        machineId: machine.machineId,
        label: machine.label,
        blocked,
        note: blocked ?? (connected ? null : "Connects first"),
      };
    });
}
