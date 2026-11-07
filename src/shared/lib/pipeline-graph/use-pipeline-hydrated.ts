// SPDX-License-Identifier: MIT
import { useConfigStore } from "@/shared/stores/config-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * Returns true only when all hydration signals needed by `usePipelineTree`
 * are ready (story 30.7 AC-19):
 *   1. config-store has loaded persisted state (`initialized === true`)
 *   2. viewer-store has loaded persisted state (`stateLoaded === true`)
 *   3. scope-recovery routine has run (`scopeRecoveryComplete === true`)
 *
 * `initialized` and `stateLoaded` already exist from earlier work —
 * we reuse them instead of introducing parallel `_hasHydrated` flags.
 * Only `scopeRecoveryComplete` is new in 30.7.
 *
 * Subscriptions use narrow selectors so any component calling this hook
 * re-renders only when one of the three booleans actually changes.
 */
export function usePipelineHydrated(): boolean {
  const configReady = useConfigStore((s) => s.initialized);
  const viewerReady = useViewerStore((s) => s.stateLoaded);
  const scopeRecoveryComplete = useViewerStore((s) => s.scopeRecoveryComplete);
  return configReady && viewerReady && scopeRecoveryComplete;
}
