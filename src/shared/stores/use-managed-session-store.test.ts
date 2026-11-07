// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";

import {
  useManagedSessionStore,
  selectManagedState,
  selectManagedSummary,
  resetManagedSessionStoreForTests,
} from "./use-managed-session-store";
import type { AskUserRequest } from "@/shared/lib/managed-session";

const store = () => useManagedSessionStore.getState();
const stateOf = (sessionId: string) =>
  selectManagedState(sessionId)(useManagedSessionStore.getState());
const summaryOf = (sessionId: string) =>
  selectManagedSummary(sessionId)(useManagedSessionStore.getState());

function ask(correlationId: string, sessionId: string): AskUserRequest {
  return { correlationId, sessionId, question: "which option?" };
}

describe("managed-session store @P1", () => {
  beforeEach(() => resetManagedSessionStoreForTests());

  describe("lifecycle basics", () => {
    it("is undefined for an unmanaged session", () => {
      expect(stateOf("s1")).toBeUndefined();
    });

    it("markRunning arms the session as running", () => {
      store().markRunning("s1");
      expect(stateOf("s1")).toBe("running");
    });
  });

  describe("ask_user → awaiting-answer → running (interlock a)", () => {
    it("addPendingAsk flips to awaiting-answer", () => {
      store().markRunning("s1");
      store().addPendingAsk(ask("c1", "s1"));
      expect(stateOf("s1")).toBe("awaiting-answer");
    });

    it("resolving the last ask returns the session to running", () => {
      store().addPendingAsk(ask("c1", "s1"));
      store().resolvePendingAsk("c1");
      expect(stateOf("s1")).toBe("running");
      expect(store().pendingAsks).toEqual({});
    });

    it("stays awaiting-answer until the LAST concurrent ask resolves", () => {
      store().addPendingAsk(ask("c1", "s1"));
      store().addPendingAsk(ask("c2", "s1"));
      store().resolvePendingAsk("c1");
      expect(stateOf("s1")).toBe("awaiting-answer"); // c2 still pending
      store().resolvePendingAsk("c2");
      expect(stateOf("s1")).toBe("running");
    });

    it("the quiet net does NOT fire while awaiting-answer (interlock a)", () => {
      store().markRunning("s1");
      store().addPendingAsk(ask("c1", "s1"));
      expect(stateOf("s1")).toBe("awaiting-answer");
    });
  });

  describe("signal_done — top of the completion precedence", () => {
    it("success → done", () => {
      store().markRunning("s1");
      store().applyDoneStatus("s1", "success");
      expect(stateOf("s1")).toBe("done");
    });

    it("preserves an attested failed state and clears pending asks", () => {
      store().addPendingAsk(ask("c1", "s1"));
      store().applyDoneStatus("s1", "failed");
      expect(stateOf("s1")).toBe("failed");
      expect(store().pendingAsks.c1).toBeUndefined();
    });

    it("needs-attention status → needs-attention", () => {
      store().applyDoneStatus("s1", "needs-attention");
      expect(stateOf("s1")).toBe("needs-attention");
    });
  });

  describe("markRunning guard (review fix — no resurrection)", () => {
    it("does NOT clobber a terminal state on re-fire (remount/rebind)", () => {
      store().applyDoneStatus("s1", "success");
      store().markRunning("s1");
      expect(stateOf("s1")).toBe("done");
      store().applyDoneStatus("s2", "needs-attention");
      store().markRunning("s2");
      expect(stateOf("s2")).toBe("needs-attention");
      store().applyDoneStatus("s3", "failed");
      store().markRunning("s3");
      expect(stateOf("s3")).toBe("failed");
    });

    it("does NOT clobber an in-flight awaiting-answer", () => {
      store().addPendingAsk(ask("c1", "s1"));
      store().markRunning("s1");
      expect(stateOf("s1")).toBe("awaiting-answer");
    });

    it("still arms an unmanaged session", () => {
      store().markRunning("s1");
      expect(stateOf("s1")).toBe("running");
    });
  });

  describe("applyDoneStatus clears stale asks (review fix)", () => {
    it("drops pending asks for the session when it goes terminal", () => {
      store().addPendingAsk(ask("c1", "s1"));
      store().applyDoneStatus("s1", "success");
      expect(stateOf("s1")).toBe("done");
      expect(store().pendingAsks.c1).toBeUndefined();
    });
  });

  describe("handleSessionDeath answer cleanup (Rust owns the outcome)", () => {
    it("mid-task death does not manufacture an outcome", () => {
      store().markRunning("s1");
      store().handleSessionDeath("s1");
      expect(stateOf("s1")).toBe("running");
    });

    it("mid-question death drops the ask without manufacturing an outcome", () => {
      store().addPendingAsk(ask("c1", "s1"));
      store().handleSessionDeath("s1");
      expect(stateOf("s1")).toBe("awaiting-answer");
      expect(store().pendingAsks.c1).toBeUndefined();
    });

    it("already-terminal death is left terminal (only sheds stray asks)", () => {
      store().applyDoneStatus("s1", "success");
      store().handleSessionDeath("s1");
      expect(stateOf("s1")).toBe("done");
    });

    it("unmanaged session death is a no-op", () => {
      store().handleSessionDeath("ghost");
      expect(stateOf("ghost")).toBeUndefined();
    });

    it("only touches the dying session", () => {
      store().markRunning("s1");
      store().addPendingAsk(ask("c2", "s2"));
      store().handleSessionDeath("s1");
      expect(stateOf("s1")).toBe("running");
      expect(stateOf("s2")).toBe("awaiting-answer");
      expect(store().pendingAsks.c2).toEqual(ask("c2", "s2"));
    });
  });

  describe("clearSession", () => {
    it("drops state + pending asks for the session only", () => {
      store().addPendingAsk(ask("c1", "s1"));
      store().addPendingAsk(ask("c2", "s2"));
      store().clearSession("s1");
      expect(stateOf("s1")).toBeUndefined();
      expect(store().pendingAsks).toEqual({ c2: ask("c2", "s2") });
      expect(stateOf("s2")).toBe("awaiting-answer");
    });
  });

  // SW-OR-6 (AC-3b / GAP-1): retain the `mcp:agent-done` summary alongside the
  // terminal state, additively — for the SW-OR-12 output panel + SW-AU-55 read-model.
  describe("applyDoneStatus summary retention (SW-OR-6 AC-3b)", () => {
    it("retains a summary passed with the done status", () => {
      store().applyDoneStatus("s1", "success", { summary: "Opened PR #42" });
      expect(stateOf("s1")).toBe("done");
      expect(summaryOf("s1")).toBe("Opened PR #42");
    });

    it("retains a summary on an attested failed outcome too", () => {
      store().applyDoneStatus("s1", "failed", { summary: "Build broke on lint" });
      expect(stateOf("s1")).toBe("failed");
      expect(summaryOf("s1")).toBe("Build broke on lint");
    });

    it("is byte-identical when no summary is provided (same map reference)", () => {
      const before = store().summaries;
      store().applyDoneStatus("s1", "success");
      expect(summaryOf("s1")).toBeUndefined();
      // No summary → the summaries map is untouched by reference (no selector churn).
      expect(store().summaries).toBe(before);
    });

    it("treats an empty / null summary as absent (no empty-string stored)", () => {
      store().applyDoneStatus("s1", "success", { summary: "" });
      store().applyDoneStatus("s2", "success", { summary: null });
      expect(summaryOf("s1")).toBeUndefined();
      expect(summaryOf("s2")).toBeUndefined();
    });

    it("does NOT alter the state axis — states stays a plain ManagedSessionState", () => {
      store().applyDoneStatus("s1", "success", { summary: "done and dusted" });
      // The summary rides a SIBLING map; `selectManagedState` is unchanged.
      expect(stateOf("s1")).toBe("done");
    });

    it("clearSession drops the retained summary too (no stale inherit on reuse)", () => {
      store().applyDoneStatus("s1", "success", { summary: "first run output" });
      store().clearSession("s1");
      expect(summaryOf("s1")).toBeUndefined();
      expect(stateOf("s1")).toBeUndefined();
    });

    it("reset clears summaries", () => {
      store().applyDoneStatus("s1", "success", { summary: "x" });
      resetManagedSessionStoreForTests();
      expect(summaryOf("s1")).toBeUndefined();
    });
  });
});
