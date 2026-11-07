// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";
import {
  __resetBackgroundSetupFanoutForTest,
  activeBackgroundSetupCount,
  BACKGROUND_SETUP_CAP_FALLBACK,
  DEFAULT_BACKGROUND_SETUP_CAP,
  resolveBackgroundSetupCap,
  setBackgroundSetupCap,
  withBackgroundSetupSlot,
} from "./background-setup-fanout";

const tick = () => new Promise<void>((r) => setTimeout(r, 0));

function deferred() {
  let release!: () => void;
  const started = { value: false };
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  const run = async () => {
    started.value = true;
    await promise;
  };
  return { run, release, started };
}

afterEach(() => __resetBackgroundSetupFanoutForTest());

describe("background-setup-fanout (RF-perf-14 Prong 2b)", () => {
  it("defaults to a LOW cap so a batch can't run a pile of heavy setups (never 8)", () => {
    expect(DEFAULT_BACKGROUND_SETUP_CAP).toBe(3);
  });

  it("caps concurrent background setups at the default (3), queues the rest FIFO", async () => {
    const tasks = Array.from({ length: 8 }, () => deferred());
    const running = tasks.map((t) => withBackgroundSetupSlot(t.run));
    await tick();
    // A quick 8-worktree batch → only 3 setups run at once, NOT 8.
    expect(activeBackgroundSetupCount()).toBe(3);
    expect(tasks.filter((t) => t.started.value).length).toBe(3);

    tasks[0].release();
    await tick();
    expect(activeBackgroundSetupCount()).toBe(3);
    expect(tasks[3].started.value).toBe(true);

    tasks.forEach((t) => t.release());
    await Promise.all(running);
    expect(activeBackgroundSetupCount()).toBe(0);
  });

  it("is INDEPENDENT of the creation gate (its own cap + state)", async () => {
    setBackgroundSetupCap(1);
    const tasks = Array.from({ length: 3 }, () => deferred());
    const running = tasks.map((t) => withBackgroundSetupSlot(t.run));
    await tick();
    expect(activeBackgroundSetupCount()).toBe(1);
    tasks.forEach((t) => t.release());
    await Promise.all(running);
  });

  it("releases the slot even when a background setup throws (no leak)", async () => {
    setBackgroundSetupCap(1);
    await expect(
      withBackgroundSetupSlot(async () => {
        throw new Error("setup failed");
      }),
    ).rejects.toThrow("setup failed");
    expect(activeBackgroundSetupCount()).toBe(0);
  });

  it("Off (cap Infinity) admits EVERY queued setup at once — no admission wait (RF-perf-23 AC-1)", async () => {
    // "Off" resolves to Infinity; the live-apply hook pushes it to the gate.
    setBackgroundSetupCap(resolveBackgroundSetupCap("off", 8));
    const tasks = Array.from({ length: 50 }, () => deferred());
    const running = tasks.map((t) => withBackgroundSetupSlot(t.run));
    await tick();
    // All 50 start immediately — the gate never blocks under Off.
    expect(activeBackgroundSetupCount()).toBe(50);
    expect(tasks.every((t) => t.started.value)).toBe(true);
    tasks.forEach((t) => t.release());
    await Promise.all(running);
    expect(activeBackgroundSetupCount()).toBe(0);
  });
});

describe("resolveBackgroundSetupCap (RF-perf-14d Prong A1)", () => {
  // The fallback IS today's shipped default — an unknown machine behaves typically.
  it("fallback constant equals the shipped default (3)", () => {
    expect(BACKGROUND_SETUP_CAP_FALLBACK).toBe(DEFAULT_BACKGROUND_SETUP_CAP);
    expect(BACKGROUND_SETUP_CAP_FALLBACK).toBe(3);
  });

  describe("auto sizing — tiered by cores (protect weak; RF-perf-23 raised ceilings for cheap clone-setup)", () => {
    it.each([
      [1, 1],
      [2, 1],
      [3, 2],
      [4, 3],
      [8, 6],
      [12, 8], // typical modern machine now sizes to 8 (was 3)
      [13, 12],
      [20, 12],
      [21, 16],
      [64, 16],
    ])("cores=%i → cap=%i", (cores, expected) => {
      expect(resolveBackgroundSetupCap("auto", cores)).toBe(expected);
    });
  });

  it("treats unset (undefined) setting the same as auto", () => {
    expect(resolveBackgroundSetupCap(undefined, 8)).toBe(6);
    expect(resolveBackgroundSetupCap(undefined, 16)).toBe(12);
  });

  it("an explicit numeric override wins over core sizing", () => {
    expect(resolveBackgroundSetupCap(2, 16)).toBe(2);
    expect(resolveBackgroundSetupCap(1, 64)).toBe(1);
    expect(resolveBackgroundSetupCap(5, 4)).toBe(5);
  });

  it("clamps an explicit override to ≥1 and floors fractional values", () => {
    expect(resolveBackgroundSetupCap(0, 8)).toBe(1);
    expect(resolveBackgroundSetupCap(-5, 8)).toBe(1);
    expect(resolveBackgroundSetupCap(2.9, 8)).toBe(2);
  });

  it("falls back to the shipped default (3) when cores are unknown/garbage under auto", () => {
    expect(resolveBackgroundSetupCap("auto", undefined)).toBe(3);
    expect(resolveBackgroundSetupCap("auto", Number.NaN)).toBe(3);
    expect(resolveBackgroundSetupCap("auto", 0)).toBe(3);
    expect(resolveBackgroundSetupCap("auto", -4)).toBe(3);
    expect(resolveBackgroundSetupCap(undefined, undefined)).toBe(3);
  });

  it("floors fractional core counts before tiering", () => {
    expect(resolveBackgroundSetupCap("auto", 12.9)).toBe(8);
    expect(resolveBackgroundSetupCap("auto", 20.5)).toBe(12);
  });

  describe('"off" → unbounded sentinel (RF-perf-23 AC-1)', () => {
    it('resolves "off" to Infinity — a well-typed numeric sentinel, never 0/NaN', () => {
      expect(resolveBackgroundSetupCap("off", 8)).toBe(Number.POSITIVE_INFINITY);
      // Independent of cores — off means off on any machine.
      expect(resolveBackgroundSetupCap("off", undefined)).toBe(Number.POSITIVE_INFINITY);
      expect(resolveBackgroundSetupCap("off", 64)).toBe(Number.POSITIVE_INFINITY);
    });

    it("Infinity is a number (not NaN), so the gate can never read it as cap 0", () => {
      const cap = resolveBackgroundSetupCap("off", 8);
      expect(typeof cap).toBe("number");
      expect(Number.isNaN(cap)).toBe(false);
      expect(cap).toBeGreaterThan(0);
    });
  });
});
