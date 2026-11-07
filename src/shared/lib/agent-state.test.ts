// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

import {
  advanceSessionAgent,
  AGENT_STATE_DOT,
  AGENT_STATE_PHASE,
  AGENT_STATE_TERMINAL_DOT,
  AGENT_STATES,
  type AgentState,
  agentProcessName,
  agentRecordDot,
  agentStateToPhase,
  agentStateToTerminalDot,
  asAgentState,
  classifyExit,
  CLAUDE_HOOK_EVENTS,
  commandEndIsAgentExit,
  deathDot,
  EXIT_CLASSES,
  type ClaudeHookEvent,
  type ExplicitSignal,
  explicitSignalState,
  type ProcessSignal,
  processSignalState,
  isAbnormalExit,
  sessionExitOutcome,
  ptyExitIsAgentExit,
  stoppedStatuses,
  DARWIN_STOP_SIGNALS,
  LINUX_STOP_SIGNALS,
  resolveAgentState,
  SCRAPE_SIGNALS,
  type ScrapeSignal,
  type SessionAgentInput,
  type SessionAgentRecord,
  SIGNAL_DONE_OUTCOMES,
  type Tier1Input,
  type Tier2Input,
  type Tier3Input,
} from "./agent-state";
import { readFileSync } from "fs";
import { resolve } from "path";
import { PHASES, type Phase } from "./roles";
import { TERMINAL_AI_STATUSES, TERMINAL_WIRE_STATUSES } from "./terminal-ai-status";

const hook = (event: ClaudeHookEvent, seq = 1): Tier1Input => ({
  signal: { kind: "claude-hook", event },
  seq,
});
const t1 = (signal: ExplicitSignal, seq = 1): Tier1Input => ({ signal, seq });
const alive = (seq = 2): Tier2Input => ({ signal: { kind: "alive" }, seq });
const exited = (code: number | null, seq = 2, signal?: string | null): Tier2Input => ({
  signal: { kind: "exited", code, signal },
  seq,
});
const scrape = (signal: ScrapeSignal, seq = 3): Tier3Input => ({ signal, seq });

/** A Tier-1 input for each state a Tier-1 signal can declare. */
const T1_FOR: Record<Exclude<AgentState, "crashed">, Tier1Input> = {
  working: hook("PreToolUse"),
  thinking: hook("UserPromptSubmit"),
  "needs-permission": hook("PermissionRequest"),
  blocked: t1({ kind: "signal-progress", phase: "blocked" }),
  done: hook("Stop"),
  errored: hook("StopFailure"),
  idle: hook("SessionEnd"),
};

describe("AgentState literal set", () => {
  it("is the eight canonical states — the seven plus self-reported blocked", () => {
    expect([...AGENT_STATES]).toEqual([
      "working",
      "thinking",
      "needs-permission",
      "blocked",
      "done",
      "errored",
      "crashed",
      "idle",
    ]);
  });

  it("asAgentState narrows on-enum strings and rejects others", () => {
    for (const s of AGENT_STATES) expect(asAgentState(s)).toBe(s);
    expect(asAgentState("running")).toBeNull();
    expect(asAgentState("")).toBeNull();
  });
});

