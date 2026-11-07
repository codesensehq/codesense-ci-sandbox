// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  applyWorkspaceOrder,
  arrangeByOrder,
  buildTeamOrder,
  moveInOrder,
  placeForSlot,
  resolveSlot,
  stepSlot,
  teamWorkspaceOrder,
  type SlotCard,
} from "./workspace-order";
import type { Workspace } from "@/shared/stores/viewer-store";

const apply = (base: string, stored?: string[]) =>
  applyWorkspaceOrder(base.split(" "), (id) => id, stored);

describe("applyWorkspaceOrder", () => {
  it("is the computed order when nothing is stored", () => {
    expect(apply("a b c")).toEqual(["a", "b", "c"]);
  });

  it("follows the stored order across the tray's groups", () => {
    // root a (alone in its group), epic b c, ungrouped d: the stored order mixes them freely.
    expect(apply("a b c d", ["c", "b", "d", "a"])).toEqual(["c", "b", "d", "a"]);
    expect(apply("a b c d", ["b", "a", "c", "d"])).toEqual(["b", "a", "c", "d"]);
  });

  it("places a workspace the stored order does not know after its computed predecessor", () => {
    expect(apply("a b n c", ["c", "a", "b"])).toEqual(["c", "a", "b", "n"]);
    expect(apply("a b n c", ["b", "c", "a"])).toEqual(["b", "n", "c", "a"]);
    // First in the computed order: first.
    expect(apply("n a b", ["b", "a"])).toEqual(["n", "b", "a"]);
    // Several new in a row keep their computed order after the predecessor.
    expect(apply("a n1 n2 b", ["b", "a"])).toEqual(["b", "a", "n1", "n2"]);
  });

  it("with groups, a newcomer lands beside its own epic's members, wherever they were moved", () => {
    // Epic X: x1 x2; epic Y: y0 (new) y1. The person moved x2 to the end and y1 to the front.
    const group = (id: string) => id[0];
    const order = applyWorkspaceOrder(
      ["x1", "x2", "y0", "y1"],
      (id) => id,
      ["y1", "x1", "x2"],
      group,
    );
    // y0 is first in its epic: it goes before y1, not after x2 (its plain predecessor).
    expect(order).toEqual(["y0", "y1", "x1", "x2"]);
    // A newcomer later in its epic goes after its epic's nearest earlier member.
    expect(
      applyWorkspaceOrder(["x1", "x2", "y1", "y2"], (id) => id, ["y1", "x2", "x1"], group),
    ).toEqual(["y1", "y2", "x2", "x1"]);
    // Alone in its group: after its plain computed predecessor (x2), wherever that went.
    expect(applyWorkspaceOrder(["x1", "x2", "z1"], (id) => id, ["x2", "x1"], group)).toEqual([
      "x2",
      "z1",
      "x1",
    ]);
  });

  it("ignores stored ids that are gone", () => {
    expect(apply("a b", ["gone", "b", "a"])).toEqual(["b", "a"]);
  });
});

describe("moveInOrder", () => {
  const order = ["root", "a", "b", "c", "g"];
  it("moves before or after any workspace of the team", () => {
    expect(moveInOrder(order, "c", { before: "a" })).toEqual(["root", "c", "a", "b", "g"]);
    expect(moveInOrder(order, "a", { after: "c" })).toEqual(["root", "b", "c", "a", "g"]);
  });
  it("moves a workspace alone in its tray group (the top one) past the others", () => {
    expect(moveInOrder(order, "root", { after: "a" })).toEqual(["a", "root", "b", "c", "g"]);
    expect(moveInOrder(order, "root", { after: "g" })).toEqual(["a", "b", "c", "g", "root"]);
    expect(moveInOrder(order, "a", { before: "root" })).toEqual(["a", "root", "b", "c", "g"]);
  });
  it("refuses an unknown id or anchor, itself, and a no-op", () => {
    expect(moveInOrder(order, "x", { before: "a" })).toBeNull();
    expect(moveInOrder(order, "a", { before: "x" })).toBeNull();
    expect(moveInOrder(order, "a", { before: "a" })).toBeNull();
    expect(moveInOrder(order, "a", { before: "b" })).toBeNull();
    expect(moveInOrder(order, "a", { after: "root" })).toBeNull();
  });
});

describe("arrangeByOrder (the pane's cards)", () => {
  it("puts cards with a pill in pill order and leaves a pill-less card in its slot", () => {
    expect(arrangeByOrder(["a", "shell", "b", "c"], ["c", "a", "b"])).toEqual([
      "c",
      "shell",
      "a",
      "b",
    ]);
  });
});

