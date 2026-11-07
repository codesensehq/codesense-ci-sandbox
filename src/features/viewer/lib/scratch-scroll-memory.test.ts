// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  initScrollMemory,
  recordScroll,
  onVisibilityChange,
  resetScroll,
} from "./scratch-scroll-memory";

describe("scratch scroll memory", () => {
  it("starts hidden or visible per the initial flag", () => {
    expect(initScrollMemory(true)).toEqual({ saved: 0, hidden: false });
    expect(initScrollMemory(false)).toEqual({ saved: 0, hidden: true });
  });

  it("records a scroll position only while visible", () => {
    let mem = initScrollMemory(true);
    mem = recordScroll(mem, true, 120);
    expect(mem.saved).toBe(120);
  });

  it("ignores a scroll reported while hidden (avoids clobbering with 0)", () => {
    let mem = initScrollMemory(true);
    mem = recordScroll(mem, true, 120);
    // A display:none container can fire scroll=0 — must not overwrite saved.
    mem = recordScroll(mem, false, 0);
    expect(mem.saved).toBe(120);
  });

  it("restores the saved position on the hidden → visible edge", () => {
    let mem = initScrollMemory(true);
    mem = recordScroll(mem, true, 240);

    // Minimize: goes hidden, nothing to restore.
    let restoreTo: number | null;
    [mem, restoreTo] = onVisibilityChange(mem, false);
    expect(mem.hidden).toBe(true);
    expect(restoreTo).toBeNull();

    // Restore: hidden → visible returns the saved scrollTop.
    [mem, restoreTo] = onVisibilityChange(mem, true);
    expect(mem.hidden).toBe(false);
    expect(restoreTo).toBe(240);
  });

  it("does NOT restore on an in-view resize (visible → visible)", () => {
    let mem = initScrollMemory(true);
    mem = recordScroll(mem, true, 90);
    const [, restoreTo] = onVisibilityChange(mem, true);
    // Already visible → a resize while shown must not yank scroll.
    expect(restoreTo).toBeNull();
  });

  it("resets saved scroll (e.g. on note switch) without changing visibility", () => {
    let mem = initScrollMemory(true);
    mem = recordScroll(mem, true, 300);
    mem = resetScroll(mem);
    expect(mem.saved).toBe(0);
    expect(mem.hidden).toBe(false);
    // After reset, restoring lands at the top, not the old note's position.
    const [, restoreTo] = onVisibilityChange({ ...mem, hidden: true }, true);
    expect(restoreTo).toBe(0);
  });
});
