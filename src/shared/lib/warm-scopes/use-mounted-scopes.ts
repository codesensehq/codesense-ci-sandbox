// SPDX-License-Identifier: MIT
import { useMemo, useSyncExternalStore } from "react";
import { warmScopes } from "./warm-scopes";
import { isMountedTier, type ScopeKey } from "./tiers";

/** The candidates whose scope is active or warm, in the candidates' order, re-evaluated whenever the
 *  owner's tiers change. The render gate and the CLI's grids read this to decide mount or unmount. */
export function useMountedScopes(candidates: readonly ScopeKey[]): ScopeKey[] {
  const version = useSyncExternalStore(warmScopes.subscribe, warmScopes.getVersion);
  return useMemo(
    () => candidates.filter((scope) => isMountedTier(warmScopes.tierOf(scope))),
    // The version stands for the owner's tiers, which `tierOf` reads outside React.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [candidates, version],
  );
}
