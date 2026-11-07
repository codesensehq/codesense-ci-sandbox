// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { arrowFilterFate, machineFactsOf, type ArrowFilterFacts } from "./arrow-filter-fate";
import type { MachineSummary } from "@/tauri/commands";

const REMOTE = { location: { kind: "machine" as const, machineId: "box", requestId: "r" } };

function facts(over: Partial<ArrowFilterFacts> = {}): ArrowFilterFacts {
  return {
    filter: { teamKey: "acme:core", workspaceId: "ws" },
    record: {},
    teamHidden: false,
    local: "done",
    roster: "done",
    remoteLoading: false,
    machineLoading: () => false,
    layoutHeld: () => false,
    machineGone: () => false,
    holdsAgent: true,
    ...over,
  };
}
const fate = (over: Partial<ArrowFilterFacts> = {}) => arrowFilterFate(facts(over)).fate;

describe("the arrow filter's one rule", () => {
  it("goes with its team hidden, whatever still loads", () => {
    expect(fate({ teamHidden: true, local: "loading", roster: "loading" })).toBe("drop");
  });

  it("waits for This Mac's own state, and is kept, never loading, once that stalls", () => {
    expect(fate({ local: "loading", record: undefined })).toBe("loading");
    expect(fate({ local: "stalled", record: undefined })).toBe("keep");
  });

  it("drops a local workspace whose record is gone once This Mac's state is known", () => {
    expect(fate({ record: undefined, remoteLoading: true, roster: "loading" })).toBe("drop");
  });

  describe("a remote workspace", () => {
    it("waits for a machine whose layout isn't read, however long it stays away", () => {
      expect(arrowFilterFate(facts({ record: REMOTE, holdsAgent: false }))).toEqual({
        fate: "waiting",
        machineId: "box",
      });
      // Known only by the machine the filter carries, its record not here yet.
      expect(
        arrowFilterFate(
          facts({
            record: undefined,
            filter: { teamKey: "t", workspaceId: "ws", machineId: "box" },
          }),
        ),
      ).toEqual({ fate: "waiting", machineId: "box" });
    });

    it("reads loading while its own machine's layout is on its way within its wait", () => {
      expect(fate({ record: REMOTE, machineLoading: (m) => m === "box" })).toBe("loading");
      // Another machine's layout loading says nothing of this one, which stays away.
      expect(
        fate({ record: REMOTE, remoteLoading: true, machineLoading: (m) => m === "other" }),
      ).toBe("waiting");
    });

    it("whose record is not here goes once its machine's layout is held without it", () => {
      expect(
        fate({
          record: undefined,
          filter: { teamKey: "t", workspaceId: "ws", machineId: "box" },
          layoutHeld: () => true,
        }),
      ).toBe("drop");
    });

    it("whose record is not here goes once its machine is gone", () => {
      expect(
        fate({
          record: undefined,
          filter: { teamKey: "t", workspaceId: "ws", machineId: "box" },
          machineGone: () => true,
        }),
      ).toBe("drop");
    });

    it("once its layout is held follows the agents rule", () => {
      const held = { record: REMOTE, layoutHeld: () => true };
      expect(fate({ ...held, holdsAgent: true })).toBe("keep");
      expect(fate({ ...held, holdsAgent: false })).toBe("drop");
    });
  });

  it("drops a workspace that holds no agent only on a roster read whole, with no layout loading", () => {
    expect(fate({ holdsAgent: false })).toBe("drop");
    expect(fate({ holdsAgent: false, roster: "loading" })).toBe("loading");
    expect(fate({ holdsAgent: false, remoteLoading: true })).toBe("loading");
    expect(fate({ holdsAgent: false, roster: "stalled" })).toBe("keep");
    expect(fate({ holdsAgent: false, roster: null })).toBe("keep");
    expect(fate({ holdsAgent: null })).toBe("keep");
  });

  it("reads the no-workspace bucket by its agents alone, with no record or machine", () => {
    const bucket = { teamKey: "t", workspaceId: "", machineId: "box" };
    expect(fate({ filter: bucket, record: undefined, holdsAgent: true })).toBe("keep");
    expect(fate({ filter: bucket, record: undefined, holdsAgent: false })).toBe("drop");
  });
});

describe("machineFactsOf", () => {
  const box = { machineId: "box" } as MachineSummary;
  const layout = {
    members: { "box/remote": {} as never },
    admission: { "gone/remote": { state: "removed" } as never },
  };

  it("holds a machine's layout once the projection has its remote member", () => {
    const f = machineFactsOf(layout, { byTeam: {}, failedTeams: [] }, "t");
    expect(f.layoutHeld("box")).toBe(true);
    expect(f.layoutHeld("other")).toBe(false);
  });

  it("reads a machine gone once it was removed, or the team's list loaded without it", () => {
    expect(machineFactsOf(layout, { byTeam: {}, failedTeams: [] }, "t").machineGone("gone")).toBe(
      true,
    );
    // The team's list is not loaded, or its read failed: nothing says it is gone.
    expect(machineFactsOf(layout, { byTeam: {}, failedTeams: [] }, "t").machineGone("box")).toBe(
      false,
    );
    expect(
      machineFactsOf(layout, { byTeam: { t: [] }, failedTeams: ["t"] }, "t").machineGone("x"),
    ).toBe(false);
    expect(
      machineFactsOf(layout, { byTeam: { t: [] }, failedTeams: [] }, "t").machineGone("x"),
    ).toBe(true);
    // Any list naming it (a personal machine listed under another team) keeps it.
    expect(
      machineFactsOf(layout, { byTeam: { t: [], u: [box] }, failedTeams: [] }, "t").machineGone(
        "box",
      ),
    ).toBe(false);
  });
});
