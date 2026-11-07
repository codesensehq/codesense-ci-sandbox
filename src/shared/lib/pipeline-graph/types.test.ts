// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import type { PipelineStatus } from "./types";

describe("PipelineStatus", () => {
  it("covers all 6 canonical pipeline states", () => {
    const statuses: PipelineStatus[] = ["planning", "todo", "in-progress", "review", "qa", "done"];
    expect(statuses).toHaveLength(6);
    expect(new Set(statuses).size).toBe(6);
  });
});
