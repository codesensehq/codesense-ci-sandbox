// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";

/**
 * Creates a debounced action that delays execution until idle for `delayMs`.
 * Each call to `trigger` resets the timer. The `label` is used for error logging.
 *
 * `flush()` resolves only after any in-flight run settles — not just the
 * pending timer — so callers that `await flush()` before reading shared state
 * (e.g. a file the action writes) never race a write that already started.
 */
export function createDebouncedAction(
  fn: (...args: unknown[]) => Promise<void>,
  delayMs: number,
  label: string,
  keyForArgs: (...args: unknown[]) => string = () => "",
): {
  trigger: (...args: unknown[]) => void;
  cancel: () => void;
  flush: () => Promise<void>;
  flushStrict: () => Promise<void>;
} {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pendingArgs: unknown[] | null = null;
  // The most recent run's promise, tracked so `flush()` can await a write
  // that already fired (timer elapsed) rather than returning immediately.
  let running: Promise<void> | null = null;
  const failures = new Map<string, { error: unknown; args: unknown[] }>();

  // Run `fn` and record the resulting promise so `flush()` can await it.
  // When a prior run is still in flight, chain after it so writes to shared
  // state never reorder (an older write can't clobber a newer one) and
  // `flush()` awaits the whole outstanding chain — not just the latest run.
  // With nothing in flight, `fn` is invoked synchronously (the timing
  // callers/tests rely on).
  function invoke(args: unknown[]): Promise<void> {
    const prev = running;
    const key = keyForArgs(...args);
    const body = (): Promise<void> =>
      fn(...args)
        .then(() => {
          failures.delete(key);
        })
        .catch((error: unknown) => {
          failures.set(key, { error, args });
          logger.error("viewer", `${label} failed: ${error}`);
        });
    const p: Promise<void> = (prev ? prev.then(body) : body()).finally(() => {
      if (running === p) running = null;
    });
    running = p;
    return p;
  }

  async function flush(): Promise<void> {
    if (timer) clearTimeout(timer);
    timer = null;
    const args = pendingArgs;
    pendingArgs = null;
    if (args) void invoke(args);
    await (running ?? Promise.resolve());
  }

  return {
    trigger(...args: unknown[]) {
      if (timer) clearTimeout(timer);
      pendingArgs = args;
      timer = setTimeout(() => {
        timer = null;
        pendingArgs = null;
        void invoke(args);
      }, delayMs);
    },
    cancel() {
      if (timer) {
        clearTimeout(timer);
        timer = null;
        pendingArgs = null;
      }
    },
    flush,
    async flushStrict() {
      // Retry a previously failed idle write on a new explicit durability attempt.
      // A failure in this attempt propagates; ordinary autosaves remain best effort.
      for (const failure of [...failures.values()]) void invoke(failure.args);
      await flush();
      const failure = failures.values().next().value;
      if (failure) throw failure.error;
    },
  };
}
