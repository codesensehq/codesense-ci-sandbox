// SPDX-License-Identifier: MIT
/**
 * In-place recursive walk of every string value under `node` (array elements
 * and object values, at any depth), replacing each with `rewrite(str)` when the
 * result differs. The shared substrate for the persisted-blob path migrations
 * in `loadPersistedState` — [`deepRewriteOrgSlugPaths`] (org-slug `owner--repo`
 * → `owner/repo`) and [`deepRewriteFlattenTeamPaths`] (SLW-IN-8 `_team/` flatten)
 * — which differ ONLY in their per-string rewrite, so the walk lives here once.
 *
 * Bounded + synchronous by design: it runs over an already-loaded blob inside
 * `loadPersistedState`, the same cost profile the org-slug pass has shipped with
 * (no worker needed). Idempotent iff `rewrite` is.
 */
export function deepRewriteStrings(node: unknown, rewrite: (value: string) => string): void {
  if (Array.isArray(node)) {
    for (let i = 0; i < node.length; i++) {
      const value = node[i];
      if (typeof value === "string") {
        const rewritten = rewrite(value);
        if (rewritten !== value) node[i] = rewritten;
      } else if (value && typeof value === "object") {
        deepRewriteStrings(value, rewrite);
      }
    }
    return;
  }
  if (node && typeof node === "object") {
    const obj = node as Record<string, unknown>;
    for (const key of Object.keys(obj)) {
      const value = obj[key];
      if (typeof value === "string") {
        const rewritten = rewrite(value);
        if (rewritten !== value) obj[key] = rewritten;
      } else if (value && typeof value === "object") {
        deepRewriteStrings(value, rewrite);
      }
    }
  }
}
