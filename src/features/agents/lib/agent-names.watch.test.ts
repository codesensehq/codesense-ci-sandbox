// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";

const { handlers, agentsList } = vi.hoisted(() => ({
  handlers: new Map<string, (e: { payload: unknown }) => void>(),
  agentsList: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    handlers.set(name, cb);
    return Promise.resolve(() => handlers.delete(name));
  },
}));
vi.mock("@/tauri/commands", () => ({ agentsList }));

import type { AgentRow, AgentSessionRow } from "@/tauri/commands";
import type { TerminalTabIndex } from "@/shared/stores/viewer-store";
import { followAgentTabTitles } from "@/features/viewer/lib/agent-tab-titles";

import { touchesAgents, useAllAgentRowsStore, watchAllAgentRows } from "./agent-names";

const row = (key: string, name: string) =>
  ({ key, name, scope: "team", org: "acme", team: "backend" }) as AgentRow;
const rowsOf = () => useAllAgentRowsStore.getState().rows?.map((r) => r.name);

afterEach(() => {
  handlers.clear();
  agentsList.mockReset();
});

describe("the shared agents list", () => {
  it("reads afresh for every reader that opens, not only the first", async () => {
    agentsList.mockResolvedValueOnce([row("acme/backend/a", "Old")]);
    const stopFollower = watchAllAgentRows();
    await vi.waitFor(() => expect(rowsOf()).toEqual(["Old"]));

    // A teammate's rename arrived without any event; a view opening later sees it.
    agentsList.mockResolvedValueOnce([row("acme/backend/a", "New")]);
    const stopView = watchAllAgentRows();
    await vi.waitFor(() => expect(rowsOf()).toEqual(["New"]));
    expect(agentsList).toHaveBeenCalledTimes(2);
    stopView();
    stopFollower();
  });

  it("re-reads when synced content changes an agent, and its tabs follow", async () => {
    vi.useFakeTimers();
    const rename = vi.fn();
    const index = {
      "acme:backend": { content: [{ id: "ctl", name: "Manager", sessionId: "ctl" }] },
    } as unknown as TerminalTabIndex;
    const sessions = [
      { sessionId: "ctl", handle: "/c#manager", agentRevision: "acme/backend/manager@1" },
    ] as AgentSessionRow[];
    agentsList.mockResolvedValueOnce([row("acme/backend/manager", "Manager")]);
    const stop = followAgentTabTitles({
      watch: watchAllAgentRows,
      onRows: (onRows) =>
        useAllAgentRowsStore.subscribe((state, prev) => {
          if (state.rows && state.rows !== prev.rows) onRows(state.rows, prev.rows);
        }),
      tabIndexes: () => [index],
      sessions: () => sessions,
      rename,
    });
    await vi.waitFor(() => expect(rowsOf()).toEqual(["Manager"]));

    // An unrelated content change reads nothing.
    handlers.get("context-index-changed")!({
      payload: {
        generation: 2,
        paths: ["docs/guide.md"],
        orgs: [],
        allOrgs: false,
        truncated: false,
      },
    });
    await vi.advanceTimersByTimeAsync(200);
    expect(agentsList).toHaveBeenCalledTimes(1);

    // A teammate's rename of the agent arrives by sync.
    agentsList.mockResolvedValueOnce([row("acme/backend/manager", "Ricky")]);
    handlers.get("context-index-changed")!({
      payload: {
        generation: 3,
        paths: ["/orgs/acme/content/agents/manager/agent.yaml"],
        orgs: ["acme"],
        allOrgs: false,
        truncated: false,
      },
    });
    await vi.advanceTimersByTimeAsync(200);
    await vi.waitFor(() =>
      expect(rename).toHaveBeenCalledWith({
        teamKey: "acme:backend",
        workspaceId: "content",
        tabId: "ctl",
        name: "Ricky",
      }),
    );
    stop();
    vi.useRealTimers();
  });

  it("knows which content changes touch agents", () => {
    const change = (paths: string[], truncated = false) => ({
      generation: 1,
      paths,
      orgs: [],
      allOrgs: false,
      truncated,
    });
    expect(touchesAgents(change(["agents/triage/agent.yaml"]))).toBe(true);
    expect(touchesAgents(change(["/x/content/agents/controller/revisions/2.yml"]))).toBe(true);
    expect(touchesAgents(change(["docs/agents.md", "notes/x.md"]))).toBe(false);
    expect(touchesAgents(change([], true))).toBe(true);
  });
});
