// SPDX-License-Identifier: MIT
/**
 * Harness-only live event-listener counter (QH-5 AC-2, INSTRUMENTED signal).
 *
 * There is no standard API for "how many event listeners are currently
 * registered", so the soak's listener-retention proxy is instrumented: wrap
 * `addEventListener`/`removeEventListener` on the shared `EventTarget` prototype
 * and keep a running count. It is an INSTRUMENTED signal (approximate — a
 * duplicate add is a no-op the DOM ignores but we still count, and removing an
 * unregistered listener still decrements), which is why it never gates the
 * verdict; only its *growth* across iterations is a diagnostic. A genuine
 * listener leak (adds without matching removes) shows as a monotonic climb.
 *
 * This module is imported ONLY from `main.tsx`'s `MODE === "webdriver"` block,
 * so Vite tree-shakes it out of the default/production bundle entirely — proven
 * by `scripts/verify-webdriver-absence.ts`. It ships nothing to end users: no
 * code, no wrapper, no counter.
 */

type ListenerProto = Pick<EventTarget, "addEventListener" | "removeEventListener">;

let installed = false;
let count = 0;
let restore: (() => void) | null = null;

/** Current live listener count (adds minus removes since install). */
export function liveListenerCount(): number {
  return count;
}

/**
 * Idempotently wrap add/remove on `proto` (default: the global `EventTarget`
 * prototype, so window/document/every element funnel through it). Safe to call
 * more than once — a second call is a no-op, mirroring the module-scope timer
 * guards elsewhere in this app. The `proto` parameter exists only so tests can
 * patch a throwaway prototype instead of the global one; production always calls
 * it with the default, and the idempotency guard is global (first install wins).
 */
export function installListenerCounter(proto: ListenerProto = EventTarget.prototype): void {
  if (installed) return;
  installed = true;

  const originalAdd = proto.addEventListener;
  const originalRemove = proto.removeEventListener;

  proto.addEventListener = function patchedAdd(
    this: EventTarget,
    ...args: Parameters<EventTarget["addEventListener"]>
  ): void {
    count += 1;
    originalAdd.apply(this, args);
  };
  proto.removeEventListener = function patchedRemove(
    this: EventTarget,
    ...args: Parameters<EventTarget["removeEventListener"]>
  ): void {
    count = Math.max(0, count - 1);
    originalRemove.apply(this, args);
  };

  restore = () => {
    proto.addEventListener = originalAdd;
    proto.removeEventListener = originalRemove;
  };

  // Expose a stable read hook for `browser.execute` sampling in the soak suite.
  (
    globalThis as unknown as { __CODESENSE_E2E_LISTENERS__?: () => number }
  ).__CODESENSE_E2E_LISTENERS__ = liveListenerCount;
}

/** Test-only teardown: restore the original methods and reset the count. */
export function __resetListenerCounterForTest(): void {
  restore?.();
  restore = null;
  installed = false;
  count = 0;
  delete (globalThis as unknown as { __CODESENSE_E2E_LISTENERS__?: () => number })
    .__CODESENSE_E2E_LISTENERS__;
}
