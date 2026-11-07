// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CONTEXT_HOLD_TIMEOUT_MS, createContextLandingGate } from "./context-landing-gate";

describe("createContextLandingGate", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const setup = () => {
    const sent: string[] = [];
    const onHold = vi.fn();
    const onTimeout = vi.fn();
    const gate = createContextLandingGate({
      send: (d) => sent.push(d),
      onHold,
      onTimeout,
    });
    return { gate, sent, onHold, onTimeout };
  };

  it("passes input straight through while context is not landing", () => {
    const { gate, sent, onHold } = setup();
    gate.write("ls -a\r");
    expect(sent).toEqual(["ls -a\r"]);
    expect(onHold).not.toHaveBeenCalled();
  });

  it("while landing, echoes typing but holds Enter and everything after it", () => {
    const { gate, sent, onHold } = setup();
    gate.setLanding(true);
    gate.write("l");
    gate.write("s -a");
    gate.write("\r");
    gate.write("pwd\r");
    expect(sent).toEqual(["l", "s -a"]);
    expect(onHold).toHaveBeenCalledTimes(1);
    expect(gate.isHolding()).toBe(true);
  });

  it("splits a chunk at the first Enter so the typed part still echoes", () => {
    const { gate, sent } = setup();
    gate.setLanding(true);
    gate.write("ls -a\rpwd");
    expect(sent).toEqual(["ls -a"]);
    gate.setLanding(false);
    expect(sent).toEqual(["ls -a", "\rpwd"]);
  });

  it("runs the held command, in order, the moment context lands", () => {
    const { gate, sent, onTimeout } = setup();
    gate.setLanding(true);
    gate.write("ls -a");
    gate.write("\r");
    gate.write("git status\r");
    gate.setLanding(false);
    expect(sent).toEqual(["ls -a", "\r", "git status\r"]);
    expect(gate.isHolding()).toBe(false);
    vi.advanceTimersByTime(CONTEXT_HOLD_TIMEOUT_MS * 2);
    expect(onTimeout).not.toHaveBeenCalled();
    gate.write("x");
    expect(sent.at(-1)).toBe("x");
  });

  it("gives up after the timeout: runs the held command and reports it", () => {
    const { gate, sent, onTimeout } = setup();
    gate.setLanding(true);
    gate.write("claude\r");
    vi.advanceTimersByTime(CONTEXT_HOLD_TIMEOUT_MS - 1);
    expect(sent).toEqual(["claude"]);
    vi.advanceTimersByTime(1);
    expect(sent).toEqual(["claude", "\r"]);
    expect(onTimeout).toHaveBeenCalledTimes(1);
    // Still landing, but the gate has given up: input flows again.
    gate.write("ls\r");
    expect(sent.at(-1)).toBe("ls\r");
    expect(gate.isHolding()).toBe(false);
  });

  it("Ctrl-C while holding cancels the held command instead of queueing behind it", () => {
    const { gate, sent } = setup();
    gate.setLanding(true);
    gate.write("rm -rf build\r");
    gate.write("\x03");
    expect(sent).toEqual(["rm -rf build", "\x03"]);
    expect(gate.isHolding()).toBe(false);
    gate.setLanding(false);
    expect(sent).toEqual(["rm -rf build", "\x03"]);
  });

  it("dispose drops held input and cancels the timer", () => {
    const { gate, sent, onTimeout } = setup();
    gate.setLanding(true);
    gate.write("ls\r");
    gate.dispose();
    vi.advanceTimersByTime(CONTEXT_HOLD_TIMEOUT_MS);
    gate.setLanding(false);
    gate.write("x");
    expect(sent).toEqual(["ls"]);
    expect(onTimeout).not.toHaveBeenCalled();
  });

  it("holds a bracketed paste whole instead of splitting it at its first Enter", () => {
    const { gate, sent } = setup();
    gate.setLanding(true);
    const paste = "\x1b[200~ls\rpwd\x1b[201~";
    gate.write(paste);
    expect(sent).toEqual([]);
    gate.setLanding(false);
    expect(sent).toEqual([paste]);
  });

  it("keeps Alt/Meta+Enter (ESC CR) together", () => {
    const { gate, sent } = setup();
    gate.setLanding(true);
    gate.write("ls\x1b\r");
    expect(sent).toEqual(["ls"]);
    gate.setLanding(false);
    expect(sent).toEqual(["ls", "\x1b\r"]);
  });

  it("keeps gating what follows a Ctrl-C in the same chunk", () => {
    const { gate, sent } = setup();
    gate.setLanding(true);
    gate.write("rm -rf build\r");
    gate.write("\x03ls\r");
    expect(sent).toEqual(["rm -rf build", "\x03", "ls"]);
    expect(gate.isHolding()).toBe(true);
    gate.setLanding(false);
    expect(sent).toEqual(["rm -rf build", "\x03", "ls", "\r"]);
  });

  it("says when a held command is dropped (Ctrl-C), and only then", () => {
    const onDiscard = vi.fn();
    const gate = createContextLandingGate({ send: () => {}, onDiscard });
    gate.write("\x03");
    expect(onDiscard).not.toHaveBeenCalled();
    gate.setLanding(true);
    gate.write("ls\r");
    gate.write("\x03");
    expect(onDiscard).toHaveBeenCalledOnce();
  });
});
