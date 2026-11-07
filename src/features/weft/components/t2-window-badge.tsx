// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import { PauseCircle, Timer, ArrowRightToLine } from "lucide-react";

/**
 * SW-WV-F5 (AC-3) — the inline Tier-2 window state chip on a T2 YOU-OWE row.
 *
 * A T2 approval is *lazy consent*: pre-approved for a bounded revocation window during
 * which the owner may still revoke. This surfaces the E3 window state the Rust state
 * machine produces — never re-deciding it:
 *   - `notSurfacedButExpired` → "carries to next session" (the window spent its budget
 *     but did not auto-close, so it carries — takes precedence over the countdown);
 *   - `paused` → "paused (away)" (the clock is frozen while the owner is away, E7);
 *   - `open` (with `ttlS` + `acquiredAt`) → a live "closes in Xs" countdown, re-derived
 *     every 10s from the SAME effective-elapsed math the Rust verdict uses, cleared on
 *     unmount;
 *   - `expired` → nothing (a truly closed window is no longer owed).
 * A non-T2 tier renders nothing (T1/T3 have no revocation window).
 */
export interface T2WindowBadgeProps {
  tier: "T1" | "T2" | "T3";
  windowState: "open" | "paused" | "expired" | null;
  ttlS: number | null;
  /** Unix seconds the window was opened (E3 `window_opened_at`). */
  acquiredAt: number | null;
  pauseAccumulatedS: number;
  notSurfacedButExpired: boolean;
}

/** The chip refresh cadence — a T2 window closes on minute-ish scales, so a 10s tick
 *  keeps the countdown honest without a per-second re-render storm. */
const TICK_MS = 10_000;

/** Effective present-time remaining, netting out banked pause (mirrors the Rust
 *  `t2_effective_elapsed`: wall − banked pause). Saturates at 0. */
function remainingS(ttlS: number, acquiredAt: number, pauseAccumulatedS: number, nowS: number) {
  const elapsed = Math.max(0, nowS - acquiredAt - pauseAccumulatedS);
  return Math.max(0, ttlS - elapsed);
}

export function T2WindowBadge({
  tier,
  windowState,
  ttlS,
  acquiredAt,
  pauseAccumulatedS,
  notSurfacedButExpired,
}: T2WindowBadgeProps): React.JSX.Element | null {
  const isOpenCountdown =
    tier === "T2" &&
    !notSurfacedButExpired &&
    windowState === "open" &&
    ttlS != null &&
    acquiredAt != null;

  const [nowS, setNowS] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    if (!isOpenCountdown) return;
    const timer = setInterval(() => setNowS(Math.floor(Date.now() / 1000)), TICK_MS);
    return () => clearInterval(timer);
  }, [isOpenCountdown]);

  if (tier !== "T2") return null;

  const chipClass =
    "flex items-center gap-0.5 rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground";

  if (notSurfacedButExpired) {
    return (
      <span data-testid="t2-window-badge" data-window="carried" className={chipClass}>
        <ArrowRightToLine size={11} aria-hidden />
        carries to next session
      </span>
    );
  }

  if (windowState === "paused") {
    return (
      <span data-testid="t2-window-badge" data-window="paused" className={chipClass}>
        <PauseCircle size={11} aria-hidden />
        paused (away)
      </span>
    );
  }

  if (isOpenCountdown) {
    const remaining = remainingS(ttlS!, acquiredAt!, pauseAccumulatedS, nowS);
    return (
      <span data-testid="t2-window-badge" data-window="open" className={chipClass}>
        <Timer size={11} aria-hidden />
        closes in {remaining}s
      </span>
    );
  }

  // `expired` (or an un-opened window with no ttl/acquired) → render nothing.
  return null;
}
