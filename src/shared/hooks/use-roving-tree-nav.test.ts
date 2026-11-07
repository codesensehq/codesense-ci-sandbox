// SPDX-License-Identifier: MIT
import type { FocusEvent, KeyboardEvent } from "react";
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { useRovingTreeNav, type RovingTreeRow } from "./use-roving-tree-nav";

const keyEvent = (k: string) =>
  ({ key: k, preventDefault: vi.fn() }) as unknown as KeyboardEvent<HTMLElement>;

const leaf = (depth = 0): RovingTreeRow => ({ depth, expandable: false, expanded: false });
const branch = (depth = 0, expanded = false): RovingTreeRow => ({
  depth,
  expandable: true,
  expanded,
});

function setup(rows: RovingTreeRow[]) {
  const focusRow = vi.fn();
  const toggle = vi.fn();
  const view = renderHook(
    (props: { rows: RovingTreeRow[] }) => useRovingTreeNav({ rows: props.rows, focusRow, toggle }),
    { initialProps: { rows } },
  );
  return { view, focusRow, toggle };
}

describe("useRovingTreeNav", () => {
  it("makes only the active row a tab stop (roving tabindex)", () => {
    const { view } = setup([leaf(), leaf(), leaf()]);
    expect(view.result.current.tabIndexFor(0)).toBe(0);
    expect(view.result.current.tabIndexFor(1)).toBe(-1);
    expect(view.result.current.tabIndexFor(2)).toBe(-1);
  });

  it("ArrowDown / ArrowUp move the active row and focus it", () => {
    const { view, focusRow } = setup([leaf(), leaf(), leaf()]);
    act(() => view.result.current.onKeyDown(keyEvent("ArrowDown")));
    expect(focusRow).toHaveBeenLastCalledWith(1);
    expect(view.result.current.tabIndexFor(1)).toBe(0);

    act(() => view.result.current.onKeyDown(keyEvent("ArrowUp")));
    expect(focusRow).toHaveBeenLastCalledWith(0);
    expect(view.result.current.tabIndexFor(0)).toBe(0);
  });

  it("clamps at the ends", () => {
    const { view, focusRow } = setup([leaf(), leaf()]);
    act(() => view.result.current.onKeyDown(keyEvent("ArrowUp"))); // already at 0
    expect(focusRow).toHaveBeenLastCalledWith(0);
    act(() => view.result.current.onKeyDown(keyEvent("End")));
    expect(focusRow).toHaveBeenLastCalledWith(1);
    act(() => view.result.current.onKeyDown(keyEvent("ArrowDown"))); // already last
    expect(focusRow).toHaveBeenLastCalledWith(1);
    act(() => view.result.current.onKeyDown(keyEvent("Home")));
    expect(focusRow).toHaveBeenLastCalledWith(0);
  });

  it("ArrowRight expands a collapsed expandable row (toggle, no move)", () => {
    const { view, focusRow, toggle } = setup([branch(0, false), leaf(1)]);
    act(() => view.result.current.onKeyDown(keyEvent("ArrowRight")));
    expect(toggle).toHaveBeenCalledWith(0);
    expect(focusRow).not.toHaveBeenCalled();
  });

  it("ArrowRight on an already-open expandable row steps into the first child", () => {
    const { view, focusRow, toggle } = setup([branch(0, true), leaf(1)]);
    act(() => view.result.current.onKeyDown(keyEvent("ArrowRight")));
    expect(toggle).not.toHaveBeenCalled();
    expect(focusRow).toHaveBeenLastCalledWith(1);
  });

  it("ArrowRight on a leaf does nothing", () => {
    const { view, focusRow, toggle } = setup([leaf(), leaf()]);
    act(() => view.result.current.onKeyDown(keyEvent("ArrowRight")));
    expect(toggle).not.toHaveBeenCalled();
    expect(focusRow).not.toHaveBeenCalled();
  });

  it("ArrowLeft collapses an open expandable row in place", () => {
    const { view, toggle } = setup([branch(0, true), leaf(1)]);
    act(() => view.result.current.onKeyDown(keyEvent("ArrowLeft")));
    expect(toggle).toHaveBeenCalledWith(0);
  });

  it("ArrowLeft on a leaf jumps to the nearest shallower ancestor row", () => {
    // rows: [epic(depth0), ownFile(depth1 leaf), story(depth1)]; active=2 (story) → ascend to 0.
    const rows = [branch(0, true), leaf(1), leaf(1)];
    const { view, focusRow } = setup(rows);
    act(() => view.result.current.onKeyDown(keyEvent("End"))); // active = 2
    focusRow.mockClear();
    act(() => view.result.current.onKeyDown(keyEvent("ArrowLeft")));
    expect(focusRow).toHaveBeenLastCalledWith(0);
  });

  it("onFocus syncs the active row from the focused element's data-index", () => {
    const { view } = setup([leaf(), leaf(), leaf()]);
    const fakeEvent = {
      target: {
        closest: () => ({ getAttribute: () => "2" }),
      },
    } as unknown as FocusEvent<HTMLElement>;
    act(() => view.result.current.onFocus(fakeEvent));
    expect(view.result.current.tabIndexFor(2)).toBe(0);
  });

  it("keeps a valid tab stop when the row set shrinks", () => {
    const { view } = setup([leaf(), leaf(), leaf()]);
    act(() => view.result.current.onKeyDown(keyEvent("End"))); // active = 2
    view.rerender({ rows: [leaf()] }); // shrink to 1
    expect(view.result.current.tabIndexFor(0)).toBe(0);
  });
});
