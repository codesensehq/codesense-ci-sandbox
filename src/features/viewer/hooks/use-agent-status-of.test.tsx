// SPDX-License-Identifier: MIT
import { act, render, renderHook, screen, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import parity from "../../../../src-tauri/src/live_hub/effective_status/parity.json";
import { AgentStatusChip } from "@/shared/components/agent-status-chip";
import { AGENT_STATUS_WORDS, type AgentStatus } from "@/shared/lib/agent-status-config";
import { useAgentListProjectionStore } from "../lib/agent-list-projection";
import { useAgentStatusOf, getAgentStatusOf } from "./use-agent-status-of";
import type { AgentSessionRow } from "@/tauri/commands";

afterEach(cleanup);
describe("projected status is the one row read", () => {
  it.each(parity.derive.map((entry, index) => ({ ...entry, index })))(
    "renders shared parity case $index",
    ({ expected }) => {
      const row = {
        id: "s",
        sessionId: "s",
        alive: false,
        status: "errored",
        effectiveStatus: expected,
      } as AgentSessionRow;
      const hook = renderHook(() => useAgentStatusOf([row]));
      expect(hook.result.current(row)).toBe(expected);
      expect(getAgentStatusOf([row])(row)).toBe(expected);
      render(<AgentStatusChip status={hook.result.current(row)} />);
      expect(screen.getByText(AGENT_STATUS_WORDS[expected as AgentStatus])).toBeTruthy();
    },
  );
  it("updates from a sessions delta without a frontend status clock", () => {
    const row = { id: "s", sessionId: "s", effectiveStatus: "thinking" } as AgentSessionRow;
    useAgentListProjectionStore.setState({ sessions: [row] });
    const hook = renderHook(() => {
      const rows = useAgentListProjectionStore((s) => s.sessions) ?? [];
      return useAgentStatusOf(rows)(rows[0]);
    });
    expect(hook.result.current).toBe("thinking");
    act(() =>
      useAgentListProjectionStore.setState({ sessions: [{ ...row, effectiveStatus: "stalled" }] }),
    );
    expect(hook.result.current).toBe("stalled");
  });
});
