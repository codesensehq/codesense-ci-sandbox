// SPDX-License-Identifier: MIT
import { BrandMark } from "@/shared/components/brand-mark";

/**
 * Centered "CodeSense" splash content. Rendered inside `<AppShell>` by
 * the four startup-gate branches in `App.tsx` (initial load, dep check,
 * Claude check, org discovery). Kept as a standalone component so the
 * gate branches each read as a two-line return.
 */
export function AppSplash() {
  return (
    <div className="flex-1 flex items-center justify-center">
      <div className="text-center">
        <BrandMark />
        <h1 className="text-lg font-semibold">CodeSense</h1>
      </div>
    </div>
  );
}
