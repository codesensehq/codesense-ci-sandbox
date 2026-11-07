// SPDX-License-Identifier: MIT
// SW-XS-10: the setup line's state machine, driven by the event fixtures the
// Rust tests generate (Step 1: `xs10_remote_workspace_progress_golden_fixture`;
// Step 2: `agent_prepare_progress_golden_fixture`), so the line and the
// emitters can't drift.

import { readFileSync } from "fs";
import { resolve } from "path";
import { describe, expect, it } from "vitest";

import {
  reducePrepareProgress,
  type AgentPrepareState,
} from "@/features/agents/lib/agent-prepare-progress";
import type { AgentPrepareProgressPayload, RemoteWorkspaceProgressPayload } from "@/tauri/events";

import {
  applyPrepare,
  clearRun,
  describeSetup,
  displayedLine,
  failRun,
  needsServerUpdate,
  NO_FLASH_MS,
  nextLineChangeAt,
  READY_LINGER_MS,
  step1StateFromLocalPhase,
  step1StateFromProgress,
  TERMINAL_SERVER_OUTDATED,
  type SetupSlots,
} from "./workspace-setup-state";

const fixture = <T>(name: string): T =>
  JSON.parse(readFileSync(resolve(__dirname, "../../../tauri/fixtures", name), "utf-8")) as T;

const step1 = fixture<{
  event: string;
  sequence: RemoteWorkspaceProgressPayload[];
  update: RemoteWorkspaceProgressPayload[];
  failed: RemoteWorkspaceProgressPayload;
}>("remote-workspace-progress.golden.json");
const step2 = fixture<{ sequence: AgentPrepareProgressPayload[] }>(
  "agent-prepare-progress.golden.json",
);

/** Folds Step 2 events per agent, as the listener does. */
function folder() {
  const by = new Map<string, AgentPrepareState>();
  return (slots: SetupSlots, event: AgentPrepareProgressPayload, now: number): SetupSlots => {
    const next = reducePrepareProgress(by.get(event.agent), event);
    by.set(event.agent, next);
    return applyPrepare(slots, next, now);
  };
}

const text = (slots: SetupSlots, now: number) => describeSetup(displayedLine(slots, now))?.text;

const ev = (agent: string, phase: AgentPrepareProgressPayload["phase"], extra = {}) =>
  ({
    workspace: "/w",
    agent,
    phase,
    files: 0,
    bytes: 0,
    total: 10,
    ...extra,
  }) as AgentPrepareProgressPayload;

describe("SW-XS-10 the Step 1 contract", () => {
  it("reads the event the Rust emitter names, including the base update", () => {
    expect(step1.event).toBe("remote-workspace-progress");
    expect(step1.update.map((p) => p.phase)).toEqual(["updatingWorktree"]);
    expect(
      describeSetup(step1StateFromProgress(step1.update[0], { machine: "studio" }))?.text,
    ).toBe("Moving to the new base on studio");
  });
});

describe("SW-XS-10 AC1: a remote create steps through every phase, Ready only after Step 2", () => {
  it("Fetching → Bundling → Copying n/N → Verifying → Creating worktree → Preparing Barb n files → Ready", () => {
    const names = { repo: "codesense", machine: "studio" };
    const lines: string[] = [];
    let slots: SetupSlots = {};
    let now = 1_000;
    const push = () => {
      const view = describeSetup(displayedLine(slots, now));
      const line = view ? [view.text, view.count].filter(Boolean).join(" · ") : "";
      if (line && lines.at(-1) !== line) lines.push(line);
    };
    for (const payload of step1.sequence) {
      slots = { base: step1StateFromProgress(payload, names) };
      push();
      now += 500;
    }
    // Step 1 is done and the agent launch follows: never Ready yet.
    slots = { base: { kind: "awaitingAgent", since: now } };
    push();
    const fold = folder();
    for (const event of step2.sequence) {
      slots = fold(slots, event, now);
      push();
      now += 300;
      push();
    }
    expect(lines).toEqual([
      "Fetching codesense · 120 / 480 objects",
      "Bundling codesense for studio · 480 objects",
      "Copying codesense to studio · 0 / 112 MB",
      "Copying codesense to studio · 41 / 112 MB",
      "Verifying the copy on studio",
      "Creating worktree on studio",
      "Preparing agent context",
      "Preparing Barb · 0 files · 0 B",
      "Preparing Barb · 1 file · 1 B",
      "Ready",
    ]);
    expect(lines.filter((l) => l === "Ready")).toHaveLength(1);
  });

  it("Step 1 finishing alone never produces Ready", () => {
    for (const payload of step1.sequence) {
      expect(describeSetup(step1StateFromProgress(payload))?.tone).toBe("busy");
    }
    expect(text({ base: { kind: "awaitingAgent", since: 0 } }, 1)).not.toBe("Ready");
  });

  it("copying is determinate with a ring fraction; verifying is indeterminate", () => {
    const copying = describeSetup(step1StateFromProgress(step1.sequence[3]));
    expect(copying?.indeterminate).toBe(false);
    expect(copying?.fraction).toBeCloseTo(42_991_616 / 117_440_512);
    expect(describeSetup(step1StateFromProgress(step1.sequence[4]))?.indeterminate).toBe(true);
  });

  it("local worktree phases map onto the same line", () => {
    expect(describeSetup(step1StateFromLocalPhase("copy"))?.text).toBe("Creating worktree");
    expect(describeSetup(step1StateFromLocalPhase("setup", "pnpm install"))?.text).toBe(
      "Running setup (pnpm install)",
    );
  });
});

