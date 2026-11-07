// SPDX-License-Identifier: MIT
/**
 * Story workspaces that live on a machine: the Where question at create, the
 * machine's preparation (connect first if needed, progress for the creating
 * overlay), and terminals opened on the machine at the workspace's worktree.
 * Installed into the viewer store as its remote-workspace driver.
 */

import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { toast } from "sonner";

import {
  ensureTeamMachines,
  findMachine,
  getTeamMachines,
} from "@/features/machines/stores/machines-store";
import { connectMachineFor } from "@/features/machines/lib/machine-actions";
import { describeMachineStatus } from "@/features/machines/lib/machine-status";
import {
  focusRemoteTab,
  openRemoteTab,
  openRemoteTabSettled,
  pendingRemoteOpens,
  refreshLayout,
} from "@/features/viewer/lib/remote-tab-open";
import { getErrorMessage } from "@/shared/lib/error-message";
import {
  registerRemoteWorkspaceDriver,
  type MachineLocation,
  type RemoteWorkspaceDriver,
  type WorkspaceLocationChoice,
} from "@/shared/lib/workspace-location";
import {
  getViewerState,
  selectWorkspaceTabs,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { prepareRemoteStoryWorkspace, type MachineSummary } from "@/tauri/commands";
import {
  machineTargets,
  machineWorkspace,
} from "@/features/machines/stores/machine-projection-selectors";
import { workspaceKey } from "@/shared/lib/workspace-key";
import {
  REMOTE_WORKSPACE_PROGRESS_EVENT,
  type RemoteWorkspaceProgressPayload,
} from "@/tauri/events";
import {
  step1StateFromProgress,
  type WorkspaceSetupState,
} from "@/features/viewer/lib/workspace-setup-state";

const WHERE_KEY = "codesense.workspace-where";

/** The team's last Where answer, the next create's default. */
export function rememberedWorkspaceWhere(teamKey: string): string {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(WHERE_KEY) ?? "{}");
    if (saved && typeof saved === "object") {
      const value = (saved as Record<string, unknown>)[teamKey];
      if (typeof value === "string") return value;
    }
  } catch {
    /* No saved answer: This Mac. */
  }
  return "local";
}

function rememberWorkspaceWhere(teamKey: string, where: string): void {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(WHERE_KEY) ?? "{}");
    const next = saved && typeof saved === "object" ? { ...saved } : {};
    localStorage.setItem(WHERE_KEY, JSON.stringify({ ...next, [teamKey]: where }));
  } catch {
    /* The default is a convenience only. */
  }
}

/** The Where question the dialog is showing, if any. */
export interface WorkspaceWhereRequest {
  teamKey: string;
  /** What is being created, for the dialog's title. */
  label: string;
  machines: MachineSummary[];
  /** The remembered answer: `"local"` or a machine id. */
  initial: string;
  answer: (choice: WorkspaceLocationChoice | null) => void;
}

export const useWorkspaceWhere = create<{
  request: WorkspaceWhereRequest | null;
  /** Bumped when another create arrives while the question is open. */
  attention: number;
}>(() => ({
  request: null,
  attention: 0,
}));

/** The machines a workspace can be created on, for the Where question. */
function whereMachines(teamKey: string): MachineSummary[] {
  return (getTeamMachines(teamKey) ?? []).filter((m) => m.kind === "ssh");
}

async function chooseLocation(
  teamKey: string,
  label: string,
): Promise<WorkspaceLocationChoice | null> {
  await ensureTeamMachines(teamKey);
  const machines = whereMachines(teamKey);
  // Nothing changes for a team without machines: no question.
  if (machines.length === 0) return { kind: "local" };
  // One question at a time: the open one keeps its answer pending and is
  // brought back into view; this second create waits for the person.
  if (useWorkspaceWhere.getState().request) {
    useWorkspaceWhere.setState((s) => ({ attention: s.attention + 1 }));
    return null;
  }
  return new Promise((resolve) => {
    const request: WorkspaceWhereRequest = {
      teamKey,
      label,
      machines,
      initial: rememberedWorkspaceWhere(teamKey),
      answer: (choice) => {
        if (useWorkspaceWhere.getState().request === request)
          useWorkspaceWhere.setState({ request: null });
        if (choice)
          rememberWorkspaceWhere(teamKey, choice.kind === "local" ? "local" : choice.machineId);
        resolve(choice);
      },
    };
    useWorkspaceWhere.setState({ request });
  });
}

