// SPDX-License-Identifier: MIT
// RF-perf-14 (decouple, Prong 2b): bound concurrent BACKGROUND setup runs.
//
// With the open decoupled from setup (createWorktree flips the workspace
// "ready" + mounts the terminal the moment `git worktree add` completes), the
// user's `setup:` commands — `npm ci` / a real build — run in the background.
// Those are heavy CPU/disk subprocesses; letting a batch launch as many as the
// CREATE fan-out allows (cap 8) would spawn 8 concurrent `npm ci` and saturate
// the machine, starving the WebKit renderer into a dropped-frame "freeze" (P2).
//
// This is a SEPARATE gate from `withCreationSlot` (which now only bounds the
// fast git-create burst) with its own, deliberately LOW cap: at most a few
// background setups run at once; the rest queue FIFO. Same proven mechanism
// (`createFanoutGate`), independent state + ceiling.

import { createFanoutGate } from "./fanout-gate";

/** Default concurrent background-setup ceiling. Deliberately low so a batch
 *  create never runs a pile of heavy `setup:` subprocesses at once (OQ-3:
 *  "start at 3; never 8"). Tunable via {@link setBackgroundSetupCap}. */
export const DEFAULT_BACKGROUND_SETUP_CAP = 3;

/** RF-perf-23: a generous sane ceiling for an explicit numeric cap — far beyond
 *  any real machine's core count or worktree count. Bounds custom values so a
 *  fat-fingered/pasted entry like `1e21` can't render as `"1e+21"` in the number
 *  input or push an absurd cap. "Off" (unbounded → Infinity) is the deliberate
 *  escape hatch for "no limit"; custom is for picking a *specific* value. */
export const MAX_BACKGROUND_SETUP_CAP = 999;

/** RF-perf-14d: cap used when Auto can't read the machine's core count (older
 *  webview / test env with no `navigator.hardwareConcurrency`). Deliberately the
 *  SHIPPED default — an unknown machine behaves like a typical one, never a
 *  made-up lower number. */
export const BACKGROUND_SETUP_CAP_FALLBACK = DEFAULT_BACKGROUND_SETUP_CAP;

/** RF-perf-14d → RF-perf-23 (Prong A1): map a machine's logical-core count to an
 *  Auto cap. A tiered lookup — NOT a `floor(cores/N)` divisor (which would drop a
 *  typical 4-core too low) and NOT a `max(N, …)` floor (which would force a
 *  super-slow 1–2 core machine up). The table is the only shape that both protects
 *  weak machines AND scales a big machine up.
 *
 *  RF-perf-23 RAISED these tiers: the old ceiling of 6 existed only because each
 *  setup was an `npm ci` and "never 8" concurrent installs saturated disk/CPU
 *  (decouple OQ-3). Setup is now an APFS copy-on-write clone (near-zero disk,
 *  I/O-light), so that saturation line is gone and Auto can scale far higher —
 *  a ~12-core machine now sizes to 8 (was 3). Auto stays BOUNDED, though:
 *  truly-unbounded fan-out is a deliberate user choice ("off"), never Auto. */
function autoCapForCores(cores: number): number {
  if (cores <= 2) return 1; // super-slow machine — don't pile on
  if (cores <= 3) return 2;
  if (cores <= 4) return 3;
  if (cores <= 8) return 6;
  if (cores <= 12) return 8; // typical modern laptop/desktop
  if (cores <= 20) return 12;
  return 16; // big desktop, still bounded
}

/**
 * RF-perf-14d (Prong A1): resolve the effective background-setup cap from the
 * user setting + the machine's core count. Pure + fail-safe, mirroring the
 * `resolveScrollback` resolver family — the single owner of
 * "what the setting means as a number" so the wiring hook and the Settings row's
 * "Auto (N on this machine)" hint can never disagree.
 *
 *   - "off"            → `Infinity` (no cap — the gate admits all, bypassing the
 *                        semaphore; RF-perf-23 AC-1). A well-typed numeric
 *                        sentinel, never `0`/`NaN` (which would deadlock the gate).
 *   - explicit number  → user override, clamped ≥1 and floored
 *   - "auto"/undefined → size to cores via {@link autoCapForCores}
 *   - unknown cores    → {@link BACKGROUND_SETUP_CAP_FALLBACK}
 *
 * The gate's own `setCap` re-clamps ≥1 + floors (and passes `Infinity` through),
 * so this is belt-and-suspenders for the gate — but it also guarantees a valid
 * number for display.
 */
/** RF-perf-14d: read the machine's logical-core count (`navigator.hardwareConcurrency`),
 *  or `undefined` where `navigator` is absent (SSR / older webview). The single
 *  read site shared by the wiring hook and the Settings "Auto (N)" hint so both
 *  feed {@link resolveBackgroundSetupCap} identically. NOTE: this does NOT validate
 *  the value — `hardwareConcurrency` can itself be `0`/`undefined`/garbage on some
 *  webviews; sanitizing (fallback for `NaN`/`< 1`) is {@link resolveBackgroundSetupCap}'s
 *  job, so always pass this through the resolver rather than using it raw. */
export function readMachineCores(): number | undefined {
  return typeof navigator !== "undefined" ? navigator.hardwareConcurrency : undefined;
}

export function resolveBackgroundSetupCap(
  setting: "auto" | "off" | number | undefined,
  cores: number | undefined,
): number {
  if (setting === "off") return Number.POSITIVE_INFINITY; // no cap — gate admits all
  if (typeof setting === "number" && Number.isFinite(setting)) {
    return Math.max(1, Math.floor(setting));
  }
  // "auto" or undefined → size to the machine.
  if (typeof cores !== "number" || !Number.isFinite(cores) || cores < 1) {
    return BACKGROUND_SETUP_CAP_FALLBACK;
  }
  return autoCapForCores(Math.floor(cores));
}

const gate = createFanoutGate(DEFAULT_BACKGROUND_SETUP_CAP);

/** Raise/lower the background-setup ceiling (renderer knob; wakes oldest waiters
 *  in FIFO order on a raise, never below 1). */
export function setBackgroundSetupCap(cap: number): void {
  gate.setCap(cap);
}

/** Test/observability hook: background setups currently holding a slot. */
export function activeBackgroundSetupCount(): number {
  return gate.activeCount();
}

/** Test hook: reset to defaults between cases. */
export function __resetBackgroundSetupFanoutForTest(): void {
  gate.resetForTest();
}

/**
 * Run `fn` (a background `setup:` run) under a background-setup slot: block
 * until fewer than `cap` setups are in flight, then run + release on settle
 * (success OR failure, so a failed/cancelled setup never leaks a slot).
 */
export async function withBackgroundSetupSlot<T>(fn: () => Promise<T>): Promise<T> {
  return gate.withSlot(fn);
}
