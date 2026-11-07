// SPDX-License-Identifier: MIT
import { act, render } from "@testing-library/react";
import { useRef } from "react";
import { Window as FrameWindow } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useSlotDebugTrace, type SlotRefreshTrigger } from "./slot-debug";

function Host({ attempts, refreshKey }: { attempts: number; refreshKey: number }) {
  const root = useRef<HTMLDivElement>(null);
  const frames = useRef(new Map<number, HTMLIFrameElement>());
  const trigger = useRef<SlotRefreshTrigger>("source");
  useSlotDebugTrace({
    slotId: "right-pane",
    panelId: "pane:private:acme/inbox",
    attempts,
    refreshKey,
    sourceRevision: "[]",
    visible: true,
    trigger,
    root,
    frames,
  });
  return (
    <div ref={root}>
      <iframe
        ref={(el) => {
          if (el) frames.current.set(attempts, el);
        }}
        title="pane"
      />
    </div>
  );
}

beforeEach(() => {
  delete window.__csDebug;
  vi.spyOn(console, "debug").mockImplementation(() => {});
});
afterEach(() => {
  vi.unstubAllEnvs();
  vi.useRealTimers();
});

describe("the slot host trace", () => {
  it("records nothing while the gate is off", () => {
    vi.useFakeTimers({
      toFake: ["requestAnimationFrame", "cancelAnimationFrame", "setTimeout", "performance"],
    });
    const { rerender } = render(<Host attempts={1} refreshKey={1} />);
    rerender(<Host attempts={2} refreshKey={2} />);
    vi.advanceTimersByTime(2000);
    expect(window.__csDebug).toBeUndefined();
  });

  it("logs a refresh with its trigger and revisions, and samples the new attempt", () => {
    vi.stubEnv("MODE", "webdriver");
    vi.useFakeTimers({
      toFake: ["requestAnimationFrame", "cancelAnimationFrame", "setTimeout", "performance"],
    });
    const { rerender } = render(<Host attempts={1} refreshKey={1} />);
    rerender(<Host attempts={2} refreshKey={2} />);
    act(() => void vi.advanceTimersByTime(2000));
    const host = window.__csDebug!.filter((e) => e.kind === "slot-host");
    expect(host[1]).toMatchObject({ trigger: "source", attempts: [1, 2], refreshKey: [1, 2] });
    const samples = window.__csDebug!.filter((e) => e.kind === "slot-sample");
    expect(samples.map((s) => s.attempt)).toEqual([1, 2]);
    expect(samples[1]).toHaveProperty("firstGapMs");
  });

  describe("the in-page probe", () => {
    const fakeTimers = () => {
      vi.stubEnv("MODE", "webdriver");
      vi.useFakeTimers({
        toFake: ["requestAnimationFrame", "cancelAnimationFrame", "setTimeout", "performance"],
      });
    };
    /** Give each iframe its own window and record the probes it is sent. */
    const wireFrames = (container: HTMLElement) =>
      [...container.querySelectorAll("iframe")].map((frame) => {
        const win = new FrameWindow();
        Object.defineProperty(frame, "contentWindow", { value: win, configurable: true });
        const sent = vi.spyOn(win, "postMessage");
        return { win, sent };
      });
    const reply = (source: unknown, data: Record<string, unknown>) =>
      window.dispatchEvent(
        new MessageEvent("message", {
          source: source as MessageEventSource,
          data: { type: "codesense:probe-result", ...data },
        }),
      );
    const probeId = (sent: ReturnType<typeof vi.spyOn>) =>
      (sent.mock.calls[0][0] as { id: number }).id;

    it("keeps each simultaneous sampler's replies to its own frame and request ids", () => {
      fakeTimers();
      const view = render(
        <>
          <Host attempts={1} refreshKey={1} />
          <Host attempts={1} refreshKey={1} />
        </>,
      );
      const [dock, inbox] = wireFrames(view.container);
      act(() => void vi.advanceTimersByTime(50));
      const dockId = probeId(dock.sent);
      const inboxId = probeId(inbox.sent);
      expect(dockId).not.toBe(inboxId);
      act(() => {
        reply(dock.win, { id: dockId, bodyBackground: "dock" });
        reply(inbox.win, { id: inboxId, bodyBackground: "inbox" });
        // Another frame answering with this frame's request id, and a reply never asked for.
        reply(inbox.win, { id: dockId, bodyBackground: "wrong-frame" });
        reply(dock.win, { id: 9999, bodyBackground: "never-asked" });
        vi.advanceTimersByTime(1600);
      });
      const [first, second] = window.__csDebug!.filter((e) => e.kind === "slot-sample");
      expect((first.probes as { bodyBackground: string }[]).map((p) => p.bodyBackground)).toEqual([
        "dock",
      ]);
      expect((second.probes as { bodyBackground: string }[]).map((p) => p.bodyBackground)).toEqual([
        "inbox",
      ]);
    });

    const ack = (frame: { win: FrameWindow }) =>
      window.dispatchEvent(
        new MessageEvent("message", {
          source: frame.win as unknown as MessageEventSource,
          data: { type: "codesense:theme-applied" },
        }),
      );
    const ackProbes = () => window.__csDebug!.filter((e) => e.kind === "slot-ack-probe");

    it.each([700, 1700])("probes the page once at a theme acknowledgment at %i ms", (ms) => {
      fakeTimers();
      const view = render(<Host attempts={1} refreshKey={1} />);
      const [frame] = wireFrames(view.container);
      act(() => void vi.advanceTimersByTime(ms));
      frame.sent.mockClear();
      act(() => {
        ack(frame);
        ack(frame);
      });
      expect(frame.sent).toHaveBeenCalledTimes(1);
      const { id } = frame.sent.mock.calls[0][0] as { id: number };
      act(() => reply(frame.win, { id, bodyBackground: "late" }));
      expect(ackProbes()).toEqual([
        expect.objectContaining({ bodyBackground: "late", attempt: 1, slot: "right-pane" }),
      ]);
      act(() => {
        ack(frame);
        reply(frame.win, { id, bodyBackground: "again" });
      });
      expect(frame.sent).toHaveBeenCalledTimes(1);
      expect(ackProbes()).toHaveLength(1);
    });

    it("disposes the acknowledgment probe when the attempt is replaced or unmounted", () => {
      fakeTimers();
      const removed = vi.spyOn(window, "removeEventListener");
      const view = render(<Host attempts={1} refreshKey={1} />);
      const [frame] = wireFrames(view.container);
      act(() => void vi.advanceTimersByTime(100));
      view.unmount();
      expect(removed.mock.calls.filter(([type]) => type === "message")).toHaveLength(2);
      frame.sent.mockClear();
      act(() => ack(frame));
      expect(frame.sent).not.toHaveBeenCalled();
      expect(ackProbes()).toHaveLength(0);
    });
  });
});