function labelOf(machineId: string): string {
  return findMachine(machineId)?.label ?? machineId;
}

/** Connect `machineId` if it is not connected; throws when it can't. */
async function connected(
  machineId: string,
  onProgress?: (state: WorkspaceSetupState) => void,
): Promise<void> {
  const machine = findMachine(machineId);
  if (machine?.status.state === "connected") return;
  const label = machine?.label ?? machineId;
  onProgress?.({ kind: "step1", phase: "connecting", machine: label });
  const summary = await connectMachineFor(machineId);
  if (summary.status.state !== "connected")
    throw new Error(`${label} didn't connect: ${describeMachineStatus(summary, Date.now()).word}`);
}

const prepare: RemoteWorkspaceDriver["prepare"] = async (input) => {
  await connected(input.machineId, input.onProgress);
  const machine = labelOf(input.machineId);
  // The line names what moves where: "Copying codesense to studio".
  const names = { repo: input.sourceRepo.split("/").pop() ?? input.sourceRepo, machine };
  input.onProgress({ kind: "step1", phase: "starting", ...names });
  // SW-XS-10: every Step 1 reading becomes the setup line's typed state; a
  // failed phase is the failure line (with Retry), not a label.
  const stop = await listen<RemoteWorkspaceProgressPayload>(
    REMOTE_WORKSPACE_PROGRESS_EVENT,
    (event) => {
      if (event.payload.requestId === input.requestId)
        input.onProgress(step1StateFromProgress(event.payload, names));
    },
  ).catch(() => () => {});
  try {
    const answer = await prepareRemoteStoryWorkspace({
      requestId: input.requestId,
      machineId: input.machineId,
      teamKey: input.teamKey,
      workspaceId: input.workspaceId,
      storyId: input.storyId,
      sourceRepo: input.sourceRepo,
      baseRef: input.baseRef,
      branch: input.branch,
    });
    const result = answer.result;
    if (
      (result.status === "prepared" || result.status === "alreadyPrepared") &&
      result.handle &&
      result.cwd
    )
      return {
        handle: result.handle,
        cwd: result.cwd,
        requestId: answer.requestId,
        baseRef: answer.baseRef,
        branch: answer.branch,
      };
    const why = result.message ?? `the machine answered ${result.status}`;
    throw new Error(
      result.status === "unknown"
        ? `${machine} didn't confirm the workspace (${why}). Open the story again to check.`
        : `${machine} couldn't create the workspace: ${why}`,
    );
  } finally {
    stop();
  }
};

/** Workspaces whose first terminal is being opened, so a second open waits. */
const firstOpens = new Set<string>();

const openTerminal: RemoteWorkspaceDriver["openTerminal"] = ({
  teamKey,
  workspaceId,
  location,
  first,
}) => {
  const open = () =>
    openRemoteTab({
      teamKey,
      workspaceId,
      machineId: location.machineId,
      label: labelOf(location.machineId),
      launch: "shell",
      cwd: location.cwd,
    });
  if (!first) {
    void connected(location.machineId).then(open, (e: unknown) => toast.error(getErrorMessage(e)));
    return;
  }
  const key = JSON.stringify([teamKey, workspaceId]);
  if (firstOpens.has(key)) return;
  if (pendingRemoteOpens().some((p) => p.teamKey === teamKey && p.workspaceId === workspaceId))
    return;
  firstOpens.add(key);
  void (async () => {
    try {
      // Connect-then-open; a machine that won't connect leaves the workspace
      // showing it offline, with Connect and Retry.
      await connected(location.machineId);
      // The machine's own layout may already hold this workspace's terminals
      // (a relaunch, another viewer): reattach those, never a duplicate.
      await refreshLayout().catch(() => {});
      if (selectWorkspaceTabs(getViewerState(), teamKey, workspaceId).length > 0) return;
      open();
    } catch {
      /* Shown on the workspace (offline, Connect / Retry). */
    } finally {
      firstOpens.delete(key);
    }
  })();
};

