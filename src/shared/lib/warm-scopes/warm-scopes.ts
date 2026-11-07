// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";
import { ScopeBag } from "./scope-bag";
import {
  DEFAULT_WARM_SCOPES_CONFIG,
  computeTier,
  isBelow,
  isExpired,
  tierRank,
  type HeldTier,
  type ScopeKey,
  type ScopeTier,
  type WarmScopesConfig,
} from "./tiers";

/** Which scopes are hot, and the lifetime of everything held on their behalf. Framework-free: the
 *  React bindings (`useScopeTier`, `useScopedResource`) sit on top of `subscribe`/`tierOf`/`hold`.
 *
 *  Tiers: `active` (one), `warm` (mounted and hidden; the most recently active scopes up to the
 *  capacity, across orgs), `suspended` (renderer freed, state and PTYs kept), `gone` (disposed).
 *  Only a team the user activated is tracked; any other scope is `gone`. */

export interface WarmScopeCounts {
  tiers: Record<ScopeKey, ScopeTier>;
  /** Live entries per cache kind. */
  caches: Record<string, number>;
  /** Live holds per scope and kind. */
  held: Record<ScopeKey, Record<string, number>>;
  holdsTotal: number;
  /** Cumulative disposals per kind, for dispose-once assertions. */
  disposed: Record<string, number>;
  activations: number;
}

interface TierCallback {
  floor: HeldTier;
  cb: () => void;
}

export class WarmScopes {
  private readonly config: WarmScopesConfig;
  private readonly bag = new ScopeBag();
  /** Tracked scopes, most recently active first. */
  private order: ScopeKey[] = [];
  private lastActiveAt = new Map<ScopeKey, number>();
  private activeKey: ScopeKey | null = null;
  private activations = 0;
  private version = 0;
  /** Pin tokens per scope, each with the reason it was taken. */
  private readonly pins = new Map<ScopeKey, Map<symbol, string>>();
  private tierCallbacks = new Map<ScopeKey, Set<TierCallback>>();
  private readonly listeners = new Set<() => void>();
  private readonly cacheSizes = new Set<{ kind: string; size: () => number }>();
  private opener: ((scope: ScopeKey) => void) | null = null;
  private goneHandler: ((scope: ScopeKey) => void) | null = null;

  constructor(config: WarmScopesConfig = DEFAULT_WARM_SCOPES_CONFIG) {
    this.config = config;
  }

  // --- writer -----------------------------------------------------------------------------

  /** Make `scope` the active one (null: no active scope). The previous active scope becomes warm,
   *  the least recently active one beyond the capacity is suspended, and a suspended scope idle
   *  past the threshold is dropped. The opener runs before any subscriber hears of the change, so
   *  the scope is opened in the same update that makes it active. */
  activate(scope: ScopeKey | null): void {
    if (scope === this.activeKey) return;
    const before = this.snapshot();
    this.activeKey = scope;
    if (scope !== null) {
      this.activations += 1;
      this.order = [scope, ...this.order.filter((k) => k !== scope)];
      this.lastActiveAt.set(scope, this.activations);
      this.opener?.(scope);
    }
    this.settle(before);
  }

  /** The team was deleted: dispose everything held for it. */
  forget(scope: ScopeKey): void {
    if (!this.order.includes(scope) && !this.hasState(scope)) return;
    const before = this.snapshot();
    this.drop(scope);
    this.settle(before, { notifyGone: false });
  }

  /** The team was renamed: its tier, holds and callbacks move to the new key without disposing. */
  rekey(from: ScopeKey, to: ScopeKey): void {
    if (from === to || !this.hasState(from)) return;
    this.forget(to);
    const before = this.snapshot();
    this.order = this.order.map((k) => (k === from ? to : k));
    this.move(this.lastActiveAt, from, to);
    this.move(this.pins, from, to);
    this.move(this.tierCallbacks, from, to);
    this.bag.move(from, to);
    if (this.activeKey === from) this.activeKey = to;
    for (const listener of this.rekeyListeners) listener(from, to);
    before.set(to, before.get(from) ?? "gone");
    before.delete(from);
    this.settle(before);
  }

