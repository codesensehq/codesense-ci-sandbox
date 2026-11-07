// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { projectSessionStatus, resetSessionProjection } from "./session-projection-fixture";

beforeEach(resetSessionProjection);

describe("session projection fixtures", () => {
  it("updates a session without losing its identity, metadata or siblings", () => {
    projectSessionStatus("a", "thinking", { handle: "worker", subagents: 2 });
    const sibling = projectSessionStatus("b", "idle");
    const updated = projectSessionStatus("a", "awaiting-answer", { statusSinceMs: 100 });
    expect(useAgentListProjectionStore.getState().sessions).toEqual([updated, sibling]);
    expect(updated).toMatchObject({
      handle: "worker",
      subagents: 2,
      effectiveStatus: "awaiting-answer",
      statusSinceMs: 100,
    });
  });

  it("resets the projection between scenarios that reuse a session id", () => {
    projectSessionStatus("a", "paused", { needsYouKind: "permission" });
    resetSessionProjection();
    expect(useAgentListProjectionStore.getState().sessions).toBeNull();
    const next = projectSessionStatus("a", "idle");
    expect(next.effectiveStatus).toBe("idle");
    expect(next.needsYouKind).toBeUndefined();
  });
});
