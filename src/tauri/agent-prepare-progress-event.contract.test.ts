// SPDX-License-Identifier: MIT
// SW-XS-7: the `agent-prepare-progress` contract. The Rust test
// `agent_prepare_progress_golden_fixture` writes one real run's event sequence to
// the golden fixture; this checks the TS constant, the payload type and the
// reducer against it, so the emitter and XS-10's renderer can't drift.

import { readFileSync } from "fs";
import { resolve } from "path";
import { describe, expect, it, vi } from "vitest";

import {
  prepareHeadline,
  prepareKey,
  keptModifiedNote,
  reducePrepareProgress,
  type AgentPrepareState,
} from "@/features/agents/lib/agent-prepare-progress";

import { AGENT_PREPARE_PROGRESS_EVENT, type AgentPrepareProgressPayload } from "./events";

const golden = JSON.parse(
  readFileSync(resolve(__dirname, "fixtures/agent-prepare-progress.golden.json"), "utf-8"),
) as { event: string; sequence: AgentPrepareProgressPayload[] };

const PHASES = new Set(["diff", "transfer", "apply", "warning", "ready", "failed", "skipped"]);

describe("SW-XS-7 agent-prepare-progress contract", () => {
  it("the TS event const equals the Rust event name", () => {
    expect(golden.event).toBe(AGENT_PREPARE_PROGRESS_EVENT);
  });

  it("every event has the documented shape", () => {
    for (const event of golden.sequence) {
      expect(PHASES.has(event.phase)).toBe(true);
      for (const key of ["workspace", "agent", "files", "bytes", "total"] as const) {
        expect(event).toHaveProperty(key);
      }
    }
  });

  it("ready is last and comes once, after the warning, carrying the summary", () => {
    const phases = golden.sequence.map((e) => e.phase);
    expect(phases[0]).toBe("diff");
    expect(phases.at(-1)).toBe("ready");
    expect(phases.filter((p) => p === "ready")).toHaveLength(1);
    expect(phases.indexOf("warning")).toBe(phases.length - 2);
    const ready = golden.sequence.at(-1)!;
    expect(ready.summary?.pinnedDrift).toEqual(["CLAUDE.md"]);
    expect(ready.summary?.keptModified).toEqual(["CLAUDE.md"]);
  });

  it("the reducer settles on ready and keeps the warning", () => {
    let state: AgentPrepareState | undefined;
    for (const event of golden.sequence) state = reducePrepareProgress(state, event);
    expect(state?.settled).toBe(true);
    expect(state?.phase).toBe("ready");
    expect(state?.warning).toContain("pinned file differs");
    // Scoped to the drifted paths, never an unscoped overwrite.
    expect(state?.warning).toContain(
      'hydrate_worktree with paths:["CLAUDE.md"] and overwriteModified:true',
    );
    expect(state?.summary?.written).toBe(1);
  });

  it("SW-XS-11: the note says the startup note's kept-modified count (§8.5, §11.2)", () => {
    let state: AgentPrepareState | undefined;
    for (const event of golden.sequence) state = reducePrepareProgress(state, event);
    expect(keptModifiedNote(state?.summary)).toBe("1 file kept (modified)");
    expect(state?.note).toContain("1 kept because you modified them: CLAUDE.md.");
    expect(state?.note).toMatch(/^Prepared 3 files /);
    expect(keptModifiedNote({ ...state!.summary!, keptModifiedCount: 2 })).toBe(
      "2 files kept (modified)",
    );
    expect(keptModifiedNote({ ...state!.summary!, keptModifiedCount: 0 })).toBeUndefined();
  });
});

// SW-XS-8: the explicit operations (Step 3) emit the same event with an
// `operationId`; the Rust test `explicit_operation_progress_golden_fixture`
// writes one hydrate (ending `ready`) and one cancelled hydrate.
const explicit = JSON.parse(
  readFileSync(resolve(__dirname, "fixtures/explicit-operation-progress.golden.json"), "utf-8"),
) as {
  event: string;
  ready: AgentPrepareProgressPayload[];
  cancelled: AgentPrepareProgressPayload[];
};

describe("SW-XS-8 explicit-operation progress contract", () => {
  it("is the same event, every payload naming its operation", () => {
    expect(explicit.event).toBe(AGENT_PREPARE_PROGRESS_EVENT);
    for (const event of [...explicit.ready, ...explicit.cancelled]) {
      expect(PHASES.has(event.phase) || event.phase === "cancelled").toBe(true);
      expect(event.operationId).toBe("op-1");
    }
  });

  it("ends in exactly one terminal state the reducer settles on", () => {
    for (const [sequence, terminal] of [
      [explicit.ready, "ready"],
      [explicit.cancelled, "cancelled"],
    ] as const) {
      const phases = sequence.map((e) => e.phase);
      expect(phases[0]).toBe("diff");
      expect(phases.at(-1)).toBe(terminal);
      let state: AgentPrepareState | undefined;
      for (const event of sequence) state = reducePrepareProgress(state, event);
      expect(state?.settled).toBe(true);
      expect(state?.phase).toBe(terminal);
    }
  });
});

describe("SW-XS-8 review r1: progress is keyed by operationId", () => {
  it("two operations on one workspace fold separately; a launch keys by workspace + agent", () => {
    const a = explicit.ready[0];
    const b = { ...a, operationId: "op-2" };
    expect(prepareKey(a.workspace, a.agent, a.operationId)).not.toBe(
      prepareKey(b.workspace, b.agent, b.operationId),
    );
    expect(prepareKey("ws", "Barb")).toBe(prepareKey("ws", "Barb", undefined));
  });

  it("a cancelled operation settles with its own line, distinct from failed", () => {
    let state: AgentPrepareState | undefined;
    for (const event of explicit.cancelled) state = reducePrepareProgress(state, event);
    expect(state?.settled).toBe(true);
    expect(state?.operationId).toBe("op-1");
    expect(state?.headline).toBe(prepareHeadline("cancelled"));
    expect(prepareHeadline("cancelled")).not.toBe(prepareHeadline("failed"));
  });
});

describe("SW-XS-8 review r1: publish headlines and eviction", () => {
  it("a publish settles with publish-specific lines", () => {
    expect(prepareHeadline("ready", "publish")).toBe("Published to team content");
    expect(prepareHeadline("cancelled", "publish")).toContain("team content was not changed");
    expect(prepareHeadline("ready", "publish")).not.toBe(prepareHeadline("ready"));
  });

  it("drops a settled operation's state; the next event for that id starts fresh", async () => {
    const handlers: Array<(e: { payload: AgentPrepareProgressPayload }) => void> = [];
    vi.doMock("@tauri-apps/api/event", () => ({
      listen: (_: string, cb: (e: { payload: AgentPrepareProgressPayload }) => void) => {
        handlers.push(cb);
        return Promise.resolve(() => {});
      },
    }));
    vi.resetModules();
    const { listenAgentPrepareProgress } =
      await import("@/features/agents/lib/agent-prepare-progress");
    const seen: AgentPrepareState[] = [];
    await listenAgentPrepareProgress((s) => seen.push(s));
    const base = explicit.ready[0];
    handlers[0]({ payload: { ...base, phase: "diff", note: "first run" } });
    handlers[0]({ payload: { ...base, phase: "ready", note: undefined } });
    handlers[0]({ payload: { ...base, phase: "diff", note: undefined } });
    expect(seen.at(-1)?.note).toBeUndefined();
    vi.doUnmock("@tauri-apps/api/event");
  });
});
