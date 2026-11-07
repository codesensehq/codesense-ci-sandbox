// SPDX-License-Identifier: MIT
export function createIntentReader<T, C>(options: {
  enabled?: () => boolean;
  capture: () => C;
  read: (context: C) => Promise<T>;
  commit: (data: T, context: C) => void;
  fail: (error: unknown, context: C) => void;
  settled?: (context: C) => void;
}): {
  intent(options?: { read?: boolean }): void;
  request(): void;
  resume(): void;
  stop(): void;
};
