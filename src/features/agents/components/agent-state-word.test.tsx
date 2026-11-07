// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";
import type { AgentRow } from "@/tauri/commands";

import { agentRowStatus, type AgentRowState } from "../lib/agent-row-state";
import type { RunningInstance } from "../lib/agent-running";
import { StateWord } from "./agent-state-word";

const ACCOUNT_OK = { spawn_eligible: true, source: "runtime_default", reason: "ok" };

function row(over: Partial<AgentRow>): AgentRow {
  return {
    key: "acme/api/triage",
    name: "Triage",
    runtime: "claude",
    archived: false,
    running_now: 0,
    account: ACCOUNT_OK,
    ...over,
  } as unknown as AgentRow;
}

const instance = (over: Partial<RunningInstance> = {}): RunningInstance => ({
  handle: "h1",
  name: "Triage 1",
  state: "running",
  stateWord: "Working",
  where: "Team folder",
  machine: null,
  since: null,
  unread: 0,
  ...over,
});

/** Every state a row can be in, each built with and without live instances. */
const CASES: {
  state: AgentRowState;
  row: Partial<AgentRow>;
  instances: (n: number) => RunningInstance[];
}[] = [
  { state: "ready", row: {}, instances: () => [] },
  {
    state: "running",
    row: {},
    instances: (n) => Array.from({ length: n }, (_, i) => instance({ handle: `r${i}` })),
  },
  {
    state: "needs",
    row: {},
    // An idle session with unread needs-you mail waits on you as much as a blocked terminal.
    instances: (n) =>
      Array.from({ length: n }, (_, i) => instance({ handle: `n${i}`, state: "needs" })),
  },
  {
    state: "broken",
    row: { account: { ...ACCOUNT_OK, spawn_eligible: false, reason: "detached" } as never },
    instances: (n) => Array.from({ length: n }, (_, i) => instance({ handle: `b${i}` })),
  },
  {
    state: "archived",
    row: { archived: true },
    instances: (n) => Array.from({ length: n }, (_, i) => instance({ handle: `a${i}` })),
  },
];

beforeEach(() => useViewerStore.setState({ agentsListFilter: null }));
afterEach(cleanup);

describe("StateWord: every row with live instances offers the reveal shortcut, whatever its state", () => {
  for (const c of CASES) {
    // Running and needs-you are states only live instances can put a row in.
    for (const n of c.state === "running" || c.state === "needs" ? [2] : [0, 2]) {
      const live = c.state === "ready" ? 0 : n;
      it(`${c.state} with ${live} live: ${live > 0 ? "a shortcut that filters the live pane" : "just the word"}`, () => {
        const r = row(c.row);
        const status = agentRowStatus(r, c.instances(live));
        expect(status.state).toBe(c.state);
        render(<StateWord row={r} status={status} />);
        const shortcut = screen.queryByTestId("agent-running");
        if (live === 0) {
          expect(shortcut).toBeNull();
          expect(screen.getByTestId("agent-state")).toBeTruthy();
          return;
        }
        expect(shortcut).toBeTruthy();
        expect(shortcut!.textContent).toContain(String(live));
        fireEvent.click(shortcut!);
        expect(useViewerStore.getState().agentsListFilter).toEqual({
          agentKey: "acme/api/triage",
          name: "Triage",
        });
      });
    }
  }

  it("a needs-you row keeps its word in the shortcut", () => {
    const r = row({});
    render(
      <StateWord row={r} status={agentRowStatus(r, [instance({ handle: "x", state: "needs" })])} />,
    );
    expect(screen.getByTestId("agent-running").textContent).toBe("Needs you · 1");
  });
});
