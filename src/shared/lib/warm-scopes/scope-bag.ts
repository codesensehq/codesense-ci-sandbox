// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";
import { isBelow, type HeldTier, type ScopeKey, type ScopeTier } from "./tiers";

interface Hold {
  dispose: () => void;
  floor: HeldTier;
  kind: string;
}

/** The disposers held on behalf of each scope. `(scope, kind, key)` is unique: holding the same
 *  triple again disposes the old entry first, so repeated switches cannot stack resources. Every
 *  entry is disposed exactly once, whether by release, replacement, a tier drop or `forget`. */
export class ScopeBag {
  private readonly byScope = new Map<ScopeKey, Map<string, Hold>>();
  private readonly disposedByKind: Record<string, number> = {};

  hold(
    scope: ScopeKey,
    kind: string,
    key: string,
    dispose: () => void,
    floor: HeldTier,
  ): () => void {
    const id = `${kind}\0${key}`;
    let entries = this.byScope.get(scope);
    if (!entries) this.byScope.set(scope, (entries = new Map()));
    const previous = entries.get(id);
    if (previous) this.run(previous);
    const hold: Hold = { dispose, floor, kind };
    entries.set(id, hold);
    return () => {
      const current = this.byScope.get(scope);
      // Only the entry this call created: a stale release must not dispose its replacement.
      if (current?.get(id) !== hold) return;
      current.delete(id);
      if (current.size === 0) this.byScope.delete(scope);
      this.run(hold);
    };
  }

  /** Dispose the scope's entries whose floor is above `tier`. */
  dropBelow(scope: ScopeKey, tier: ScopeTier): void {
    const entries = this.byScope.get(scope);
    if (!entries) return;
    for (const [id, hold] of [...entries]) {
      if (!isBelow(tier, hold.floor)) continue;
      entries.delete(id);
      this.run(hold);
    }
    if (entries.size === 0) this.byScope.delete(scope);
  }

  disposeAll(scope: ScopeKey): void {
    const entries = this.byScope.get(scope);
    this.byScope.delete(scope);
    for (const hold of entries?.values() ?? []) this.run(hold);
  }

  /** Move a scope's entries to another key without disposing them. */
  move(from: ScopeKey, to: ScopeKey): void {
    const entries = this.byScope.get(from);
    if (!entries) return;
    this.byScope.delete(from);
    this.byScope.set(to, entries);
  }

  held(): Record<ScopeKey, Record<string, number>> {
    const out: Record<ScopeKey, Record<string, number>> = {};
    for (const [scope, entries] of this.byScope) {
      const kinds: Record<string, number> = {};
      for (const hold of entries.values()) kinds[hold.kind] = (kinds[hold.kind] ?? 0) + 1;
      out[scope] = kinds;
    }
    return out;
  }

  total(): number {
    let n = 0;
    for (const entries of this.byScope.values()) n += entries.size;
    return n;
  }

  disposed(): Record<string, number> {
    return { ...this.disposedByKind };
  }

  private run(hold: Hold): void {
    this.disposedByKind[hold.kind] = (this.disposedByKind[hold.kind] ?? 0) + 1;
    try {
      hold.dispose();
    } catch (e) {
      logger.warn("warm-scopes", `dispose of a ${hold.kind} hold threw: ${String(e)}`);
    }
  }
}
