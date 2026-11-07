// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import {
  CONTEXT_LANDS_ATTACHED,
  CONTEXT_LANDS_MANAGED,
  LOCAL_CLONE_EXPLAINER,
  TEAM_EXPLAINER,
} from "./onboarding-copy";

describe("onboarding-copy (SW-OB-5)", () => {
  it("TEAM_EXPLAINER is the finalized 'what a team is' line", () => {
    // Locks the wording so the two consuming surfaces (create-team dialog +
    // choose-your-team empty state) can't drift. Matches onboarding-ux-design
    // Screen 4a (L118).
    expect(TEAM_EXPLAINER).toBe(
      "A team groups the repos and context for one slice of work — start with one; you can add more later.",
    );
  });

  // SW-XS-11: nothing is delivered on its own. An agent gets what its
  // definition declares when it starts; the copy must not promise more.
  it("the attach copy promises no automatic delivery", () => {
    expect(CONTEXT_LANDS_ATTACHED).toBe(
      "Agents you start in worktrees CodeSense opens for this repo get the context their definition declares. Your main checkout isn't touched.",
    );
    expect(CONTEXT_LANDS_MANAGED).toBe(
      "CodeSense manages this clone. Agents you start in worktrees CodeSense opens for this repo get the context their definition declares.",
    );
    expect(LOCAL_CLONE_EXPLAINER).toBe(
      "Your local checkout is what codesense shows alongside the team's context.",
    );
    for (const line of [CONTEXT_LANDS_ATTACHED, CONTEXT_LANDS_MANAGED, LOCAL_CLONE_EXPLAINER]) {
      expect(line).not.toMatch(/in sync|every worktree|lands in/i);
    }
  });
});
