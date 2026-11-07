// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";

import { useRovingTabs } from "@/shared/hooks/use-roving-tabs";

const TABS = ["one", "two", "three"] as const;

function keyEvent(key: string) {
  const preventDefault = vi.fn();
  return {
    event: { key, preventDefault } as unknown as React.KeyboardEvent<HTMLButtonElement>,
    preventDefault,
  };
}

describe("useRovingTabs", () => {
  it("exposes roving props: only the selected tab is tabbable and aria-selected", () => {
    const { result } = renderHook(() => useRovingTabs(TABS, "two", vi.fn()));
    expect(result.current.tabProps("two")).toMatchObject({
      role: "tab",
      "aria-selected": true,
      tabIndex: 0,
    });
    expect(result.current.tabProps("one")).toMatchObject({ "aria-selected": false, tabIndex: -1 });
  });

  it("ArrowRight/ArrowLeft move to the next/previous tab and wrap", () => {
    const onSelect = vi.fn();
    const { result, rerender } = renderHook(
      ({ selected }) => useRovingTabs(TABS, selected, onSelect),
      { initialProps: { selected: "two" as (typeof TABS)[number] } },
    );

    const right = keyEvent("ArrowRight");
    result.current.tabProps("two").onKeyDown(right.event);
    expect(onSelect).toHaveBeenLastCalledWith("three");
    expect(right.preventDefault).toHaveBeenCalled();

    rerender({ selected: "three" });
    result.current.tabProps("three").onKeyDown(keyEvent("ArrowRight").event);
    expect(onSelect).toHaveBeenLastCalledWith("one"); // wraps

    rerender({ selected: "one" });
    result.current.tabProps("one").onKeyDown(keyEvent("ArrowLeft").event);
    expect(onSelect).toHaveBeenLastCalledWith("three"); // wraps back
  });

  it("Home/End jump to the ends; unrelated keys are ignored", () => {
    const onSelect = vi.fn();
    const { result } = renderHook(() => useRovingTabs(TABS, "two", onSelect));

    result.current.tabProps("two").onKeyDown(keyEvent("Home").event);
    expect(onSelect).toHaveBeenLastCalledWith("one");
    result.current.tabProps("two").onKeyDown(keyEvent("End").event);
    expect(onSelect).toHaveBeenLastCalledWith("three");

    const other = keyEvent("Enter");
    result.current.tabProps("two").onKeyDown(other.event);
    expect(other.preventDefault).not.toHaveBeenCalled();
    expect(onSelect).toHaveBeenCalledTimes(2);
  });
});
