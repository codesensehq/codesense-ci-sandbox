// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";

import { useAgentResultStore } from "@/shared/stores/use-agent-result-store";

/**
 * SW-WV-F4 (AC-2) — the run-detail store threads `defaultView` so an opener can land
 * the panel on Findings. This covers the load-bearing store plumbing directly (the
 * `openAgentResult` opener is a thin wrapper over `setTarget`); the panel-mount side
 * of AC-2 is covered in `run-detail-views.test.tsx`.
 */
describe("useAgentResultStore.setTarget defaultView (SW-WV-F4 AC-2)", () => {
  beforeEach(() => {
    useAgentResultStore.setState({
      runId: null,
      sessionId: null,
      storyId: null,
      defaultView: "output",
      teamKey: null,
      restoreTerminal: null,
    });
  });

  it("records defaultView: 'findings' when requested", () => {
    useAgentResultStore.getState().setTarget("r1", null, "s1", "findings");
    expect(useAgentResultStore.getState().defaultView).toBe("findings");
    expect(useAgentResultStore.getState().runId).toBe("r1");
  });

  it("defaults to 'output' when no defaultView is passed (backward-compatible)", () => {
    useAgentResultStore.getState().setTarget("r2", null, "s2");
    expect(useAgentResultStore.getState().defaultView).toBe("output");
  });

  it("pins the team and clears everything on close", () => {
    useAgentResultStore.getState().setTarget("r3", "sess-3", "s3", "output", "org:team", {
      mode: "viewer",
      visible: false,
    });
    expect(useAgentResultStore.getState()).toMatchObject({
      runId: "r3",
      teamKey: "org:team",
      restoreTerminal: { mode: "viewer", visible: false },
    });
    useAgentResultStore.getState().forfeitRestore();
    expect(useAgentResultStore.getState().restoreTerminal).toBeNull();
    useAgentResultStore.getState().close();
    expect(useAgentResultStore.getState()).toMatchObject({
      runId: null,
      sessionId: null,
      storyId: null,
      teamKey: null,
      restoreTerminal: null,
    });
  });
});
