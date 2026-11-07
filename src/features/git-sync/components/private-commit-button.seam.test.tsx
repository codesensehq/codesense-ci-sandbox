// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useAgentsStore } from "@/features/agents/stores/agents-store";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { PrivateCommitButton } from "./git-status-button";

const command = vi.hoisted(() => vi.fn(async () => ({ status: "sent" })));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  runContextSyncAction: command,
}));
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

const key = "acme/plans/_scratch/helper";
beforeEach(() => {
  command.mockClear();
  useAgentsStore.setState({
    rows: [{ key, name: "helper", org: "acme/plans", team: "_scratch", archived: false }] as never,
  });
});

it("offers a locally running Only me agent and sends the existing commit request", async () => {
  useAgentListProjectionStore.setState({
    sessions: [
      { handle: "private-helper", alive: true, machineId: null, agentRevision: `${key}@1` },
      { handle: "remote-helper", alive: true, machineId: "another-mac", agentRevision: `${key}@1` },
      { handle: "ended-helper", alive: false, machineId: null, agentRevision: `${key}@1` },
    ] as never,
  });
  render(<PrivateCommitButton teamKey="acme/plans:dev" orgRoot="/private/repo" />);
  fireEvent.click(screen.getByRole("button", { name: "Ask private-helper to commit" }));
  await waitFor(() =>
    expect(command).toHaveBeenCalledWith(
      { org: "acme/plans", team: "dev" },
      "/private/repo",
      "commit",
      "private-helper",
      null,
    ),
  );
  expect(screen.queryByRole("button", { name: "Ask another agent" })).toBeNull();
});

it("keeps remote Only me agents out of the local commit action", () => {
  useAgentListProjectionStore.setState({
    sessions: [
      { handle: "remote-helper", alive: true, machineId: "another-mac", agentRevision: `${key}@1` },
    ] as never,
  });
  render(<PrivateCommitButton teamKey="acme/plans:dev" orgRoot="/private/repo" />);
  expect(screen.getByText("No agent on this Mac to ask.")).toBeVisible();
});
