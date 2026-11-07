// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { subscribeToAge } from "./age-clock";

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  vi.setSystemTime(100_000);
});
afterEach(() => vi.useRealTimers());

describe("subscribeToAge", () => {
  it("one timer for every subscriber, set for the soonest phrase change", () => {
    const young = vi.fn();
    const old = vi.fn();
    const stopOld = subscribeToAge(100_000 - 5 * 60_000 - 20_000, old); // 5m20s: next change in 40s
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(39_999);
    expect(old).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(old).toHaveBeenCalledTimes(1);
    const stopYoung = subscribeToAge(Date.now() - 250, young); // 0s: next change in 750ms
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(750);
    expect(young).toHaveBeenCalledTimes(1);
    stopYoung();
    stopOld();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("unsubscribing between ticks re-times the clock for who is left", () => {
    const young = vi.fn();
    const old = vi.fn();
    const stopYoung = subscribeToAge(Date.now(), young); // ticks each second
    const stopOld = subscribeToAge(Date.now() - 2 * 60_000, old); // next minute in 60s
    vi.advanceTimersByTime(3_000);
    expect(young).toHaveBeenCalledTimes(3);
    const oldCalls = old.mock.calls.length;
    stopYoung();
    // Only the minute-phrased age is left: nothing until its next minute.
    vi.advanceTimersByTime(56_999);
    expect(old.mock.calls.length).toBe(oldCalls);
    vi.advanceTimersByTime(1);
    expect(old.mock.calls.length).toBe(oldCalls + 1);
    expect(young).toHaveBeenCalledTimes(3);
    stopOld();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("subscribing between ticks joins the running clock without a second timer", () => {
    const a = vi.fn();
    const b = vi.fn();
    const stopA = subscribeToAge(Date.now(), a);
    vi.advanceTimersByTime(400);
    const stopB = subscribeToAge(Date.now() - 900, b); // changes in 100ms
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(100);
    expect(b).toHaveBeenCalledTimes(1);
    stopA();
    stopB();
    expect(vi.getTimerCount()).toBe(0);
  });
});
