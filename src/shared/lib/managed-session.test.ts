// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import {
  type ManagedSessionState,
  type AskUserResponse,
  TERMINAL_MANAGED_STATES,
  isTerminalManagedState,
  managedStateForDoneStatus,
  acceptAnswer,
  declineAnswer,
  cancelAnswer,
} from "./managed-session";

describe("managed-session shapes @P1", () => {
  describe("isTerminalManagedState", () => {
    it("treats done, failed, and needs-attention as terminal", () => {
      expect(isTerminalManagedState("done")).toBe(true);
      expect(isTerminalManagedState("failed")).toBe(true);
      expect(isTerminalManagedState("needs-attention")).toBe(true);
    });

    it("treats running and awaiting-answer as non-terminal", () => {
      expect(isTerminalManagedState("running")).toBe(false);
      expect(isTerminalManagedState("awaiting-answer")).toBe(false);
    });

    it("TERMINAL_MANAGED_STATES contains every terminal-class parked state", () => {
      expect([...TERMINAL_MANAGED_STATES].sort()).toEqual(["done", "failed", "needs-attention"]);
    });
  });

  describe("managedStateForDoneStatus", () => {
    it("maps success → done", () => {
      expect(managedStateForDoneStatus("success")).toBe("done");
    });

    it("preserves failed attestation and needs-attention uncertainty", () => {
      const failed: ManagedSessionState = managedStateForDoneStatus("failed");
      expect(failed).toBe("failed");
      expect(managedStateForDoneStatus("needs-attention")).toBe("needs-attention");
    });
  });

  describe("AskUserResponse constructors (accept/decline/cancel)", () => {
    it("acceptAnswer carries content only on accept", () => {
      const r: AskUserResponse = acceptAnswer("use option B");
      expect(r).toEqual({ action: "accept", content: "use option B" });
    });

    it("declineAnswer has no content", () => {
      const r = declineAnswer();
      expect(r.action).toBe("decline");
      expect(r.content).toBeUndefined();
    });

    it("cancelAnswer has no content (dismiss/timeout/session-death)", () => {
      const r = cancelAnswer();
      expect(r.action).toBe("cancel");
      expect(r.content).toBeUndefined();
    });
  });
});
