// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { STATUS_CONFIG } from "./status-config";
import { PIPELINE_STATUSES } from "./types";

describe("STATUS_CONFIG", () => {
  it("has an entry for every PipelineStatus value", () => {
    for (const status of PIPELINE_STATUSES) {
      expect(STATUS_CONFIG[status]).toBeDefined();
    }
  });

  it("each entry has icon, color, and label", () => {
    for (const status of PIPELINE_STATUSES) {
      const entry = STATUS_CONFIG[status];
      expect(entry.icon).toBeDefined();
      expect(typeof entry.icon === "function" || typeof entry.icon === "object").toBe(true);
      expect(entry.color).toMatch(/^#[0-9a-f]{6}$/i);
      expect(typeof entry.label).toBe("string");
      expect(entry.label.length).toBeGreaterThan(0);
    }
  });

  it("has exactly 7 entries matching PIPELINE_STATUSES length", () => {
    expect(Object.keys(STATUS_CONFIG)).toHaveLength(PIPELINE_STATUSES.length);
  });
});
