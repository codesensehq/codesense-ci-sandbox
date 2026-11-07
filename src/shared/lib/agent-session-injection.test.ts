// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import {
  decideAtPrompt,
  injectAgentLine,
  injectWhenReady,
  pressEnter,
  type InjectDeps,
} from "@/shared/lib/agent-session-injection";

describe("pressEnter (SW-GS-2d submit-nudge)", () => {
  it("writes a bare carriage return (the discrete Enter) to the session", async () => {
    const write = vi.fn(async () => {});
    await pressEnter("s1", write);
    expect(write).toHaveBeenCalledWith("s1", "\r");
  });
});

/** Build deps with sane defaults; override per case. */
function makeDeps(over: Partial<InjectDeps> = {}): {
  deps: InjectDeps;
  write: ReturnType<typeof vi.fn>;
  submit: ReturnType<typeof vi.fn>;
} {
  const write = vi.fn(async () => {});
  // SW-GS-30 submit seam — defaults to a confirmed submit; override per case.
  const submit = vi.fn(async () => ({ submitted: true }));
  const deps: InjectDeps = {
    sessionExists: () => true,
    getAiStatus: () => undefined,
    isShellIdle: async () => true,
    write,
    submit,
    ...over,
  };
  return { deps, write, submit };
}

describe("decideAtPrompt (AC-8b pure gate)", () => {
  it("treats aiStatus 'done' as at-prompt regardless of tree-idle", () => {
    // A live claude waiting at its prompt keeps the tree non-idle, so 'done'
    // must win even when shellIdle is false.
    expect(decideAtPrompt("done", false)).toBe("at-prompt");
  });

  it("treats an active agent as busy", () => {
    expect(decideAtPrompt("running", true)).toBe("busy");
    expect(decideAtPrompt("needs-permission", true)).toBe("busy");
    expect(decideAtPrompt("error", true)).toBe("busy");
  });

  it("falls back to shell-idle when aiStatus is unknown (pre-claude zsh)", () => {
    expect(decideAtPrompt(undefined, true)).toBe("at-prompt");
    expect(decideAtPrompt(undefined, false)).toBe("busy");
  });

  it("gate 'shell' ignores aiStatus (relaunch must not wedge on a dead agent's stale status)", () => {
    // a Ctrl+C'd agent left aiStatus "running" but the zsh is idle → relaunch sends
    expect(decideAtPrompt("running", true, "shell")).toBe("at-prompt");
    expect(decideAtPrompt("done", false, "shell")).toBe("busy");
  });

  it("gate 'none' always sends (caller already confirmed the agent is live)", () => {
    expect(decideAtPrompt("running", false, "none")).toBe("at-prompt");
    expect(decideAtPrompt(undefined, false, "none")).toBe("at-prompt");
  });
});

