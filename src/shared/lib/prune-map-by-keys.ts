// SPDX-License-Identifier: MIT
/** Return a copy of `map` keeping only entries whose key is in `liveKeys`.
 *  Referentially stable: returns the SAME map instance when nothing drops
 *  (and on the empty-map fast path) so React skips the re-render. This
 *  reference-equality bail-out is load-bearing for render perf — the inline
 *  versions this centralizes (teams-card, pipeline-status-nav) hand-rolled it
 *  for exactly that reason. */
export function pruneMapByKeys<K, V>(map: Map<K, V>, liveKeys: Set<K>): Map<K, V> {
  if (map.size === 0) return map;
  let mutated = false;
  const next = new Map<K, V>();
  for (const [key, value] of map) {
    if (liveKeys.has(key)) next.set(key, value);
    else mutated = true;
  }
  return mutated ? next : map;
}
