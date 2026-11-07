// SPDX-License-Identifier: MIT
/**
 * Race a promise against a bounded timeout — a fail-open watchdog primitive.
 *
 * If `promise` settles first, its resolution (or rejection) wins unchanged. If
 * `timeoutMs` elapses first, `onTimeout()` is invoked: its return value
 * resolves the race (a fail-open fallback), or — if `onTimeout` throws — the
 * race rejects with that error (so callers with an existing catch can treat a
 * timeout as "skip this one"). Either way a hung/never-settling promise can
 * never wedge the caller. The internal timer is always cleared on settle, and a
 * late resolution/rejection of the original promise after the timeout already
 * won is swallowed (the attached rejection handler means it never surfaces as
 * an unhandled rejection).
 *
 * SW-FX-75: used to guarantee every boot gate flips within a bound. Purely
 * `setTimeout`-based — it runs off the render thread and only ever *shortens*
 * the worst case; it adds zero latency to the happy path. It must never be
 * placed in front of a render/paint step as an added `await`; wrap the
 * already-async gate owner, not the UI.
 */
export function promiseWithTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  onTimeout: () => T,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try {
        resolve(onTimeout());
      } catch (error) {
        reject(error);
      }
    }, timeoutMs);
    promise.then(
      (value) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}