describe("SW-XS-10 AC2: a no-op Step 2 under 400 ms never paints Preparing", () => {
  it("during setup it goes straight to Ready", () => {
    const fold = folder();
    const painted: string[] = [];
    let slots: SetupSlots = { base: { kind: "step1", phase: "creatingWorktree" } };
    slots = fold(slots, ev("Barb", "diff"), 0);
    for (let t = 0; t < 200; t += 50) painted.push(text(slots, t) ?? "");
    slots = fold(slots, ev("Barb", "ready"), 200);
    expect(painted.some((t) => t.startsWith("Preparing"))).toBe(false);
    expect(text(slots, 200)).toBe("Ready");
  });

  it("on a settled workspace (a re-launch) the line stays quiet: no Preparing, no Ready", () => {
    const fold = folder();
    let slots: SetupSlots = {};
    slots = fold(slots, ev("Barb", "diff"), 10_000);
    slots = fold(slots, ev("Barb", "ready"), 10_120);
    expect(slots).toEqual({});
  });

  it("a slow Step 2 shows Preparing once the window has passed, then Ready, then quiet", () => {
    const fold = folder();
    let slots = fold({}, ev("Barb", "diff"), 0);
    expect(text(slots, NO_FLASH_MS - 1)).toBeUndefined();
    expect(text(slots, NO_FLASH_MS)).toBe("Preparing Barb");
    expect(nextLineChangeAt(slots, 10)).toBe(NO_FLASH_MS);
    slots = fold(slots, ev("Barb", "ready"), 900);
    expect(text(slots, 900)).toBe("Ready");
    expect(text(slots, 900 + READY_LINGER_MS)).toBeUndefined();
  });
});

describe("SW-XS-10 review r1 (MUST 2): two agents preparing in one workspace", () => {
  it("shows the busiest, and says Ready only when neither is running", () => {
    const fold = folder();
    let slots: SetupSlots = { base: { kind: "awaitingAgent", since: 0 } };
    slots = fold(slots, ev("Barb", "diff"), 0);
    slots = fold(slots, ev("Mia", "diff"), 0);
    slots = fold(slots, ev("Barb", "transfer", { files: 2, bytes: 100, total: 1_000 }), 500);
    slots = fold(slots, ev("Mia", "transfer", { files: 9, bytes: 100, total: 90_000 }), 500);
    // Mia has more left to move: she is the line, with Barb counted.
    expect(text(slots, 600)).toBe("Preparing Mia (+1)");
    slots = fold(slots, ev("Mia", "ready"), 700);
    // Barb still runs: no Ready yet.
    expect(text(slots, 700)).toBe("Preparing Barb");
    expect(slots.base?.kind).toBe("awaitingAgent");
    slots = fold(slots, ev("Barb", "ready"), 800);
    expect(text(slots, 800)).toBe("Ready");
  });

  it("a failure wins the line and blocks Ready; one agent's progress never overwrites another's", () => {
    const fold = folder();
    let slots: SetupSlots = { base: { kind: "awaitingAgent", since: 0 } };
    slots = fold(slots, ev("Barb", "diff"), 0);
    slots = fold(slots, ev("Mia", "diff"), 0);
    slots = fold(slots, ev("Barb", "failed", { note: "disk full" }), 500);
    slots = fold(slots, ev("Mia", "ready"), 600);
    expect(text(slots, 700)).toBe("Couldn't prepare Barb — disk full");
    expect(slots.base?.kind).toBe("awaitingAgent");
    // Retrying Barb clears only Barb; Mia's done run waits with it.
    slots = clearRun(slots, "Barb", 800);
    expect(text(slots, 800)).toBe("Ready");
  });

  it("retrying one agent keeps the other's failure", () => {
    let slots = failRun({}, "Barb", "boom");
    slots = failRun(slots, "Mia", "bang");
    slots = clearRun(slots, "Barb", 0);
    expect(text(slots, 0)).toBe("Couldn't prepare Mia — bang");
  });
});

