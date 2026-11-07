// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render } from "@/test-utils";
import { useRef } from "react";

import type { PaneRow } from "../lib/agents-pane-rows";
import { CARD_MOVE_REFUSED, type RowBox } from "../lib/agents-pane-drag";
import { useWorkspaceCardDrag } from "./use-workspace-card-drag";

const T = "acme:default";
const team: PaneRow = { kind: "team", key: "team", teamKey: T, folded: false, needCount: 0 };
const ws = (workspaceId: string): PaneRow =>
  ({
    kind: "ws",
    key: `ws:${workspaceId}`,
    teamKey: T,
    group: { workspaceId, label: workspaceId, nodes: [], kind: "folder" },
    folded: false,
    arrowOn: false,
    dragging: false,
  }) as PaneRow;

function Pane({
  rows,
  order,
  onDrop,
  seen,
  refusal,
  onRefused,
}: {
  rows: PaneRow[];
  order: string[];
  onDrop: (...args: unknown[]) => void;
  seen: (dragging: boolean) => void;
  refusal?: (teamKey: string, workspaceId: string) => string | null;
  onRefused?: (words: string) => void;
}) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  // Each test renders one list and keeps it.
  const rowsRef = useRef<readonly PaneRow[]>(rows);
  const boxesRef = useRef<() => readonly (RowBox | undefined)[]>(() =>
    rows.map((_, i) => ({ start: i * 40, end: i * 40 + 40 })),
  );
  const { drag, begin } = useWorkspaceCardDrag({
    scrollRef,
    rowsRef,
    boxesRef,
    orderOf: () => order,
    onDrop,
    refusal,
    onRefused,
  });
  seen(drag !== null);
  return (
    <div ref={scrollRef}>
      <div data-testid="head-a" onPointerDown={(e) => begin(e, T, "ws-a")} />
    </div>
  );
}

const P = { pointerId: 1, button: 0, isPrimary: true, clientX: 10 };

function setup(
  rows: PaneRow[],
  order: string[],
  extra: {
    refusal?: (teamKey: string, workspaceId: string) => string | null;
    onRefused?: (words: string) => void;
  } = {},
) {
  const onDrop = vi.fn();
  const states: boolean[] = [];
  const view = render(
    <Pane rows={rows} order={order} onDrop={onDrop} seen={(d) => states.push(d)} {...extra} />,
  );
  const dragging = () => states[states.length - 1];
  return { ...view, onDrop, dragging };
}

describe("useWorkspaceCardDrag", () => {
  it("a mouse moving with no button down ends the drag and moves nothing (its release was lost)", () => {
    const { getByTestId, onDrop, dragging } = setup(
      [team, ws("ws-a"), ws("ws-b")],
      ["ws-a", "ws-b"],
    );
    fireEvent.pointerDown(getByTestId("head-a"), { ...P, clientY: 60 });
    act(() => {
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 1, clientY: 110 });
    });
    expect(dragging()).toBe(true);
    act(() => {
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 0, clientY: 115 });
    });
    expect(dragging()).toBe(false);
    act(() => {
      fireEvent.pointerUp(window, { ...P, clientY: 115 });
    });
    expect(onDrop).not.toHaveBeenCalled();
  });

  it("a mouse moving with no button down ends a press below the threshold too", () => {
    const { getByTestId, dragging } = setup([team, ws("ws-a"), ws("ws-b")], ["ws-a", "ws-b"]);
    fireEvent.pointerDown(getByTestId("head-a"), { ...P, clientY: 60 });
    act(() => {
      // Its release was lost before the press became a drag.
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 0, clientY: 62 });
    });
    // The press is over: a later move past the threshold starts nothing.
    act(() => {
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 1, clientY: 110 });
    });
    expect(dragging()).toBe(false);
  });

  it("a card alone in its team starts no drag: the press stays a click", () => {
    const { getByTestId, dragging } = setup([team, ws("ws-a")], ["ws-a"]);
    fireEvent.pointerDown(getByTestId("head-a"), { ...P, clientY: 60 });
    act(() => {
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 1, clientY: 110 });
    });
    expect(dragging()).toBe(false);
  });

  it("Escape on a press below the threshold ends the press but keeps its own meaning", () => {
    const { getByTestId, dragging } = setup([team, ws("ws-a"), ws("ws-b")], ["ws-a", "ws-b"]);
    const outer = vi.fn();
    window.addEventListener("keydown", outer);
    try {
      fireEvent.pointerDown(getByTestId("head-a"), { ...P, clientY: 60 });
      const esc = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
      act(() => {
        window.dispatchEvent(esc);
      });
      expect(esc.defaultPrevented).toBe(false);
      expect(outer).toHaveBeenCalled();
      // The press is over: a later move past the threshold starts nothing.
      act(() => {
        fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 1, clientY: 110 });
      });
      expect(dragging()).toBe(false);
    } finally {
      window.removeEventListener("keydown", outer);
    }
  });

  it("Escape mid-drag cancels it and is taken", () => {
    const { getByTestId, dragging, onDrop } = setup(
      [team, ws("ws-a"), ws("ws-b")],
      ["ws-a", "ws-b"],
    );
    fireEvent.pointerDown(getByTestId("head-a"), { ...P, clientY: 60 });
    act(() => {
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 1, clientY: 110 });
    });
    const esc = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => {
      window.dispatchEvent(esc);
    });
    expect(esc.defaultPrevented).toBe(true);
    expect(dragging()).toBe(false);
    expect(onDrop).not.toHaveBeenCalled();
  });

  it("a refused press says why once it is dragged, starts no drag, and its release is no click", () => {
    const onRefused = vi.fn();
    const { getByTestId, dragging } = setup([team, ws("ws-a"), ws("ws-b")], ["ws-a", "ws-b"], {
      refusal: () => "Cards can't move while a search or filter is on.",
      onRefused,
    });
    const head = getByTestId("head-a");
    const click = vi.fn();
    head.addEventListener("click", click);
    fireEvent.pointerDown(head, { ...P, clientY: 60 });
    // A click-sized wobble says nothing.
    act(() => {
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 1, clientY: 62 });
    });
    expect(onRefused).not.toHaveBeenCalled();
    act(() => {
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 1, clientY: 110 });
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 1, clientY: 130 });
    });
    expect(onRefused).toHaveBeenCalledTimes(1);
    expect(onRefused).toHaveBeenCalledWith("Cards can't move while a search or filter is on.");
    expect(dragging()).toBe(false);
    act(() => {
      fireEvent.pointerUp(window, { ...P, clientY: 130 });
      fireEvent.click(head);
    });
    expect(click).not.toHaveBeenCalled();
  });

  it("a card with nowhere to go says so when dragged, if the pane listens", () => {
    const onRefused = vi.fn();
    const { getByTestId } = setup([team, ws("ws-a")], ["ws-a"], { onRefused });
    fireEvent.pointerDown(getByTestId("head-a"), { ...P, clientY: 60 });
    act(() => {
      fireEvent.pointerMove(window, { ...P, pointerType: "mouse", buttons: 1, clientY: 110 });
    });
    expect(onRefused).toHaveBeenCalledWith(CARD_MOVE_REFUSED.nowhere);
  });
});
