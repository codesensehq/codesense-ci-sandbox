// SPDX-License-Identifier: MIT
import type { UnlistenFn } from "@tauri-apps/api/event";

/**
 * Invoke a Tauri `UnlistenFn` without ever leaking an unhandled rejection.
 *
 * `UnlistenFn` is typed `() => void`, but at runtime it returns the internal
 * `plugin:event|unlisten` invoke promise. That promise can reject while the
 * webview is tearing down (React StrictMode remount, Vite HMR, window close) —
 * and because the type hides it, callers that do a bare `fn()` never catch it,
 * so it surfaces as an `unhandledrejection` (SW-FX-75 measured 11+ per packaged
 * boot). Routing every teardown through here funnels that promise into a caught
 * chain. Fire-and-forget: it never blocks and never throws.
 */
export function safeUnlisten(fn: UnlistenFn): void {
  try {
    void Promise.resolve(fn() as unknown as void | Promise<unknown>).catch(() => {});
  } catch {
    // A synchronous throw from the unlisten path — swallow it too.
  }
}

/**
 * A small registration group for Tauri event listeners that standardizes the
 * teardown boilerplate reimplemented across many hooks: a `disposed` guard that
 * closes the register race (dispose before `listen()` resolves → unlisten
 * immediately), and safe teardown via {@link safeUnlisten}.
 *
 * Usage inside a `useEffect`:
 * ```ts
 * const group = createListenerGroup();
 * group.track(listen(EVENT, handler));
 * return () => group.dispose();
 * ```
 */
export interface ListenerGroup {
  /** Register a pending `listen()` promise; its unlisten is tracked (or fired
   *  immediately if the group was already disposed). Registration rejections
   *  are swallowed. */
  track(pending: Promise<UnlistenFn>): void;
  /** Tear down every tracked listener safely and mark the group disposed so any
   *  still-pending registration unlistens as soon as it resolves. */
  dispose(): void;
}

export function createListenerGroup(): ListenerGroup {
  const unlisteners: UnlistenFn[] = [];
  let disposed = false;
  return {
    track(pending: Promise<UnlistenFn>): void {
      pending
        .then((fn) => {
          if (disposed) safeUnlisten(fn);
          else unlisteners.push(fn);
        })
        .catch(() => {});
    },
    dispose(): void {
      if (disposed) return;
      disposed = true;
      for (const fn of unlisteners) safeUnlisten(fn);
      unlisteners.length = 0;
    },
  };
}
