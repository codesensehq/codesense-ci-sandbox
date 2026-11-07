// SPDX-License-Identifier: MIT
import { describe, it, expect, afterEach } from "vitest";
import { renderHook } from "@testing-library/react";
import type { StatusState } from "@/tauri/commands";
import {
  useStatusTaxonomyStore,
  useStatusState,
  getStatusState,
  roleOf,
  isKnownStatusId,
  firstStatusId,
  firstStatusIdOfRole,
} from "./status-taxonomy-store";
import { DEFAULT_STATUS_STATES } from "@/shared/lib/pipeline-graph/status-config";

// The global test-setup already resets after each test; belt-and-suspenders here
// so this suite is self-contained even run in isolation.
afterEach(() => useStatusTaxonomyStore.getState().resetForTest());

const custom: StatusState[] = [
  {
    id: "icebox",
    label: "Icebox",
    color: "#111",
    icon: "Snowflake",
    order: 2,
    role: "not_started",
  },
  { id: "building", label: "Building", color: "#222", icon: "Hammer", order: 0, role: "active" },
  { id: "shipped", label: "Shipped", color: "#333", icon: "Rocket", order: 1, role: "done" },
];

describe("status-taxonomy-store (SW-GT-3)", () => {
  it("is seeded synchronously at import with the default states (never empty)", () => {
    expect(useStatusTaxonomyStore.getState().states).toEqual(DEFAULT_STATUS_STATES);
    expect(useStatusTaxonomyStore.getState().states.length).toBe(7);
  });

  it("getStatusState / roleOf / isKnownStatusId read the live snapshot", () => {
    expect(getStatusState("in-progress")?.label).toBe("In progress");
    expect(roleOf("in-progress")).toBe("active");
    expect(roleOf("qa")).toBe("review");
    expect(isKnownStatusId("done")).toBe(true);
    expect(isKnownStatusId("nope")).toBe(false);
    expect(roleOf("nope")).toBeUndefined();
  });

  it("firstStatusId / firstStatusIdOfRole reflect the default order + roles", () => {
    expect(firstStatusId()).toBe("planning");
    expect(firstStatusIdOfRole("done")).toBe("done");
    expect(firstStatusIdOfRole("active")).toBe("in-progress");
    expect(firstStatusIdOfRole("review")).toBe("review"); // review before qa by order
  });

  it("setStates hydrates a custom taxonomy, re-sorted by order, and reindexes", () => {
    useStatusTaxonomyStore.getState().setStates(custom);
    // Re-sorted by `order`: building(0), shipped(1), icebox(2).
    expect(useStatusTaxonomyStore.getState().states.map((s) => s.id)).toEqual([
      "building",
      "shipped",
      "icebox",
    ]);
    expect(roleOf("shipped")).toBe("done");
    expect(firstStatusId()).toBe("building");
    // Default ids are gone under a full replacement.
    expect(isKnownStatusId("todo")).toBe(false);
  });

  it("setStates preserves the non-empty invariant for an empty write", () => {
    useStatusTaxonomyStore.getState().setStates(custom);
    useStatusTaxonomyStore.getState().setStates([]);
    expect(useStatusTaxonomyStore.getState().states).toEqual(DEFAULT_STATUS_STATES);
    expect(getStatusState("building")).toBeUndefined();
    expect(roleOf("done")).toBe("done");
  });

  it("resetForTest restores the default seed (AC-8a isolation)", () => {
    useStatusTaxonomyStore.getState().setStates(custom);
    expect(isKnownStatusId("building")).toBe(true);
    useStatusTaxonomyStore.getState().resetForTest();
    expect(useStatusTaxonomyStore.getState().states).toEqual(DEFAULT_STATUS_STATES);
    expect(isKnownStatusId("building")).toBe(false);
  });

  it("useStatusState (SW-GT-20) subscribes to a state, undefined for statusless/off-taxonomy", () => {
    expect(renderHook(() => useStatusState("in-progress")).result.current?.label).toBe(
      "In progress",
    );
    expect(renderHook(() => useStatusState("mystery")).result.current).toBeUndefined();
    // Statusless surfaces (null/empty/undefined) short-circuit to undefined.
    expect(renderHook(() => useStatusState(null)).result.current).toBeUndefined();
    expect(renderHook(() => useStatusState(undefined)).result.current).toBeUndefined();
    expect(renderHook(() => useStatusState("")).result.current).toBeUndefined();
  });
});