describe("writer mapping table", () => {
  it.each<[ClaudeHookEvent, AgentState]>([
    ["Stop", "done"],
    ["StopFailure", "errored"],
    ["PermissionRequest", "needs-permission"],
    ["UserPromptSubmit", "thinking"],
    ["PreToolUse", "working"],
    ["PostToolUse", "thinking"],
    ["PostToolUseFailure", "thinking"],
    ["SessionEnd", "idle"],
  ])("Tier-1 Claude hook %s → %s", (event, expected) => {
    expect(explicitSignalState({ kind: "claude-hook", event })).toBe(expected);
    // And through the resolver with no other tier.
    expect(resolveAgentState(hook(event), null, null, null)).toBe(expected);
  });

  it("covers every Claude hook event the table knows", () => {
    expect([...CLAUDE_HOOK_EVENTS].sort()).toEqual(
      [
        "PermissionRequest",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "SessionEnd",
        "Stop",
        "StopFailure",
        "UserPromptSubmit",
      ].sort(),
    );
  });

  it.each([
    ["success", "done"],
    ["failed", "errored"],
    ["needs-attention", "done"],
  ] as const)("Tier-1 MCP signal_done %s → %s", (outcome, expected) => {
    expect(explicitSignalState({ kind: "signal-done", outcome })).toBe(expected);
    expect(resolveAgentState(t1({ kind: "signal-done", outcome }), null, null, null)).toBe(
      expected,
    );
  });

  it("covers every signal_done outcome", () => {
    expect([...SIGNAL_DONE_OUTCOMES]).toEqual(["success", "failed", "needs-attention"]);
  });

  it("Tier-1 MCP ask_user open → needs-permission", () => {
    expect(explicitSignalState({ kind: "ask-user" })).toBe("needs-permission");
    expect(resolveAgentState(t1({ kind: "ask-user" }), null, null, null)).toBe("needs-permission");
  });

  it("Tier-1 MCP signal_progress blocked → blocked (its own state, distinct from needs-permission)", () => {
    expect(explicitSignalState({ kind: "signal-progress", phase: "blocked" })).toBe("blocked");
    expect(
      resolveAgentState(t1({ kind: "signal-progress", phase: "blocked" }), null, null, null),
    ).toBe("blocked");
  });

  it.each(PHASES.filter((p) => p !== "blocked"))(
    "Tier-1 MCP signal_progress %s → working",
    (phase: Phase) => {
      expect(explicitSignalState({ kind: "signal-progress", phase })).toBe("working");
      expect(resolveAgentState(t1({ kind: "signal-progress", phase }), null, null, null)).toBe(
        "working",
      );
    },
  );

  it("Tier-2 alive → floor working when Tier-1 empty", () => {
    expect(processSignalState({ kind: "alive" })).toBe("working");
    expect(resolveAgentState(null, alive(), null, null)).toBe("working");
  });

  it("Tier-2 alive → keeps a thinking floor when Tier-1 empty", () => {
    expect(resolveAgentState(null, alive(), null, "thinking")).toBe("thinking");
  });

  it.each<[string, ProcessSignal, AgentState]>([
    ["exit 0", { kind: "exited", code: 0 }, "done"],
    ["exit 0 with empty signal", { kind: "exited", code: 0, signal: "" }, "done"],
    ["exit 0 with null signal", { kind: "exited", code: 0, signal: null }, "done"],
    ["exit 1", { kind: "exited", code: 1 }, "crashed"],
    ["exit 137", { kind: "exited", code: 137 }, "crashed"],
    ["exit -1", { kind: "exited", code: -1 }, "crashed"],
    ["SIGKILL (no code)", { kind: "exited", code: null, signal: "SIGKILL" }, "crashed"],
    ["signal with code 0", { kind: "exited", code: 0, signal: "SIGTERM" }, "crashed"],
    ["unknown code, no signal (code-less local walk)", { kind: "exited", code: null }, "done"],
    ["unknown code, empty signal", { kind: "exited", code: null, signal: "" }, "done"],
  ])("Tier-2 %s → %s", (_label, p, expected) => {
    expect(processSignalState(p)).toBe(expected);
    expect(resolveAgentState(null, { signal: p, seq: 1 }, null, null)).toBe(expected);
  });

  it.each<[ScrapeSignal, AgentState]>([
    ["known-approval-prompt", "needs-permission"],
    ["byte-quiet", "idle"],
    ["unknown", "idle"],
  ])("Tier-3 scrape %s → %s", (s, expected) => {
    expect(resolveAgentState(null, null, scrape(s), null)).toBe(expected);
  });

  it("covers every scrape signal", () => {
    expect([...SCRAPE_SIGNALS]).toEqual(["known-approval-prompt", "byte-quiet", "unknown"]);
  });
});

describe("resolveAgentState — precedence", () => {
  it("Tier-1 done + a LATER Tier-2 alive tick → stays done (no false-running)", () => {
    expect(resolveAgentState(hook("Stop", 5), alive(6), null, "done")).toBe("done");
  });

  it("Tier-2 process death + a stale Tier-1 working → crashed", () => {
    expect(
      resolveAgentState(hook("PreToolUse", 5), exited(null, 9, "SIGKILL"), null, "working"),
    ).toBe("crashed");
    expect(resolveAgentState(hook("PreToolUse", 5), exited(1, 9), null, "working")).toBe("crashed");
  });

  it("Tier-2 process death + a stale Tier-1 thinking → crashed (kill -9 mid-turn)", () => {
    expect(
      resolveAgentState(hook("UserPromptSubmit", 5), exited(null, 9, "SIGKILL"), null, "thinking"),
    ).toBe("crashed");
  });

  it("Tier-3 unknown prompt with no higher tier → idle, never needs-permission", () => {
    expect(resolveAgentState(null, null, scrape("unknown"), null)).toBe("idle");
    expect(resolveAgentState(null, null, scrape("unknown"), "needs-permission")).toBe("idle");
  });

  it("an uncleared Tier-1 input beats any Tier-2 alive and any Tier-3 input", () => {
    for (const [state, input] of Object.entries(T1_FOR)) {
      if (state === "idle") continue;
      for (const s of SCRAPE_SIGNALS) {
        expect(resolveAgentState(input, alive(50), scrape(s, 60), null)).toBe(state);
        expect(resolveAgentState(input, null, scrape(s, 60), null)).toBe(state);
      }
    }
  });

  it("Tier-2 exit beats every Tier-3 input", () => {
    for (const s of SCRAPE_SIGNALS) {
      expect(resolveAgentState(null, exited(0, 2), scrape(s, 9), null)).toBe("done");
      expect(resolveAgentState(null, exited(2, 2), scrape(s, 9), null)).toBe("crashed");
    }
  });

  it("Tier-2 alive: known approval prompt refines the floor to needs-permission", () => {
    expect(resolveAgentState(null, alive(), scrape("known-approval-prompt"), null)).toBe(
      "needs-permission",
    );
  });

  it("Tier-2 alive: byte-quiet / unknown scrape never pulls a live process below working", () => {
    expect(resolveAgentState(null, alive(), scrape("byte-quiet"), null)).toBe("working");
    expect(resolveAgentState(null, alive(), scrape("unknown"), null)).toBe("working");
    expect(resolveAgentState(null, alive(), scrape("unknown"), "thinking")).toBe("thinking");
  });
});

