// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";

import { acquireSessionInput } from "./session-input";

// A pane takes the session's input path while mounted (the effect's setup) and
// releases it on unmount (its cleanup). A pane that moves unmounts and mounts
// again in one commit; StrictMode runs setup → cleanup → setup.

const flush = () => new Promise<void>((r) => setTimeout(r, 0));
const microtask = () => new Promise<void>((r) => queueMicrotask(r));

afterEach(() => vi.useRealTimers());

describe("acquireSessionInput", () => {
  it("keystrokes typed before the attach survive a remount and flush once it settles", async () => {
    const sent: string[] = [];
    const first = acquireSessionInput("s1#0", (d) => sent.push(d), {});
    first.setLanding(false);
    first.write("ls");
    first.write(" -la");
    // The pane moves: unmount, then mount again.
    first.release();
    const moved = acquireSessionInput("s1#0", (d) => sent.push(d), {});
    first.write("dropped"); // a released handle no longer types
    moved.write("\r");
    expect(sent).toEqual([]);
    moved.markReady(true);
    await microtask();
    expect(sent.join("")).toBe("ls -la\r");
    moved.release();
  });

  it("a command held for the context survives a remount, and the moved pane hears about it", async () => {
    const sent: string[] = [];
    const firstDiscard = vi.fn();
    const first = acquireSessionInput("s2#0", (d) => sent.push(d), { onDiscard: firstDiscard });
    first.setLanding(true);
    first.markReady(true);
    first.write("make test\r");
    first.release();
    const movedDiscard = vi.fn();
    const moved = acquireSessionInput("s2#0", (d) => sent.push(d), { onDiscard: movedDiscard });
    expect(sent).toEqual([]);
    moved.setLanding(false);
    await microtask();
    expect(sent.join("")).toBe("make test\r");
    expect(firstDiscard).not.toHaveBeenCalled();
    moved.release();
  });

  it("StrictMode's setup → cleanup → setup keeps one working path", async () => {
    const sent: string[] = [];
    const a = acquireSessionInput("s3#0", (d) => sent.push(d), {});
    a.release();
    const b = acquireSessionInput("s3#0", (d) => sent.push(d), {});
    b.markReady(true);
    b.write("x");
    await microtask();
    expect(sent).toEqual(["x"]);
    b.release();
  });

  it("with no pane taking it back, the path is torn down and held input dropped", async () => {
    const sent: string[] = [];
    const a = acquireSessionInput("s4#0", (d) => sent.push(d), {});
    a.write("held");
    a.release();
    await flush();
    const fresh = acquireSessionInput("s4#0", (d) => sent.push(d), {});
    fresh.markReady(true);
    await microtask();
    expect(sent).toEqual([]);
    fresh.release();
  });

  it("a new life of the session gets its own path", async () => {
    const sent: string[] = [];
    const old = acquireSessionInput("s5#0", (d) => sent.push(`old:${d}`), {});
    old.write("stale");
    const next = acquireSessionInput("s5#1", (d) => sent.push(`new:${d}`), {});
    next.markReady(true);
    next.write("fresh");
    await microtask();
    expect(sent).toEqual(["new:fresh"]);
    old.release();
    next.release();
  });
});
