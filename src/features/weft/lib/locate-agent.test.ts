// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import type { AgentSessionRow } from "@/tauri/commands";
import { locateAgentByAgentId } from "./locate-agent";

const h = vi.hoisted(() => ({ sessions: [] as AgentSessionRow[], show: vi.fn() }));
vi.mock("@/features/viewer/lib/agent-list-projection", () => ({
  useAgentListProjectionStore: { getState: () => ({ sessions: h.sessions }) },
}));
vi.mock("@/features/viewer/lib/show-agent", () => ({ showAgent: h.show }));

const row: AgentSessionRow = {
  id: "row-a",
  sessionId: "session-a",
  handle: "worker",
  agentRevision: "builtin/worker@1",
  launchTeamKey: "acme:a",
  kind: "dashboard-launched",
  alive: true,
  backgrounded: false,
  startedAtMs: 1,
  startedWith: null,
  launcher: null,
  lastOutputMs: null,
};

beforeEach(() => {
  h.show.mockReset().mockResolvedValue({ shown: "tab" });
  h.sessions = [
    row,
    {
      ...row,
      id: "row-b",
      sessionId: "session-b",
      launchTeamKey: "acme:b",
      startedAtMs: 2,
    },
  ];
});

it("resolves an exact sessionId before handle and definition matches and forwards Show options", async () => {
  h.sessions.push({
    ...row,
    id: "row-c",
    sessionId: "session-c",
    handle: "session-a",
    agentRevision: "builtin/session-a@1",
    startedAtMs: 3,
  });
  await expect(locateAgentByAgentId("session-a", { preserveFocus: true })).resolves.toEqual({
    located: true,
    outcome: { shown: "tab" },
  });
  expect(h.show).toHaveBeenCalledExactlyOnceWith(row, { preserveFocus: true });
});

it("keeps the newest live handle fallback", async () => {
  await locateAgentByAgentId("worker");
  expect(h.show).toHaveBeenCalledExactlyOnceWith(h.sessions[1], undefined);
});

it("keeps the bare definition fallback", async () => {
  h.sessions = h.sessions.map((session) => ({
    ...session,
    handle: `${session.handle}-${session.sessionId}`,
  }));
  await locateAgentByAgentId("worker");
  expect(h.show).toHaveBeenCalledExactlyOnceWith(h.sessions[1], undefined);
});
