// SPDX-License-Identifier: MIT
export { warmScopes, WarmScopes, type WarmScopeCounts } from "./warm-scopes";
export {
  SUSPENDED_AFTER_SWITCHES,
  WARM_CAPACITY,
  isMountedTier,
  type HeldTier,
  type ScopeKey,
  type ScopeTier,
  type WarmScopesConfig,
} from "./tiers";
export { createWarmCache, type WarmCache, type WarmCacheOptions } from "./create-warm-cache";
export { isScopeActive, useScopeActive, useScopeTier } from "./use-scope-tier";
export { useScopedResource } from "./use-scoped-resource";
export { useMountedScopes } from "./use-mounted-scopes";
export { isInHiddenTeam, queryAllShown, queryShown } from "./hidden-team";
export { useFocusWhenShown } from "./use-focus-when-shown";
export {
  OwningScopeProvider,
  ScopeGate,
  ScopedPortal,
  useOwnsScope,
  useOwningScope,
} from "./owning-scope";
