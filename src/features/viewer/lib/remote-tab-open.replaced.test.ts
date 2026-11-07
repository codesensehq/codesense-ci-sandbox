// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// Seam: the real remote open list and the real terminal-server prompt flow.
// A home-shell open refused by a terminal server too old to serve it is sent
// again, by itself, once Update terminal server replaced that server.

const backend = vi.hoisted(() => ({
  outdated: true,
  reply: null as Record<string, unknown> | null,
  requests: [] as import("@/tauri/commands").RemoteShellTabRequest[],
  creates: [] as { machineId: string; requestId: string }[],
}));

vi.mock("@/features/viewer/lib/cli-workspace/transport/terminal-launch-source", () => ({
  remoteShellTab: vi.fn(
    async (request: { operation: string; machineId: string; requestId: string }) => {
      backend.requests.push(request as import("@/tauri/commands").RemoteShellTabRequest);
      if (backend.reply) return backend.reply;
      if (request.operation === "create")
        backend.creates.push({ machineId: request.machineId, requestId: request.requestId });
      return backend.outdated
        ? {
            status: "refused",
            headed: true,
            code: "terminalServerOutdated",
            message: "This machine's terminal server is too old to open in its home folder",
          }
        : { status: "pending", headed: true, message: "Waiting for the machine." };
    },
  ),
}));
vi.mock("@/features/viewer/lib/cli-workspace/transport/terminal-layout-source", () => ({
  terminalLayoutMemberSnapshots: vi.fn(async () => []),
}));
vi.mock("@/shared/lib/private-terminal-authority", () => ({
  currentPrivateAuthorityEpoch: () => "7",
  onPrivateAuthorityRetired: () => () => {},
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  machineTerminalServerUpdateMode: vi.fn(async () => ({ mode: "replace", sessions: null })),
  prepareMachineTerminalServerReplacement: vi.fn(async () => ({ serverToken: "t", count: null })),
  restartMachineTerminalServer: vi.fn(async () => {
    backend.outdated = false;
    return { status: "replaced", count: null };
  }),
}));
vi.mock("@/features/machines/stores/machines-store", () => ({
  refreshMachine: vi.fn(async () => null),
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

import {
  openRemoteTab,
  pendingRemoteOpens,
  clearRemoteOpens,
  recheckRemoteOpen,
  restartRefusedRemoteOpen,
  restoreRemoteOpens,
} from "./remote-tab-open";
import {
  confirmMachineTerminalServerReplacement,
  openMachineTerminalServerPrompt,
} from "@/features/shell/lib/machine-terminal-server";

beforeEach(() => {
  backend.outdated = true;
  backend.reply = null;
  backend.requests = [];
  backend.creates = [];
  clearRemoteOpens();
});

describe("Update terminal server, then the home shell opens", () => {
  it("re-sends the open the old server refused, as a new request, with no reconnect", async () => {
    openRemoteTab({ teamKey: "acme/default", workspaceId: "ws", machineId: "box", label: "Box" });
    openRemoteTab({
      teamKey: "acme/default",
      workspaceId: "ws",
      machineId: "other",
      label: "Other",
    });
    await vi.waitFor(() =>
      expect(pendingRemoteOpens().map((p) => [p.machineId, p.status])).toEqual([
        ["box", "refused"],
        ["other", "refused"],
      ]),
    );
    const refused = pendingRemoteOpens()[0]!.requestId;

    openMachineTerminalServerPrompt({
      machineId: "box",
      label: "Box",
      count: null,
      compatible: true,
    });
    await confirmMachineTerminalServerReplacement();

    await vi.waitFor(() => expect(backend.creates).toHaveLength(3));
    const again = backend.creates[2]!;
    expect(again.machineId).toBe("box");
    expect(again.requestId).not.toBe(refused);
    // Only the replaced machine's refusal is retried; the other stays as it was.
    await vi.waitFor(() =>
      expect(pendingRemoteOpens().map((p) => [p.machineId, p.status])).toEqual([
        ["other", "refused"],
        ["box", "unknown"],
      ]),
    );
  });
});

describe("saved Agent remote intent recovery", () => {
  const selection = {
    teamKey: "acme/default",
    workspaceId: "ws",
    machineId: "box",
    label: "Box",
    launch: "codex" as const,
    agentKey: "acme/default/builder",
    preparedWorkspaceHandle: "prepared-1",
    cwd: "/remote/tree",
  };
  it("retries preparation with the original request, never a second create", async () => {
    backend.reply = {
      status: "refused",
      code: "preparationIncomplete",
      retryable: true,
      headed: false,
    };
    const id = openRemoteTab(selection);
    await vi.waitFor(() => expect(pendingRemoteOpens()[0]?.retryable).toBe(true));
    expect(backend.requests[0]).toMatchObject({
      agentKey: selection.agentKey,
      preparedWorkspaceHandle: "prepared-1",
      cwd: "/remote/tree",
    });
    expect(backend.requests[0]).not.toHaveProperty("launch");
    recheckRemoteOpen(id);
    await vi.waitFor(() => expect(backend.requests).toHaveLength(2));
    expect(backend.requests[1]).toMatchObject({
      operation: "attach",
      reference: { requestId: id },
    });
  });
  it("requires an authoritative newIntentRequired before a fresh launch", async () => {
    backend.reply = { status: "unknown", code: "preparationFailed", headed: false };
    const id = openRemoteTab(selection);
    await vi.waitFor(() => expect(pendingRemoteOpens()[0]?.status).toBe("unknown"));
    restartRefusedRemoteOpen(id);
    expect(backend.requests).toHaveLength(1);
    backend.reply = {
      status: "refused",
      code: "newIntentRequired",
      retryable: false,
      headed: false,
    };
    recheckRemoteOpen(id);
    await vi.waitFor(() => expect(pendingRemoteOpens()[0]?.code).toBe("newIntentRequired"));
    restartRefusedRemoteOpen(id);
    await vi.waitFor(() => expect(backend.requests).toHaveLength(3));
    expect(backend.requests[2]).toMatchObject({
      operation: "create",
      agentKey: selection.agentKey,
      preparedWorkspaceHandle: "prepared-1",
      cwd: "/remote/tree",
    });
    expect(backend.requests[2]).not.toMatchObject({ requestId: id });
  });
  it("retains Agent selection when the user replaces an outdated terminal server", async () => {
    openRemoteTab(selection);
    await vi.waitFor(() => expect(pendingRemoteOpens()[0]?.code).toBe("terminalServerOutdated"));
    openMachineTerminalServerPrompt({
      machineId: "box",
      label: "Box",
      count: null,
      compatible: true,
    });
    await confirmMachineTerminalServerReplacement();
    await vi.waitFor(() => expect(backend.requests).toHaveLength(2));
    expect(backend.requests[1]).toMatchObject({
      agentKey: selection.agentKey,
      preparedWorkspaceHandle: "prepared-1",
      cwd: "/remote/tree",
    });
  });
  it("restores owner-scoped receipts as unknown without replaying create", () => {
    localStorage.setItem(
      "codesense.pending-remote-opens.alice",
      JSON.stringify([
        { ...selection, requestId: "saved", status: "refused", code: "newIntentRequired" },
      ]),
    );
    restoreRemoteOpens("alice");
    expect(pendingRemoteOpens()[0]).toMatchObject({
      requestId: "saved",
      status: "unknown",
      agentKey: selection.agentKey,
    });
    expect(pendingRemoteOpens()[0].code).toBeUndefined();
    expect(backend.requests).toHaveLength(0);
    restoreRemoteOpens("bob");
    expect(pendingRemoteOpens()).toEqual([]);
    restoreRemoteOpens(null);
  });
});
