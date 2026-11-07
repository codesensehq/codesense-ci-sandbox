// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  LIFECYCLE_PHASE_OWNER,
  PTY_TRANSITIONS,
  VIEWPORT_TRANSITIONS,
  cancelTransition,
  isTransitionLegal,
  validateTransition,
} from "./lifecycle";

/** OC-4 AC3 — lifecycle phases have owners, legal transitions, structured errors,
 *  and the viewport axis is independent of the PTY axis. */
describe("OC-4 lifecycle transition tables (AC3)", () => {
  it("accepts legal transitions on each axis", () => {
    expect(isTransitionLegal("pty", "spawn", "ready")).toBe(true);
    expect(isTransitionLegal("pty", "ready", "active")).toBe(true);
    expect(isTransitionLegal("viewport", "attach", "ready")).toBe(true);
    expect(isTransitionLegal("viewport", "hidden", "restored")).toBe(true);
  });

  it("rejects illegal transitions with a structured error", () => {
    const err = validateTransition("pty", "spawn", "active");
    expect(err).not.toBeNull();
    expect(err?.code).toBe("illegal-transition");
    expect(err?.axis).toBe("pty");
    expect(err?.from).toBe("spawn");
    expect(err?.to).toBe("active");
  });

  it("terminated is absorbing on both axes (teardown is final)", () => {
    expect(PTY_TRANSITIONS.terminated).toEqual([]);
    expect(VIEWPORT_TRANSITIONS.terminated).toEqual([]);
    expect(isTransitionLegal("pty", "terminated", "ready")).toBe(false);
  });

  it("teardown is reachable from every live phase (kill / cancel / exit)", () => {
    for (const from of Object.keys(PTY_TRANSITIONS)) {
      if (from === "terminated") continue;
      expect(isTransitionLegal("pty", from as never, "terminated")).toBe(true);
    }
    for (const from of Object.keys(VIEWPORT_TRANSITIONS)) {
      if (from === "terminated") continue;
      expect(isTransitionLegal("viewport", from as never, "terminated")).toBe(true);
    }
  });

  it("enforces axis independence: a viewport-only phase is cross-axis on the PTY axis", () => {
    // `attach`/`hidden`/`detached`/`restored` are viewport concepts — driving the
    // PTY axis to them (or vice versa) is the exact coupling AC3 forbids.
    const err = validateTransition("pty", "spawn", "hidden");
    expect(err?.code).toBe("cross-axis");
    const err2 = validateTransition("viewport", "attach", "spawn");
    expect(err2?.code).toBe("cross-axis");
  });

  it("every named phase has a recorded owner; dual-axis phases are 'shared'", () => {
    for (const phase of Object.keys(PTY_TRANSITIONS)) {
      expect(LIFECYCLE_PHASE_OWNER).toHaveProperty(phase);
    }
    for (const phase of Object.keys(VIEWPORT_TRANSITIONS)) {
      expect(LIFECYCLE_PHASE_OWNER).toHaveProperty(phase);
    }
    expect(LIFECYCLE_PHASE_OWNER.spawn).toBe("pty");
    expect(LIFECYCLE_PHASE_OWNER.attach).toBe("viewport");
    // ready/active/terminated are real states on both axes — owner is 'shared',
    // not arbitrarily flattened to one axis.
    expect(LIFECYCLE_PHASE_OWNER.ready).toBe("shared");
    expect(LIFECYCLE_PHASE_OWNER.active).toBe("shared");
    expect(LIFECYCLE_PHASE_OWNER.terminated).toBe("shared");
  });

  it("cancellation is a first-class outcome (AC3 cancellation rules)", () => {
    const err = cancelTransition("viewport", "active");
    expect(err.code).toBe("cancelled");
    expect(err.axis).toBe("viewport");
    expect(err.from).toBe("active");
    expect(err.to).toBe("active"); // cancellation lands nowhere new
  });

  it("the two axes share no transition targets (structural independence, AC3)", () => {
    // No PTY transition may target a viewport-only phase, and vice versa — a
    // stronger check than the hand-picked cross-axis pairs above.
    const viewportOnly = new Set(["attach", "hidden", "detached", "restored"]);
    const ptyOnly = new Set(["spawn"]);
    for (const targets of Object.values(PTY_TRANSITIONS)) {
      for (const t of targets) expect(viewportOnly.has(t)).toBe(false);
    }
    for (const targets of Object.values(VIEWPORT_TRANSITIONS)) {
      for (const t of targets) expect(ptyOnly.has(t)).toBe(false);
    }
  });
});
