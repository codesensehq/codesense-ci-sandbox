// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  fileListErrorMessage,
  isNotIndexedError,
  onContextIndexGeneration,
  requestContentFileIndex,
  resetContentFileIndexForTests,
} from "./content-scan-queue";

const listContextFiles = vi.fn();
vi.mock("@/tauri/commands", () => ({
  scanPipelineTree: vi.fn(),
  listContextFiles: (...a: unknown[]) => listContextFiles(...(a as [string])),
}));

// The index-changed event: tests publish a generation through `emit`.
const bus = vi.hoisted(() => {
  const listeners: ((event: { payload: { generation: number } }) => void)[] = [];
  return {
    listeners,
    emit: (generation: number) => listeners.forEach((l) => l({ payload: { generation } })),
  };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (_name: string, cb: (event: { payload: { generation: number } }) => void) => {
    bus.listeners.push(cb);
    return () => {};
  },
}));

const file = (name: string) => ({ name, relativePath: name, fullPath: `/r/${name}` });
const settle = async () => {
  for (let i = 0; i < 6; i++) await Promise.resolve();
};

beforeEach(() => {
  vi.clearAllMocks();
  resetContentFileIndexForTests();
  bus.listeners.length = 0;
  listContextFiles.mockResolvedValue([file("a.md")]);
});

describe("the shared per-root file index (SV-PBF-267)", () => {
  it("joins in-flight requests for one root into one backend call", async () => {
    let finish!: (files: unknown[]) => void;
    listContextFiles.mockReturnValue(new Promise((resolve) => (finish = resolve)));
    const a = requestContentFileIndex("/r");
    const b = requestContentFileIndex("/r");
    expect(a).toBe(b);
    finish([file("a.md")]);
    await a;
    expect(listContextFiles).toHaveBeenCalledTimes(1);
  });

  it("serves a repeat at the same index generation from the cache", async () => {
    await requestContentFileIndex("/r");
    await settle();
    await requestContentFileIndex("/r");
    expect(listContextFiles).toHaveBeenCalledTimes(1);
    await requestContentFileIndex("/other");
    expect(listContextFiles).toHaveBeenCalledTimes(2);
  });

  it("drops the cache when the index publishes a newer generation, not for an older one", async () => {
    await requestContentFileIndex("/r");
    await settle();
    bus.emit(3);
    listContextFiles.mockResolvedValue([file("b.md")]);
    expect((await requestContentFileIndex("/r"))[0].name).toBe("b.md");
    await settle();
    bus.emit(3);
    bus.emit(2);
    await requestContentFileIndex("/r");
    expect(listContextFiles).toHaveBeenCalledTimes(2);
  });

  it("does not cache a listing that raced a newer generation", async () => {
    let finish!: (files: unknown[]) => void;
    listContextFiles.mockReturnValueOnce(new Promise((resolve) => (finish = resolve)));
    const stale = requestContentFileIndex("/r");
    await settle();
    bus.emit(4);
    finish([file("old.md")]);
    expect((await stale)[0].name).toBe("old.md");
    await settle();
    listContextFiles.mockResolvedValue([file("new.md")]);
    expect((await requestContentFileIndex("/r"))[0].name).toBe("new.md");
  });

  it("notifies listeners of a newer generation and stops after unsubscribe", async () => {
    const seen: number[] = [];
    const stop = onContextIndexGeneration((g) => seen.push(g));
    await settle();
    bus.emit(1);
    stop();
    bus.emit(2);
    expect(seen).toEqual([1]);
  });

  it("does not cache a failed listing", async () => {
    listContextFiles.mockRejectedValueOnce(new Error("boom"));
    await expect(requestContentFileIndex("/r")).rejects.toThrow("boom");
    await expect(requestContentFileIndex("/r")).resolves.toHaveLength(1);
  });
});

describe("a root the index refuses", () => {
  it("is recognised and explained in plain words, not as an empty list", async () => {
    listContextFiles.mockRejectedValue(new Error("not indexed: /elsewhere"));
    const error = await requestContentFileIndex("/elsewhere").catch((e: unknown) => e);
    expect(isNotIndexedError(error)).toBe(true);
    expect(isNotIndexedError("not indexed: /x")).toBe(true);
    expect(isNotIndexedError(new Error("Reading team files timed out"))).toBe(false);
    expect(fileListErrorMessage(error)).toMatch(/isn't part of your synced context/);
    expect(fileListErrorMessage(new Error("disk on fire"))).toBe("disk on fire");
  });
});
