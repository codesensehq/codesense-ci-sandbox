// SPDX-License-Identifier: MIT
import { useSyncExternalStore } from "react";
import { warmScopes } from "./warm-scopes";
import type { ScopeKey, ScopeTier } from "./tiers";

/** The scope's tier, re-rendering when it changes. A null scope is `gone`. The render gate, terminal
 *  grids and hosts read this to decide mount, hidden or unmount. */
export function useScopeTier(scope: ScopeKey | null): ScopeTier {
  const read = () => (scope === null ? "gone" : warmScopes.tierOf(scope));
  return useSyncExternalStore(warmScopes.subscribe, read, read);
}

/** Whether the scope is the one on screen, read now. The hidden-team rule for a handler that
 *  decides at event time (the hook form below is for effects that install or skip a listener). */
export function isScopeActive(scope: ScopeKey | null | undefined): boolean {
  return scope == null || warmScopes.tierOf(scope) === "active";
}

/** Whether the scope is the one on screen. The gate for every global listener (document keydown,
 *  focus capture) and every per-team identity key (a worktree watch) a retained, hidden scope's
 *  subtree installs: a warm team stays mounted, so those must not take input meant for the active
 *  team. No scope (undefined): nothing to gate. */
export function useScopeActive(scope: ScopeKey | null | undefined): boolean {
  const tier = useScopeTier(scope ?? null);
  return scope == null || tier === "active";
}
