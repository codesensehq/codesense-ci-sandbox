// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { asPhase, PHASE_ROLE, PHASES, type Phase, type Role, roleForPhase } from "./roles";

describe("roles — phase→role contract (SW-GW-4)", () => {
  it("maps every phase to its exact role", () => {
    const expected: Record<Phase, Role> = {
      planning: "not_started",
      dev: "active",
      review: "review",
      pr: "review",
      blocked: "blocked",
      done: "done",
    };
    expect(PHASE_ROLE).toEqual(expected);
    for (const p of PHASES) expect(roleForPhase(p)).toBe(expected[p]);
  });

  it("lists exactly the six lifecycle phases in order", () => {
    expect(PHASES).toEqual(["planning", "dev", "review", "pr", "blocked", "done"]);
  });

  it("narrows only in-enum strings via asPhase", () => {
    for (const p of PHASES) expect(asPhase(p)).toBe(p);
    for (const bad of ["", "PLANNING", "shipped", "in-progress", "todo", "qa"]) {
      expect(asPhase(bad)).toBeNull();
    }
  });
});