describe("resolveAgentState — process-death is terminal", () => {
  it("abnormal exit supersedes every live Tier-1 state when at least as fresh", () => {
    for (const [state, input] of Object.entries(T1_FOR)) {
      const want = state === "idle" ? "idle" : "crashed";
      expect(resolveAgentState({ ...input, seq: 3 }, exited(1, 4), null, null)).toBe(want);
      expect(resolveAgentState({ ...input, seq: 3 }, exited(null, 4, "SIGKILL"), null, null)).toBe(
        want,
      );
    }
  });

  it("after SessionEnd the agent is over: a later nonzero exit is its shell's, not a crash", () => {
    expect(resolveAgentState(hook("SessionEnd", 3), exited(1, 4), null, null)).toBe("idle");
    expect(resolveAgentState(hook("SessionEnd", 3), exited(null, 4, "Hangup: 1"), null, null)).toBe(
      "idle",
    );
  });

  it("a requested exit (tab close, stop) is never crashed", () => {
    const requested = (code: number | null, signal?: string): Tier2Input => ({
      signal: { kind: "exited", code, signal, requested: true },
      seq: 4,
    });
    expect(resolveAgentState(hook("Stop", 3), requested(null, "Killed: 9"), null, null)).toBe(
      "done",
    );
    expect(resolveAgentState(hook("PreToolUse", 3), requested(null, "Killed: 9"), null, null)).toBe(
      "done",
    );
    expect(resolveAgentState(hook("StopFailure", 3), requested(1), null, null)).toBe("errored");
    expect(resolveAgentState(hook("SessionEnd", 3), requested(null, "Killed: 9"), null, null)).toBe(
      "idle",
    );
    // The same kill without the flag is a crash.
    expect(resolveAgentState(hook("Stop", 3), exited(null, 4, "Killed: 9"), null, null)).toBe(
      "crashed",
    );
  });

  it("an unannounced code-less exit crashes an in-flight turn only", () => {
    const vanished: Tier2Input = {
      signal: { kind: "exited", code: null, unannounced: true },
      seq: 4,
    };
    for (const s of ["working", "thinking", "needs-permission"] as const) {
      expect(resolveAgentState({ ...T1_FOR[s], seq: 3 }, vanished, null, s)).toBe("crashed");
    }
    expect(resolveAgentState(hook("Stop", 3), vanished, null, null)).toBe("done");
    expect(resolveAgentState(hook("SessionEnd", 3), vanished, null, null)).toBe("idle");
    expect(resolveAgentState(null, vanished, null, null)).toBe("done");
  });

  it("crashed (process died) is distinct from errored (declared failure, still alive)", () => {
    expect(resolveAgentState(hook("StopFailure", 3), alive(4), null, null)).toBe("errored");
    expect(resolveAgentState(hook("StopFailure", 3), exited(0, 4), null, null)).toBe("errored");
    expect(resolveAgentState(hook("StopFailure", 3), exited(137, 4), null, null)).toBe("crashed");
    expect(agentStateToTerminalDot("errored")).toBe(agentStateToTerminalDot("crashed"));
  });

  it("clean exit supersedes an in-flight Tier-1 state → done", () => {
    for (const s of ["working", "thinking", "needs-permission"] as const) {
      expect(resolveAgentState({ ...T1_FOR[s], seq: 3 }, exited(0, 4), null, s)).toBe("done");
    }
    expect(resolveAgentState(t1({ kind: "ask-user" }, 3), exited(0, 4), null, null)).toBe("done");
  });

  it("clean exit keeps a declared settled Tier-1 state (done / errored / idle)", () => {
    expect(resolveAgentState(hook("Stop", 3), exited(0, 4), null, null)).toBe("done");
    expect(resolveAgentState(hook("StopFailure", 3), exited(0, 4), null, null)).toBe("errored");
    expect(
      resolveAgentState(
        t1({ kind: "signal-done", outcome: "failed" }, 3),
        exited(0, 4),
        null,
        null,
      ),
    ).toBe("errored");
    expect(resolveAgentState(hook("SessionEnd", 3), exited(0, 4), null, null)).toBe("idle");
  });

  it("clean exit with no Tier-1 → done", () => {
    expect(resolveAgentState(null, exited(0), null, "working")).toBe("done");
  });

  it("an abnormal exit resolves crashed whatever prev was", () => {
    for (const prev of [...AGENT_STATES, null, undefined]) {
      expect(resolveAgentState(hook("PreToolUse", 1), exited(9, 2), null, prev)).toBe("crashed");
    }
  });

  it("an exit with an UNKNOWN code is never crashed (a normal /exit seen by the local walk)", () => {
    // SessionEnd then the process disappears, code unknown → idle, not crashed.
    expect(resolveAgentState(hook("SessionEnd", 3), exited(null, 4), null, null)).toBe("idle");
    // Stop then gone → stays done.
    expect(resolveAgentState(hook("Stop", 3), exited(null, 4), null, null)).toBe("done");
    // In-flight then gone, code unknown → done (no evidence of a crash).
    expect(resolveAgentState(hook("PreToolUse", 3), exited(null, 4), null, null)).toBe("done");
    // No Tier-1 at all, gone with unknown code → done.
    expect(resolveAgentState(null, exited(null, 4), null, "working")).toBe("done");
    // A real signal still crashes even when the code is unknown.
    expect(resolveAgentState(hook("Stop", 3), exited(null, 4, "SIGKILL"), null, null)).toBe(
      "crashed",
    );
  });
});

