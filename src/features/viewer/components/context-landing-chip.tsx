// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { AlertTriangle, Check, Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { logger } from "@/shared/lib/logger";

/** How long "context ready" stays up after the context lands. */
export const CONTEXT_READY_FLASH_MS = 3_000;

interface Props {
  /** Team context is still landing in this worktree. */
  landing: boolean;
  /** The context actually arrived (the wave settled usable). */
  landed: boolean;
  /** The materialize wave failed: the context is not (all) there. */
  failed: boolean;
  /** A command is held, waiting for the context. */
  held?: boolean;
  /** A command held for context ran anyway after the hold timed out. */
  timedOut: boolean;
  /** The worktree this chip reports on, named in the warning logged when it
   *  turns "not ready". */
  surface?: string;
}

/**
 * The context-landing chip on a new worktree's terminal: "context landing…"
 * while the team context is being written into the worktree, "context ready"
 * for a few seconds once it is, and "context not ready" if a held command had
 * to run without it, the context failed to land, or landing stopped without
 * the context arriving. Shows nothing on a terminal that never saw context
 * land, such as a reopened worktree.
 *
 * The chip reports on ONE landing. Once the context has arrived it stays
 * arrived: a later wave on the same worktree (a team
 * edit deferred as "out of date" while the worktree isn't focused) changes the
 * surface's sync state, but the context is still there, so the chip never turns
 * amber for it.
 */
export function ContextLandingChip({
  landing,
  landed,
  failed,
  held = false,
  timedOut,
  surface,
}: Props) {
  const [sawLanding, setSawLanding] = useState(landing);
  const [sawLanded, setSawLanded] = useState(false);
  const [readyFlash, setReadyFlash] = useState(false);

  useEffect(() => {
    if (landing) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- latches the one landing edge this chip reports on
      setSawLanding(true);
      return;
    }
    if (!sawLanding || !landed || sawLanded) return;
    setSawLanded(true);
    setReadyFlash(true);
  }, [landing, sawLanding, landed, sawLanded]);

  // The flash runs its full length whatever the surface does meanwhile.
  useEffect(() => {
    if (!readyFlash) return;
    const t = setTimeout(() => setReadyFlash(false), CONTEXT_READY_FLASH_MS);
    return () => clearTimeout(t);
  }, [readyFlash]);

  const arrived = sawLanded || landed;
  const notReadyReason: string | null = !sawLanding
    ? null
    : timedOut
      ? "a held command ran before the context landed"
      : arrived
        ? null
        : failed
          ? "preparing the context failed"
          : !landing
            ? "landing stopped without the context arriving"
            : null;
  const notReady = notReadyReason !== null;

  useEffect(() => {
    if (notReadyReason) {
      logger.warn(
        "context-landing",
        `context not ready in ${surface ?? "worktree"}: ${notReadyReason}`,
      );
    }
  }, [notReadyReason, surface]);
  const phase = notReady ? "not-ready" : landing ? "landing" : readyFlash ? "ready" : null;
  if (!phase) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      data-testid="context-landing-chip"
      data-phase={phase}
      className={cn(
        "pointer-events-none absolute right-2 top-2 z-20 flex items-center gap-1.5 rounded-full border bg-[var(--popover)] px-2 py-0.5 text-[11px] font-medium shadow-sm",
        phase === "landing" && "border-sky-500/40 text-sky-700 dark:text-sky-300",
        phase === "ready" && "border-emerald-500/40 text-emerald-700 dark:text-emerald-300",
        phase === "not-ready" && "border-amber-500/50 text-amber-700 dark:text-amber-300",
      )}
    >
      {phase === "landing" ? (
        <>
          <Loader2 className="size-3 animate-spin motion-reduce:animate-none" aria-hidden />
          {held ? "context landing… your command runs when it's ready" : "context landing…"}
        </>
      ) : phase === "ready" ? (
        <>
          <Check className="size-3" aria-hidden />
          context ready
        </>
      ) : (
        <>
          <AlertTriangle className="size-3" aria-hidden />
          {timedOut ? "context not ready — your command ran without it" : "context not ready"}
        </>
      )}
    </div>
  );
}
