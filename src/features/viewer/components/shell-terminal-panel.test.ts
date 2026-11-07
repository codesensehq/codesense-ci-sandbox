// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createPtyInputBuffer, type PtyInputBuffer } from "@/shared/lib/pty-input-buffer";
import { coordinatorAllowsRevealFocus, gateRevealFocus } from "./shell-terminal-panel";
import { activateAndFocus, currentActivationGeneration } from "../lib/focus-cascade";
import { routeTerminalInput } from "../lib/terminal-input-gate";

describe("coordinatorAllowsRevealFocus (OC-9c panel composition — fail-open + delegation)", () => {
  // Drive the real coordinator: activateAndFocus with an explicit workspaceId
  // never reads the viewer store synchronously, and rAF is stubbed to a no-op, so
  // the record is written without a store mock. Distinct teamKeys per case.
  beforeEach(() => vi.stubGlobal("requestAnimationFrame", () => 0));
  afterEach(() => vi.unstubAllGlobals());

  it("fails open (allows) when the panel has no teamKey to consult", () => {
    expect(coordinatorAllowsRevealFocus(undefined, "ws-a", 0)).toBe(true);
  });

  it("fails open (allows) when the panel has no workspaceId to consult", () => {
    expect(coordinatorAllowsRevealFocus("car-t", undefined, 0)).toBe(true);
  });

  it("delegates to the coordinator: allows a still-current restore", () => {
    activateAndFocus({ teamKey: "car-current", workspaceId: "ws-a" });
    const stamp = currentActivationGeneration("car-current");
    expect(coordinatorAllowsRevealFocus("car-current", "ws-a", stamp)).toBe(true);
  });

  it("a real explicit file open vetoes a delayed attach at the same activation generation", () => {
    const orgBefore = useOrgStore.getState();
    const viewerBefore = useViewerStore.getState();
    useOrgStore.setState({ activeOrgSlug: "car", activeTeamName: "file" });
    try {
      activateAndFocus({ teamKey: "car:file", workspaceId: "ws-a" });
      const stamp = currentActivationGeneration("car:file");
      useViewerStore.getState().openFile("car:file", "", "/chosen.md");
      expect(currentActivationGeneration("car:file")).toBe(stamp);
      expect(coordinatorAllowsRevealFocus("car:file", "ws-a", stamp)).toBe(false);
    } finally {
      useOrgStore.setState(orgBefore);
      useViewerStore.setState(viewerBefore);
    }
  });

  it("delegates to the coordinator: suppresses a restore superseded by a reselect", () => {
    activateAndFocus({ teamKey: "car-super", workspaceId: "ws-a" });
    const stamp = currentActivationGeneration("car-super");
    activateAndFocus({ teamKey: "car-super", workspaceId: "ws-b" }); // supersede with a new target
    expect(coordinatorAllowsRevealFocus("car-super", "ws-a", stamp)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// SW-EW-39 hotfix — pre-spawn input buffer must survive a React StrictMode
// setup → cleanup → setup double-invoke of the spawn effect that owns it.
//
// The original SW-EW-39 landing lazy-init'd the buffer in the RENDER body but
// disposed it in the SPAWN EFFECT's cleanup. Under `<React.StrictMode>` (dev)
// React runs each effect as setup → cleanup → setup on mount; a render-body
// init doesn't re-run between the cleanup and the re-setup, so the second
// setup found the ref pointing at the ALREADY-DISPOSED buffer from the first
// setup. Every subsequent `write()` hit `if (disposed) return` → 100% input
// loss on every terminal. Confirmed against `src/main.tsx` wrapping the app in
// `<React.StrictMode>`.
//
// The fix moves buffer create+dispose INTO the effect body (a local `buffer`
// binding stored into `inputBufferRef` on setup, disposed and nulled on
// cleanup). These tests model the effect's setup/cleanup lifecycle directly
// (the panel can't render under happy-dom) and lock in that a re-setup after
// a cleanup produces a working buffer — the invariant that was silently
// broken by the render-body init pattern.
// ---------------------------------------------------------------------------

/** Mirrors the spawn effect's buffer create+dispose scoped to a single effect
 *  invocation. The write funnels (`onWrite`, `term.onData`) read the ref live,
 *  so both callers naturally see whichever buffer the CURRENT setup owns. */
function effectSetup(
  ref: { current: PtyInputBuffer | null },
  send: (data: string) => void,
): { buffer: PtyInputBuffer; cleanup: () => void } {
  const buffer = createPtyInputBuffer(send);
  ref.current = buffer;
  const cleanup = () => {
    buffer.dispose();
    ref.current = null;
  };
  return { buffer, cleanup };
}

describe("SW-EW-39 buffer lifecycle — StrictMode double-invoke", () => {
  it("re-setup after cleanup produces a fresh, writeable buffer (the exact regression class)", async () => {
    const ref: { current: PtyInputBuffer | null } = { current: null };
    const send = vi.fn<(data: string) => void>();

    // Simulate the effect's double-invoke: setup → cleanup → setup.
    const { cleanup: cleanup1 } = effectSetup(ref, send);
    cleanup1();
    const { buffer: buffer2 } = effectSetup(ref, send);
    // Arm the fresh buffer as the real spawn/attach resolve would.
    buffer2.markReady();

    // The write funnels reference `ref.current` — the CURRENT buffer, which
    // must be the live re-setup instance, not the disposed first one.
    ref.current!.write("hello");

    // SW-FX-51: ready-egress writes coalesce behind a microtask flush window.
    await Promise.resolve();
    expect(send).toHaveBeenCalledExactlyOnceWith("hello");
  });

  it("holds pre-ready input on the fresh buffer and flushes on markReady (spawn-window contract survives)", () => {
    const ref: { current: PtyInputBuffer | null } = { current: null };
    const send = vi.fn<(data: string) => void>();

    const { cleanup: cleanup1 } = effectSetup(ref, send);
    cleanup1();
    const { buffer: buffer2 } = effectSetup(ref, send);

    // Typing during the spawn window (buffer holding) — held, not sent.
    ref.current!.write("l");
    ref.current!.write("s");
    ref.current!.write("\r");
    expect(send).not.toHaveBeenCalled();

    // Session registered — flush in order.
    buffer2.markReady();
    expect(send.mock.calls.map((c) => c[0])).toEqual(["l", "s", "\r"]);
  });

  it("real unmount — dispose + null the ref, then no further writes reach send", async () => {
    const ref: { current: PtyInputBuffer | null } = { current: null };
    const send = vi.fn<(data: string) => void>();

    const { buffer, cleanup } = effectSetup(ref, send);
    buffer.markReady();
    ref.current!.write("live");
    // SW-FX-51: let the coalesced ready-egress flush run before unmounting.
    await Promise.resolve();
    expect(send).toHaveBeenCalledExactlyOnceWith("live");

    cleanup();
    // Ref is null, so the write funnels' `?.write(data)` no-ops — nothing is
    // sent, nothing throws.
    ref.current?.write("post-unmount");
    await Promise.resolve();
    expect(send).toHaveBeenCalledExactlyOnceWith("live");
    expect(ref.current).toBeNull();
  });
});

// SW-GS-1 AC-11 — the input-lock gate. The gate must cover the FULL input
// surface: keydown AND paste AND IME/composition AND bracketed-paste (the
// structural `/clear` fix). Both of the panel's funnels (xterm's `onData` and
// the custom key handler's `onWrite`) go through `routeTerminalInput`.
describe("input-lock gate (AC-11)", () => {
  const route = (data: string, gate: { exited: boolean; locked: boolean }) => {
    const write = vi.fn<(d: string) => void>();
    const toBar = vi.fn();
    routeTerminalInput(data, gate, { write, term: null, toBar });
    return { write, toBar };
  };
  const locked = { exited: false, locked: true };

  it.each([
    ["a keydown", "a"],
    ["a paste (the /clear fix)", "/clear\n"],
    ["a bracketed paste", "\x1b[200~rm -rf /\x1b[201~"],
    ["IME composition output", "日本語"],
    ["Ctrl-C", "\x03"],
  ])("%s does NOT reach the PTY while locked", (_what, data) => {
    expect(route(data, locked).write).not.toHaveBeenCalled();
  });

  it("forwards input once unlocked", () => {
    const { write } = route("ls\n", { exited: false, locked: false });
    expect(write).toHaveBeenCalledWith("ls\n");
  });

  it("nothing reaches a shell that is gone, locked or not; a key goes back to the bar", () => {
    for (const gate of [
      { exited: true, locked: false },
      { exited: true, locked: true },
    ]) {
      const { write, toBar } = route("a", gate);
      expect(write).not.toHaveBeenCalled();
      expect(toBar).toHaveBeenCalledTimes(1);
    }
  });
});

describe("the ended pane's decisions", () => {
  it("while ended, focus headed for the terminal goes to the bar; otherwise the coordinator decides", () => {
    const claim = vi.fn();
    const coordinator = vi.fn(() => true);
    expect(gateRevealFocus(true, claim, coordinator)).toBe(false);
    expect(claim).toHaveBeenCalledTimes(1);
    expect(coordinator).not.toHaveBeenCalled();
    expect(gateRevealFocus(false, claim, coordinator)).toBe(true);
    expect(claim).toHaveBeenCalledTimes(1);
  });
});
