// SPDX-License-Identifier: MIT
/**
 * Debug-only profile of one team switch (SV-PLG-46 D0): the store write, the first commit of the
 * incoming team view, the point its effects have settled, and the moment the incoming team's
 * right pane is shown. Every phase is a user-timing mark the code places itself, because the
 * packaged WebDriver build runs production React, which never calls `<Profiler>` callbacks.
 * Everything is gated by {@link csDebugEnabled}.
 */
import { useLayoutEffect } from "react";

import { useOrgStore } from "@/shared/stores/org-store";

import { csDebug, csDebugEnabled, csMark, csMeasure } from "./cs-debug";

/** A switch no reveal follows still ends. */
const FALLBACK_MS = 1000;

interface Switch {
  to: string | null;
  writeAt: number;
  firstCommitAt?: number;
  settledAt?: number;
  revealedAt?: number;
  timer?: ReturnType<typeof setTimeout>;
}

let current: Switch | null = null;

function finish(done: Switch): void {
  if (current !== done) return;
  current = null;
  clearTimeout(done.timer);
  const at = (t?: number) => (t === undefined ? null : Math.round(t - done.writeAt));
  csDebug("switch", {
    to: done.to,
    writeToFirstCommitMs: at(done.firstCommitAt),
    writeToSettledMs: at(done.settledAt),
    writeToRevealedMs: at(done.revealedAt),
  });
}

/** Record `name` once per switch, measured from the store write. */
function record(active: Switch, name: string, field: "firstCommitAt" | "settledAt" | "revealedAt") {
  if (active[field] !== undefined) return;
  const now = performance.now();
  active[field] = now;
  csMark(`switch:${name}`, { to: active.to });
  csMeasure(`switch:write-to-${name}`, active.writeAt, now);
}

/** Watch the active team and start a profile at each change. Returns the unsubscribe. */
export function startSwitchProfile(): () => void {
  if (!csDebugEnabled()) return () => {};
  return useOrgStore.subscribe((state, previous) => {
    if (
      state.activeTeamName === previous.activeTeamName &&
      state.activeOrgSlug === previous.activeOrgSlug
    )
      return;
    const started: Switch = {
      to: `${state.activeOrgSlug}:${state.activeTeamName}`,
      writeAt: performance.now(),
    };
    current = started;
    csMark("switch:store-write", { to: started.to });
    started.timer = setTimeout(() => finish(started), FALLBACK_MS);
  });
}

/**
 * The incoming team view has committed: called from its first layout effect after the store
 * write. Effects have settled one animation frame later, after the commit's passive effects.
 */
export function markSwitchCommitted(): void {
  const active = current;
  if (!csDebugEnabled() || !active || active.firstCommitAt !== undefined) return;
  record(active, "first-commit", "firstCommitAt");
  requestAnimationFrame(() => {
    if (current === active) record(active, "effects-settled", "settledAt");
  });
}

/** The incoming team's right pane is on screen: called from a post-commit effect. */
export function markSwitchRevealed(): void {
  const active = current;
  if (!csDebugEnabled() || !active || active.revealedAt !== undefined) return;
  requestAnimationFrame(() => {
    if (current !== active) return;
    record(active, "right-pane-revealed", "revealedAt");
    finish(active);
  });
}

/** Mark the commit of a team view that follows a switch. A no-op while the gate is off. */
export function useSwitchCommitMark(teamKey: string | null): void {
  useLayoutEffect(() => {
    markSwitchCommitted();
  }, [teamKey]);
}