  /** Keep the scope from dropping below warm (a dirty editor buffer lives only in a mounted tree). */
  pin(scope: ScopeKey, reason: string): () => void {
    const token = Symbol(reason);
    // A pinned scope the owner has not seen is tracked, so it can age out once released.
    if (!this.order.includes(scope)) {
      this.order = [...this.order, scope];
      this.lastActiveAt.set(scope, this.activations);
    }
    const before = this.snapshot();
    const held = this.pins.get(scope) ?? new Map<symbol, string>();
    held.set(token, reason);
    this.pins.set(scope, held);
    this.settle(before);
    // The token follows its scope across a rename, so it is looked up, not captured.
    return () => {
      for (const [owner, current] of this.pins) {
        if (!current.has(token)) continue;
        const before = this.snapshot();
        current.delete(token);
        if (current.size === 0) this.pins.delete(owner);
        this.settle(before);
        return;
      }
    };
  }

  // --- readers ----------------------------------------------------------------------------

  tierOf(scope: ScopeKey): ScopeTier {
    return computeTier(
      scope,
      this.order,
      this.activeKey,
      this.config.warmCapacity,
      this.pins.has(scope),
    );
  }

  /** An org is active while its active team is, warm while any team of it is, else cold. */
  orgTier(org: string): "active" | "warm" | "cold" {
    let warm = false;
    for (const scope of this.order) {
      if (scopeOrg(scope) !== org) continue;
      const tier = this.tierOf(scope);
      if (tier === "active") return "active";
      if (tier === "warm") warm = true;
    }
    return warm ? "warm" : "cold";
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => void this.listeners.delete(listener);
  };

  getVersion = (): number => this.version;

  // --- resources --------------------------------------------------------------------------

  /** Hold a resource on behalf of a scope. It is disposed exactly once: by the returned release,
   *  by a later `hold` of the same `(scope, kind, key)`, when the scope drops below `floor`
   *  (default: when it is gone), or on `forget`. */
  hold(
    scope: ScopeKey,
    kind: string,
    key: string,
    dispose: () => void,
    floor: HeldTier = "suspended",
  ): () => void {
    const release = this.bag.hold(scope, kind, key, dispose, floor);
    this.bump();
    return release;
  }

  /** Run `cb` once when the scope drops below `floor`. The returned function cancels it. */
  onTier(scope: ScopeKey, floor: HeldTier, cb: () => void): () => void {
    const entry: TierCallback = { floor, cb };
    const set = this.tierCallbacks.get(scope) ?? new Set<TierCallback>();
    set.add(entry);
    this.tierCallbacks.set(scope, set);
    return () => void this.tierCallbacks.get(scope)?.delete(entry);
  }

  /** Called with the scope when a suspended scope ages out (not on `forget`). Set once by the
   *  owner of team state, which closes the team keeping its PTYs. */
  setGoneHandler(handler: ((scope: ScopeKey) => void) | null): void {
    this.goneHandler = handler;
  }

  /** Called with the scope each time it is activated, before subscribers are notified. Set once by
   *  the owner of the opened-team list. */
  setOpener(opener: ((scope: ScopeKey) => void) | null): void {
    this.opener = opener;
  }

  // --- diagnostics and internals for the cache --------------------------------------------

  debugCounts(): WarmScopeCounts {
    const caches: Record<string, number> = {};
    for (const { kind, size } of this.cacheSizes) caches[kind] = (caches[kind] ?? 0) + size();
    return {
      tiers: Object.fromEntries(this.order.map((k) => [k, this.tierOf(k)])),
      caches,
      held: this.bag.held(),
      holdsTotal: this.bag.total(),
      disposed: this.bag.disposed(),
      activations: this.activations,
    };
  }

