// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import type { AgentSessionRow } from "@/tauri/commands";

import {
  isFleetAgent,
  isPlainRegistration,
  isUtilitySession,
  orderAgentRows,
  UTILITY_SESSION_KINDS,
} from "./agent-rows";

function row(handle: string, over: Partial<AgentSessionRow> = {}): AgentSessionRow {
  return {
    handle,
    sessionId: `sid-${handle}`,
    kind: "shell",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: 1000,
    launcher: null,
    lastOutputMs: null,
    ...over,
  };
}

function ordered(
  handle: string,
  foreground: boolean,
  over: Partial<AgentSessionRow> = {},
): OrderedAgentRow {
  return { row: row(handle, over), foreground };
}

type OrderedAgentRow = Parameters<typeof orderAgentRows>[0][number];

const handles = (rows: OrderedAgentRow[]) => rows.map((r) => r.row.handle);

describe("orderAgentRows", () => {
  it("groups foreground first, then background-alive, then stopped", () => {
    const rows = [
      ordered("stopped", false, { alive: false }),
      ordered("bg", false),
      ordered("fg", true),
    ];
    expect(handles(orderAgentRows(rows))).toEqual(["fg", "bg", "stopped"]);
  });

  it("orders most-recent first within a group by startedAtMs desc", () => {
    const rows = [
      ordered("old", false, { startedAtMs: 100 }),
      ordered("new", false, { startedAtMs: 900 }),
      ordered("mid", false, { startedAtMs: 500 }),
    ];
    expect(handles(orderAgentRows(rows))).toEqual(["new", "mid", "old"]);
  });

  it("sorts a null startedAtMs last within its group", () => {
    const rows = [
      ordered("unknown", false, { startedAtMs: null }),
      ordered("known", false, { startedAtMs: 100 }),
    ];
    expect(handles(orderAgentRows(rows))).toEqual(["known", "unknown"]);
  });

  it("breaks ties by handle asc when startedAtMs is equal", () => {
    const rows = [
      ordered("charlie", false, { startedAtMs: 500 }),
      ordered("alpha", false, { startedAtMs: 500 }),
      ordered("bravo", false, { startedAtMs: 500 }),
    ];
    expect(handles(orderAgentRows(rows))).toEqual(["alpha", "bravo", "charlie"]);
  });

  it("is STABLE — a new array of identical rows yields the identical order (AC-4)", () => {
    const build = (): OrderedAgentRow[] => [
      ordered("fg", true, { startedAtMs: 300 }),
      ordered("b2", false, { startedAtMs: 200 }),
      ordered("b1", false, { startedAtMs: 200 }),
      ordered("z", false, { alive: false }),
    ];
    // Two independently-built arrays (as a poll refresh produces) must order the same.
    expect(handles(orderAgentRows(build()))).toEqual(handles(orderAgentRows(build())));
    expect(handles(orderAgentRows(build()))).toEqual(["fg", "b1", "b2", "z"]);
  });

  it("does not mutate the input array", () => {
    const rows = [ordered("b", false), ordered("a", false)];
    const snapshot = handles(rows);
    orderAgentRows(rows);
    expect(handles(rows)).toEqual(snapshot);
  });
});

// --- SW-AV-2: fleet agents ---------------------------------------------------

describe("SW-AV-2 fleet-agent predicate", () => {
  it("treats every KIND_LIVE_PTY + utility kind as a NON-fleet (utility) session", () => {
    for (const kind of ["live-pty", "root", "notes", "#"]) {
      expect(isUtilitySession(row("x", { kind }))).toBe(true);
      expect(isFleetAgent(row("x", { kind }))).toBe(false);
    }
    // The set is the single source of truth for the predicate.
    expect([...UTILITY_SESSION_KINDS].sort()).toEqual(["#", "live-pty", "notes", "root"].sort());
  });

  it("treats real agent kinds as fleet agents", () => {
    for (const kind of ["dashboard-launched", "headless", "controller", "shell"]) {
      expect(isFleetAgent(row("x", { kind }))).toBe(true);
      expect(isUtilitySession(row("x", { kind }))).toBe(false);
    }
  });

  it("treats a registered shell as an agent only while it names or runs one", () => {
    const shell = row("x", { kind: "registered", agentRevision: null });
    expect(isPlainRegistration(shell)).toBe(true);
    expect(isFleetAgent(shell)).toBe(false);
    expect(isFleetAgent({ ...shell, detected: { cli: "claude", dot: null } })).toBe(true);
    expect(isFleetAgent({ ...shell, agentRevision: "worker@1" })).toBe(true);
    expect(isPlainRegistration(row("x", { kind: "dashboard-launched" }))).toBe(false);
  });

  it("keys on kind ALONE — a fork handle containing '#' stays a fleet agent", () => {
    // `SW-DX-3#attempt-2` is a real dashboard-launched fork, not a `#` scratch shell.
    expect(isFleetAgent(row("SW-DX-3#attempt-2", { kind: "dashboard-launched" }))).toBe(true);
  });
});
