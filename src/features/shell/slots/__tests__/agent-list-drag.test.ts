// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { describe, expect, it } from "vitest";
import {
  cardDropTarget,
  CARD_MOVE_REFUSED,
  teamSlotCards,
} from "@/features/viewer/lib/agents-pane-drag";
import { stepSlot } from "@/features/viewer/lib/workspace-order";
import { NO_WORKSPACE_ID } from "@/features/agents/lib/agent-list-layout";
import type { PaneRow } from "@/features/viewer/lib/agents-pane-rows";

const html = readFileSync("src/features/shell/slots/__fixtures__/agent-list-pane.html", "utf8");
const source = html.match(/<script id="al-card-math">([\s\S]*?)<\/script>/)?.[1];
const context = { window: {} as any };
if (source) runInNewContext(source, context);
const math = context.window.agentListCardMath;
const rows = [
  { kind: "heading", height: 26 },
  { kind: "team", team: "a", height: 30 },
  { kind: "workspace", workspace: "one", height: 34 },
  { kind: "agent", height: 92 },
  { kind: "workspace", workspace: "", height: 34 },
  { kind: "agent", height: 52 },
  { kind: "workspace", workspace: "no-pill", height: 34 },
  { kind: "workspace", workspace: "two", height: 34 },
  { kind: "agent", height: 5000 },
  { kind: "workspace", workspace: "three", height: 34 },
  { kind: "team", team: "b", height: 30 },
  { kind: "workspace", workspace: "other", height: 34 },
];
const native = rows.map((r) => ({
  ...r,
  kind: r.kind === "workspace" ? "ws" : r.kind,
  teamKey: r.team,
  group: { workspaceId: r.workspace || NO_WORKSPACE_ID },
})) as unknown as PaneRow[];
// Architecture: compare the shipped pane's math with native, including undrawn rows.
describe("pane card math at native parity", () => {
  it("provides the pane math", () => expect(math).toBeDefined());
  it("uses native refusal words", () => expect(math.CARD_MOVE_REFUSED).toEqual(CARD_MOVE_REFUSED));
  for (const order of [["one", "two", "three"], ["one", "three"], ["one"], []]) {
    it(`matches every boundary, midpoint and horizontal slack for ${order}`, () => {
      const boxes = math.boxes(rows);
      const ys = boxes.flatMap((b: any) => [
        b.start - 25,
        b.start - 8,
        b.start,
        (b.start + b.end) / 2,
        b.end,
        b.end + 24,
      ]);
      for (const id of ["one", "two", "three", "no-pill"]) {
        for (const y of ys)
          for (const x of [-49, -48, 118, 284, 285]) {
            const pane = { x, left: 0, right: 236 };
            expect(math.cardDropTarget(rows, boxes, "a", id, y, order, pane)).toEqual(
              cardDropTarget(native, boxes, "a", id, y, order, pane),
            );
          }
        const cards = math.teamSlotCards(rows, "a", order);
        expect(cards).toEqual(teamSlotCards(native, "a", order));
        for (const dir of ["up", "down"] as const)
          expect(math.stepSlot(cards, id, dir)).toEqual(stepSlot(cards, id, dir));
      }
    });
  }
});
