// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import type { PublishFile } from "@/features/git-sync/lib/types";

import { isRoutable, routeStateFromOutcome, type RouteOutcome } from "./route-to-context";

describe("route-to-context — isRoutable (AC-1)", () => {
  const f = (over: Partial<PublishFile>): PublishFile => ({
    path: "x",
    status: "added",
    ...over,
  });

  it("routes only a brand-new (added), non-renamed file", () => {
    expect(isRoutable(f({ status: "added" }))).toBe(true);
  });

  it("refuses modified / deleted rows", () => {
    expect(isRoutable(f({ status: "modified" }))).toBe(false);
    expect(isRoutable(f({ status: "deleted" }))).toBe(false);
  });

  it("refuses a rename even when its status is added-ish", () => {
    expect(isRoutable(f({ status: "added", renamedFrom: "old" }))).toBe(false);
  });
});

describe("route-to-context — routeStateFromOutcome (AC-3/AC-5)", () => {
  const cases: Array<[RouteOutcome, RouteState_kind]> = [
    [{ outcome: "routed" }, "routed"],
    [{ outcome: "alreadyRouted" }, "routed"],
    [{ outcome: "collision", sourceHash: "a", destHash: "b" }, "collision"],
    [{ outcome: "alreadyTracked" }, "alreadyTracked"],
    [{ outcome: "refused" }, "blocked"],
    [{ outcome: "notProjectable" }, "blocked"],
  ];

  it.each(cases)("maps %o to a %s row state", (outcome, kind) => {
    expect(routeStateFromOutcome(outcome).kind).toBe(kind);
  });

  it("carries a human reason for every blocked outcome", () => {
    for (const [outcome] of cases) {
      const s = routeStateFromOutcome(outcome);
      if (s.kind === "blocked") expect(s.reason.length).toBeGreaterThan(0);
    }
  });
});

type RouteState_kind = ReturnType<typeof routeStateFromOutcome>["kind"];
