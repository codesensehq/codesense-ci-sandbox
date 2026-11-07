// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";

import type { WakeDeliveredEvent } from "@/shared/lib/wake-events";
import {
  MARKER_RETRY_MS,
  annotateDelivery,
  annotateWake,
  findDeliveryLine,
  resetWakeMarkersForTests,
  scrollToWakeMarker,
  wakeKind,
  wakeMarkerLabel,
} from "./wake-marker";

function delivered(patch: Partial<WakeDeliveredEvent> = {}): WakeDeliveredEvent {
  return {
    machineId: "local",
    sessionId: "s-a",
    deliveryId: "d-1",
    marker: "Morning triage",
    caption: "Woken by Morning triage",
    origin: { kind: "routine", routineKey: "k", runId: "run_8f2c" },
    deliveredAtMs: new Date(2026, 9, 5, 9, 0, 4).getTime(),
    source: "push",
    replayed: false,
    ...patch,
  };
}

const TIMER = delivered({
  deliveryId: "d-2",
  marker: "Check the build (set by Barb)",
  origin: { kind: "timer", timerId: "t1" },
});

/** A terminal whose buffer holds `lines`, the cursor on the last one. */
function fakeTerm(lines: string[]) {
  const buffer = {
    lines,
    get length() {
      return this.lines.length;
    },
    baseY: 0,
    get cursorY() {
      return this.lines.length - 1;
    },
    getLine(y: number) {
      const text = this.lines[y];
      return text === undefined ? undefined : { translateToString: () => text };
    },
  };
  const disposers: Array<() => void> = [];
  const registered: number[] = [];
  const term = {
    buffer: { active: buffer },
    cols: 80,
    registerMarker: vi.fn((offset: number) => {
      const line = buffer.baseY + buffer.cursorY + offset;
      registered.push(line);
      return {
        line,
        isDisposed: false,
        dispose: () => disposers.forEach((d) => d()),
        onDispose: (cb: () => void) => {
          disposers.push(cb);
          return { dispose: vi.fn() };
        },
      };
    }),
    registerDecoration: vi.fn(() => ({
      dispose: vi.fn(),
      onRender: (cb: (el: HTMLElement) => void) => {
        paintTo = cb;
        return { dispose: vi.fn() };
      },
    })),
    scrollToLine: vi.fn(),
  };
  let paintTo: ((el: HTMLElement) => void) | null = null;
  return {
    term,
    registered,
    paint: (el: HTMLElement) => paintTo?.(el),
    disposeAll: () => disposers.forEach((d) => d()),
  };
}

const hhmm = (ms: number) => {
  const d = new Date(ms);
  return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
};

afterEach(() => {
  vi.useRealTimers();
  resetWakeMarkersForTests();
});

