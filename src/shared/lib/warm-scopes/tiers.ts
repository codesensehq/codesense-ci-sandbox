// SPDX-License-Identifier: MIT
/** Pure tier rules for the warm-scope owner: no state, no side effects. */

/** A scope is a full team key `org:team`. */
export type ScopeKey = string;
export type ScopeTier = "active" | "warm" | "suspended" | "gone";
/** The lowest tier a held entry survives in. */
export type HeldTier = "warm" | "suspended";

export interface WarmScopesConfig {
  /** Scopes kept mounted (hidden), the active one included, across orgs. */
  warmCapacity: number;
  /** A suspended scope is gone once more than this many switches passed since it was last active. */
  suspendedAfterSwitches: number;
}

export const WARM_CAPACITY = 3;
export const SUSPENDED_AFTER_SWITCHES = 4;
export const DEFAULT_WARM_SCOPES_CONFIG: WarmScopesConfig = {
  warmCapacity: WARM_CAPACITY,
  suspendedAfterSwitches: SUSPENDED_AFTER_SWITCHES,
};

const RANK: Record<ScopeTier, number> = { gone: 0, suspended: 1, warm: 2, active: 3 };

export const tierRank = (tier: ScopeTier): number => RANK[tier];

/** Whether a scope at `tier` has its renderer mounted (shown, or hidden in its wrapper). */
export const isMountedTier = (tier: ScopeTier): boolean => tier === "active" || tier === "warm";

/** Whether `tier` is below `floor`. */
export const isBelow = (tier: ScopeTier, floor: HeldTier): boolean => RANK[tier] < RANK[floor];

/** The tier a scope has by its place in `order` (most recently active first). A scope absent from
 *  `order` is gone; a pin lifts anything below warm to warm. */
export function computeTier(
  scope: ScopeKey,
  order: readonly ScopeKey[],
  active: ScopeKey | null,
  capacity: number,
  pinned: boolean,
): ScopeTier {
  const rank = order.indexOf(scope);
  if (rank === -1) return "gone";
  if (scope === active) return "active";
  return rank < capacity || pinned ? "warm" : "suspended";
}

/** Whether a scope outside the warm set has been idle long enough to be dropped. */
export function isExpired(
  activations: number,
  lastActiveAt: number,
  config: WarmScopesConfig,
): boolean {
  return activations - lastActiveAt > config.suspendedAfterSwitches;
}