type LaunchAgentInput = Parameters<RemoteWorkspaceDriver["launchAgent"]>[0];

/** The live tab in the workspace on its machine already running this agent:
 *  the same Agent, under the same name. */
export function liveRemoteAgentTab(input: LaunchAgentInput): string | null {
  const { teamKey, workspaceId, location } = input;
  const ws = getViewerState().workspaces[teamKey]?.find((w) => w.id === workspaceId);
  if (!ws) return null;
  const key = workspaceKey(teamKey, ws);
  for (const target of machineTargets(teamKey, location.machineId)) {
    const tabs = machineWorkspace(teamKey, target, key)?.tabs ?? [];
    for (const tab of tabs) {
      if (tab.state.kind === "exited" || !tab.tags.remoteHandle) continue;
      if (input.name !== undefined && tab.name !== input.name) continue;
      if (tab.tags.remoteAgentKey === input.agentKey) return tab.session;
    }
  }
  return null;
}

const launchAgent: RemoteWorkspaceDriver["launchAgent"] = async (input) => {
  const { teamKey, workspaceId, location, agentKey } = input;
  const label = labelOf(location.machineId);
  if (!location.handle || !location.cwd)
    throw new Error(`unavailable: the workspace is still being created on ${label}`);
  await connected(location.machineId);
  // A second open of the same agent focuses the one already running there.
  if (input.sessionMode !== "new") {
    await refreshLayout().catch(() => {});
    const live = liveRemoteAgentTab(input);
    if (live) {
      if (input.headed !== false) focusRemoteTab(teamKey, workspaceId, live);
      return { sessionId: live, fresh: false };
    }
  }
  const settled = await openRemoteTabSettled({
    frameId: input.frameId,
    pagePath: input.pagePath,
    teamKey,
    workspaceId,
    machineId: location.machineId,
    label,
    cwd: location.cwd,
    ...(input.name ? { name: input.name } : {}),
    ...(input.headed === false ? { headed: false } : {}),
    agentKey,
    preparedWorkspaceHandle: location.handle,
    ...(input.initialPrompt?.trim() ? { initialPrompt: input.initialPrompt } : {}),
  });
  if (settled.status === "opened") return { sessionId: settled.session, fresh: true };
  // SW-XS-10: the machine's typed reason (e.g. `terminalServerOutdated`) rides
  // along, so the setup line can offer its one-click fix without parsing text.
  throw Object.assign(
    new Error(
      `${settled.status === "refused" ? "unavailable" : "unconfirmed"}: ${settled.message || `${label} didn't open the agent`}`,
    ),
    "code" in settled && settled.code ? { code: settled.code } : {},
  );
};

/** Open a remote workspace's first terminal now: the workspace's Connect /
 *  Retry. Resolves once the machine is connected (or rejects with why). */
export async function connectRemoteWorkspace(
  teamKey: string,
  workspaceId: string,
  location: MachineLocation,
): Promise<void> {
  await connected(location.machineId);
  openTerminal({ teamKey, workspaceId, location, first: true });
}

export const remoteStoryWorkspaceDriver: RemoteWorkspaceDriver = {
  chooseLocation,
  prepare,
  openTerminal,
  launchAgent,
};

registerRemoteWorkspaceDriver(remoteStoryWorkspaceDriver);
