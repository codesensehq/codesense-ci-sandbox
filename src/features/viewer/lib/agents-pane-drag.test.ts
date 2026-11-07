// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { NO_WORKSPACE_ID } from "@/features/agents/lib/agent-list-layout";
import type { PaneRow } from "./agents-pane-rows";
import {
  CARD_DROP_SLACK_BELOW_PX,
  CARD_DROP_SLACK_X_PX,
  cardDropTarget,
  DROP_LINE_INSET_PX,
  teamCardBlocks,
  teamSlotCards,
  type RowBox,
} from "./agents-pane-drag";

const T = "acme:default";
const team = (teamKey: string): PaneRow =>
  ({ kind: "team", key: `team:${teamKey}`, teamKey, folded: false, needCount: 0 }) as PaneRow;
const ws = (workspaceId: string, teamKey = T): PaneRow =>
  ({
    kind: "ws",
    key: `ws:${teamKey}|${workspaceId}`,
    teamKey,
    group: { workspaceId, label: workspaceId || "No workspace", nodes: [], kind: "folder" },
    folded: false,
    arrowOn: false,
    dragging: false,
  }) as PaneRow;
const agent = (workspaceId: string | null, handle: string): PaneRow =>
  ({ kind: "agent", key: `agent:${handle}`, teamKey: T, workspaceId, depth: 0 }) as PaneRow;
/** Every row 40px tall, one after another. */
const boxes = (rows: readonly PaneRow[]): RowBox[] =>
  rows.map((_, i) => ({ start: i * 40, end: i * 40 + 40 }));

describe("the No workspace bucket in the middle of a team's cards", () => {
  // A bucket between two cards (wherever a list puts it): it is never a card.
  const rows: PaneRow[] = [
    team(T),
    ws("ws-a"),
    agent("ws-a", "a1"),
    ws(NO_WORKSPACE_ID),
    agent(null, "n1"),
    ws("ws-b"),
    agent("ws-b", "b1"),
    team("acme:ops"),
    ws("ws-x", "acme:ops"),
  ];

  it("the drop line and the keyboard step read the same cards", () => {
    expect(teamCardBlocks(rows, boxes(rows), T).map((b) => b.id)).toEqual(["ws-a", "ws-b"]);
    expect(teamSlotCards(rows, T, ["ws-a", "ws-b"]).map((c) => c.id)).toEqual(["ws-a", "ws-b"]);
  });

  it("the line after a team's last card never lands inside a No workspace bucket below it", () => {
    const tail: PaneRow[] = [
      team(T),
      ws("ws-a"),
      agent("ws-a", "a1"),
      ws("ws-b"),
      agent("ws-b", "b1"),
      ws(NO_WORKSPACE_ID),
      agent(null, "n1"),
    ];
    const b = boxes(tail);
    // ws-b's block ends at 200, where the bucket's row starts.
    const t = cardDropTarget(tail, b, T, "ws-a", 190, ["ws-a", "ws-b"]);
    expect(t.place).toEqual({ after: "ws-b" });
    expect(t.lineY).toBe(200);
    expect(t.lineY!).toBeLessThanOrEqual(b[5]!.start);
  });

  it("a card dragged below the bucket's rows lands after the card past it", () => {
    // ws-b's block is rows 5–6 (200–280): below its middle (240) is after it.
    const t = cardDropTarget(rows, boxes(rows), T, "ws-a", 260, ["ws-a", "ws-b"]);
    expect(t.place).toEqual({ after: "ws-b" });
  });
});

