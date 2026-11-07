// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { computeClampedRatio, migrateScratchNoteRatio } from "./drag-resize-utils";

describe("computeClampedRatio — terminal drag handle", () => {
  const CONTAINER_H = 800;
  const START_RATIO = 0.55; // 55% viewer, 45% terminal

  it("no movement returns start ratio unchanged", () => {
    expect(computeClampedRatio(START_RATIO, 400, 400, CONTAINER_H)).toBe(0.55);
  });

  it("dragging down increases viewer ratio (shrinks terminal)", () => {
    // Drag 80px down → deltaRatio = 80/800 = +0.1
    const ratio = computeClampedRatio(START_RATIO, 400, 480, CONTAINER_H);
    expect(ratio).toBeCloseTo(0.65, 5);
  });

  it("dragging up decreases viewer ratio (grows terminal)", () => {
    // Drag 80px up → deltaRatio = -80/800 = -0.1
    const ratio = computeClampedRatio(START_RATIO, 400, 320, CONTAINER_H);
    expect(ratio).toBeCloseTo(0.45, 5);
  });

  it("clamps to min viewer height (prevent viewer < 80px)", () => {
    // Drag way up — try to make viewer 0%
    const ratio = computeClampedRatio(START_RATIO, 400, 0, CONTAINER_H);
    // min ratio = 80/800 = 0.1
    expect(ratio).toBeCloseTo(0.1, 5);
  });

  it("clamps to min terminal height (prevent terminal < 80px)", () => {
    // Drag way down — try to make terminal 0%
    const ratio = computeClampedRatio(START_RATIO, 400, 1200, CONTAINER_H);
    // max ratio = 1 - 80/800 = 0.9
    expect(ratio).toBeCloseTo(0.9, 5);
  });

  it("handles small container — clamping still enforces minimums", () => {
    // Container only 200px → min 80px means min ratio = 0.4
    const ratio = computeClampedRatio(0.5, 100, 0, 200);
    expect(ratio).toBeCloseTo(0.4, 5);
  });

  it("locks at 50% when container too small for both minimums", () => {
    // Container 100px with 80px min → minRatio=0.8, maxRatio=0.2 → inverted
    const ratio = computeClampedRatio(0.5, 50, 0, 100);
    expect(ratio).toBe(0.5);
  });

  it("handles zero container height gracefully", () => {
    // Edge case: containerH = 0 → early return with startRatio
    const ratio = computeClampedRatio(0.5, 100, 100, 0);
    expect(ratio).toBe(0.5);
  });
});

describe("computeClampedRatio — invertDelta", () => {
  const CONTAINER = 800;
  const START = 0.45;

  it("invertDelta=true: dragging toward lower coords increases ratio", () => {
    // Drag left 80px (currentPos < startPos) → rawDelta negative → inverted to positive
    const ratio = computeClampedRatio(START, 400, 320, CONTAINER, 80, true);
    expect(ratio).toBeCloseTo(0.55, 5);
  });

  it("invertDelta=true: dragging toward higher coords decreases ratio", () => {
    // Drag right 80px (currentPos > startPos) → rawDelta positive → inverted to negative
    const ratio = computeClampedRatio(START, 400, 480, CONTAINER, 80, true);
    expect(ratio).toBeCloseTo(0.35, 5);
  });

  it("invertDelta=true: no movement returns start ratio", () => {
    expect(computeClampedRatio(START, 400, 400, CONTAINER, 80, true)).toBe(START);
  });

  it("invertDelta=true: still clamps to min", () => {
    // Drag way right → ratio goes negative → clamped to minPx/container
    const ratio = computeClampedRatio(START, 400, 1200, CONTAINER, 80, true);
    expect(ratio).toBeCloseTo(0.1, 5);
  });

  it("invertDelta=true: still clamps to max", () => {
    // Drag way left → ratio goes high → clamped to 1 - minPx/container = 0.9
    const ratio = computeClampedRatio(START, 400, 0, CONTAINER, 80, true);
    expect(ratio).toBeCloseTo(0.9, 5);
  });
});