describe("resolveAgentState — freshness edge cases", () => {
  it("equal seq: exit and Tier-1 tie → process death wins", () => {
    expect(resolveAgentState(hook("PreToolUse", 5), exited(1, 5), null, null)).toBe("crashed");
    expect(resolveAgentState(hook("PreToolUse", 5), exited(0, 5), null, null)).toBe("done");
  });

  it("a Tier-1 declaration strictly fresher than the exit is a new life → Tier-1 wins", () => {
    expect(resolveAgentState(hook("UserPromptSubmit", 10), exited(1, 5), null, "crashed")).toBe(
      "thinking",
    );
    expect(resolveAgentState(hook("Stop", 10), exited(0, 5), null, null)).toBe("done");
  });

  it("a cleared Tier-1 newer than an old exit → idle (the exit is a previous life)", () => {
    expect(resolveAgentState(hook("SessionEnd", 10), exited(1, 5), null, null)).toBe("idle");
  });

  it("stale higher tier vs fresh lower tier: alive/scrape never override a declared state", () => {
    expect(resolveAgentState(hook("PreToolUse", 1), alive(100), null, null)).toBe("working");
    expect(
      resolveAgentState(hook("PreToolUse", 1), null, scrape("known-approval-prompt", 100), null),
    ).toBe("working");
    expect(resolveAgentState(hook("Stop", 1), null, scrape("unknown", 100), null)).toBe("done");
  });

  it("cleared Tier-1 (SessionEnd) is filled by a FRESHER alive tick, not a staler one", () => {
    expect(resolveAgentState(hook("SessionEnd", 5), alive(6), null, null)).toBe("working");
    expect(resolveAgentState(hook("SessionEnd", 5), alive(5), null, null)).toBe("idle");
    expect(resolveAgentState(hook("SessionEnd", 5), alive(4), null, null)).toBe("idle");
  });

  it("cleared Tier-1 is filled by a FRESHER scrape only (no process knowledge)", () => {
    expect(
      resolveAgentState(hook("SessionEnd", 5), null, scrape("known-approval-prompt", 6), null),
    ).toBe("needs-permission");
    expect(
      resolveAgentState(hook("SessionEnd", 5), null, scrape("known-approval-prompt", 4), null),
    ).toBe("idle");
  });

  it("a Tier-2 input older than the clear does not block a fresher scrape", () => {
    // Stale alive (pre-clear) + fresher known prompt → the scrape fills.
    expect(
      resolveAgentState(hook("SessionEnd", 5), alive(4), scrape("known-approval-prompt", 6), null),
    ).toBe("needs-permission");
    // Stale exit (pre-clear, even abnormal) + fresher known prompt → the scrape fills.
    expect(
      resolveAgentState(
        hook("SessionEnd", 5),
        exited(1, 4),
        scrape("known-approval-prompt", 6),
        null,
      ),
    ).toBe("needs-permission");
    // The scrape must itself post-date the clear.
    expect(
      resolveAgentState(hook("SessionEnd", 5), alive(4), scrape("known-approval-prompt", 5), null),
    ).toBe("idle");
  });

  it("all tiers empty → idle, whatever prev was", () => {
    for (const prev of [...AGENT_STATES, null, undefined]) {
      expect(resolveAgentState(null, null, null, prev)).toBe("idle");
      expect(resolveAgentState(undefined, undefined, undefined, prev)).toBe("idle");
    }
  });

  it("prev carry: the alive floor keeps thinking/working from prev, else floors working", () => {
    expect(resolveAgentState(null, alive(), null, "thinking")).toBe("thinking");
    expect(resolveAgentState(null, alive(), null, "working")).toBe("working");
    for (const prev of ["needs-permission", "done", "errored", "crashed", "idle", null] as const) {
      expect(resolveAgentState(null, alive(), null, prev)).toBe("working");
    }
  });

  it("prev never overrides a Tier-1 input", () => {
    for (const prev of AGENT_STATES) {
      expect(resolveAgentState(hook("Stop", 5), alive(6), null, prev)).toBe("done");
    }
  });

  it("is pure: same inputs give the same output and inputs are not mutated", () => {
    const a = hook("PreToolUse", 5);
    const b = exited(null, 9, "SIGKILL");
    const c = scrape("unknown", 10);
    const snapshot = JSON.stringify([a, b, c]);
    const first = resolveAgentState(a, b, c, "working");
    expect(resolveAgentState(a, b, c, "working")).toBe(first);
    expect(JSON.stringify([a, b, c])).toBe(snapshot);
  });

  it("resolves to a member of AGENT_STATES across the full tier matrix", () => {
    const t1s: (Tier1Input | null)[] = [
      null,
      ...Object.values(T1_FOR).map((i) => ({ ...i, seq: 2 })),
    ];
    const t2s: (Tier2Input | null)[] = [
      null,
      alive(1),
      alive(3),
      exited(0, 1),
      exited(0, 3),
      exited(1, 3),
    ];
    const t3s: (Tier3Input | null)[] = [null, ...SCRAPE_SIGNALS.map((s) => scrape(s, 4))];
    for (const a of t1s)
      for (const b of t2s)
        for (const c of t3s)
          for (const prev of [null, ...AGENT_STATES]) {
            const out = resolveAgentState(a, b, c, prev);
            expect(AGENT_STATES).toContain(out);
            // Strict-blocked bias: needs-permission only from a declaration or a KNOWN prompt.
            if (out === "needs-permission") {
              const declared = a != null && explicitSignalState(a.signal) === "needs-permission";
              expect(declared || c?.signal === "known-approval-prompt").toBe(true);
            }
          }
  });
});