describe("pane drop slots", () => {
  const cards: SlotCard[] = [
    { id: "root", pill: true },
    { id: "a", pill: true },
    { id: "shell", pill: false },
    { id: "b", pill: true },
    { id: "x", pill: true },
  ];
  it("maps a slot to a place next to the nearest other pill card", () => {
    expect(placeForSlot(cards, "b", 0)).toEqual({ before: "root" });
    expect(placeForSlot(cards, "root", 2)).toEqual({ after: "a" });
    expect(placeForSlot(cards, "root", 5)).toEqual({ after: "x" });
    expect(placeForSlot(cards, "x", 1)).toEqual({ after: "root" });
  });
  it("its own place and slots that only pass pill-less cards are home (no place)", () => {
    expect(placeForSlot(cards, "b", 3)).toBeNull();
    expect(placeForSlot(cards, "b", 2)).toBeNull();
    expect(placeForSlot(cards, "b", 4)).toBeNull();
    expect(placeForSlot(cards, "root", 0)).toBeNull();
  });
  it("resolveSlot clamps only to the team's edges", () => {
    expect(resolveSlot(cards, "root", 9)).toEqual({ slot: 5, place: { after: "x" } });
    expect(resolveSlot(cards, "x", -3)).toEqual({ slot: 0, place: { before: "root" } });
    expect(resolveSlot(cards, "x", 5)).toEqual({ slot: 5, place: null });
  });
  it("a pill-less card cannot be dragged", () => {
    expect(placeForSlot(cards, "shell", 0)).toBeNull();
    expect(stepSlot(cards, "shell", "down")).toBeNull();
  });
  it("stepSlot moves one pill card at a time, across groups, and stops at the team's ends", () => {
    expect(stepSlot(cards, "root", "down")).toEqual({ slot: 2, place: { after: "a" } });
    expect(stepSlot(cards, "b", "up")).toEqual({ slot: 1, place: { before: "a" } });
    expect(stepSlot(cards, "a", "down")).toEqual({ slot: 4, place: { after: "b" } });
    expect(stepSlot(cards, "root", "up")).toBeNull();
    expect(stepSlot(cards, "x", "down")).toBeNull();
  });
});

describe("teamWorkspaceOrder", () => {
  const ws = (id: string, kind: Workspace["kind"] = "generic"): Workspace =>
    ({ id, kind, cwd: `/w/${id}`, status: "ready" }) as Workspace;
  it("reads the cached computed order, adds workspaces opened since, then applies the stored order", () => {
    const order = teamWorkspaceOrder({
      base: ["g-1", "g-2"],
      workspaces: [ws("g-1"), ws("g-2"), ws("g-3")],
      softClosed: {},
      stored: ["g-2", "g-1"],
    });
    // g-3 is new: it follows g-2, its computed predecessor.
    expect(order).toEqual(["g-2", "g-3", "g-1"]);
  });
  it("with the cached groups, a newly opened workspace joins its group's members", () => {
    const order = teamWorkspaceOrder({
      base: ["g-1", "g-2", "g-3"],
      workspaces: [ws("g-1"), ws("g-2"), ws("g-3"), ws("g-0")],
      softClosed: {},
      stored: ["g-3", "g-1", "g-2"],
      groups: { "g-0": "B", "g-1": "A", "g-2": "A", "g-3": "B" },
    });
    // g-0 is new and B's: it lands before g-3, B's other member, not first by id.
    expect(order).toEqual(["g-0", "g-3", "g-1", "g-2"]);
  });
  it("falls back to the tree-less computed order when nothing is cached", () => {
    const order = teamWorkspaceOrder({
      base: undefined,
      workspaces: [ws("g-2"), ws("g-1")],
      softClosed: {},
      stored: undefined,
    });
    expect(order).toEqual(["g-1", "g-2"]);
  });
});

describe("buildTeamOrder (the tray's and the store's one builder)", () => {
  it("applies the base, then the stored moves, newcomers beside their group", () => {
    const groups: Record<string, string> = { a1: "A", a2: "A", b1: "B", b2: "B" };
    const order = buildTeamOrder(
      ["a1", "a2", "b1", "b2"],
      (id) => id,
      ["b1", "a1", "a2"],
      ["a2", "b1", "a1"],
      (id) => groups[id],
    );
    // b2 is new to both lists: it lands after b1, its group's member.
    expect(order).toEqual(["a2", "b1", "b2", "a1"]);
  });
  it("draws the same order as teamWorkspaceOrder from the same state", () => {
    const ws = (id: string): Workspace =>
      ({ id, kind: "generic", cwd: `/w/${id}`, status: "ready" }) as Workspace;
    const src = {
      base: ["g-3", "g-1"],
      workspaces: [ws("g-1"), ws("g-2"), ws("g-3")],
      softClosed: {},
      stored: ["g-1", "g-3"],
    };
    const computed = ["g-1", "g-2", "g-3"];
    expect(teamWorkspaceOrder(src)).toEqual(
      buildTeamOrder(computed, (id) => id, src.base, src.stored),
    );
  });
});
