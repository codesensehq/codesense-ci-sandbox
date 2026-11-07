// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  personalContextId,
  teamAllContextId,
  teamRepoContextId,
  viewOnlyRepoContextId,
} from "@/shared/lib/context-model";
import {
  contextOrg,
  contextSharing,
  isCrossOrgMove,
  isPersonalContext,
  planMoveGates,
  type MoveGateFacts,
  type MoveGateKind,
} from "@/shared/lib/move-gate-ladder";

const personal = personalContextId();
const teamAll = teamAllContextId("acme", "default");
const teamRepo = teamRepoContextId("acme", "default", { owner: "acme", repo: "api" });
const teamRepo2 = teamRepoContextId("acme", "default", { owner: "acme", repo: "web" });
const otherOrgTeam = teamAllContextId("other", "default");
const viewOnly = viewOnlyRepoContextId({ owner: "acme", repo: "api" });

/** Order-preserving list of the gate kinds a plan yields. */
function kinds(facts: MoveGateFacts): MoveGateKind[] {
  return planMoveGates(facts).steps.map((s) => s.kind);
}

describe("context classification", () => {
  it("identifies the Personal context", () => {
    expect(isPersonalContext(personal)).toBe(true);
    expect(isPersonalContext(teamAll)).toBe(false);
    expect(isPersonalContext(viewOnly)).toBe(false);
  });

  it("resolves the org only for team contexts; Personal + view-only are org-agnostic", () => {
    expect(contextOrg(teamAll)).toBe("acme");
    expect(contextOrg(teamRepo)).toBe("acme");
    expect(contextOrg(personal)).toBeNull();
    expect(contextOrg(viewOnly)).toBeNull();
  });

  it("classifies privacy: Personal is personal, every team context is shared", () => {
    expect(contextSharing(personal)).toBe("personal");
    expect(contextSharing(teamAll)).toBe("shared");
    expect(contextSharing(teamRepo)).toBe("shared");
  });
});

describe("isCrossOrgMove", () => {
  it("is true only when both sides resolve to different concrete orgs", () => {
    expect(isCrossOrgMove(teamAll, otherOrgTeam)).toBe(true);
    expect(isCrossOrgMove(teamAll, teamRepo)).toBe(false);
  });

  it("is false when either side is org-agnostic (Personal follows you)", () => {
    expect(isCrossOrgMove(personal, otherOrgTeam)).toBe(false);
    expect(isCrossOrgMove(otherOrgTeam, personal)).toBe(false);
    expect(isCrossOrgMove(viewOnly, otherOrgTeam)).toBe(false);
  });
});

describe("planMoveGates — §P precedence", () => {
  const clean: Pick<MoveGateFacts, "targetCollision"> = {
    targetCollision: false,
  };

  it("a same-org shared->Personal no-collision move plans ZERO gates and just executes", () => {
    // shared -> Personal makes it more private (no privacy gate); source not
    // Personal (no sole-copy); no collision. AC: zero extra steps.
    const plan = planMoveGates({ from: teamAll, to: personal, ...clean });
    expect(plan.blocked).toBe(false);
    expect(plan.steps).toEqual([]);
  });

  it("a same-org team->team no-collision move plans ZERO gates", () => {
    expect(kinds({ from: teamRepo, to: teamRepo2, ...clean })).toEqual([]);
    expect(kinds({ from: teamAll, to: teamRepo, ...clean })).toEqual([]);
  });

  it("cross-org is gate 1 and STOPS the ladder (blocked; no other gate)", () => {
    const plan = planMoveGates({
      from: teamAll,
      to: otherOrgTeam,
      targetCollision: true, // even with a collision, cross-org wins and stops
    });
    expect(plan.blocked).toBe(true);
    expect(plan.steps.map((s) => s.kind)).toEqual(["crossOrg"]);
  });

  it("Personal->Team (Share) fires privacy only — a move never asks about a sole copy", () => {
    expect(kinds({ from: personal, to: teamAll, targetCollision: false })).toEqual(["privacy"]);
  });

  it("shared->Personal needs NO privacy gate", () => {
    expect(kinds({ from: teamAll, to: personal, targetCollision: false })).toEqual([]);
  });

  it("Personal->Team colliding plans privacy, then collision", () => {
    expect(kinds({ from: personal, to: teamAll, targetCollision: true })).toEqual([
      "privacy",
      "collision",
    ]);
  });

  it("a move that stays in the scratch space has no gate", () => {
    // The item stays where it was kept (e.g. into or out of all orgs).
    expect(kinds({ from: personal, to: personal, targetCollision: false })).toEqual([]);
  });

  it("collision alone (team->team, colliding) plans just the collision gate, last", () => {
    expect(kinds({ from: teamAll, to: teamRepo, targetCollision: true })).toEqual(["collision"]);
  });

  it("a clean team->team move plans no gate", () => {
    expect(kinds({ from: teamAll, to: teamRepo, targetCollision: false })).toEqual([]);
  });
});

describe("planMoveGates — SLW-PT-13 COPY (op:'copy')", () => {
  it("COPY Personal->Team plans the same privacy gate a MOVE does", () => {
    expect(
      kinds({
        from: personal,
        to: teamAll,
        targetCollision: false,
        op: "copy",
      }),
    ).toEqual(["privacy"]);
  });

  it("COPY Personal->Team still fires the privacy gate (Personal->shared)", () => {
    expect(
      kinds({
        from: personal,
        to: teamAll,
        targetCollision: false,
        op: "copy",
      }),
    ).toEqual(["privacy"]);
  });

  it("COPY Personal->Team colliding plans privacy then collision", () => {
    expect(kinds({ from: personal, to: teamAll, targetCollision: true, op: "copy" })).toEqual([
      "privacy",
      "collision",
    ]);
  });

  it("COPY shared->Personal with collision plans just the collision gate", () => {
    expect(
      kinds({
        from: teamAll,
        to: personal,
        targetCollision: true,
        op: "copy",
      }),
    ).toEqual(["collision"]);
  });
});

import gateCases from "@/shared/lib/mobility-gate-cases.json";
import { planRemoveGate } from "@/shared/lib/remove-gate";
import {
  allOrgsContextId,
  parseContextId,
  scratchSpaceContextId,
} from "@/shared/lib/context-model";

describe("SLW-PT-22: UI and MCP walk the same gates", () => {
  // The same table context_mobility.rs checks the MCP ladder against.
  it.each(gateCases.cases)("$name", (c) => {
    const steps =
      c.op === "remove"
        ? planRemoveGate(c.from).kind === "soleCopyBlock"
          ? ["soleCopy"]
          : []
        : kinds({
            from: c.from,
            to: c.to ?? "",
            targetCollision: c.collision,
            op: c.op as "move" | "copy",
          });
    expect(steps).toEqual(c.steps);
  });

  it("names one org's scratch space and the all-orgs folder by the MCP ids", () => {
    expect(scratchSpaceContextId("acme/ctx")).toBe("personal:acme/ctx");
    expect(allOrgsContextId()).toBe("all-orgs");
    expect(parseContextId("personal:acme/ctx")).toEqual({ kind: "personal", orgSlug: "acme/ctx" });
    expect(parseContextId("all-orgs")).toEqual({ kind: "personal", allOrgs: true });
    expect(parseContextId("personal:")).toBeNull();
    expect(contextOrg("personal:acme/ctx")).toBe("acme/ctx");
    expect(contextOrg("all-orgs")).toBeNull();
    expect(isPersonalContext("all-orgs")).toBe(true);
  });
});
