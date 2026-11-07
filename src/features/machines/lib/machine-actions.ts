// SPDX-License-Identifier: MIT
import { toast } from "sonner";
import { getErrorMessage } from "@/shared/lib/error-message";

import { useUiStore, type MachinesPanel } from "@/shared/stores/ui-store";
import { teamLifecycleVersion, useViewerStore } from "@/shared/stores/viewer-store";
import { currentPrivateAuthorityEpoch } from "@/shared/lib/private-terminal-authority";
import { activeWorkspaceIdForTeam } from "@/shared/lib/team-key";
import {
  connectMachine,
  retireLegacyTerminalServer,
  setMachineRemoteMcpControl,
  type MachineSummary,
} from "@/tauri/commands";
import {
  promptMachineTerminalServerUpdate,
  type PromptOutcome,
} from "@/features/shell/lib/machine-terminal-server";
import { openRemoteTab, relaunchRefusedRemoteOpen } from "@/features/viewer/lib/remote-tab-open";

import { reloadMachines, useMachinesStore } from "../stores/machines-store";
import { terminalServerUpdateAction, type MachineActionId } from "./machine-status";

/**
 * SW-RM-UI-1 — the one router every machine action funnels through, so the picker
 * row, the Machines row and the CLI area can't drift on what "Retry" or "Set up"
 * does. Setup-shaped actions open the Machines reserve panel (the Add-machine
 * two-pane pattern, never a nested modal).
 */

/** Open the Machines tab — on a machine reserve panel, or (null) just on the
 *  Machines group ("Manage machines…"). */
export function openMachinesPanel(panel: MachinesPanel | null): void {
  useUiStore.getState().setMachinesPanel(panel);
  void import("@/features/shell/machines-virtual-tab")
    .then((m) => m.openMachines())
    .catch((e) => toast.error(`Couldn't open Machines: ${getErrorMessage(e)}`));
}

/** The latest user action per machine: a slow connect that resolves after the
 *  user already disconnected (or removed) the machine must not undo that. */
const latestAction = new Map<string, number>();

/** Mark a newer user action on `machineId`, superseding any in-flight connect. */
export function supersedeMachineAction(machineId: string): number {
  const next = (latestAction.get(machineId) ?? 0) + 1;
  latestAction.set(machineId, next);
  return next;
}

export type { MachineSummary };

/** Connect for a caller that continues once the machine is up (a new terminal
 *  waiting on it). The backend's answer lands in every list holding the
 *  machine and is returned; a failure is the caller's to show in place. */
export async function connectMachineFor(machineId: string): Promise<MachineSummary> {
  supersedeMachineAction(machineId);
  const summary = await connectMachine(machineId);
  useMachinesStore.getState().apply(summary);
  return summary;
}

async function connect(machine: MachineSummary, teamKey: string): Promise<void> {
  const token = supersedeMachineAction(machine.machineId);
  const authority = currentPrivateAuthorityEpoch();
  const version = teamLifecycleVersion(teamKey);
  const current = () =>
    latestAction.get(machine.machineId) === token &&
    authority === currentPrivateAuthorityEpoch() &&
    version === teamLifecycleVersion(teamKey);
  const { apply } = useMachinesStore.getState();
  if (machine.status.state === "idle") apply({ ...machine, status: { state: "connecting" } });
  try {
    const result = await connectMachine(machine.machineId);
    if (current()) apply(result);
  } catch (e) {
    if (!current()) return;
    toast.error(`Couldn't connect to ${machine.label}: ${getErrorMessage(e)}`);
    // Re-read: the backend's status is the truth, and a newer one may have
    // arrived meanwhile. Only if even that read fails, drop our optimistic
    // "connecting" (it was never the backend's word).
    const reread = await reloadMachines(teamKey);
    if (!reread && current() && machine.status.state === "idle") apply(machine);
  }
}

/** Allow agents on `machineId` (the person's own remote-MCP permission for
 *  it), reconnect it, then ask again for the launch it refused. */
export async function allowAgentsAndRelaunch(
  machineId: string,
  teamKey: string,
  requestId: string,
): Promise<void> {
  supersedeMachineAction(machineId);
  await setMachineRemoteMcpControl(machineId, true);
  await reloadMachines(teamKey);
  await connectMachineFor(machineId);
  relaunchRefusedRemoteOpen(requestId);
}

/** A shell in `machine`'s home folder, as a new tab in the team's active
 *  workspace — the same open the new-terminal chooser makes. */
