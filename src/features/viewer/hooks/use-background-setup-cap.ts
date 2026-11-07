// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import {
  readMachineCores,
  resolveBackgroundSetupCap,
  setBackgroundSetupCap,
} from "@/shared/lib/worktree/background-setup-fanout";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * RF-perf-14d (Prong A2): keep the background-setup concurrency gate sized to the
 * machine + the user setting.
 *
 * Mirrors `use-warm-pool`: fires once at launch (after settings hydrate) to size
 * the gate off `navigator.hardwareConcurrency`, and again whenever
 * `worktreeSetupParallelism` changes — applying the new cap LIVE (no restart).
 * The effect body is a single synchronous module call (no await, no Rust
 * round-trip), so it runs after paint and can never block a frame. Lowering the
 * cap only bounds FUTURE admissions; any in-flight `npm ci` keeps running.
 *
 * This is the net-new caller of `setBackgroundSetupCap` — before this the gate
 * ran at its hard-coded default of 3 regardless of the machine.
 */
export function useBackgroundSetupCap(): void {
  const setting = useViewerStore((s) => s.settings.worktreeSetupParallelism);
  useEffect(() => {
    // Deps are `[setting]` only — `readMachineCores()` is intentionally NOT a dep:
    // logical-core count is fixed for the life of a webview session, so it can
    // only change with `setting`. (If cores ever became dynamic, add them here.)
    setBackgroundSetupCap(resolveBackgroundSetupCap(setting, readMachineCores()));
  }, [setting]);
}
