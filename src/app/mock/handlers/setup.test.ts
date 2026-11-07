// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { setupHandlers } from "./setup";
import type { DependencyStatus } from "@/tauri/commands";

describe("setupHandlers", () => {
  it("re-checks dependencies to a ready git+agent status", () => {
    const h = setupHandlers();
    const deps = h.recheck_dependencies({}) as DependencyStatus;
    expect(deps.git.available).toBe(true);
    expect(deps.agents.claude.available).toBe(true);
  });

  it("resolves an agent CLI path by basename", () => {
    const h = setupHandlers();
    expect(h.resolve_agent_cli({ basename: "claude" })).toBe("/usr/local/bin/claude");
    expect(h.resolve_agent_cli({ basename: "codex" })).toBeNull();
  });
});
