// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

const { warn } = vi.hoisted(() => ({ warn: vi.fn() }));
vi.mock("@/shared/lib/logger", () => ({ logger: { warn } }));

import { awaitWorktreeEstablished, trackWorktreeEstablish } from "./pending-establish";

describe("pending worktree establish", () => {
  it("resolves immediately when nothing is in flight for the cwd", async () => {
    await expect(awaitWorktreeEstablished("/wt/none")).resolves.toBeUndefined();
  });

  it("waits for the tracked establish to settle", async () => {
    let release!: () => void;
    trackWorktreeEstablish("/wt/a", new Promise<void>((r) => (release = r)));
    let done = false;
    const waiting = awaitWorktreeEstablished("/wt/a").then(() => {
      done = true;
    });
    await Promise.resolve();
    expect(done).toBe(false);
    release();
    await waiting;
    expect(done).toBe(true);
  });

  it("retains a registration failure until a successful retry", async () => {
    trackWorktreeEstablish("/wt/b", Promise.reject(new Error("boom")));
    await expect(awaitWorktreeEstablished("/wt/b")).rejects.toThrow("boom");
    await expect(awaitWorktreeEstablished("/wt/b")).rejects.toThrow("boom");
    trackWorktreeEstablish("/wt/b", Promise.resolve());
    await expect(awaitWorktreeEstablished("/wt/b")).resolves.toBeUndefined();
  });

  it("gives up after the cap so a stuck establish never wedges a launch", async () => {
    trackWorktreeEstablish("/wt/c", new Promise<void>(() => {}));
    await expect(awaitWorktreeEstablished("/wt/c", 5)).rejects.toThrow(
      "registration is still pending",
    );
  });

  it("does not warn when the establish settles before the cap", async () => {
    warn.mockClear();
    trackWorktreeEstablish("/wt/d", Promise.resolve());
    await awaitWorktreeEstablished("/wt/d", 1_000);
    expect(warn).not.toHaveBeenCalled();
  });
});
