// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  DEFAULT_BACKGROUND_AGENT_LIMIT,
  resolveBackgroundAgentLimit,
} from "./background-agent-limit";

describe("resolveBackgroundAgentLimit (SW-DX-17d AC-3)", () => {
  it("resolves 'auto'/undefined to the attention-based default (not core-sized)", () => {
    expect(resolveBackgroundAgentLimit("auto")).toBe(DEFAULT_BACKGROUND_AGENT_LIMIT);
    expect(resolveBackgroundAgentLimit(undefined)).toBe(DEFAULT_BACKGROUND_AGENT_LIMIT);
  });

  it("honors an explicit numeric override", () => {
    expect(resolveBackgroundAgentLimit(2)).toBe(2);
    expect(resolveBackgroundAgentLimit(8)).toBe(8);
  });

  it("floors and clamps a garbage override to ≥1 rather than returning 0/NaN", () => {
    expect(resolveBackgroundAgentLimit(0)).toBe(1);
    expect(resolveBackgroundAgentLimit(-3)).toBe(1);
    expect(resolveBackgroundAgentLimit(3.9)).toBe(3);
    expect(resolveBackgroundAgentLimit(Number.NaN)).toBe(DEFAULT_BACKGROUND_AGENT_LIMIT);
  });
});
