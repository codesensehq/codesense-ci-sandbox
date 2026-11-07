// SPDX-License-Identifier: MIT
import { acceptsPrivateAuthority } from "@/shared/lib/private-terminal-authority";
import { memberKey } from "@/shared/lib/terminal-layout-projection";
import { parseWorkspaceKey } from "@/shared/lib/workspace-key";
import { parseTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  LOCAL_MACHINE_ID,
  type RuntimeMemberTarget,
  type RuntimeSessionTarget,
  type TerminalWorkspaceLayout,
} from "@/tauri/commands";
import { getTeamMachines, machineVisibleToTeam } from "./machines-store";

function sameTarget(a: RuntimeMemberTarget, b: RuntimeMemberTarget): boolean {
  return (
    memberKey(a.member) === memberKey(b.member) &&
    a.admissionRevision === b.admissionRevision &&
    a.namespace.serverId === b.namespace.serverId &&
    a.namespace.epoch === b.namespace.epoch &&
    a.namespace.generation === b.namespace.generation
  );
}

/** Visibility and admission are checked on every read, including cached layouts.
 * A registry declaration or persisted preference alone grants no runtime access. */
export function machineTargets(teamKey: string, machineId: string): RuntimeMemberTarget[] {
  if (!teamKeyHasTeam(teamKey)) return [];
  const local = machineId === LOCAL_MACHINE_ID;
  const machine = getTeamMachines(teamKey)?.find((m) => m.machineId === machineId);
  // Existing local tabs keep working while the remote registry is unavailable.
  if (
    !local &&
    (!machine ||
      !machineVisibleToTeam(machine, teamKey) ||
      machine.kind !== "ssh" ||
      machine.status.state !== "connected")
  )
    return [];
  return Object.values(useViewerStore.getState().terminalLayout.admission).flatMap((row) => {
    if (row.member.machineId !== machineId || row.state !== "live") return [];
    if (
      !local &&
      (row.member.slot !== "remote" ||
        !row.privateAuthorityEpoch ||
        !acceptsPrivateAuthority(row.privateAuthorityEpoch))
    )
      return [];
    if (local && row.member.slot === "remote") return [];
    return [{ member: row.member, admissionRevision: row.revision, namespace: row.namespace }];
  });
}

/** Exact-origin workspace projection for both selected and existing tabs.
 * A bare ServerId never determines tab authority. */
export function machineWorkspace(
  teamKey: string,
  target: RuntimeMemberTarget,
  workspaceKey: string,
): TerminalWorkspaceLayout | undefined {
  if (parseWorkspaceKey(workspaceKey)?.teamKey !== teamKey) return undefined;
  if (!machineTargets(teamKey, target.member.machineId).some((t) => sameTarget(t, target)))
    return undefined;
  const member = useViewerStore.getState().terminalLayout.members[memberKey(target.member)];
  if (!member || !sameTarget(member.target, target)) return undefined;
  if (
    target.member.slot === "remote" &&
    (!member.privateAuthorityEpoch || !acceptsPrivateAuthority(member.privateAuthorityEpoch))
  )
    return undefined;
  const workspace = member.workspaces[workspaceKey];
  if (!workspace) return undefined;
  const team = parseTeamKey(teamKey);
  if (
    target.member.slot === "remote" &&
    (workspace.descriptor.org !== team.orgSlug || workspace.descriptor.team !== team.teamName)
  )
    return undefined;
  return workspace;
}

/** A qualified session target; collisions on another member never redirect it. */
export function machineSessionTarget(
  teamKey: string,
  target: RuntimeMemberTarget,
  workspaceKey: string,
  session: string,
): RuntimeSessionTarget | undefined {
  return machineWorkspace(teamKey, target, workspaceKey)?.tabs.some(
    (tab) => tab.session === session,
  )
    ? { ...target, session }
    : undefined;
}
