// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";

/**
 * RF-perf-4 (AC-4): the launch path must stay async / non-render-blocking so startup
 * never shows a spinner or a frozen first frame ("no loading or slow-downs").
 *
 * The concrete regression signal is `useLayoutEffect` in the app shell: React runs a
 * layout effect **synchronously before the browser paints**, so any startup work placed
 * there blocks first paint. Startup effects must instead be `useEffect` (listener /
 * fire-and-forget-async), which runs *after* paint. The audit (RF-perf-4) confirmed
 * `App.tsx` uses zero layout effects today; this guard fails the moment one is added, so
 * a reviewer must consciously justify blocking the launch paint.
 *
 * A source-level assertion (not a render test) on purpose: it catches the regression at
 * the shell boundary regardless of which effect a future change lands in, with no need
 * to mount the full app tree.
 */
describe("App startup stays async (RF-perf-4 AC-4)", () => {
  const appSrc = readFileSync(resolve(__dirname, "./App.tsx"), "utf-8");

  it("uses no useLayoutEffect in the app shell (would block first paint)", () => {
    expect(appSrc).not.toMatch(/\buseLayoutEffect\b/);
  });
});
