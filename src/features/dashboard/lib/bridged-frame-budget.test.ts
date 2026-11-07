// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
import { BridgedFrameBudget } from "./bridged-frame-budget";

it("flushes and evicts the least recently visible eligible frame", () => {
  const calls: string[] = [];
  const budget = new BridgedFrameBudget(2, vi.fn());
  const entry = (id: string, visible = false) => ({
    visible,
    unsaved: false,
    privateSpace: false,
    flush: () => calls.push(`flush:${id}`),
    evict: () => calls.push(`evict:${id}`),
  });
  budget.register("a", entry("a", true));
  budget.update("a", { visible: false });
  budget.register("b", entry("b", true));
  budget.update("b", { visible: false });
  budget.register("c", entry("c", true));
  expect(calls).toEqual(["flush:a", "evict:a"]);
  expect(budget.size).toBe(2);
});

it("pins visible, unsaved and private frames while evicting an eligible frame", () => {
  const budget = new BridgedFrameBudget(3, vi.fn());
  const evict = vi.fn();
  const entry = { visible: false, unsaved: false, privateSpace: false, flush: vi.fn(), evict };
  budget.register("visible", { ...entry, visible: true });
  budget.register("dirty", { ...entry, unsaved: true });
  budget.register("private", { ...entry, privateSpace: true });
  const eligible = vi.fn();
  budget.register("hidden", { ...entry, evict: eligible });
  expect(eligible).toHaveBeenCalledOnce();
  expect(evict).not.toHaveBeenCalled();
});

it("allows pinned overflow, warns once with its count, and evicts nothing else", () => {
  const warn = vi.fn();
  const budget = new BridgedFrameBudget(1, warn);
  const evict = vi.fn();
  const entry = { visible: true, unsaved: false, privateSpace: false, flush: vi.fn(), evict };
  budget.register("a", entry);
  budget.register("b", entry);
  budget.register("c", entry);
  budget.register("hidden", { ...entry, visible: false });
  expect(budget.size).toBe(4);
  expect(evict).not.toHaveBeenCalled();
  expect(warn).toHaveBeenCalledExactlyOnceWith(2);
});

it("still evicts when flush delivery fails", () => {
  const budget = new BridgedFrameBudget(1, vi.fn());
  const evict = vi.fn();
  budget.register("hidden", {
    visible: false,
    unsaved: false,
    privateSpace: false,
    flush: () => {
      throw new Error("frame gone");
    },
    evict,
  });
  expect(() =>
    budget.register("shown", {
      visible: true,
      unsaved: false,
      privateSpace: false,
      flush: vi.fn(),
      evict: vi.fn(),
    }),
  ).not.toThrow();
  expect(evict).toHaveBeenCalledOnce();
});
