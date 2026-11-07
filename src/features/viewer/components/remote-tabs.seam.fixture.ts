// SPDX-License-Identifier: MIT
import type { MachineSummary } from "@/tauri/commands";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { refetchTestLayout } from "@/test/terminal-layout-harness";
import { testRemoteHost } from "@/test/terminal-layout-source-mock";

export const TEAM = "acme:default";
export const WS = "ws-a";

export const BOX: MachineSummary = {
  machineId: "box",
  kind: "ssh",
  label: "codesense-test-1",
  scope: { kind: "personal" },
  declaration: null,
  binding: null,
  canPublish: true,
  status: { state: "connected", serverId: "box-server" },
  attention: 0,
  capabilities: { agentCallbacks: true },
} as unknown as MachineSummary;

function setStatus(state: "connected" | "disconnected"): MachineSummary {
  const summary = {
    ...BOX,
    status: state === "connected" ? BOX.status : { state: "disconnected" },
  } as MachineSummary;
  useMachinesStore.setState({ byTeam: { [TEAM]: [summary] } });
  return summary;
}

/** The app's connection to the Machine drops: it answers its last layout,
 *  frozen, under a new admission. The sessions keep running there. */
export function disconnect(): void {
  const host = testRemoteHost!;
  host.online = false;
  host.admissionRevision += 1;
  setStatus("disconnected");
  const reply = host.reply();
  if (reply.mode !== "historical") throw new Error("expected a frozen layout");
  useViewerStore.getState().applyLayout({
    kind: "admission",
    row: {
      member: reply.member,
      revision: reply.admissionRevision,
      privateAuthorityEpoch: host.privateAuthorityEpoch,
      state: "historical",
      namespace: null,
      historical: reply.frozen,
    },
  });
}

/** Connect again: a new connection (generation) to the same server lifetime. */
export function reconnect(): MachineSummary {
  const host = testRemoteHost!;
  host.online = true;
  host.generation += 1;
  host.admissionRevision += 1;
  const summary = setStatus("connected");
  useViewerStore.getState().applyLayout({
    kind: "admission",
    row: {
      member: host.target().member,
      revision: String(host.admissionRevision),
      privateAuthorityEpoch: host.privateAuthorityEpoch,
      state: "live",
      namespace: host.target().namespace,
      historical: null,
    },
  });
  refetchTestLayout();
  return summary;
}
