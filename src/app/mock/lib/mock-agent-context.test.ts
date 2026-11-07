// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { mockRuntimeDefault } from "./mock-agent-context";

describe("mockRuntimeDefault", () => {
  it("places OpenCode skills in the same directory as native launches", () => {
    expect(mockRuntimeDefault("opencode").map(({ path }) => path)).toContain(".agents/skills");
    expect(mockRuntimeDefault("opencode").map(({ path }) => path)).not.toContain(".opencode/skill");
  });
});
