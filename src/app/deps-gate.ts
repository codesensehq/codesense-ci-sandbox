// SPDX-License-Identifier: MIT
import type { DependencyStatus } from "@/tauri/commands";

/**
 * SW-AN-3: Claude is optional — git-readiness alone unlocks the app. With no
 * `claude` on PATH (or a below-floor one), CodeSense boots, opens a team, and
 * syncs; absence is a fully-supported state (ADR-3), so `claude.available` no
 * longer gates. Git remains the one genuinely-required managed dependency.
 *
 * Tri-state readiness (SW-OB-12 anti-flash splash preserved):
 * - `null`  → claude detection still in flight (`resolving`) → splash. We keep
 *   the brief splash so a Claude-*present* machine settles discovery before
 *   first paint; it never blocks a Claude-*absent* machine — `resolving` clears
 *   in bounded time (the probe is timeout-guarded), after which git-readiness
 *   unlocks the app. The initial not-yet-checked `null` lives in the
 *   `dependenciesReady` useState default.
 * - `false` → git missing/too-old → DependencyInstallView (git-required guidance).
 * - `true`  → git ready → app. Claude may be present, absent, or below-floor.
 */
export function computeDepsReady(status: DependencyStatus, activeBasename: string): boolean | null {
  // SW-AN-44: gate on the ACTIVE agent's probe (was the single claude-named
  // `claude.resolving`).
  if (activeBasename) {
    // Known active agent: gate on ITS entry only. An absent entry (a not-yet-warmed
    // drop-in) is treated as not-resolving (the bounded-resolving safeguard), so
    // git-readiness still unlocks and the splash never wedges.
    if (status.agents[activeBasename]?.resolving) return null;
  } else if (Object.values(status.agents).some((a) => a.resolving)) {
    // Agent list not resolved yet (""): hold the splash while ANY seeded agent is
    // still probing. At boot only the bundled agent is seeded, so this is exactly
    // the old single-`claude` gate — and it is independent of `agents` key order
    // (`serde_json` emits the Rust BTreeMap key-sorted, which is NOT seed order),
    // unlike indexing `[0]`.
    return null;
  }
  return status.git.available;
}

/**
 * Fallback status when `getDependencyStatus` itself fails — treat git as
 * confirmed-unavailable (not resolving) so the gate shows the install view.
 * SW-AN-3/SW-AN-44: agents are irrelevant to readiness now; the map stays empty
 * purely to satisfy the `DependencyStatus` shape.
 */
export const UNAVAILABLE_DEP_STATUS: DependencyStatus = {
  git: { available: false, path: null, version: null },
  agents: {},
};
