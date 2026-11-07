// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  agentRowLabel,
  foregroundSessionId,
  formatElapsed,
  formatLauncher,
  formatSessionHandle,
  isHeadlessSession,
} from "./agent-row-presentation";
import type { AgentSessionRow } from "@/tauri/commands";

function row(over: Partial<AgentSessionRow> = {}): AgentSessionRow {
  return {
    handle: "SW-A",
    sessionId: "sess-a",
    kind: "dashboard-launched",
    alive: true,
    backgrounded: true,
    startedWith: null,
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
    ...over,
  };
}

// SW-OR-6 (AC-3): the kind-aware guard predicate — only a PTY-less headless run is
// excluded from the Bring-to-foreground affordance.
describe("isHeadlessSession", () => {
  it("is true for a headless (PTY-less) pseudo-session", () => {
    expect(isHeadlessSession(row({ kind: "headless" }))).toBe(true);
  });

  it("is false for every PTY-backed kind (dashboard-launched / registered)", () => {
    expect(isHeadlessSession(row({ kind: "dashboard-launched" }))).toBe(false);
    expect(isHeadlessSession(row({ kind: "registered" }))).toBe(false);
  });
});

describe("row display helpers", () => {
  it("foregroundSessionId is the row's existing PTY session", () => {
    expect(foregroundSessionId(row({ handle: "SW-A", sessionId: "sess-a" }))).toBe("sess-a");
  });

  it("foregroundSessionId is null for a reserved-but-unbound launch", () => {
    expect(foregroundSessionId(row({ sessionId: null }))).toBeNull();
  });

  // A headless (PTY-less) row has a bound sessionId but must never mint a foreground tab.
  it("foregroundSessionId is null for a headless (PTY-less) row", () => {
    expect(foregroundSessionId(row({ sessionId: "sess-hl", kind: "headless" }))).toBeNull();
  });

  it("formatSessionHandle renders a fork/named handle as 'stem · Label', bare handles unchanged", () => {
    expect(formatSessionHandle("SW-DX-3#attempt-2")).toBe("SW-DX-3 · Attempt 2");
    expect(formatSessionHandle("SW-GW-11#architecture-review")).toBe(
      "SW-GW-11 · Architecture Review",
    );
    expect(formatSessionHandle("SW-A")).toBe("SW-A");
    expect(formatSessionHandle("/ctx/epic/README.md")).toBe("README.md");
  });

  it("formatSessionHandle leaves branch/team, path and multi-# handles opaque (no mis-split)", () => {
    expect(formatSessionHandle("owner/name::feature/x")).toBe("x");
    expect(formatSessionHandle("owner/name::feature/x#attempt-2")).toBe("x#attempt-2");
    expect(formatSessionHandle("SW-A#b#c")).toBe("SW-A#b#c");
    expect(formatSessionHandle("SW-A#")).toBe("SW-A#");
    expect(formatSessionHandle("/Users/x/my#proj")).toBe("my#proj");
    expect(formatSessionHandle("/ctx/epics/e#1/README.md")).toBe("README.md");
  });

  it("agentRowLabel prefers the launch display name, else the compact handle", () => {
    expect(agentRowLabel({ handle: "SW-A#attempt-2", displayName: "Reviewer" })).toBe("Reviewer");
    expect(agentRowLabel({ handle: "SW-A#attempt-2", displayName: null })).toBe("SW-A · Attempt 2");
  });

  it("agentRowLabel names a session of an agent called controller like any other", () => {
    const row = { handle: "/ctx/content#controller", displayName: null };
    expect(agentRowLabel(row)).toBe(formatSessionHandle(row.handle));
    expect(agentRowLabel({ ...row, displayName: "Ricky" })).toBe("Ricky");
  });

  it("formatElapsed buckets seconds/minutes/hours and guards null/future", () => {
    const t0 = 1_000_000_000_000;
    expect(formatElapsed(t0, t0 + 5_000)).toBe("5s");
    expect(formatElapsed(t0, t0 + 90_000)).toBe("1m");
    expect(formatElapsed(t0, t0 + 60 * 60_000)).toBe("1h");
    expect(formatElapsed(t0, t0 + (60 + 4) * 60_000)).toBe("1h 4m");
    expect(formatElapsed(null, t0)).toBe("");
    expect(formatElapsed(t0 + 5_000, t0)).toBe("");
  });

  it("formatLauncher humanizes codesense, shortens a docPath, and hides when absent", () => {
    expect(formatLauncher("codesense")).toBe("CodeSense");
    expect(formatLauncher("/ctx/boards/tech-debt.html")).toBe("tech-debt.html");
    expect(formatLauncher("mycorp/9north-context:smart-web::DEMO-1888")).toBe("DEMO-1888");
    expect(formatLauncher(null)).toBeNull();
    expect(formatLauncher("")).toBeNull();
  });
});
