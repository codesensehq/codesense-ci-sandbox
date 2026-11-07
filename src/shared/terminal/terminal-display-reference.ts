// SPDX-License-Identifier: MIT
import type { RuntimeSessionTarget } from "@/tauri/commands";
import { acceptsPrivateAuthority } from "@/shared/lib/private-terminal-authority";
import { machineSessionTarget } from "@/features/machines/stores/machine-projection-selectors";

export interface RemoteTerminalDisplayReference {
  target: RuntimeSessionTarget;
  teamKey: string;
  workspaceKey: string;
  privateAuthorityEpoch: string;
}
export type TerminalDisplayReference = string | RemoteTerminalDisplayReference;

export function terminalDisplayKey(reference: TerminalDisplayReference): string {
  if (typeof reference === "string") return reference;
  const { target, privateAuthorityEpoch } = reference;
  return JSON.stringify([
    target.member.machineId,
    target.member.slot,
    target.namespace.serverId,
    target.namespace.epoch,
    target.session,
    privateAuthorityEpoch,
  ]);
}

export function currentTerminalDisplay(reference: TerminalDisplayReference): boolean {
  return (
    typeof reference === "string" ||
    (!!reference.privateAuthorityEpoch &&
      acceptsPrivateAuthority(reference.privateAuthorityEpoch) &&
      !!machineSessionTarget(
        reference.teamKey,
        reference.target,
        reference.workspaceKey,
        reference.target.session,
      ))
  );
}

export function terminalSessionReference(reference: TerminalDisplayReference) {
  if (!currentTerminalDisplay(reference)) throw new Error("Terminal origin is no longer available");
  return typeof reference === "string" ? reference : reference.target;
}