describe("projections", () => {
  it("the dot projection is total; blocked folds onto the needs-permission dot (richer kind reaches Needs-you via the record)", () => {
    expect(Object.keys(AGENT_STATE_DOT).sort()).toEqual([...AGENT_STATES].sort());
    const dots = AGENT_STATES.filter((s) => s !== "idle").map((s) => AGENT_STATE_DOT[s]);
    for (const dot of dots) expect(TERMINAL_AI_STATUSES).toContain(dot);
    // The dot vocabulary is coarser than the state set — `blocked` and
    // `needs-permission` both project onto `needs-permission`. The
    // richer `NeedsYouKind.blocked` is read off the agent-state record by
    // `selectNeedsYouKind`, not the dot.
    expect(new Set(dots).size).toBe(TERMINAL_AI_STATUSES.length);
    expect([...new Set(dots)].sort()).toEqual([...TERMINAL_AI_STATUSES].sort());
    expect(AGENT_STATE_DOT.idle).toBeUndefined();
    expect(AGENT_STATE_DOT.blocked).toBe(AGENT_STATE_DOT["needs-permission"]);
  });

  it("agentStateToTerminalDot is total over AgentState and lands on wire literals", () => {
    expect(Object.keys(AGENT_STATE_TERMINAL_DOT).sort()).toEqual([...AGENT_STATES].sort());
    for (const s of AGENT_STATES) {
      const dot = agentStateToTerminalDot(s);
      if (s === "idle") expect(dot).toBeUndefined();
      else expect(TERMINAL_WIRE_STATUSES).toContain(dot);
    }
  });

  it("the wire projection is coarser: thinking→running, crashed→error, blocked→needs-permission", () => {
    expect(agentStateToTerminalDot("working")).toBe("running");
    expect(agentStateToTerminalDot("thinking")).toBe("running");
    expect(agentStateToTerminalDot("needs-permission")).toBe("needs-permission");
    expect(agentStateToTerminalDot("blocked")).toBe("needs-permission");
    expect(agentStateToTerminalDot("done")).toBe("done");
    expect(agentStateToTerminalDot("errored")).toBe("error");
    expect(agentStateToTerminalDot("crashed")).toBe("error");
    expect(agentStateToTerminalDot("idle")).toBeUndefined();
  });

  it("the dot projection reproduces today's receiver mapping for every Claude hook", () => {
    // Pins the wire status each hook sends. The Rust receiver (`resolve_status`) derives
    // from the same tables via `src-tauri/src/agent_state.rs`, whose parity test reads
    // this module. SessionEnd sends the transient "idle", which clears.
    const today: Record<ClaudeHookEvent, string | undefined> = {
      Stop: "done",
      StopFailure: "error",
      PermissionRequest: "needs-permission",
      UserPromptSubmit: "running",
      PreToolUse: "running",
      PostToolUse: "running",
      PostToolUseFailure: "running",
      SessionEnd: undefined,
    };
    for (const e of CLAUDE_HOOK_EVENTS) {
      expect(agentStateToTerminalDot(resolveAgentState(hook(e), null, null, null))).toBe(today[e]);
    }
  });

  it("agentStateToPhase is total over AgentState and lands on existing Phase values", () => {
    expect(Object.keys(AGENT_STATE_PHASE).sort()).toEqual([...AGENT_STATES].sort());
    for (const s of AGENT_STATES) {
      const phase = agentStateToPhase(s);
      if (s === "idle") expect(phase).toBeUndefined();
      else expect(PHASES).toContain(phase);
    }
  });

  it("phase projection choices", () => {
    expect(agentStateToPhase("working")).toBe("dev");
    expect(agentStateToPhase("thinking")).toBe("dev");
    expect(agentStateToPhase("needs-permission")).toBe("blocked");
    expect(agentStateToPhase("blocked")).toBe("blocked");
    expect(agentStateToPhase("done")).toBe("done");
    expect(agentStateToPhase("errored")).toBe("blocked");
    expect(agentStateToPhase("crashed")).toBe("blocked");
    expect(agentStateToPhase("idle")).toBeUndefined();
  });
});

