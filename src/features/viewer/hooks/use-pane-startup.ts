// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";

/** A pane whose first paint is this late raises the quiet "starting" spinner.
 *  Warm-pool opens and reattaches paint well under it, so they never flash one. */
export const PANE_SPINNER_DELAY_MS = 300;
/** A pane still unpainted this long says so: "Still starting…". */
export const PANE_STILL_STARTING_MS = 2_000;
/** A pane still unpainted this long has failed: it offers Retry. */
export const PANE_START_TIMEOUT_MS = 15_000;

/**
 * What a pane shows before its first paint:
 * - `quiet`: nothing yet (the first {@link PANE_SPINNER_DELAY_MS}, and while hidden);
 * - `starting`: the spinner;
 * - `slow`: "Still starting…";
 * - `failed`: the attach failed, or the pane never painted — Retry;
 * - `painted`: the terminal has content and the renderer has drawn it.
 */
export type PaneStartupPhase = "quiet" | "starting" | "slow" | "failed" | "painted";

export interface PaneStartupFailure {
  reason: "error" | "timeout";
  detail?: string;
  /** Replaces the overlay's generic headline. */
  title?: string;
}

export interface PaneStartup {
  phase: PaneStartupPhase;
  failure: PaneStartupFailure | null;
  /** The attach settled: a snapshot was written, the first live bytes arrived,
   *  or the attach completed with nothing to write (a quiet shell, an ended
   *  session). Not a paint by itself: the renderer may not have drawn it. */
  markReady: () => void;
  /** xterm's renderer drew a frame (`onRender`). Counts only after
   *  {@link markReady}: a frame drawn before the attach settled is an empty grid.
   *  Returns whether it counted. */
  markRendered: () => boolean;
  /** Spawn or attach failed. */
  markFailed: (detail?: string, title?: string) => void;
}

/**
 * The user-facing safety net for a pane that stays blank. First paint is the
 * renderer's first frame after the attach settled (`markReady` then
 * `markRendered`), so a pane whose renderer never draws still reads "Still starting…" and then
 * "Failed · Retry" instead of staying blank. The clocks run only while the pane
 * is visible (a hidden pane can't paint), and restart when it is shown again.
 */
export function usePaneStartup(visible: boolean): PaneStartup {
  const [ready, setReady] = useState(false);
  const [rendered, setRendered] = useState(false);
  const [failure, setFailure] = useState<PaneStartupFailure | null>(null);
  const [stage, setStage] = useState<"quiet" | "starting" | "slow">("quiet");

  const painted = ready && rendered;
  const waiting = visible && !painted && failure === null;

  useEffect(() => {
    if (!waiting) return;
    const timers = [
      setTimeout(() => setStage("starting"), PANE_SPINNER_DELAY_MS),
      setTimeout(() => setStage("slow"), PANE_STILL_STARTING_MS),
      setTimeout(() => setFailure({ reason: "timeout" }), PANE_START_TIMEOUT_MS),
    ];
    return () => {
      timers.forEach(clearTimeout);
      setStage("quiet");
    };
  }, [waiting]);

  const readyRef = useRef(false);
  const markReady = useCallback(() => {
    readyRef.current = true;
    setReady(true);
  }, []);
  const markRendered = useCallback(() => {
    if (!readyRef.current) return false;
    setRendered(true);
    return true;
  }, []);
  const markFailed = useCallback((detail?: string, title?: string) => {
    setFailure((f) => f ?? { reason: "error", detail, ...(title ? { title } : {}) });
  }, []);

  const phase: PaneStartupPhase = painted
    ? "painted"
    : failure
      ? "failed"
      : waiting
        ? stage
        : "quiet";
  return { phase, failure: painted ? null : failure, markReady, markRendered, markFailed };
}