describe("injectAgentLine (AC-8/AC-9)", () => {
  it("instruments reliable shell submits after admission without changing live-agent submits", async () => {
    const submitShell = vi.fn(async () => ({ submitted: false }));
    const { deps, submit } = makeDeps({ submitShell, isShellIdle: async () => false });
    expect(await injectAgentLine("s1", "codex", deps, { gate: "shell", submit: true })).toEqual({
      ok: false,
      reason: "agent-busy",
    });
    expect(submitShell).not.toHaveBeenCalled();
    deps.isShellIdle = async () => true;
    expect(await injectAgentLine("s1", "codex", deps, { gate: "shell", submit: true })).toEqual({
      ok: false,
      reason: "not-submitted",
    });
    expect(submitShell).toHaveBeenCalledExactlyOnceWith("s1", " codex");
    expect(submit).not.toHaveBeenCalled();
    await injectAgentLine("s1", "continue", deps, { gate: "none", submit: true });
    expect(submit).toHaveBeenCalledExactlyOnceWith("s1", "continue");
  });
  it("prepares managed shell activity only after the shell gate admits the launch", async () => {
    const writeShell = vi.fn(async () => {});
    const { deps, write } = makeDeps({ writeShell, isShellIdle: async () => false });
    expect(await injectAgentLine("s1", "claude", deps, { gate: "shell" })).toEqual({
      ok: false,
      reason: "agent-busy",
    });
    expect(writeShell).not.toHaveBeenCalled();
    deps.isShellIdle = async () => true;
    await injectAgentLine("s1", "claude", deps, { gate: "shell" });
    expect(writeShell).toHaveBeenCalledExactlyOnceWith("s1", " claude\r");
    expect(write).not.toHaveBeenCalled();
    await injectAgentLine("s1", "/status", deps, { gate: "none" });
    expect(write).toHaveBeenCalledWith("s1", "/status\r");
    expect(writeShell).toHaveBeenCalledTimes(1);
  });
  it("AC-8c: throws loudly when the session is gone (never a silent no-op)", async () => {
    const { deps, write } = makeDeps({ sessionExists: () => false });
    await expect(injectAgentLine("dead", "/cs-status", deps)).rejects.toThrow(/not found/i);
    expect(write).not.toHaveBeenCalled();
  });

  it("AC-8a: submits the line with a trailing Enter (\\r) in ONE write — same as the chat box", async () => {
    const { deps, write } = makeDeps({ isShellIdle: async () => true });
    const out = await injectAgentLine("s1", "claude", deps);
    expect(out).toEqual({ ok: true });
    // \n does not submit a raw TUI, and a SEPARATE \r write gets dropped; the
    // proven path (chat box) is text+\r atomically in one write.
    expect(write.mock.calls).toEqual([["s1", "claude\r"]]);
  });

  it("AC-8a: normalizes a trailing newline to a single \\r (never sends \\n)", async () => {
    const { deps, write } = makeDeps();
    await injectAgentLine("s1", "claude\n", deps);
    expect(write.mock.calls).toEqual([["s1", "claude\r"]]);
  });

  it("AC-8b: blocks (no write) when the agent is busy", async () => {
    const { deps, write } = makeDeps({ getAiStatus: () => "running" });
    const out = await injectAgentLine("s1", "/cs-status", deps);
    expect(out).toEqual({ ok: false, reason: "agent-busy" });
    expect(write).not.toHaveBeenCalled();
  });

  it("RF-21: gate 'shell' prefixes a LEADING SPACE so HIST_IGNORE_SPACE hides the plumbing line", async () => {
    // The agent launch line typed into the bare zsh must not land in the user's
    // ↑ history. Applied at THE chokepoint so every shell-gate caller shares it.
    const { deps, write } = makeDeps({ isShellIdle: async () => true });
    await injectAgentLine("s1", "claude --dangerously-skip-permissions", deps, { gate: "shell" });
    expect(write.mock.calls).toEqual([["s1", " claude --dangerously-skip-permissions\r"]]);
  });

  it("RF-21: shell-gate space applies on the submit seam too (workspace-tab-verbs launch path)", async () => {
    // workspace-tab-verbs' `submitLine(..., "shell")` calls injectAgentLine
    // directly with submit:true — the review gap. It must get the space here.
    const { deps, submit, write } = makeDeps({ isShellIdle: async () => true });
    await injectAgentLine("s1", "claude", deps, { gate: "shell", submit: true });
    expect(submit.mock.calls).toEqual([["s1", " claude"]]);
    expect(write).not.toHaveBeenCalled();
  });

  it("RF-21: gate 'none' is NOT space-prefixed (text bound for the live agent TUI, not the shell)", async () => {
    const { deps, write } = makeDeps();
    await injectAgentLine("s1", "hello agent", deps, { gate: "none" });
    expect(write.mock.calls).toEqual([["s1", "hello agent\r"]]);
  });

  it("injects on aiStatus 'done' WITHOUT an idle probe (no extra IPC)", async () => {
    const isShellIdle = vi.fn(async () => false);
    const { deps, write } = makeDeps({ getAiStatus: () => "done", isShellIdle });
    const out = await injectAgentLine("s1", "/cs-publish", deps);
    expect(out).toEqual({ ok: true });
    expect(write.mock.calls).toEqual([["s1", "/cs-publish\r"]]);
    expect(isShellIdle).not.toHaveBeenCalled();
  });

  // ---- SW-GS-30: opt-in reliable submit via `submit_pty_line` ----

  it("SW-GS-30 submit:true routes the RAW line through the submit seam (no hand-appended \\r, no raw write)", async () => {
    const { deps, write, submit } = makeDeps();
    const out = await injectAgentLine("s1", "/cs-status", deps, { gate: "none", submit: true });
    expect(out).toEqual({ ok: true });
    // The command owns the submit key — the seam gets the RAW line, and the raw
    // `write` fast path is NOT used.
    expect(submit.mock.calls).toEqual([["s1", "/cs-status"]]);
    expect(write).not.toHaveBeenCalled();
  });

  it("SW-GS-30 submit:true maps a held line (submitted:false) to {ok:false, reason:'not-submitted'}", async () => {
    const submit = vi.fn(async () => ({ submitted: false }));
    const { deps } = makeDeps({ submit });
    const out = await injectAgentLine("s1", "long paste", deps, { gate: "none", submit: true });
    expect(out).toEqual({ ok: false, reason: "not-submitted" });
  });

  it("SW-GS-30 submit:true still THROWS on a gone session before submitting", async () => {
    const submit = vi.fn(async () => ({ submitted: true }));
    const { deps } = makeDeps({ sessionExists: () => false, submit });
    await expect(
      injectAgentLine("dead", "/cs-status", deps, { gate: "none", submit: true }),
    ).rejects.toThrow(/not found/i);
    expect(submit).not.toHaveBeenCalled();
  });

  it("SW-GS-30 submit:true still gates on busy (no submit when mid-turn)", async () => {
    const submit = vi.fn(async () => ({ submitted: true }));
    const { deps } = makeDeps({ getAiStatus: () => "running", submit });
    const out = await injectAgentLine("s1", "/cs-status", deps, { submit: true }); // gate:auto
    expect(out).toEqual({ ok: false, reason: "agent-busy" });
    expect(submit).not.toHaveBeenCalled();
  });

  it("submit omitted keeps the raw text\\r fast path (no submit seam call)", async () => {
    const { deps, write, submit } = makeDeps();
    await injectAgentLine("s1", "claude", deps, { gate: "none" });
    expect(write.mock.calls).toEqual([["s1", "claude\r"]]);
    expect(submit).not.toHaveBeenCalled();
  });
});