describe("advanceSessionAgent — turn tracking with the liveness heartbeat", () => {
  const onHook = (event: ClaudeHookEvent): SessionAgentInput => ({
    tier: 1,
    signal: { kind: "claude-hook", event },
  });
  const tick: SessionAgentInput = { tier: 2, signal: { kind: "alive" } };
  const gone: SessionAgentInput = { tier: 2, signal: { kind: "exited", code: null } };

  /** Fold inputs in order with a monotonic seq; returns every intermediate record. */
  function fold(inputs: SessionAgentInput[], start?: SessionAgentRecord): SessionAgentRecord[] {
    const out: SessionAgentRecord[] = [];
    let rec = start;
    inputs.forEach((input, i) => {
      rec = advanceSessionAgent(rec, input, (start ? 1000 : 0) + i + 1);
      out.push(rec);
    });
    return out;
  }
  const states = (recs: SessionAgentRecord[]) => recs.map((r) => r.state);

  it("a hook establishes the probed agent; MCP inputs keep it; its end drops it", () => {
    const mcp = (signal: ExplicitSignal): SessionAgentInput => ({ tier: 1, signal });
    const agents = (recs: SessionAgentRecord[]) => recs.map(agentProcessName);
    expect(agents(fold([mcp({ kind: "ask-user" })]))).toEqual([null]);
    expect(
      agents(
        fold([
          onHook("UserPromptSubmit"),
          mcp({ kind: "signal-progress", phase: "dev" }),
          mcp({ kind: "ask-resolved" }),
          tick,
          gone,
          mcp({ kind: "ask-user" }),
        ]),
      ),
    ).toEqual(["claude", "claude", "claude", "claude", null, null]);
    expect(agents(fold([onHook("Stop"), onHook("SessionEnd"), mcp({ kind: "ask-user" })]))).toEqual(
      ["claude", null, null],
    );
  });

  it("submit → thinking; tool → working; between tools → thinking; Stop → done", () => {
    expect(
      states(
        fold([
          onHook("UserPromptSubmit"),
          onHook("PreToolUse"),
          onHook("PostToolUse"),
          onHook("PreToolUse"),
          onHook("PostToolUseFailure"),
          onHook("Stop"),
        ]),
      ),
    ).toEqual(["thinking", "working", "thinking", "working", "thinking", "done"]);
  });

  it("a long silent turn stays thinking across heartbeat ticks; done-at-prompt stays done", () => {
    const silent = fold([onHook("UserPromptSubmit"), ...Array<SessionAgentInput>(50).fill(tick)]);
    expect(new Set(states(silent))).toEqual(new Set(["thinking"]));

    const atPrompt = fold([
      onHook("UserPromptSubmit"),
      onHook("Stop"),
      ...Array<SessionAgentInput>(50).fill(tick),
    ]);
    expect(atPrompt.at(-1)?.state).toBe("done");
    expect(silent.at(-1)?.state).not.toBe(atPrompt.at(-1)?.state);
  });

  it("Claude vanishing mid-turn after it was seen running, with no SessionEnd, reads crashed", () => {
    const recs = fold([onHook("UserPromptSubmit"), tick, tick, gone]);
    expect(recs.at(-1)?.state).toBe("crashed");
    // Repeated "gone" ticks keep it: nothing new was observed.
    const after = fold([gone, gone], recs.at(-1));
    expect(after.every((r) => r === recs.at(-1))).toBe(true);
  });

  it("a code-less vanish is not a crash when nothing says it was one", () => {
    // Never seen running (a misrouted hook opened a turn on this session).
    expect(fold([onHook("UserPromptSubmit"), gone]).at(-1)?.state).toBe("done");
    // The turn had closed.
    expect(fold([onHook("UserPromptSubmit"), tick, onHook("Stop"), gone]).at(-1)?.state).toBe(
      "done",
    );
    // SessionEnd came first.
    expect(
      fold([onHook("UserPromptSubmit"), tick, onHook("Stop"), onHook("SessionEnd"), gone]).at(-1)
        ?.state,
    ).toBe("idle");
    // An agent that never hooked (MCP only) does not announce its end.
    const mcpOnly: SessionAgentInput = {
      tier: 1,
      signal: { kind: "signal-progress", phase: "dev" },
    };
    expect(fold([mcpOnly, tick, gone]).at(-1)?.state).toBe("done");
  });

  it("a PTY exit needs no running tick: the session's process hosted the agent", () => {
    const ptyGone: SessionAgentInput = {
      tier: 2,
      signal: { kind: "exited", code: null },
      source: "pty",
    };
    expect(fold([onHook("UserPromptSubmit"), ptyGone]).at(-1)?.state).toBe("crashed");
    const requestedGone: SessionAgentInput = {
      tier: 2,
      signal: { kind: "exited", code: null, requested: true },
      source: "pty",
    };
    expect(fold([onHook("UserPromptSubmit"), requestedGone]).at(-1)?.state).toBe("done");
  });

  it("a command-end status refines an unannounced vanish; later shell commands do not", () => {
    const code = (c: number): SessionAgentInput => ({
      tier: 2,
      signal: { kind: "exited", code: c },
      source: "command-end",
    });
    const vanished = fold([onHook("UserPromptSubmit"), tick, gone]).at(-1)!;
    expect(vanished.state).toBe("crashed");
    expect(commandEndIsAgentExit(vanished, 0)).toBe(true);
    const clean = advanceSessionAgent(vanished, code(0), 100);
    expect(clean.state).toBe("done");
    expect(commandEndIsAgentExit(clean, 0)).toBe(false);
    const killed = advanceSessionAgent(vanished, code(137), 100);
    expect(killed.state).toBe("crashed");
    expect(commandEndIsAgentExit(killed, 0)).toBe(false);
    // A later code-less "gone" does not blur the known status.
    const exited137 = fold([onHook("UserPromptSubmit"), code(137)]).at(-1)!;
    expect(advanceSessionAgent(exited137, gone, 200)).toBe(exited137);
  });

  it("heartbeat ticks never override a higher-severity declaration", () => {
    for (const [event, state] of [
      ["PermissionRequest", "needs-permission"],
      ["StopFailure", "errored"],
    ] as const) {
      const recs = fold([
        onHook("UserPromptSubmit"),
        onHook(event),
        ...Array<SessionAgentInput>(10).fill(tick),
      ]);
      expect(recs.slice(1).every((r) => r.state === state)).toBe(true);
    }
  });

  it("a sibling PreToolUse never clears needs-permission; PostToolUse does", () => {
    expect(
      states(
        fold([
          onHook("UserPromptSubmit"),
          onHook("PreToolUse"),
          onHook("PermissionRequest"),
          onHook("PreToolUse"),
          onHook("PostToolUse"),
          onHook("PostToolUse"),
        ]),
      ),
    ).toEqual([
      "thinking",
      "working",
      "needs-permission",
      "needs-permission",
      "working",
      "thinking",
    ]);
    for (const clearing of ["PostToolUseFailure", "UserPromptSubmit", "Stop"] as const) {
      const recs = fold([onHook("PermissionRequest"), onHook("PreToolUse"), onHook(clearing)]);
      expect(recs[1].state).toBe("needs-permission");
      expect(recs[2].state).toBe(explicitSignalState({ kind: "claude-hook", event: clearing }));
    }
    const blocked = fold([onHook("PermissionRequest"), gone]);
    expect(blocked.at(-1)?.state).toBe("done");
  });

  it("a live process with no hook floors to working (non-hooking agents)", () => {
    expect(states(fold([tick, tick]))).toEqual(["working", "working"]);
  });

  it("a repeated tick that changes nothing returns the same record", () => {
    const [, first, second] = fold([onHook("UserPromptSubmit"), tick, tick]);
    expect(second).toBe(first);
  });

  it("the first tick after a hook is recorded, so a later clear still orders correctly", () => {
    const recs = fold([tick, onHook("UserPromptSubmit"), tick]);
    expect(recs[2]).not.toBe(recs[1]);
    expect(recs[2].t2!.seq).toBeGreaterThan(recs[2].t1!.seq);
    // SessionEnd clears; a FRESHER live tick re-floors, an exit keeps idle.
    const cleared = advanceSessionAgent(recs[2], onHook("SessionEnd"), 10);
    expect(cleared.state).toBe("idle");
    expect(advanceSessionAgent(cleared, tick, 11).state).toBe("working");
    expect(advanceSessionAgent(cleared, gone, 11).state).toBe("idle");
  });
});

