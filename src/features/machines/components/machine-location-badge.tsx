// SPDX-License-Identifier: MIT
import { useMachine } from "@/features/machines/stores/machines-store";
import { machineOfflineWord, UNKNOWN_MACHINE_LABEL } from "@/features/machines/lib/machine-offline";
import { Server, WifiOff } from "lucide-react";
import { InfoTip } from "@/components/ui/tooltip";
import { useMachineColor } from "@/features/machines/lib/machine-color";
import { isRemoteWorkspace, type MachineLocation } from "@/shared/lib/workspace-location";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

export { machineOfflineWord };

/** A remote workspace's compact identity — its machine's mark in its color,
 *  the words in the tooltip and accessible name; nothing for a local one. */
export function WorkspaceMachineMark({
  workspace,
  testId,
}: {
  workspace: Pick<Workspace, "id" | "location">;
  testId?: string;
}) {
  const identity = useRemoteMachineIdentity(workspace);
  if (!identity) return null;
  return (
    <InfoTip label={`Lives ${identity.words}`}>
      <span
        role="img"
        aria-label={identity.words}
        data-testid={testId ?? `workspace-machine-${workspace.id}`}
        className="inline-flex flex-shrink-0"
      >
        <MachineMark identity={identity} />
      </span>
    </InfoTip>
  );
}

export interface RemoteMachineIdentity {
  machineId: string;
  /** The machine's name, or {@link UNKNOWN_MACHINE_LABEL} when no loaded list knows it. */
  label: string;
  color: string;
  /** "offline" / "offline since 14:05", or `null` while connected. */
  offline: string | null;
  /** "on codesense-test-1" (+ " · offline since 14:05"): the words the
   *  machine's color and icon stand for. */
  words: string;
}

/** Who a remote workspace lives on, for its compact colored identity; `null`
 *  for a workspace on This Mac. */
export function useRemoteMachineIdentity(
  workspace: Pick<Workspace, "location">,
): RemoteMachineIdentity | null {
  const machineId = isRemoteWorkspace(workspace) ? workspace.location.machineId : "";
  const machine = useMachine(machineId);
  const color = useMachineColor(machineId);
  if (!machineId) return null;
  // A machine no loaded list knows is named in words, never by its raw id.
  const label = machine?.label ?? UNKNOWN_MACHINE_LABEL;
  const offline = machineOfflineWord(machine);
  return {
    machineId,
    label,
    color,
    offline,
    words: offline ? `on ${label} · ${offline}` : `on ${label}`,
  };
}

/** A remote machine's mark: a small server in its color, or an amber
 *  disconnected glyph while it is offline (shape, not just color, carries it). */
export function MachineMark({
  identity,
  size = 11,
  testId,
}: {
  identity: RemoteMachineIdentity;
  size?: number;
  testId?: string;
}) {
  const Icon = identity.offline ? WifiOff : Server;
  return (
    <Icon
      aria-hidden
      size={size}
      strokeWidth={2.25}
      data-testid={testId}
      data-offline={identity.offline ? "true" : undefined}
      className="flex-shrink-0"
      style={{ color: identity.offline ? "#d29922" : identity.color }}
    />
  );
}

/** Where a ready workspace lives, when that is a remote machine. */
export function useRemoteWorkspaceLocation(
  teamKey: string,
  workspaceId: string,
): MachineLocation | null {
  return useViewerStore((s) => {
    const ws = s.workspaces[teamKey]?.find((w) => w.id === workspaceId);
    return isRemoteWorkspace(ws) && ws.status === "ready" ? ws.location : null;
  });
}
