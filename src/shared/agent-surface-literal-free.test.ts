// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

/**
 * SW-GS-2d (AC-4a/AC-8) — the literal-free guard. The shared agent-surface
 * modules (engine, prime, handshake, keyed store, picker primitive, the plug-in
 * hook + its orchestration, the picker UI) must carry NO `"claude"` or
 * agent-specific STRING LITERAL in code — those belong only in each consumer's
 * own agent-config wiring. This is what lets a story/epic surface reuse the
 * whole stack by passing its own config. Only code literals are forbidden, so
 * comments are stripped before scanning.
 */

const sources = import.meta.glob(
  [
    "./lib/agent-session-launcher.ts",
    "./lib/agent-chat-prime.ts",
    "./lib/agent-handshake.ts",
    "./stores/use-agent-session-store.ts",
    "./hooks/use-workspace-agent-tabs.ts",
    "./hooks/use-agent-liveness-monitor.ts",
    "./lib/run-agent-bind.ts",
    "./hooks/use-agent-workspace-session.ts",
    "./components/agent-session-picker.tsx",
  ],
  { query: "?raw", import: "default", eager: true },
) as Record<string, string>;

function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");
}

describe("shared agent-surface literal-free guard (AC-4a/AC-8)", () => {
  it("globbed all 9 shared modules (guards against a silent no-match)", () => {
    expect(Object.keys(sources)).toHaveLength(9);
  });

  it("no shared agent module contains a claude string literal in code", () => {
    const offenders = Object.entries(sources)
      .filter(([, src]) => /["']claude["']/.test(stripComments(src)))
      .map(([path]) => path);
    expect(offenders).toEqual([]);
  });
});
