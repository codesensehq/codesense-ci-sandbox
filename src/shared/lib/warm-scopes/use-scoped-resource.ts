// SPDX-License-Identifier: MIT
import { useEffect, useLayoutEffect, useRef } from "react";
import { warmScopes } from "./warm-scopes";
import type { ScopeKey } from "./tiers";

type Disposable = { dispose(): void };

/** Acquire a resource on behalf of a scope for the life of the component. It goes into the scope's
 *  bag: a re-acquire of the same `(scope, kind, key)` replaces the old one, and the scope's tier
 *  drop, `forget` or unmount disposes it exactly once. `acquire` may be an inline function: it runs
 *  again only when `scope`, `kind` or `key` change. */
export function useScopedResource(
  scope: ScopeKey,
  kind: string,
  key: string,
  acquire: () => Disposable | (() => void),
): void {
  const acquireRef = useRef(acquire);
  useLayoutEffect(() => {
    acquireRef.current = acquire;
  });
  useEffect(() => {
    const resource = acquireRef.current();
    return warmScopes.hold(scope, kind, key, () =>
      typeof resource === "function" ? resource() : resource.dispose(),
    );
  }, [scope, kind, key]);
}
