// SPDX-License-Identifier: MIT
import { vi } from "vitest";

import type { UseCodeMergeViewResult } from "./use-code-merge-view";

/**
 * Build a complete `UseCodeMergeViewResult` for tests that mock `useCodeMergeView`,
 * so each mock does not hand-roll (and drift on) the full field set. Pass only the
 * fields under test as `overrides`.
 */
export function makeCodeMergeViewResult(
  overrides: Partial<UseCodeMergeViewResult> = {},
): UseCodeMergeViewResult {
  return {
    handle: null,
    loadState: "ready",
    loadError: null,
    retry: vi.fn(),
    live: null,
    breadcrumb: null,
    ...overrides,
  };
}
