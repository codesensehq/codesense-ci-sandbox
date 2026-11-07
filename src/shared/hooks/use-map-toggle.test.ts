// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useMapToggle } from "./use-map-toggle";
import { pruneMapByKeys } from "@/shared/lib/prune-map-by-keys";

describe("useMapToggle", () => {
  it("opens a key (sets it to true)", () => {
    const { result } = renderHook(() => useMapToggle<string>());
    act(() => result.current[1]("a", true));
    expect(result.current[0].get("a")).toBe(true);
  });

  it("closes a key (deletes it)", () => {
    const { result } = renderHook(() => useMapToggle<string>());
    act(() => result.current[1]("a", true));
    act(() => result.current[1]("a", false));
    expect(result.current[0].has("a")).toBe(false);
  });

  it("returns the SAME Map instance on a no-op toggle (referential stability)", () => {
    const { result } = renderHook(() => useMapToggle<string>());
    act(() => result.current[1]("a", true));
    const afterOpen = result.current[0];
    // Toggling to the value it already has must not create a new Map.
    act(() => result.current[1]("a", true));
    expect(result.current[0]).toBe(afterOpen);
  });

  it("returns the SAME Map instance when closing an already-absent key", () => {
    const { result } = renderHook(() => useMapToggle<string>());
    const initial = result.current[0];
    act(() => result.current[1]("missing", false));
    expect(result.current[0]).toBe(initial);
  });

  it("keeps a stable setOpen identity across renders", () => {
    const { result, rerender } = renderHook(() => useMapToggle<string>());
    const first = result.current[1];
    rerender();
    expect(result.current[1]).toBe(first);
  });

  it("exposes the raw setMap for referentially-stable prune from an effect", () => {
    const { result } = renderHook(() => useMapToggle<string>());
    act(() => result.current[1]("a", true));
    act(() => result.current[1]("b", true));
    const beforePrune = result.current[0];
    // No key drops → prune must return the same instance (no re-render churn).
    act(() => result.current[2]((prev) => pruneMapByKeys(prev, new Set(["a", "b"]))));
    expect(result.current[0]).toBe(beforePrune);
    // Dropping a key produces a fresh, pruned Map.
    act(() => result.current[2]((prev) => pruneMapByKeys(prev, new Set(["a"]))));
    expect(result.current[0]).not.toBe(beforePrune);
    expect(result.current[0].has("b")).toBe(false);
    expect(result.current[0].get("a")).toBe(true);
  });
});
