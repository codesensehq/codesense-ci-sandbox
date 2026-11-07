// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import type { SprintDef, SprintRegistry, SprintState } from "@/tauri/commands";
import { buildSprintTargetOptions } from "./sprint-targets";

function sprint(id: string, state: SprintState): SprintDef {
  return { id, state, goal: "", focusEpics: [], start: "", end: "" };
}

function registry(over?: Partial<SprintRegistry>): SprintRegistry {
  return { activeSprint: null, sprints: [], warnings: [], ...over };
}

describe("buildSprintTargetOptions", () => {
  it("null / empty registry → no targets", () => {
    expect(buildSprintTargetOptions(null)).toEqual([]);
    expect(buildSprintTargetOptions(registry())).toEqual([]);
  });

  it("orders active → planning → closed(newest-first) and marks/annotates active", () => {
    const reg = registry({
      activeSprint: "2026-08-A",
      sprints: [
        sprint("2026-06-A", "closed"),
        sprint("2026-07-A", "closed"),
        sprint("2026-08-A", "active"),
        sprint("2026-09-A", "planning"),
      ],
    });
    const opts = buildSprintTargetOptions(reg);
    expect(opts.map((o) => o.id)).toEqual(["2026-08-A", "2026-09-A", "2026-07-A", "2026-06-A"]);
    expect(opts[0]).toEqual({ id: "2026-08-A", label: "2026-08-A (active)", active: true });
    expect(opts.filter((o) => o.active)).toHaveLength(1);
  });
});
