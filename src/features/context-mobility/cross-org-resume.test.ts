// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { captureCrossOrgOp, resumeRestatement } from "@/features/context-mobility/cross-org-resume";
import { personalContextId, teamAllContextId, type Context } from "@/shared/lib/context-model";

const source: Context = {
  id: personalContextId(),
  kind: "personal",
  sharing: "personal",
  label: "Personal",
};

const target: Context = {
  id: teamAllContextId("globex", "core"),
  kind: "team-all",
  sharing: "shared",
  label: "All of core",
  teamName: "core",
};

describe("cross-org-resume", () => {
  it("captures the full pending op self-contained", () => {
    const pending = captureCrossOrgOp({
      op: "move",
      source,
      logicalName: "notes/plan.md",
      fileName: "plan.md",
      targetOrgSlug: "globex",
      targetOrgName: "Globex",
      target,
    });
    expect(pending).toEqual({
      op: "move",
      source,
      logicalName: "notes/plan.md",
      fileName: "plan.md",
      targetOrgSlug: "globex",
      targetOrgName: "Globex",
      target,
    });
  });

  it("restates the op after the org switch (move + copy verbs)", () => {
    const move = captureCrossOrgOp({
      op: "move",
      source,
      logicalName: "plan.md",
      fileName: "plan.md",
      targetOrgSlug: "globex",
      targetOrgName: "Globex",
      target,
    });
    expect(resumeRestatement(move)).toBe("Continue moving 'plan.md' into Globex › All of core?");

    const copy = captureCrossOrgOp({
      op: "copy",
      source,
      logicalName: "plan.md",
      fileName: "plan.md",
      targetOrgSlug: "globex",
      targetOrgName: "Globex",
      target,
    });
    expect(resumeRestatement(copy)).toBe("Continue copying 'plan.md' into Globex › All of core?");
  });

  it("no banned internal word in the restatement", () => {
    const pending = captureCrossOrgOp({
      op: "move",
      source,
      logicalName: "plan.md",
      fileName: "plan.md",
      targetOrgSlug: "globex",
      targetOrgName: "Globex",
      target,
    });
    expect(resumeRestatement(pending)).not.toMatch(
      /\b(provider|overlay|layer|scratch|TeamWorkspace|TeamRepo|Workspace)\b/,
    );
  });
});
