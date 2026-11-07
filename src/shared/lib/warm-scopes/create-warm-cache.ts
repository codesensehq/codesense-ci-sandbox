// SPDX-License-Identifier: MIT
import { useSyncExternalStore } from "react";
import { warmScopes as defaultOwner, type WarmScopes } from "./warm-scopes";
import { isBelow, type HeldTier, type ScopeKey } from "./tiers";

export interface WarmCacheOptions<K extends string, V> {
  kind: string;
  scopeOf: (key: K) => ScopeKey;
  /** The lowest tier an entry survives in: it is dropped (and disposed) when its scope falls below. */
  tier: HeldTier;
  dispose?: (key: K, value: V) => void;
  /** Backstop only: the oldest entry goes past this many. */
  maxEntries?: number;
  /** The key an entry takes when its scope is renamed. Without it a rename drops the scope's entries. */
  rekeyKey?: (key: K, from: ScopeKey, to: ScopeKey) => K;
}

export interface WarmCache<K extends string, V> {
  get(key: K): V | undefined;
  set(key: K, value: V): void;
  update(key: K, f: (prev: V | undefined) => V): void;
  delete(key: K): void;
  useValue(key: K): V | undefined;
  subscribe(listener: () => void): () => void;
  size(): number;
}

interface Entry<V> {
  value: V;
  stop: () => void;
}

/** A scope-keyed cache: entries live while their scope is at or above `tier`, are disposed exactly
 *  once, and one tier callback is held per entry, so repeated `set`s cannot stack. */
export function createWarmCache<K extends string, V>(
  opts: WarmCacheOptions<K, V>,
  owner: WarmScopes = defaultOwner,
): WarmCache<K, V> {
  const entries = new Map<K, Entry<V>>();
  const listeners = new Set<() => void>();
  const notify = () => {
    for (const listener of [...listeners]) listener();
  };

  const remove = (key: K): void => {
    const entry = entries.get(key);
    if (!entry) return;
    entries.delete(key);
    entry.stop();
    opts.dispose?.(key, entry.value);
  };

  const drop = (key: K): void => {
    remove(key);
    notify();
  };

  owner.registerCache(opts.kind, () => entries.size);
  owner.onRekey((from, to) => {
    for (const key of [...entries.keys()]) {
      if (opts.scopeOf(key) !== from) continue;
      const entry = entries.get(key) as Entry<V>;
      if (!opts.rekeyKey) {
        drop(key);
        continue;
      }
      entries.delete(key);
      entry.stop();
      const next = opts.rekeyKey(key, from, to);
      entries.set(next, {
        value: entry.value,
        stop: owner.onTier(opts.scopeOf(next), opts.tier, () => drop(next)),
      });
    }
    notify();
  });

  const get = (key: K): V | undefined => entries.get(key)?.value;

  const set = (key: K, value: V): void => {
    const previous = entries.get(key);
    if (previous && Object.is(previous.value, value)) return;
    const scope = opts.scopeOf(key);
    if (isBelow(owner.tierOf(scope), opts.tier)) {
      // The scope is already below the tier: nothing would ever drop this entry, so refuse it.
      opts.dispose?.(key, value);
      if (previous) drop(key);
      return;
    }
    if (previous) remove(key);
    entries.set(key, { value, stop: owner.onTier(scope, opts.tier, () => drop(key)) });
    if (opts.maxEntries !== undefined && entries.size > opts.maxEntries) {
      const oldest = entries.keys().next().value as K;
      remove(oldest);
    }
    notify();
  };

  const subscribe = (listener: () => void): (() => void) => {
    listeners.add(listener);
    return () => void listeners.delete(listener);
  };

  return {
    get,
    set,
    update: (key, f) => set(key, f(get(key))),
    delete: drop,
    useValue: (key) =>
      useSyncExternalStore(
        subscribe,
        () => get(key),
        () => get(key),
      ),
    subscribe,
    size: () => entries.size,
  };
}