describe("computeClampedRatio — maxRatio override", () => {
  const CONTAINER = 800;

  it("custom maxRatio caps the upper bound", () => {
    // Without override, max would be 1 - 80/800 = 0.9
    // With maxRatio=0.7, drag way down is capped at 0.7
    const ratio = computeClampedRatio(0.5, 400, 1200, CONTAINER, 80, false, 0.7);
    expect(ratio).toBeCloseTo(0.7, 5);
  });

  it("maxRatio does not affect lower bound", () => {
    // Min is still 80/800 = 0.1
    const ratio = computeClampedRatio(0.5, 400, 0, CONTAINER, 80, false, 0.7);
    expect(ratio).toBeCloseTo(0.1, 5);
  });

  it("maxRatio with invertDelta works together", () => {
    // Right-pane-layout: invertDelta=true, maxRatio=0.84
    // Drag way up → ratio goes high → capped at 0.84
    const ratio = computeClampedRatio(0.45, 400, 0, CONTAINER, 80, true, 0.84);
    expect(ratio).toBeCloseTo(0.84, 5);
  });

  it("locks at 50% when minRatio >= maxRatio", () => {
    // minPx=80, container=100 → minR=0.8, maxRatio=0.5 → minR >= maxR
    const ratio = computeClampedRatio(0.5, 50, 80, 100, 80, false, 0.5);
    expect(ratio).toBe(0.5);
  });
});

describe("terminal height from ratio", () => {
  it("default ratio 0.55 produces ~45% terminal height", () => {
    const viewerRatio = 0.55;
    const terminalPercent = (1 - viewerRatio) * 100;
    expect(terminalPercent).toBeCloseTo(45, 1);
  });

  it("ratio 0.7 produces 30% terminal height", () => {
    const viewerRatio = 0.7;
    const terminalPercent = (1 - viewerRatio) * 100;
    expect(terminalPercent).toBeCloseTo(30, 1);
  });

  it("ratio 0.3 produces 70% terminal height", () => {
    const viewerRatio = 0.3;
    const terminalPercent = (1 - viewerRatio) * 100;
    expect(terminalPercent).toBeCloseTo(70, 1);
  });
});

describe("migrateScratchNoteRatio", () => {
  const VP = 1200; // viewport width

  it("converts legacy pixel width (400px) to ratio", () => {
    // 400 / 1200 = 0.333…
    expect(migrateScratchNoteRatio(400, VP)).toBeCloseTo(0.333, 2);
  });

  it("converts large pixel width and clamps to max 0.5", () => {
    // 800 / 1200 = 0.667 → clamped to 0.5
    expect(migrateScratchNoteRatio(800, VP)).toBe(0.5);
  });

  it("converts small pixel width and clamps to min 0.15", () => {
    // 100 / 1200 = 0.083 → clamped to 0.15
    expect(migrateScratchNoteRatio(100, VP)).toBe(0.15);
  });

  it("passes through already-migrated ratio values", () => {
    expect(migrateScratchNoteRatio(0.33, VP)).toBeCloseTo(0.33, 5);
    expect(migrateScratchNoteRatio(0.4, VP)).toBeCloseTo(0.4, 5);
  });

  it("clamps ratio below 0.15 to 0.15", () => {
    expect(migrateScratchNoteRatio(0.05, VP)).toBe(0.15);
  });

  it("clamps ratio above 0.5 to 0.5", () => {
    expect(migrateScratchNoteRatio(0.8, VP)).toBe(0.5);
  });

  it("returns default 0.33 for NaN", () => {
    expect(migrateScratchNoteRatio(NaN, VP)).toBe(0.33);
  });

  it("returns default 0.33 for Infinity", () => {
    expect(migrateScratchNoteRatio(Infinity, VP)).toBe(0.33);
    expect(migrateScratchNoteRatio(-Infinity, VP)).toBe(0.33);
  });

  it("returns default 0.33 for non-number types", () => {
    expect(migrateScratchNoteRatio("400" as unknown, VP)).toBe(0.33);
    expect(migrateScratchNoteRatio(null as unknown, VP)).toBe(0.33);
    expect(migrateScratchNoteRatio(undefined as unknown, VP)).toBe(0.33);
  });
});
