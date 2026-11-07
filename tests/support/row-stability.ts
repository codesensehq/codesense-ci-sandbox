// SPDX-License-Identifier: MIT
import type { Frame, Locator } from "@playwright/test";

/** Track the row and the element actually under a stationary pointer across paints. */
export async function watchRow(frame: Frame, row: Locator) {
  await row.hover();
  const selector = await row.evaluate((el) => {
    const key = (el as HTMLElement).dataset.key!;
    return `[data-key="${CSS.escape(key)}"]`;
  });
  await frame.evaluate((selector) => {
    const row = document.querySelector(selector)!;
    const bounds = row.getBoundingClientRect();
    const x = bounds.left + bounds.width / 2,
      y = bounds.top + bounds.height / 2;
    const underPointer = document.elementFromPoint(x, y);
    const state = { replacements: 0, pointerReplacements: 0, removals: 0, frames: 0 };
    let previous = row,
      previousPointer = underPointer;
    const observer = new MutationObserver((mutations) => {
      for (const mutation of mutations)
        for (const removed of mutation.removedNodes)
          if (removed === row || removed.contains(row)) state.removals++;
    });
    observer.observe(row.parentElement!, { childList: true, subtree: true });
    let frameId = 0;
    const tick = () => {
      const current = document.querySelector(selector)!;
      const pointer = document.elementFromPoint(x, y);
      state.frames++;
      if (current !== previous) state.replacements++;
      if (pointer !== previousPointer && !previousPointer?.isConnected) state.pointerReplacements++;
      previous = current;
      previousPointer = pointer;
      frameId = requestAnimationFrame(tick);
    };
    frameId = requestAnimationFrame(tick);
    (window as any).__rowStability = () => {
      cancelAnimationFrame(frameId);
      observer.disconnect();
      return {
        ...state,
        sameRow: document.querySelector(selector) === row,
        samePointer: document.elementFromPoint(x, y) === underPointer,
      };
    };
  }, selector);
}

export async function rowStability(frame: Frame) {
  return frame.evaluate(
    () =>
      (window as any).__rowStability() as {
        replacements: number;
        pointerReplacements: number;
        removals: number;
        frames: number;
        sameRow: boolean;
        samePointer: boolean;
      },
  );
}