describe("wake marker (R4, §9.10): a scrollback annotation, nothing in the PTY stream", () => {
  it("labels a routine and a timer, naming who set the timer", () => {
    expect(wakeMarkerLabel(delivered(), hhmm)).toBe("Morning triage · routine · 9:00");
    expect(wakeMarkerLabel(TIMER, hhmm)).toBe("Check the build · timer set by Barb · 9:00");
    // The tab's own timer row wins over the marker's suffix.
    expect(wakeMarkerLabel(TIMER, hhmm, "set by you")).toBe(
      "Check the build · timer set by you · 9:00",
    );
    expect(wakeKind({ origin: { kind: "timer", timerId: null }, marker: "Ping" })).toBe("timer");
  });

  it("finds the delivery point by its run header, newest first", () => {
    const { term } = fakeTerm([
      "$ claude",
      "[routine: Morning triage | run run_old]",
      "old text",
      "> [routine: Morning triage | run run_8f2c]",
      "  triage new issues",
      "> ",
    ]);
    expect(findDeliveryLine(term as never, delivered())).toBe(3);
    const timer = fakeTerm(["> [timer: Check the build | fire 2]", "  go", "> "]);
    expect(findDeliveryLine(timer.term as never, TIMER)).toBe(0);
  });

  it("finds a header the agent's TUI wrapped onto the next row in a narrow pane (SW-AGR-7)", () => {
    // Claude Code 2.1.280 in a 60-column pane, observed: Ink word-wraps the
    // run header and indents the continuation; a long id may be hard-broken.
    const { term } = fakeTerm([
      "$ claude",
      "❯ [routine: Morning triage | run",
      "  run_8f2c]",
      "  triage new issues",
      "⏺ On it.",
    ]);
    expect(findDeliveryLine(term as never, delivered())).toBe(1);
    const broken = fakeTerm(["❯ [routine: Morning triage | run run_", "  8f2c]", "  triage"]);
    expect(findDeliveryLine(broken.term as never, delivered())).toBe(0);
    const timer = fakeTerm(["❯ [timer: Check the build |", "  fire 2]", "  go"]);
    expect(findDeliveryLine(timer.term as never, TIMER, { fires: [2] })).toBe(0);
    expect(findDeliveryLine(timer.term as never, TIMER)).toBe(0);
    // Another run's header, or the words split across unrelated rows, never match.
    const other = fakeTerm(["❯ [routine: Morning triage | run", "  run_old]", "x"]);
    expect(findDeliveryLine(other.term as never, delivered())).toBeNull();
  });

  it("prefers the header typed after the prompt glyph over one the agent quoted later", () => {
    const { term } = fakeTerm([
      "❯ [routine: Morning triage | run run_8f2c]",
      "  triage new issues",
      "⏺ You sent [routine: Morning triage | run run_8f2c] — on it.",
      "❯ ",
    ]);
    expect(findDeliveryLine(term as never, delivered())).toBe(0);
  });

  it("marks the row above the header — not the cursor — aria-hidden, titled with the run", () => {
    const { term, registered, paint } = fakeTerm([
      "$ claude",
      "working…",
      "> [routine: Morning triage | run run_8f2c]",
      "  triage new issues",
      "more output",
      "> ",
    ]);
    expect(annotateWake(term as never, delivered(), "Morning triage · routine · 9:00")).toBe(true);
    expect(registered).toEqual([1]);
    const el = document.createElement("div");
    paint(el);
    expect(el.dataset.testid).toBe("wake-marker");
    expect(el.getAttribute("aria-hidden")).toBe("true");
    expect(el.textContent).toContain("Morning triage · routine · 9:00");
    expect(el.title).toMatch(/^Delivered by Morning triage at 0?9:00:04(\s?AM)? · run run_8f2c$/);
  });

  it("looks again while the paste has not reached the terminal, then gives up", async () => {
    vi.useFakeTimers();
    const { term, registered } = fakeTerm(["$ claude", "> "]);
    annotateDelivery(delivered(), "x", () => term as never);
    expect(registered).toEqual([]);
    term.buffer.active.lines.push("> [routine: Morning triage | run run_8f2c]", "  text", "> ");
    await vi.advanceTimersByTimeAsync(MARKER_RETRY_MS[0]);
    expect(registered).toEqual([1]);

    const late = fakeTerm(["> "]);
    annotateDelivery(delivered({ deliveryId: "d-9" }), "x", () => late.term as never);
    await vi.advanceTimersByTimeAsync(MARKER_RETRY_MS.reduce((a, b) => a + b, 0));
    expect(late.term.registerMarker).not.toHaveBeenCalled();
  });

  it("annotates whichever display the tab has, local or on its machine", () => {
    const { term } = fakeTerm(["> [timer: Check the build | fire 1]", "> "]);
    const lookup = vi.fn(() => term as never);
    annotateDelivery({ ...TIMER, machineId: "codesense-test-1" }, "x", lookup);
    expect(lookup).toHaveBeenCalledWith("codesense-test-1", "s-a");
    expect(term.registerMarker).toHaveBeenCalledTimes(1);
  });

  it("scrolls to a delivery's marker until the line leaves the buffer", () => {
    const { term, disposeAll } = fakeTerm([
      "a",
      "> [routine: Morning triage | run run_8f2c]",
      "> ",
    ]);
    annotateWake(term as never, delivered(), "x");
    expect(scrollToWakeMarker("local", "s-a", "d-1")).toBe(true);
    expect(term.scrollToLine).toHaveBeenCalledWith(0);
    disposeAll();
    expect(scrollToWakeMarker("local", "s-a", "d-1")).toBe(false);
  });

  it("a replayed delivery is not annotated; a delivery is annotated once", () => {
    const { term } = fakeTerm(["> [routine: Morning triage | run run_8f2c]", "> "]);
    annotateWake(term as never, delivered({ replayed: true }), "x");
    expect(term.registerMarker).not.toHaveBeenCalled();
    annotateWake(term as never, delivered(), "x");
    annotateWake(term as never, delivered(), "x");
    expect(term.registerMarker).toHaveBeenCalledTimes(1);
  });

  it("a recurring timer's marker lands on this fire's header, not the previous fire still in scrollback", async () => {
    vi.useFakeTimers();
    // Fire 2's header is in scrollback; fire 3's paste has not reached the terminal yet.
    const { term, registered } = fakeTerm([
      "> [timer: Check the build | fire 2]",
      "  go",
      "> done",
      "> ",
    ]);
    annotateDelivery(TIMER, "x", () => term as never, { fires: [3] });
    expect(registered).toEqual([]);
    await vi.advanceTimersByTimeAsync(MARKER_RETRY_MS[0]);
    expect(registered).toEqual([]);
    term.buffer.active.lines.push("> [timer: Check the build | fire 3]", "  go", "> ");
    await vi.advanceTimersByTimeAsync(MARKER_RETRY_MS[1]);
    expect(registered).toEqual([3]);
  });

  it("without a fire number, only headers below the session's last marker count", () => {
    const { term, registered } = fakeTerm([
      "a",
      "> [timer: Check the build | fire 1]",
      "  go",
      "> ",
    ]);
    annotateWake(term as never, TIMER, "x");
    expect(registered).toEqual([0]);
    // The next delivery of the same timer: fire 1's header is above the marker, so no match yet.
    expect(annotateWake(term as never, { ...TIMER, deliveryId: "d-3" }, "x")).toBe(false);
    term.buffer.active.lines.push("> [timer: Check the build | fire 2]", "> ");
    expect(annotateWake(term as never, { ...TIMER, deliveryId: "d-3" }, "x")).toBe(true);
    expect(registered).toEqual([0, 3]);
  });

  it("a header on the buffer's first row keeps a marker to jump to, with no decoration over it", () => {
    const { term, registered } = fakeTerm(["> [routine: Morning triage | run run_8f2c]", "> "]);
    expect(annotateWake(term as never, delivered(), "x")).toBe(true);
    expect(registered).toEqual([0]);
    expect(term.registerDecoration).not.toHaveBeenCalled();
    expect(scrollToWakeMarker("local", "s-a", "d-1")).toBe(true);
  });
});