describe("SW-XS-10 AC3 / failures: Step 1 and Step 2 failure lines", () => {
  it("a Step 1 phase failure names the phase and the reason", () => {
    const failed = step1StateFromProgress(step1.failed);
    expect(failed).toMatchObject({ kind: "failed", step: 1, phase: "copying" });
    expect(describeSetup(failed)).toMatchObject({
      tone: "failed",
      text: "Couldn't create the workspace while copying — the connection to build-mac dropped",
    });
  });

  it("a Step 2 failure reads `Couldn't prepare {agent} — {reason}` from its failed entries", () => {
    const fold = folder();
    let slots = fold({}, ev("Barb", "diff"), 0);
    slots = fold(
      slots,
      ev("Barb", "failed", {
        summary: {
          ...step2.sequence.at(-1)!.summary!,
          status: "failed",
          failed: [[".claude/skills/a.md", "permission denied"]],
          failedCount: 1,
        },
      }),
      100,
    );
    expect(text(slots, 100)).toBe(
      "Couldn't prepare Barb — .claude/skills/a.md couldn't be written: permission denied",
    );
  });

  it("an outdated terminal server is recognized by its typed code, not its words", () => {
    const slots = failRun({}, "Barb", "anything at all", TERMINAL_SERVER_OUTDATED);
    const line = displayedLine(slots, 0)!;
    expect(line.kind === "failed" && needsServerUpdate(line)).toBe(true);
    expect(needsServerUpdate({ code: undefined })).toBe(false);
  });

  it("a new run starts fresh: the last run's note and summary never leak in", () => {
    let state = reducePrepareProgress(undefined, ev("Barb", "failed", { note: "old" }));
    state = reducePrepareProgress(state, ev("Barb", "diff"));
    expect(state.note).toBeUndefined();
    expect(state.summary).toBeUndefined();
    expect(state.settled).toBe(false);
  });
});

describe("SW-XS-10 AC5: one announcement per phase, never per count", () => {
  it("count ticks inside a phase announce the same text", () => {
    const a = describeSetup(step1StateFromProgress(step1.sequence[2]))!;
    const b = describeSetup(step1StateFromProgress(step1.sequence[3]))!;
    expect(a.count).not.toBe(b.count);
    expect(a.announce).toBe(b.announce);
  });
});

describe("SW-XS-10 review r1 (S1): the update-terminal-server path", () => {
  const launchNames = ["claude", "builtin/claude", "Claude"];

  it.each([
    ["Claude", "an event named by the display name"],
    ["claude", "an event named by the id"],
    ["builtin/claude", "an event named by the key"],
  ])("event (%s) then rejection updates that one run, not a second", (eventName) => {
    const fold = folder();
    let slots: SetupSlots = { base: { kind: "awaitingAgent", since: 0 } };
    slots = fold(slots, ev(eventName, "diff"), 0);
    slots = failRun(
      slots,
      launchNames,
      "Update the terminal server on studio",
      TERMINAL_SERVER_OUTDATED,
    );
    expect(Object.keys(slots.runs ?? {})).toEqual([eventName]);
    expect(slots.runs?.[eventName]).toMatchObject({
      kind: "failed",
      code: TERMINAL_SERVER_OUTDATED,
    });
  });

  it("a rejection with no event yet creates the run under the first name", () => {
    const slots = failRun({}, launchNames, "nope");
    expect(Object.keys(slots.runs ?? {})).toEqual(["claude"]);
  });

  it("a failed event carrying the backend's marker gets the typed code, without the marker in its words", () => {
    const fold = folder();
    let slots = fold({}, ev("Claude", "diff"), 0);
    slots = fold(
      slots,
      ev("Claude", "failed", {
        note: "[terminalServerOutdated] Update the terminal server on studio: it is too old to sync agent context",
      }),
      100,
    );
    const line = displayedLine(slots, 100)!;
    expect(line.kind === "failed" && needsServerUpdate(line)).toBe(true);
    expect(describeSetup(line)?.text).toBe(
      "Couldn't prepare Claude — update the terminal server on studio: it is too old to sync agent context",
    );
  });

  it("a marker in a rejection's message is read the same way", () => {
    const slots = failRun({}, ["Claude"], "[terminalServerOutdated] Update the terminal server");
    expect(slots.runs?.Claude).toMatchObject({
      code: TERMINAL_SERVER_OUTDATED,
      reason: "Update the terminal server",
    });
  });

  it("a path after the dash keeps its case", () => {
    const slots = failRun({}, ["Barb"], "CLAUDE.md couldn't be written");
    expect(describeSetup(displayedLine(slots, 0))?.text).toBe(
      "Couldn't prepare Barb — CLAUDE.md couldn't be written",
    );
  });
});