  /** For `createWarmCache`: count its entries in `debugCounts`. */
  registerCache(kind: string, size: () => number): () => void {
    const entry = { kind, size };
    this.cacheSizes.add(entry);
    return () => void this.cacheSizes.delete(entry);
  }

  private readonly rekeyListeners = new Set<(from: ScopeKey, to: ScopeKey) => void>();

  /** For `createWarmCache`: told when a scope is renamed so it can move its entries. */
  onRekey(listener: (from: ScopeKey, to: ScopeKey) => void): () => void {
    this.rekeyListeners.add(listener);
    return () => void this.rekeyListeners.delete(listener);
  }

  // --- internals --------------------------------------------------------------------------

  private hasState(scope: ScopeKey): boolean {
    return (
      this.order.includes(scope) ||
      this.bag.held()[scope] !== undefined ||
      this.tierCallbacks.has(scope)
    );
  }

  private move<V>(map: Map<ScopeKey, V>, from: ScopeKey, to: ScopeKey): void {
    if (!map.has(from)) return;
    map.set(to, map.get(from) as V);
    map.delete(from);
  }

  private snapshot(): Map<ScopeKey, ScopeTier> {
    const tiers = new Map<ScopeKey, ScopeTier>();
    for (const k of this.order) tiers.set(k, this.tierOf(k));
    return tiers;
  }

  private drop(scope: ScopeKey): void {
    this.order = this.order.filter((k) => k !== scope);
    this.lastActiveAt.delete(scope);
    this.pins.delete(scope);
    if (this.activeKey === scope) this.activeKey = null;
  }

  /** Drop scopes that aged out, then fire what each tier change owes and tell the subscribers. */
  private settle(before: Map<ScopeKey, ScopeTier>, opts: { notifyGone?: boolean } = {}): void {
    const expired = this.order.filter(
      (k) =>
        this.tierOf(k) === "suspended" &&
        isExpired(this.activations, this.lastActiveAt.get(k) ?? 0, this.config),
    );
    for (const k of expired) this.drop(k);
    const scopes = new Set([...before.keys(), ...this.order]);
    const gone: ScopeKey[] = [];
    for (const scope of scopes) {
      const was = before.get(scope) ?? "gone";
      const now = this.tierOf(scope);
      if (tierRank(now) >= tierRank(was)) continue;
      this.fireDrop(scope, now);
      if (now === "gone" && expired.includes(scope)) gone.push(scope);
    }
    this.bump();
    if (opts.notifyGone !== false) {
      for (const scope of gone) {
        try {
          this.goneHandler?.(scope);
        } catch (e) {
          logger.warn("warm-scopes", `gone handler for ${scope} threw: ${String(e)}`);
        }
      }
    }
  }

  /** What a scope owes now that it is at `tier`: holds below their floor are disposed, callbacks
   *  whose floor it fell under fire once. */
  private fireDrop(scope: ScopeKey, tier: ScopeTier): void {
    if (tier === "gone") this.bag.disposeAll(scope);
    else this.bag.dropBelow(scope, tier);
    const callbacks = this.tierCallbacks.get(scope);
    if (callbacks) {
      for (const entry of [...callbacks]) {
        if (!isBelow(tier, entry.floor)) continue;
        callbacks.delete(entry);
        try {
          entry.cb();
        } catch (e) {
          logger.warn("warm-scopes", `tier callback for ${scope} threw: ${String(e)}`);
        }
      }
      if (callbacks.size === 0 || tier === "gone") this.tierCallbacks.delete(scope);
    }
  }

  private bump(): void {
    this.version += 1;
    for (const listener of [...this.listeners]) listener();
  }
}

function scopeOrg(scope: ScopeKey): string {
  const i = scope.indexOf(":");
  return i === -1 ? scope : scope.slice(0, i);
}

/** The app's one owner. */
export const warmScopes = new WarmScopes();
