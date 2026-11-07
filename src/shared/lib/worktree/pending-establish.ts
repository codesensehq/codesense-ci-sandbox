// SPDX-License-Identifier: MIT
/**
 * In-flight worktree→story establish calls, keyed by worktree cwd.
 *
 * Story opens await durable registration before setup and readiness. Launch
 * callers share that registration promise so concurrent continuations cannot
 * race ownership or essential agent wiring. Failures remain visible until retry.
 */

const pending = new Map<string, Promise<void>>();

/** Upper bound on how long a launch waits for an in-flight establish. Past it
 *  the launch fails; a pending registration must never race process startup. */
export const ESTABLISH_WAIT_CAP_MS = 15_000;

/** Record an in-flight establish for `cwd`. Successful entries clear themselves; failures remain until retry. */
export function trackWorktreeEstablish(cwd: string, establish: Promise<unknown>): void {
  const settled = establish.then(() => undefined);
  pending.set(cwd, settled);
  void settled.then(
    () => {
      if (pending.get(cwd) === settled) pending.delete(cwd);
    },
    () => {
      /* Retain failure until a new establish attempt replaces it. */
    },
  );
}

/** Resolve once the in-flight establish for `cwd` (if any) settles, or after
 *  `capMs`. Registration failures and timeout reject the launch. */
export function awaitWorktreeEstablished(
  cwd: string,
  capMs: number = ESTABLISH_WAIT_CAP_MS,
): Promise<void> {
  const inFlight = pending.get(cwd);
  if (!inFlight) return Promise.resolve();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const cap = new Promise<void>((_resolve, reject) => {
    timer = setTimeout(() => {
      reject(
        new Error(
          `Workspace registration is still pending after ${capMs}ms; retry launch once it completes.`,
        ),
      );
    }, capMs);
  });
  return Promise.race([inFlight, cap]).finally(() => clearTimeout(timer));
}