describe("exit classification — one classifier, shared with Rust", () => {
  interface ExitCase {
    code: number | null;
    signal: string | null;
    requested: boolean;
    class: string;
    outcome: string;
  }
  const { cases } = JSON.parse(
    readFileSync(
      resolve(__dirname, "../../../src-tauri/src/fixtures/agent-state/exit-classification.json"),
      "utf-8",
    ),
  ) as { cases: ExitCase[] };

  it("matches every case of the fixture the Rust classifier is tested against", () => {
    expect(cases.length).toBeGreaterThanOrEqual(10);
    for (const c of cases) {
      const exit = {
        kind: "exited" as const,
        code: c.code,
        signal: c.signal,
        requested: c.requested,
      };
      expect(classifyExit(c.code, c.signal), JSON.stringify(c)).toBe(c.class);
      expect(isAbnormalExit(exit) ? "crashed" : "done", JSON.stringify(c)).toBe(c.outcome);
    }
    expect(new Set(cases.map((c) => c.class))).toEqual(new Set(EXIT_CLASSES));
  });
});

describe("deathDot — what the dot keeps once the agent is gone", () => {
  const rec = (inputs: SessionAgentInput[]): SessionAgentRecord => {
    let r: SessionAgentRecord | undefined;
    inputs.forEach((input, i) => {
      r = advanceSessionAgent(r, input, i + 1);
    });
    return r!;
  };
  const submit: SessionAgentInput = {
    tier: 1,
    signal: { kind: "claude-hook", event: "UserPromptSubmit" },
  };
  const stop: SessionAgentInput = { tier: 1, signal: { kind: "claude-hook", event: "Stop" } };
  const tick: SessionAgentInput = { tier: 2, signal: { kind: "alive" } };
  const gone: SessionAgentInput = { tier: 2, signal: { kind: "exited", code: null } };
  const code = (c: number): SessionAgentInput => ({
    tier: 2,
    signal: { kind: "exited", code: c },
    source: "command-end",
  });

  it("no record → clear", () => {
    expect(deathDot(undefined)).toBeUndefined();
  });

  it("a crash always holds error; a known exit code holds its state", () => {
    expect(deathDot(rec([submit, tick, gone]))).toBe("error");
    expect(deathDot(rec([submit, code(137)]))).toBe("error");
    expect(deathDot(rec([submit, stop, code(0)]))).toBe("done");
  });

  it("a code-less clean vanish clears, as the floor always has", () => {
    expect(deathDot(rec([submit, stop, gone]))).toBeUndefined();
  });

  it("an agent that did not end on an exit clears (its tab's shell exited)", () => {
    expect(deathDot(rec([submit, stop]))).toBeUndefined();
    expect(deathDot(rec([submit, tick]))).toBeUndefined();
  });
});

