// SPDX-License-Identifier: MIT
/**
 * Debug-only trace of a `SlotDashboardHost` (SV-PLG-46 items 1 and 2), gated by {@link csDebugEnabled}.
 *
 * Every refreshKey / source / attempt change is logged with the trigger that caused it and the
 * revisions on either side. Each new attempt also starts a 1.5 s requestAnimationFrame sampler of
 * the frames under the slot root, so a real-app run can say whether the user ever saw a moment
 * with no opaque frame, or a frame shown before its page applied the theme.
 */
import { useEffect, useRef, type RefObject } from "react";

import { csDebug, csDebugEnabled } from "@/shared/lib/cs-debug";

/** What moved the host: a user Reload, a source change, or a deferred source change on reveal. */
export type SlotRefreshTrigger = "reload" | "source" | "readyEpoch";

const SAMPLE_MS = 1500;
/** The in-page probe is asked every frame for this long after a mint. */
const PROBE_MS = 600;

/** Request ids are unique across samplers, so a reply names the one request it answers. */
let lastProbeId = 0;

/** Ask `frame`'s page for its paint state; the returned id names the reply. */
function sendProbe(frame: HTMLIFrameElement): number {
  const id = ++lastProbeId;
  frame.contentWindow!.postMessage({ type: "codesense:probe", id }, "*");
  return id;
}

interface FrameSample {
  attempt: number | null;
  opacity: string;
  visibility: string;
  display: string;
  inert: boolean;
  rect: [number, number, number, number];
}

/** Visible and opaque to the user: shown, not hidden by an ancestor, and with an area. */
function opaque(sample: FrameSample): boolean {
  return (
    sample.display !== "none" &&
    sample.visibility === "visible" &&
    Number(sample.opacity) >= 1 &&
    sample.rect[2] > 0 &&
    sample.rect[3] > 0
  );
}

function sampleFrames(
  root: HTMLElement,
  attemptOf: (frame: HTMLIFrameElement) => number | null,
): FrameSample[] {
  return [...root.querySelectorAll("iframe")].map((frame) => {
    const style = getComputedStyle(frame);
    const rect = frame.getBoundingClientRect();
    return {
      attempt: attemptOf(frame),
      opacity: style.opacity,
      visibility: style.visibility,
      display: style.display,
      inert: frame.inert || frame.closest("[inert]") !== null,
      rect: [
        Math.round(rect.x),
        Math.round(rect.y),
        Math.round(rect.width),
        Math.round(rect.height),
      ],
    };
  });
}

/**
 * Sample the slot for {@link SAMPLE_MS} after `attempt` started. `acked` is the set of frame
 * windows that posted `codesense:theme-applied` while sampling; the incoming attempt's frame
 * has not acked when sampling starts, so a visible one outside it is a "visible before ack".
 * The incoming frame is probed every frame for {@link PROBE_MS}; only a reply to one of those
 * requests, from that frame, is kept. Returns the stop function, which also logs the summary.
 */
function startSlotSampler(
  root: HTMLElement,
  attempt: number,
  label: Record<string, unknown>,
  attemptOf: (frame: HTMLIFrameElement) => number | null,
): () => void {
  const started = performance.now();
  const acked = new Set<MessageEventSource>();
  const probes: Record<string, unknown>[] = [];
  const asked = new Map<number, Window>();
  const incomingFrame = () =>
    [...root.querySelectorAll("iframe")].find((frame) => attemptOf(frame) === attempt);
  const probe = (frame: HTMLIFrameElement | undefined) => {
    if (frame?.contentWindow) asked.set(sendProbe(frame), frame.contentWindow);
  };
  const onMessage = (event: MessageEvent) => {
    const data = event.data as { type?: string; id?: number } | null;
    if (data?.type === "codesense:theme-applied" && event.source) acked.add(event.source);
    // Only the e2e-webdriver shim answers; a release page ignores the request.
    if (data?.type === "codesense:probe-result" && data.id !== undefined) {
      if (event.source && event.source === asked.get(data.id))
        probes.push({ ...data, token: undefined, at: Math.round(performance.now() - started) });
    }
  };
  window.addEventListener("message", onMessage);
  const changes: { at: number; frames: FrameSample[] }[] = [];
  let last = "";
  let firstGapMs: number | null = null;
  let gapFrames = 0;
  let visibleBeforeAckMs: number | null = null;
  let raf = 0;
  const tick = () => {
    const at = Math.round(performance.now() - started);
    const frames = sampleFrames(root, attemptOf);
    const key = JSON.stringify(frames);
    if (key !== last) {
      last = key;
      changes.push({ at, frames });
    }
    if (!frames.some(opaque)) {
      gapFrames += 1;
      firstGapMs ??= at;
    }
    const incoming = incomingFrame();
    if (at < PROBE_MS) probe(incoming);
    if (visibleBeforeAckMs === null) {
      const sample = frames.find((s) => s.attempt === attempt);
      if (incoming && sample && opaque(sample) && !acked.has(incoming.contentWindow!))
        visibleBeforeAckMs = at;
    }
    if (at < SAMPLE_MS) raf = requestAnimationFrame(tick);
    else stop();
  };
  let stopped = false;
  const stop = () => {
    if (stopped) return;
    stopped = true;
    cancelAnimationFrame(raf);
    window.removeEventListener("message", onMessage);
    const style = getComputedStyle(root);
    csDebug("slot-sample", {
      ...label,
      attempt,
      firstGapMs,
      gapFrames,
      visibleBeforeAckMs,
      rootBackground: style.backgroundColor,
      rootColorScheme: style.colorScheme,
      probes,
      changes,
    });
  };
  raf = requestAnimationFrame(tick);
  return stop;
}

