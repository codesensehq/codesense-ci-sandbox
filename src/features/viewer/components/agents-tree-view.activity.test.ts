// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { AgentTreeNode } from "@/features/viewer/lib/agent-list-view";
import type { AgentSessionRow } from "@/tauri/commands";
import {
  activityLine,
  askLeadsWithWord,
  searchTextOf,
  type SearchIdentity,
} from "./agents-tree-view";

describe("SW-AA-1 (Phase 1): activityLine", () => {
  it("renders a trimmed progress note", () => {
    expect(activityLine({ phase: "dev", note: "  writing src/auth/login.tsx  " })).toBe(
      "writing src/auth/login.tsx",
    );
  });

  it("falls back to an N-of-M · phase hint when there is no note", () => {
    expect(activityLine({ phase: "review", step: 3, total: 5 })).toBe("3 of 5 · review");
  });

  it("prefers a note over an N-of-M hint when both are present", () => {
    expect(activityLine({ phase: "dev", step: 2, total: 4, note: "running tests" })).toBe(
      "running tests",
    );
  });

  it("returns null when the entry has only a phase (status dot already carries it)", () => {
    expect(activityLine({ phase: "dev" })).toBeNull();
  });

  it("returns null when the note is only whitespace and no step/total", () => {
    expect(activityLine({ phase: "dev", note: "   " })).toBeNull();
  });

  it("returns null when total is zero (never a 'N of 0' line)", () => {
    expect(activityLine({ phase: "dev", step: 1, total: 0 })).toBeNull();
  });

  it("returns null for an undefined entry (no progress yet)", () => {
    expect(activityLine(undefined)).toBeNull();
  });

  it("renders a settled note the same way (the component softens it via data-settled)", () => {
    expect(activityLine({ phase: "done", note: "Shipped the fix.", settled: true })).toBe(
      "Shipped the fix.",
    );
  });
});

describe("askLeadsWithWord", () => {
  it("matches an ask that leads with the word, alone or before a colon, in any case", () => {
    expect(askLeadsWithWord("Blocked: disk full", "Blocked")).toBe(true);
    expect(askLeadsWithWord("blocked", "Blocked")).toBe(true);
    expect(askLeadsWithWord("BLOCKED: waiting on CI", "blocked")).toBe(true);
  });

  it("does not match the word inside the ask, or another word that starts the same", () => {
    expect(askLeadsWithWord("Which ticket needs answer first?", "Needs answer")).toBe(false);
    expect(askLeadsWithWord("Blockedness: none", "Blocked")).toBe(false);
    // A hyphen that joins a word is not a separator.
    expect(askLeadsWithWord("Blocked-by-review", "Blocked")).toBe(false);
    expect(askLeadsWithWord("Blocked-waiting", "Blocked")).toBe(false);
    expect(askLeadsWithWord("Blocked/waiting", "Blocked")).toBe(false);
  });

  it("matches the word followed by a space, a spaced hyphen, a dash, or a stop or comma", () => {
    expect(askLeadsWithWord("Blocked on the migration", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked\tby CI", "Blocked")).toBe(true);
    expect(askLeadsWithWord("blocked — waiting on review", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked—waiting", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked - waiting", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked– waiting", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked; need a key", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked! Need a key", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked? On what", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked-", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked. Need a key", "Blocked")).toBe(true);
    expect(askLeadsWithWord("Blocked, need a key", "Blocked")).toBe(true);
    // Leading space in the ask, and around the word, is ignored.
    expect(askLeadsWithWord("   Blocked: x", " blocked ")).toBe(true);
    expect(askLeadsWithWord("Needs answer: which DB?", "Needs answer")).toBe(true);
  });

  it("does not match the word followed by other punctuation or letters", () => {
    expect(askLeadsWithWord("Blockers: none", "Blocked")).toBe(false);
    expect(askLeadsWithWord("Blocked_maybe", "Blocked")).toBe(false);
    expect(askLeadsWithWord("Blocked(maybe)", "Blocked")).toBe(false);
    expect(askLeadsWithWord("Blocked/unblocked", "Blocked")).toBe(false);
    expect(askLeadsWithWord("Block", "Blocked")).toBe(false);
  });

  it("matches nothing for an empty or missing word", () => {
    expect(askLeadsWithWord("Blocked: disk full", "")).toBe(false);
    expect(askLeadsWithWord(": anything", "  ")).toBe(false);
    expect(askLeadsWithWord("Blocked", undefined)).toBe(false);
    expect(askLeadsWithWord("Blocked", null)).toBe(false);
    expect(askLeadsWithWord("", "Blocked")).toBe(false);
  });
});

describe("searchTextOf", () => {
  const row = (over: Partial<AgentSessionRow> = {}): AgentSessionRow => ({
    handle: "h-1",
    sessionId: "sid-1",
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: "fix the login",
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
    displayName: "Nova",
    ...over,
  });
  const node = (r: AgentSessionRow, teamKey?: string): AgentTreeNode => ({
    row: r,
    story: null,
    workspace: { id: "ws-a", label: "Auth flow", kind: "folder" },
    children: [],
    ...(teamKey ? { teamKey } : {}),
  });
  const identity = (over: Partial<SearchIdentity> = {}): SearchIdentity => ({
    launched: new Map(),
    choicesOf: () => ({}),
    homeTeamKey: "acme:default",
    machineOf: () => "This Mac",
    machineIdOf: () => "local",
    offlineMachineOf: () => null,
    routineOf: () => null,
    isHidden: () => false,
    activityOf: () => null,
    ...over,
  });

  it("reads the name, prompt, workspace, team, role, machine and handle", () => {
    const text = searchTextOf(node(row()), identity());
    for (const word of ["Nova", "fix the login", "Auth flow", "default", "Worker", "This Mac"]) {
      expect(text).toContain(word);
    }
    expect(text).toContain("h-1");
  });

  it("reads another team's node by its own team, and a remote machine by its id too", () => {
    const text = searchTextOf(
      node(row(), "acme:ops"),
      identity({ machineOf: () => "Studio", machineIdOf: () => "m-7f3a9" }),
    );
    expect(text).toContain("ops");
    expect(text).not.toContain("default");
    expect(text).toContain("Studio");
    expect(text).toContain("m-7f3a9");
  });

  it('says "offline" and "hidden" only for an agent whose row says so', () => {
    const plain = searchTextOf(node(row()), identity());
    expect(plain).not.toMatch(/\boffline\b|\bhidden\b/);
    const text = searchTextOf(
      node(row()),
      identity({ offlineMachineOf: () => "Studio", isHidden: () => true }),
    );
    expect(text).toMatch(/\boffline\b/);
    expect(text).toMatch(/\bhidden\b/);
  });

  it("reads its activity note and the routine that started it", () => {
    const text = searchTextOf(
      node(row()),
      identity({ activityOf: () => "running migrations", routineOf: () => "Nightly triage" }),
    );
    expect(text).toContain("running migrations");
    expect(text).toContain("Nightly triage");
  });
});
