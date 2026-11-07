// SPDX-License-Identifier: MIT
import type { Frame } from "@playwright/test";

/** Sample, every frame for `ms`, whether the point at an element's centre still hits it. */
export async function hitStability(frame: Frame, selector: string, ms = 1500) {
  return frame.evaluate(
    async ([sel, ms]) => {
      const first = document.querySelector(sel as string) as HTMLElement | null;
      if (!first) return { found: false };
      const r = first.getBoundingClientRect();
      const x = r.left + r.width / 2,
        y = r.top + r.height / 2;
      let frames = 0,
        misses = 0,
        replaced = 0,
        hidden = 0;
      const log: string[] = [];
      const t0 = performance.now();
      await new Promise<void>((done) => {
        const tick = () => {
          frames++;
          const now = document.querySelector(sel as string) as HTMLElement | null;
          if (now !== first) replaced++;
          const hit = document.elementFromPoint(x, y);
          if (!now || !(now === hit || now.contains(hit))) {
            misses++;
            if (log.length < 5) log.push(String((hit as HTMLElement)?.className));
          }
          if (now && getComputedStyle(now).visibility === "hidden") hidden++;
          if (performance.now() - t0 < (ms as number)) requestAnimationFrame(tick);
          else done();
        };
        requestAnimationFrame(tick);
      });
      return { found: true, frames, misses, replaced, hidden, log };
    },
    [selector, ms] as const,
  );
}