export function openShellOn(machine: MachineSummary, teamKey: string): void {
  const viewer = useViewerStore.getState();
  const workspaceId = activeWorkspaceIdForTeam(teamKey, viewer.activeWorkspace, viewer.workspaces);
  if (!viewer.workspaces[teamKey]?.some((w) => w.id === workspaceId)) {
    toast.error("Open a workspace first, then open a shell on this machine.");
    return;
  }
  viewer.revealTerminalInPlace(teamKey);
  openRemoteTab({
    teamKey,
    workspaceId,
    machineId: machine.machineId,
    label: machine.label,
    launch: "shell",
  });
}

/** After a legacy machine switched to a private account: stop the old
 *  shared-account terminal server it left running. One still holding
 *  terminals is stopped only once the person says so; an unreachable host
 *  leaves nothing to report. */
export async function retireLeftoverLegacyServer(machineId: string, label: string): Promise<void> {
  const stop = async (withSessions: boolean) => {
    const result = await retireLegacyTerminalServer(machineId, withSessions);
    if (result.status === "stopped") toast(`Stopped the old terminal server on ${label}.`);
    return result;
  };
  let result;
  try {
    result = await stop(false);
  } catch {
    return;
  }
  if (result.status !== "running") return;
  const terminals = result.sessions === 1 ? "1 terminal" : `${result.sessions} terminals`;
  toast(`The old terminal server on ${label} still runs ${terminals}.`, {
    description: "Stopping it ends them.",
    duration: Infinity,
    action: {
      label: "Stop it",
      onClick: () =>
        void stop(true).catch((e) =>
          toast.error(`Couldn't stop the old terminal server: ${getErrorMessage(e)}`),
        ),
    },
  });
}

export async function runMachineAction(
  action: MachineActionId,
  machine: MachineSummary,
  teamKey: string,
): Promise<void> {
  switch (action) {
    case "connect":
    case "retry":
      return connect(machine, teamKey);
    case "setUp":
    case "reconnect":
    case "editAddress":
    case "install":
      return openMachinesPanel({ mode: "setUp", machineId: machine.machineId });
    case "reviewKey":
      return openMachinesPanel({ mode: "reviewKey", machineId: machine.machineId });
    case "keepReadOnly":
      useMachinesStore.getState().keepReadOnly(machine.machineId);
      return;
    case "restartServer":
      await promptTerminalServerUpdate(machine);
      return;
  }
}

/** Open the update confirmation for `machine`'s terminal server and wait for
 *  how it ended (`null`: it cannot be updated from here). */
async function promptTerminalServerUpdate(machine: MachineSummary): Promise<PromptOutcome | null> {
  const s = machine.status;
  if (
    s.state !== "connected" &&
    s.state !== "terminalServerFrozen" &&
    s.state !== "replacementFailed"
  ) {
    toast.error("Connect the machine first: its terminal server is not running.");
    return null;
  }
  return promptMachineTerminalServerUpdate({
    machineId: machine.machineId,
    label: machine.label,
    count: s.state === "connected" ? (s.terminalServerSessions ?? null) : s.count,
    compatible: s.state === "connected",
    update: s.state !== "connected" || s.terminalServerUpdate === true,
    shared: machine.scope?.kind === "team",
  });
}

/** The person closed the confirmation: nothing ran. */
export class MachineActionCancelled extends Error {
  constructor() {
    super("cancelled");
    this.name = "MachineActionCancelled";
  }
}

/** Update the terminal server on `machineId` from wherever its being out of
 *  date shows (a remote tab, an agent's status, a refused open): the same
 *  action its Machines entry offers. Resolves once the update finished;
 *  rejects with {@link MachineActionCancelled} when the person closed the
 *  confirmation, or with the reason it failed. */
export async function updateMachineTerminalServer(machineId: string): Promise<void> {
  const lists = useMachinesStore.getState().byTeam;
  for (const machines of Object.values(lists)) {
    const machine = machines.find((m) => m.machineId === machineId);
    if (!machine) continue;
    if (terminalServerUpdateAction(machine).id !== "restartServer") {
      // Not connected: set-up connects it and installs its server.
      openMachinesPanel({ mode: "setUp", machineId });
      throw new Error(
        `Connect ${machine.label} in Machines first, then update its terminal server`,
      );
    }
    const outcome = await promptTerminalServerUpdate(machine);
    if (outcome === null) throw new Error("its terminal server is not running");
    if (outcome.kind === "failed") throw new Error(outcome.repair);
    if (outcome.kind === "closed") throw new MachineActionCancelled();
    return;
  }
  // Not listed for any loaded team yet: its Machines entry has the action.
  openMachinesPanel(null);
  throw new MachineActionCancelled();
}

/** {@link updateMachineTerminalServer} from a button that only offers it: the
 *  confirmation itself shows how it went. */
export function offerTerminalServerUpdate(machineId: string): void {
  updateMachineTerminalServer(machineId).catch(() => undefined);
}