/**
 * Probe the attempt's page once, when its frame posts `codesense:theme-applied`, and log the
 * reply as `slot-ack-probe`. Unlike the bounded sampler this lives until that reply arrives,
 * so a cold document that acknowledges after {@link SAMPLE_MS} is still probed. Returns the
 * dispose function.
 */
function startAckProbe(
  root: HTMLElement,
  attempt: number,
  label: Record<string, unknown>,
  attemptOf: (frame: HTMLIFrameElement) => number | null,
): () => void {
  const started = performance.now();
  let asked: { id: number; window: Window } | null = null;
  const onMessage = (event: MessageEvent) => {
    const data = event.data as { type?: string; id?: number } | null;
    if (!asked && data?.type === "codesense:theme-applied" && event.source) {
      const incoming = [...root.querySelectorAll("iframe")].find((f) => attemptOf(f) === attempt);
      if (incoming?.contentWindow && event.source === incoming.contentWindow)
        asked = { id: sendProbe(incoming), window: incoming.contentWindow };
    } else if (asked && data?.type === "codesense:probe-result" && data.id === asked.id) {
      if (event.source !== asked.window) return;
      csDebug("slot-ack-probe", {
        ...label,
        attempt,
        ...data,
        token: undefined,
        at: Math.round(performance.now() - started),
      });
      dispose();
    }
  };
  const dispose = () => window.removeEventListener("message", onMessage);
  window.addEventListener("message", onMessage);
  return dispose;
}

export interface SlotTraceInput {
  slotId: string;
  panelId?: string;
  teamKey?: string;
  attempts: number;
  refreshKey: number;
  sourceRevision: string;
  visible: boolean;
  trigger: RefObject<SlotRefreshTrigger>;
  root: RefObject<HTMLElement | null>;
  frames: RefObject<Map<number, HTMLIFrameElement>>;
}

/** Log every refreshKey / source / attempt change of a host, and sample each new attempt. */
export function useSlotDebugTrace(input: SlotTraceInput): void {
  const previous = useRef<{ attempts: number; refreshKey: number; sourceRevision: string } | null>(
    null,
  );
  const { slotId, panelId, teamKey, attempts, refreshKey, sourceRevision, visible } = input;
  const { trigger, root, frames } = input;
  useEffect(() => {
    if (!csDebugEnabled()) return;
    const before = previous.current;
    if (
      before &&
      before.refreshKey === refreshKey &&
      before.sourceRevision === sourceRevision &&
      before.attempts === attempts
    )
      return;
    previous.current = { attempts, refreshKey, sourceRevision };
    csDebug("slot-host", {
      slot: slotId,
      panelId,
      teamKey,
      trigger: before ? trigger.current : "mount",
      visible,
      attempts: before ? [before.attempts, attempts] : [attempts],
      refreshKey: before ? [before.refreshKey, refreshKey] : [refreshKey],
      sourceRevision: before ? [before.sourceRevision, sourceRevision] : [sourceRevision],
    });
  }, [slotId, panelId, teamKey, attempts, refreshKey, sourceRevision, visible, trigger]);
  useEffect(() => {
    const element = root.current;
    if (!csDebugEnabled() || !element) return;
    const attemptOf = (frame: HTMLIFrameElement) => {
      for (const [attempt, candidate] of frames.current) if (candidate === frame) return attempt;
      return null;
    };
    const label = { slot: slotId, panelId, teamKey };
    const stopSampler = startSlotSampler(element, attempts, label, attemptOf);
    const disposeAckProbe = startAckProbe(element, attempts, label, attemptOf);
    return () => {
      stopSampler();
      disposeAckProbe();
    };
  }, [slotId, panelId, teamKey, attempts, root, frames]);
}
