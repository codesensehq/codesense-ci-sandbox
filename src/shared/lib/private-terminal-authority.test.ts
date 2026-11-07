// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import retirementContract from "../../../tests/fixtures/private-authority-retirement.json";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

describe("private terminal presentation authority", () => {
  beforeEach(() => vi.resetModules());

  it("consumes the backend watch payload contract including expiry and exhaustion tombstones", async () => {
    const guard = await import("./private-terminal-authority");
    await guard.listenForPrivateAuthorityRetirement();
    guard.observePrivateAuthority(retirementContract[0]);
    guard.retirePrivateAuthority(retirementContract[1]);
    expect(guard.acceptsPrivateAuthority("1")).toBe(true);
    for (const status of retirementContract.slice(2)) {
      guard.retirePrivateAuthority(status);
      expect(guard.acceptsPrivateAuthority("1")).toBe(false);
      guard.observePrivateAuthority(status);
      expect(guard.acceptsPrivateAuthority(status.epoch)).toBe(status.available);
      if (!status.available) {
        guard.observePrivateAuthority({ ...status, available: true });
        expect(guard.acceptsPrivateAuthority(status.epoch)).toBe(false);
      }
    }
  });

  it("bounds a stalled listener and disposes its late registration without granting", async () => {
    vi.useFakeTimers();
    try {
      const { listen } = await import("@tauri-apps/api/event");
      let resolve!: (unlisten: () => void) => void;
      vi.mocked(listen).mockImplementationOnce(
        () =>
          new Promise((done) => {
            resolve = done;
          }),
      );
      const guard = await import("./private-terminal-authority");
      const ready = guard.listenForPrivateAuthorityRetirement();
      await vi.advanceTimersByTimeAsync(1_000);
      await ready;
      guard.observePrivateAuthority({ epoch: "1", available: true });
      expect(guard.acceptsPrivateAuthority("1")).toBe(false);
      const unlisten = vi.fn();
      resolve(unlisten);
      await Promise.resolve();
      expect(unlisten).toHaveBeenCalledOnce();
      expect(guard.acceptsPrivateAuthority("1")).toBe(false);
      await guard.listenForPrivateAuthorityRetirement();
      guard.observePrivateAuthority({ epoch: "2", available: true });
      expect(guard.acceptsPrivateAuthority("2")).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("refuses private status after listener failure until a successful retry and fresh status", async () => {
    const { listen } = await import("@tauri-apps/api/event");
    vi.mocked(listen).mockRejectedValueOnce(new Error("unavailable"));
    const guard = await import("./private-terminal-authority");
    await guard.listenForPrivateAuthorityRetirement();
    guard.observePrivateAuthority({ epoch: "1", available: true });
    expect(guard.acceptsPrivateAuthority("1")).toBe(false);
    await guard.listenForPrivateAuthorityRetirement();
    expect(guard.acceptsPrivateAuthority("1")).toBe(false);
    guard.observePrivateAuthority({ epoch: "2", available: true });
    expect(guard.acceptsPrivateAuthority("2")).toBe(true);
  });

  it("drops queued old output and never revives it after A to B to A", async () => {
    const guard = await import("./private-terminal-authority");
    await guard.listenForPrivateAuthorityRetirement();
    guard.observePrivateAuthority({ epoch: "1", available: true });
    const clear = vi.fn();
    guard.onPrivateAuthorityRetired(clear);
    expect(guard.acceptsPrivateAuthority("1")).toBe(true);
    guard.retirePrivateAuthority({ epoch: "3", available: true });
    expect(clear).toHaveBeenCalledOnce();
    expect(guard.acceptsPrivateAuthority("1")).toBe(false);
    guard.observePrivateAuthority({ epoch: "1", available: true });
    expect(guard.acceptsPrivateAuthority("1")).toBe(false);
    guard.observePrivateAuthority({ epoch: "3", available: true });
    expect(guard.acceptsPrivateAuthority("3")).toBe(true);
    expect(guard.acceptsPrivateAuthority("1")).toBe(false);
  });

  it("same-epoch extension notifications do not retire valid output", async () => {
    const guard = await import("./private-terminal-authority");
    await guard.listenForPrivateAuthorityRetirement();
    guard.observePrivateAuthority({ epoch: "9", available: true });
    const clear = vi.fn();
    guard.onPrivateAuthorityRetired(clear);
    guard.retirePrivateAuthority({ epoch: "9", available: true });
    expect(clear).not.toHaveBeenCalled();
    expect(guard.acceptsPrivateAuthority("9")).toBe(true);
  });

  it("auth response overtaking retirement still closes old epoch before publication", async () => {
    const guard = await import("./private-terminal-authority");
    await guard.listenForPrivateAuthorityRetirement();
    guard.observePrivateAuthority({ epoch: "9007199254740993", available: true });
    guard.observePrivateAuthority({ epoch: "9007199254740994", available: true });
    guard.retirePrivateAuthority({ epoch: "9007199254740994", available: true });
    expect(guard.acceptsPrivateAuthority("9007199254740993")).toBe(false);
    expect(guard.acceptsPrivateAuthority("9007199254740994")).toBe(true);
  });

  it("unavailable authority clears current access and events cannot grant", async () => {
    const guard = await import("./private-terminal-authority");
    await guard.listenForPrivateAuthorityRetirement();
    guard.observePrivateAuthority({ epoch: "5", available: true });
    guard.observePrivateAuthority({ epoch: "6", available: false });
    guard.retirePrivateAuthority({ epoch: "6", available: true });
    expect(guard.acceptsPrivateAuthority("5")).toBe(false);
    expect(guard.acceptsPrivateAuthority("6")).toBe(false);
    expect(guard.acceptsPrivateAuthority(undefined)).toBe(true);
  });
});