describe("injectWhenReady (AC-11 reconnect poll)", () => {
  it("retries while busy, then injects once the agent reaches its prompt", async () => {
    // busy for the first two probes, then idle.
    const idleSeq = [false, false, true];
    let i = 0;
    const { deps, write } = makeDeps({ isShellIdle: async () => idleSeq[i++] ?? true });
    const sleep = vi.fn(async () => {});
    const out = await injectWhenReady("s1", "/cs-status", { sleepMs: 0, sleep }, deps);
    expect(out).toEqual({ ok: true });
    expect(write.mock.calls).toEqual([["s1", "/cs-status\r"]]);
    expect(sleep).toHaveBeenCalledTimes(2); // two busy rounds before success
  });

  it("propagates a loud throw when the session vanishes", async () => {
    const { deps } = makeDeps({ sessionExists: () => false });
    await expect(
      injectWhenReady("gone", "/cs-status", { sleep: async () => {} }, deps),
    ).rejects.toThrow(/not found/i);
  });

  it("SW-GS-30 submit:true threads to the submit seam (raw line, gate:'none')", async () => {
    const { deps, submit } = makeDeps();
    const out = await injectWhenReady("s1", "draft body", { gate: "none", submit: true }, deps);
    expect(out).toEqual({ ok: true });
    expect(submit.mock.calls).toEqual([["s1", "draft body"]]);
  });

  it("SW-GS-30 does NOT retry a not-submitted line (the write already spent its nudge budget)", async () => {
    const submit = vi.fn(async () => ({ submitted: false }));
    const sleep = vi.fn(async () => {});
    const { deps } = makeDeps({ submit });
    const out = await injectWhenReady(
      "s1",
      "held",
      { gate: "none", submit: true, sleepMs: 0, sleep },
      deps,
    );
    expect(out).toEqual({ ok: false, reason: "not-submitted" });
    expect(submit).toHaveBeenCalledTimes(1); // single attempt, no 30x retry storm
    expect(sleep).not.toHaveBeenCalled();
  });
});
