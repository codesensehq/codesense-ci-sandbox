// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { acquireInstance, type InstanceKind } from "@/shared/lib/fe-instance-registry";

/**
 * RF-perf-4 (AC-1): count this component as one live instance of `kind` for as long as
 * it is mounted. A pure side effect on the module-level tally — it holds no state, so it
 * never triggers a re-render, and the acquire/release is O(1). StrictMode's
 * mount→unmount→mount double-invoke nets +1 because each `acquireInstance` call returns
 * its own single-shot release.
 */
export function useInstanceCount(kind: InstanceKind): void {
  useEffect(() => acquireInstance(kind), [kind]);
}
