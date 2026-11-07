// SPDX-License-Identifier: MIT
import { describe, it, expect, afterEach } from "vitest";
import type { StatusState } from "@/tauri/commands";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { parseCsQuery } from "@/shared/lib/csquery/parse-csquery";
import { blockQuery } from "@/shared/lib/csquery/block-query";
import { isCsQueryError } from "@/shared/lib/csquery/types";

/**
 * SW-GT-20 · AC-4 keystone: hydrate ONE custom taxonomy and prove the passive
 * query surfaces reflect it — no hardcoded status list
 * survives. The interactive node and the workspace pill assert the same property
 * in their own suites (pipeline-story-node.test.tsx / workspace-pill.test.tsx),
 * which mock the React Flow + workspace runtime those components need.
 */

const CUSTOM: StatusState[] = [
  { id: "shipped", label: "Shipped", color: "#abcdef", icon: "Rocket", order: 0, role: "done" },
  { id: "building", label: "Building", color: "#123456", icon: "Hammer", order: 1, role: "active" },
  {
    id: "icebox",
    label: "Icebox",
    color: "#0b0b0b",
    icon: "Snowflake",
    order: 2,
    role: "not_started",
  },
];

describe("FE status consumers reflect a hydrated custom taxonomy (SW-GT-20)", () => {
  afterEach(() => useStatusTaxonomyStore.getState().resetForTest());

  it("a csquery block's status sort carries the custom `order` to the engine", () => {
    useStatusTaxonomyStore.getState().setStates(CUSTOM);
    const spec = parseCsQuery("view: board");
    if (isCsQueryError(spec)) throw new Error(spec.error);
    const statusOrder = useStatusTaxonomyStore.getState().states.map((s) => s.id);
    const q = blockQuery(spec, { statusOrder });
    // The engine ranks listed values by position and sorts the rest (off-taxonomy) after.
    expect(q.sort?.[0]).toEqual({
      key: "status",
      dir: "asc",
      order: ["shipped", "building", "icebox"],
    });
  });
});