describe("agentRecordDot — when the record decides the dot", () => {
  const rec = (inputs: SessionAgentInput[]): SessionAgentRecord => {
    let r: SessionAgentRecord | undefined;
    inputs.forEach((input, i) => {
      r = advanceSessionAgent(r, input, i + 1);
    });
    return r!;
  };
  const hookIn = (event: ClaudeHookEvent): SessionAgentInput => ({
    tier: 1,
    signal: { kind: "claude-hook", event },
  });
  const tick: SessionAgentInput = { tier: 2, signal: { kind: "alive" } };
  const gone: SessionAgentInput = { tier: 2, signal: { kind: "exited", code: null } };
  const code = (c: number): SessionAgentInput => ({
    tier: 2,
    signal: { kind: "exited", code: c },
    source: "command-end",
  });

  it("a live record projects its finer state", () => {
    expect(agentRecordDot(rec([hookIn("UserPromptSubmit")]))).toBe("thinking");
    expect(agentRecordDot(rec([hookIn("UserPromptSubmit"), hookIn("PreToolUse")]))).toBe("running");
    expect(agentRecordDot(rec([hookIn("PermissionRequest")]))).toBe("needs-permission");
    expect(agentRecordDot(rec([hookIn("Stop")]))).toBe("done");
    expect(agentRecordDot(rec([hookIn("StopFailure")]))).toBe("error");
  });

  it("a crash holds crashed; a known exit status holds its state", () => {
    expect(agentRecordDot(rec([hookIn("UserPromptSubmit"), code(137)]))).toBe("crashed");
    expect(agentRecordDot(rec([hookIn("UserPromptSubmit"), tick, gone]))).toBe("crashed");
    expect(agentRecordDot(rec([hookIn("Stop"), code(0)]))).toBe("done");
  });

  it("no say — the stored status stands — without a record or once the agent left", () => {
    expect(agentRecordDot(undefined)).toBeNull();
    expect(agentRecordDot(rec([hookIn("UserPromptSubmit"), hookIn("SessionEnd")]))).toBeNull();
    expect(agentRecordDot(rec([hookIn("Stop"), gone]))).toBeNull();
  });
});

describe("which exits are the agent's", () => {
  const rec = (inputs: SessionAgentInput[]): SessionAgentRecord => {
    let r: SessionAgentRecord | undefined;
    inputs.forEach((input, i) => {
      r = advanceSessionAgent(r, input, i + 1);
    });
    return r!;
  };
  const hookIn = (event: ClaudeHookEvent): SessionAgentInput => ({
    tier: 1,
    signal: { kind: "claude-hook", event },
  });
  const signalDone: SessionAgentInput = {
    tier: 1,
    signal: { kind: "signal-done", outcome: "success" },
  };

  it("the shell's exit is the agent's only while the agent is established with its turn open", () => {
    expect(ptyExitIsAgentExit(rec([hookIn("UserPromptSubmit")]))).toBe(true);
    expect(ptyExitIsAgentExit(rec([hookIn("UserPromptSubmit"), hookIn("PreToolUse")]))).toBe(true);
    // Finished: Stop, SessionEnd, or an MCP-only agent (never established).
    expect(ptyExitIsAgentExit(rec([hookIn("UserPromptSubmit"), hookIn("Stop")]))).toBe(false);
    expect(ptyExitIsAgentExit(rec([hookIn("Stop"), hookIn("SessionEnd")]))).toBe(false);
    expect(ptyExitIsAgentExit(rec([signalDone]))).toBe(false);
    expect(
      ptyExitIsAgentExit(rec([{ tier: 1, signal: { kind: "signal-progress", phase: "dev" } }])),
    ).toBe(false);
  });

  it("stop statuses are 128 + this host OS's STOP/TSTP/TTIN/TTOU", () => {
    expect([...DARWIN_STOP_SIGNALS]).toEqual([17, 18, 21, 22]);
    expect([...LINUX_STOP_SIGNALS]).toEqual([19, 20, 21, 22]);
    expect([...stoppedStatuses(true)].sort()).toEqual([145, 146, 149, 150]);
    expect([...stoppedStatuses(false)].sort()).toEqual([147, 148, 149, 150]);
  });

  it("a stop status (Ctrl-Z) is never the agent's exit; the other OS's is", () => {
    const running = rec([hookIn("UserPromptSubmit")]);
    const platform = vi.spyOn(navigator, "platform", "get");
    try {
      platform.mockReturnValue("MacIntel");
      expect(commandEndIsAgentExit(running, 146)).toBe(false);
      expect(commandEndIsAgentExit(running, 148)).toBe(true);
      platform.mockReturnValue("Linux x86_64");
      expect(commandEndIsAgentExit(running, 148)).toBe(false);
      expect(commandEndIsAgentExit(running, 146)).toBe(true);
      expect(commandEndIsAgentExit(running, 137)).toBe(true);
      expect(commandEndIsAgentExit(running, 1)).toBe(true);
    } finally {
      platform.mockRestore();
    }
  });
});

describe("sessionExitOutcome (the ended/crashed marks)", () => {
  const exit = (code: number | null, signal: string | null = null, requested = false) => ({
    code,
    signal,
    requested,
  });
  it.each([
    ["exit 0", exit(0), false, "done"],
    ["`false; exit` (1)", exit(1), false, "done"],
    ["exit 127", exit(127), false, "done"],
    ["`exit` after a Ctrl-C'd command (130)", exit(130), false, "done"],
    ["exit 137 from the shell itself", exit(137), false, "done"],
    ["killed by a signal", exit(null, "Killed: 9"), false, "crashed"],
    ["unknown status", exit(null), false, "done"],
    ["the exit that crashed its agent", exit(1), true, "crashed"],
    ["CodeSense's own kill", exit(null, "Killed: 9", true), true, "done"],
  ] as const)("%s → %s", (_name, e, agentCrashed, expected) => {
    expect(sessionExitOutcome(e, agentCrashed)).toBe(expected);
  });
});