describe("cardDropTarget", () => {
  // Rows (40px each): team 0–40, ws-a 40–80, a1 80–120, ws-b 120–160, b1 160–200, ws-c 200–240,
  // then another team from 240.
  const rows: PaneRow[] = [
    team(T),
    ws("ws-a"),
    agent("ws-a", "a1"),
    ws("ws-b"),
    agent("ws-b", "b1"),
    ws("ws-c"),
    team("acme:ops"),
    ws("ws-x", "acme:ops"),
  ];
  const order = ["ws-a", "ws-b", "ws-c"];

  it("at its own place there is no move and no line, though the pointer is in its team", () => {
    // Inside ws-b's own block, above its middle (160).
    const t = cardDropTarget(rows, boxes(rows), T, "ws-b", 150, order);
    expect(t.slot).not.toBeNull();
    expect(t.place).toBeNull();
    expect(t.lineY).toBeNull();
  });

  it("above the first card's middle it lands before it, the line just inside that card's top", () => {
    const t = cardDropTarget(rows, boxes(rows), T, "ws-c", 50, order);
    expect(t.place).toEqual({ before: "ws-a" });
    expect(t.lineY).toBe(40 + DROP_LINE_INSET_PX);
  });

  it("past the last card's middle it lands after it, the line under that card's block", () => {
    const t = cardDropTarget(rows, boxes(rows), T, "ws-a", 230, order);
    expect(t.place).toEqual({ after: "ws-c" });
    // At the block's end: the next team's header starts there, and the line stays out of it.
    expect(t.lineY).toBe(240);
  });

  it("after the list's last card, with nothing below, the line sits in the air under it", () => {
    const solo: PaneRow[] = [team(T), ws("ws-a"), ws("ws-b")];
    const t = cardDropTarget(solo, boxes(solo), T, "ws-a", 110, ["ws-a", "ws-b"]);
    expect(t.lineY).toBe(120 + DROP_LINE_INSET_PX);
  });

  it("outside its team (in another team's cards) nothing drops", () => {
    expect(cardDropTarget(rows, boxes(rows), T, "ws-a", 300, order)).toEqual({
      slot: null,
      place: null,
      lineY: null,
    });
  });

  it("a card with no pill is no place of its own: passing only it moves nothing", () => {
    // ws-b has no pill, so the gap above it is still ws-c's own place.
    const t = cardDropTarget(rows, boxes(rows), T, "ws-c", 110, ["ws-a", "ws-c"]);
    expect(t.place).toBeNull();
    expect(cardDropTarget(rows, boxes(rows), T, "ws-c", 50, ["ws-a", "ws-c"]).place).toEqual({
      before: "ws-a",
    });
  });
});

describe("the drop's slack stops at the rows around the team", () => {
  // Rows (40px each): ops team 0–40, ws-x 40–80, then team T 80–120, ws-a 120–160, ws-b 160–200,
  // then another team 200–240 and its card.
  const rows: PaneRow[] = [
    team("acme:ops"),
    ws("ws-x", "acme:ops"),
    team(T),
    ws("ws-a"),
    ws("ws-b"),
    team("acme:qa"),
    ws("ws-q", "acme:qa"),
  ];
  const order = ["ws-a", "ws-b"];

  it("a pointer inside the team above (its last card's rows) is no drop", () => {
    // Within the old 8px slack above T's header (at 80), but over ws-x's row.
    expect(cardDropTarget(rows, boxes(rows), T, "ws-b", 75, order).slot).toBeNull();
    expect(cardDropTarget(rows, boxes(rows), T, "ws-b", 85, order).place).toEqual({
      before: "ws-a",
    });
  });

  it("a pointer on the next team's header is no drop, though it is within the slack below", () => {
    expect(CARD_DROP_SLACK_BELOW_PX).toBeGreaterThan(5);
    expect(cardDropTarget(rows, boxes(rows), T, "ws-a", 205, order).slot).toBeNull();
  });

  it("the last team's slack below still reaches past its end", () => {
    const last: PaneRow[] = [team(T), ws("ws-a"), ws("ws-b")];
    expect(cardDropTarget(last, boxes(last), T, "ws-a", 130, order).place).toEqual({
      after: "ws-b",
    });
  });

  it("a release far to either side of the pane is no drop", () => {
    const pane = { left: 100, right: 400 };
    const at = (x: number) =>
      cardDropTarget(rows, boxes(rows), T, "ws-a", 190, order, { x, ...pane }).slot;
    expect(at(250)).not.toBeNull();
    expect(at(100 - CARD_DROP_SLACK_X_PX + 1)).not.toBeNull();
    expect(at(100 - CARD_DROP_SLACK_X_PX - 1)).toBeNull();
    expect(at(400 + CARD_DROP_SLACK_X_PX + 1)).toBeNull();
  });
});

describe("a card's block holds whatever rows sit under it", () => {
  it("a row of another kind under a card stays inside the card's block", () => {
    const other = { kind: "status-head", key: "h", id: "x", label: "x", count: 0 } as PaneRow;
    const rows: PaneRow[] = [team(T), ws("ws-a"), agent("ws-a", "a1"), other, ws("ws-b")];
    const blocks = teamCardBlocks(rows, boxes(rows), T);
    expect(blocks.map((b) => [b.id, b.end])).toEqual([
      ["ws-a", 160],
      ["ws-b", 200],
    ]);
  });

  it("a press and the drop math read the same cards: one with no box is in neither", () => {
    const rows: PaneRow[] = [team(T), ws("ws-a"), ws("ws-b"), ws("ws-c")];
    const b = boxes(rows);
    const missing = [b[0], b[1], undefined, b[3]];
    const order = ["ws-a", "ws-b", "ws-c"];
    expect(teamSlotCards(rows, T, order, { measured: missing }).map((c) => c.id)).toEqual(
      teamCardBlocks(rows, missing, T).map((x) => x.id),
    );
    // Without the boxes, every card of the team (the keyboard step's list).
    expect(teamSlotCards(rows, T, order).map((c) => c.id)).toEqual(order);
  });
});
