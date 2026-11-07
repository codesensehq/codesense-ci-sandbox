// SPDX-License-Identifier: MIT
import { vi } from "vitest";

/**
 * Flush the microtask queue by advancing fake timers by 0ms.
 * Repeat multiple times to allow chained async operations to settle
 * (e.g. pollJiraAuth resolves -> completeJiraAuth called -> resolves).
 */
export async function flushMicrotasks(rounds = 1): Promise<void> {
  for (let i = 0; i < rounds; i++) {
    await vi.advanceTimersByTimeAsync(0);
  }
}
