// SPDX-License-Identifier: MIT
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

import {
  receiveAskUser,
  receiveAskUserClosed,
  receiveAgentDone,
  receiveSessionDeath,
  answerAskUser,
  type AgentDonePayload,
  type AskUserAnswerDeps,
} from "./use-managed-session-bridge";
import {
  useManagedSessionStore,
  selectManagedState,
  selectManagedSummary,
  resetManagedSessionStoreForTests,
} from "@/shared/stores/use-managed-session-store";
import {
  useAgentChatStore,
  resetAgentChatStoreForTests,
} from "@/shared/stores/use-agent-chat-store";
import {
  resetTranscriptPruneForTests,
  TRANSCRIPT_PRUNE_GRACE_MS_FOR_TESTS,
} from "@/shared/lib/transcript-prune";
import type { AskUserRequest } from "@/shared/lib/managed-session";
import {
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import type { ActivityCompletion } from "@/tauri/commands";

const stateOf = (sessionId: string) =>
  selectManagedState(sessionId)(useManagedSessionStore.getState());
const summaryOf = (sessionId: string) =>
  selectManagedSummary(sessionId)(useManagedSessionStore.getState());
const req = (correlationId: string, sessionId: string): AskUserRequest => ({
  correlationId,
  sessionId,
  question: "which?",
});

describe("managed-session bridge @P1", () => {
  beforeEach(() => {
    useAgentListProjectionStore.setState({ sessions: [] });
    resetManagedSessionStoreForTests();
    resetPrunedAgentSessionsForTests();
    useAgentStateStore.setState({
      bySession: {},
      activityBySession: {},
      exitBySession: {},
      completionSeqBySession: {},
      completionActivityBySession: {},
    });
  });

  describe("receiveAskUser", () => {
    it("records the ask and flips to awaiting-answer", () => {
      receiveAskUser(req("c1", "s1"));
      expect(stateOf("s1")).toBe("awaiting-answer");
      expect(useManagedSessionStore.getState().pendingAsks.c1).toEqual(req("c1", "s1"));
    });

    it("ignores a malformed payload (missing question)", () => {
      receiveAskUser({ correlationId: "c1", sessionId: "s1" } as AskUserRequest);
      expect(stateOf("s1")).toBeUndefined();
    });
  });

  describe("receiveAskUserClosed", () => {
    it("drops the closed ask only, and resumes the session when none remain", () => {
      receiveAskUser(req("c1", "s1"));
      receiveAskUser(req("c2", "s2"));
      receiveAskUserClosed({ sessionId: "s1", correlationId: "c1", reason: "timeout" });
      expect(useManagedSessionStore.getState().pendingAsks.c1).toBeUndefined();
      expect(useManagedSessionStore.getState().pendingAsks.c2).toBeDefined();
      expect(stateOf("s1")).toBe("running");
    });

    it("ignores an unknown id and a malformed payload", () => {
      receiveAskUser(req("c1", "s1"));
      receiveAskUserClosed({ sessionId: "s1", correlationId: "nope", reason: "cancelled" });
      receiveAskUserClosed({} as Parameters<typeof receiveAskUserClosed>[0]);
      expect(useManagedSessionStore.getState().pendingAsks.c1).toBeDefined();
    });
  });

  describe("receiveAgentDone", () => {
    const boundary: ActivityCompletion = {
      identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
      turnGeneration: 2,
      revision: 5,
    };
    function laterTurn() {
      useAgentStateStore.getState().recordActivity(
        {
          ...boundary,
          session: "s1",
          revision: 6,
          turnGeneration: 3,
          provider: "claude",
          observed: true,
          limited: false,
          state: "thinking",
          exit: null,
          asks: [],
          activityAgeMs: 0,
          toolOpen: false,
        },
        Infinity,
      );
    }

    it("consumes an authoritative completion boundary without hiding an already received later turn", () => {
      laterTurn();
      const activityAt = useAgentStateStore.getState().bySession.s1.activityAt;
      receiveAgentDone({ sessionId: "s1", status: "success", activityCompletion: boundary });
      expect(stateOf("s1")).toBe("done");
      expect(useAgentStateStore.getState().completionActivityBySession.s1).toEqual(boundary);
      expect(useAgentStateStore.getState().bySession.s1.activityAt).toBe(activityAt);
    });

    it.each([
      undefined,
      null,
      {},
      "bad",
      { ...boundary, identity: null },
      { ...boundary, identity: { ...boundary.identity, hostBootId: "" } },
      { ...boundary, identity: { ...boundary.identity, invocationId: 4 } },
      ...[-1, 1.5, Infinity, Number.MAX_SAFE_INTEGER + 1, "2"].flatMap((value) => [
        { ...boundary, turnGeneration: value },
        { ...boundary, revision: value },
      ]),
    ])("keeps legacy completion semantics for absent or malformed optional proof %#", (value) => {
      laterTurn();
      receiveAgentDone({
        sessionId: "s1",
        status: "success",
        activityCompletion: value,
      } as AgentDonePayload);
      expect(stateOf("s1")).toBe("done");
      expect(useAgentStateStore.getState().completionActivityBySession.s1).toBeUndefined();
    });

    it("applies a valid status", () => {
      receiveAgentDone({ sessionId: "s1", status: "success" });
      expect(stateOf("s1")).toBe("done");
    });

    it("ignores an unknown status", () => {
      receiveAgentDone({ sessionId: "s1", status: "bogus" as never } as AgentDonePayload);
      expect(stateOf("s1")).toBeUndefined();
    });

    // SW-OR-6 (AC-3b / GAP-1): the payload's summary is now threaded into the store,
    // not dropped — the SW-OR-12 output surface / SW-AU-55 read-model read it.
    it("retains the payload summary (was previously dropped)", () => {
      receiveAgentDone({ sessionId: "s1", status: "success", summary: "Opened PR #42" });
      expect(stateOf("s1")).toBe("done");
      expect(summaryOf("s1")).toBe("Opened PR #42");
    });

    it("applies status with no summary unchanged (byte-identical path)", () => {
      receiveAgentDone({ sessionId: "s1", status: "success" });
      expect(stateOf("s1")).toBe("done");
      expect(summaryOf("s1")).toBeUndefined();
    });
  });

  describe("receiveSessionDeath (RF-perf-15 F-5)", () => {
    beforeEach(() => {
      vi.useFakeTimers();
      resetAgentChatStoreForTests();
      resetTranscriptPruneForTests();
    });
    afterEach(() => vi.useRealTimers());

    it("surfaces a mid-task death as needs-attention, then prunes after the grace window", () => {
      useManagedSessionStore.getState().markRunning("s1");
      useAgentChatStore.getState().appendMessage("s1", "agent", "hi");

      projectSessionStatus("s1", "dropped", { managedState: "needs-attention" });
      receiveSessionDeath("s1");

      // Existing AC-3 behavior preserved: mid-task death → needs-attention, still visible.
      expect(stateOf("s1")).toBe("needs-attention");
      expect("s1" in useAgentChatStore.getState().transcripts).toBe(true);

      // Deferred prune reclaims both after the grace window.
      vi.advanceTimersByTime(TRANSCRIPT_PRUNE_GRACE_MS_FOR_TESTS);
      expect("s1" in useAgentChatStore.getState().transcripts).toBe(false);
      expect(stateOf("s1")).toBeUndefined();
    });
  });

  describe("answerAskUser (no-wedge reply)", () => {
    it("resolves the store and forwards accept+content to the command", async () => {
      const resolve = vi.fn<AskUserAnswerDeps["resolve"]>().mockResolvedValue(undefined);
      receiveAskUser(req("c1", "s1"));

      await answerAskUser("c1", { action: "accept", content: "option B" }, { resolve });

      expect(resolve).toHaveBeenCalledWith("c1", "accept", "option B");
      expect(useManagedSessionStore.getState().pendingAsks.c1).toBeUndefined();
      expect(stateOf("s1")).toBe("running"); // back to running after the answer
    });

    it("passes null content for cancel/decline", async () => {
      const resolve = vi.fn<AskUserAnswerDeps["resolve"]>().mockResolvedValue(undefined);
      receiveAskUser(req("c1", "s1"));
      await answerAskUser("c1", { action: "cancel" }, { resolve });
      expect(resolve).toHaveBeenCalledWith("c1", "cancel", null);
    });

    it("still drops the pending ask when the command rejects (no wedge)", async () => {
      const resolve = vi
        .fn<AskUserAnswerDeps["resolve"]>()
        .mockRejectedValue(new Error("ipc down"));
      receiveAskUser(req("c1", "s1"));
      await expect(
        answerAskUser("c1", { action: "accept", content: "x" }, { resolve }),
      ).resolves.toBeUndefined();
      expect(useManagedSessionStore.getState().pendingAsks.c1).toBeUndefined();
    });

    it("routes concurrent correlations independently", async () => {
      const resolve = vi.fn<AskUserAnswerDeps["resolve"]>().mockResolvedValue(undefined);
      receiveAskUser(req("c1", "s1"));
      receiveAskUser(req("c2", "s2"));
      await answerAskUser("c2", { action: "decline" }, { resolve });
      // c1 untouched, c2 resolved
      expect(useManagedSessionStore.getState().pendingAsks.c1).toEqual(req("c1", "s1"));
      expect(useManagedSessionStore.getState().pendingAsks.c2).toBeUndefined();
      expect(stateOf("s1")).toBe("awaiting-answer");
      expect(stateOf("s2")).toBe("running");
    });
  });
});
