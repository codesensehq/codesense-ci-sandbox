// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import parity from "../../../../src-tauri/src/live_hub/effective_status/parity.json";
import { getAgentStatusOf } from "../hooks/use-agent-status-of";
import type { AgentSessionRow } from "@/tauri/commands";
import type { AgentStatus } from "@/shared/lib/agent-status-config";

// The input/producer permutations remain Rust tests. Frontend consumers receive only their projected result.
describe("shared producer sequence projection reads", () => {
  it.each(parity.supervision)("$name", ({ events }) => {
    for (const event of events) {
      if (event.event !== "check" || !("status" in event)) continue;
      const row = {
        sessionId: "s",
        alive: true,
        effectiveStatus: event.status as AgentStatus,
      } as AgentSessionRow;
      expect(getAgentStatusOf([row])(row)).toBe(event.status);
    }
  });
});
