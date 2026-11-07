// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { buildAgentLaunchCommand, GUARDED_ENV_STRIP } from "@/shared/lib/agent-session-launcher";

/**
 * SW-AW-5 (AC-3) — the module must keep the agent on the INTERACTIVE, subscription-
 * billed path and never route it onto the API-billed `claude -p` surface
 * (`claude_task.rs`). It spawns ONLY via the caller's session config →
 * `connectAgentSession` → `buildAgentLaunchCommand` (structurally `-p`-free) with
 * `ANTHROPIC_API_KEY` stripped. This suite (1) re-affirms the launcher guarantees
 * the module inherits and (2) statically scans the module source so no file can
 * smuggle in a `-p`/`--print` flag or a claude-task surface.
 */
describe("agent-module billing guard (AC-3)", () => {
  it("inherits the -p-free interactive launch command + ANTHROPIC_API_KEY strip", () => {
    expect(buildAgentLaunchCommand("/opt/homebrew/bin/claude")).not.toMatch(/\s-p\b|--print/);
    expect(GUARDED_ENV_STRIP).toContain("ANTHROPIC_API_KEY");
  });

  const sources = import.meta.glob("./*.{ts,tsx}", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>;

  it("no module source references the API-billed claude -p / claude_task surface", () => {
    const offenders = Object.entries(sources)
      .filter(([p]) => !p.includes(".test."))
      .filter(([, src]) => /claude_task|claudeTask|claude\s+-p\b|--print/.test(src))
      .map(([p]) => p);
    expect(offenders).toEqual([]);
  });

  it("does not construct its own spawn command — delegates to the audited launcher", () => {
    const offenders = Object.entries(sources)
      .filter(([p]) => !p.includes(".test."))
      // The module spawns via the caller's session `connect` adapter, never by
      // calling `terminalOpenTab` / `buildAgentLaunchCommand` itself.
      .filter(([, src]) => /\bterminalOpenTab\b|\bbuildAgentLaunchCommand\b/.test(src))
      .map(([p]) => p);
    expect(offenders).toEqual([]);
  });
});
