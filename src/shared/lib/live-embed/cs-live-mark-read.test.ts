// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// SV-INP-46: CS.markRead in the hosted toolkit, run against a stand-in shell. The owner-only
// and shape rules hold in the page before anything is posted; the relay and the Mac repeat them.

const SOURCE = readFileSync(
  join(process.cwd(), "src-tauri/src/dashboard_publish/toolkit/cs-live.js"),
  "utf8",
);

type Markable = { markRead: (ids: unknown) => Promise<{ marked: number }> };

function loadToolkit(role: "owner" | "viewer") {
  const posted: Record<string, unknown>[] = [];
  const parent = {};
  Object.defineProperty(window, "parent", { value: parent, configurable: true });
  vi.spyOn(window, "postMessage").mockImplementation(((msg: Record<string, unknown>) => {
    posted.push(msg);
  }) as typeof window.postMessage);
  // One core per page: a test that loads the toolkit again starts from a fresh budget.
  delete (window as unknown as Record<string, unknown>).__csKitCore;
  const CS = new Function(`${SOURCE}\nreturn CS;`)() as Markable;
  const deliver = (data: Record<string, unknown>) =>
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { cs: "live", v: 1, ...data },
        source: parent as Window,
      }),
    );
  deliver({ op: "hello", role, control: false });
  return { CS, posted, deliver };
}

describe("CS.markRead", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("sends ownerMarkRead for the owner and resolves {marked}", async () => {
    const { CS, posted, deliver } = loadToolkit("owner");
    const pending = CS.markRead([3, 4]);
    await vi.advanceTimersByTimeAsync(0);
    const act = posted.find((m) => m.op === "act") as {
      aid: string;
      action: string;
      args: unknown;
    };
    expect(act.action).toBe("ownerMarkRead");
    expect(act.args).toEqual({ messageIds: [3, 4] });
    deliver({ op: "acted", aid: act.aid, ok: true, result: { status: "marked", marked: 2 } });
    await expect(pending).resolves.toEqual({ marked: 2 });
  });

  it("takes 200 ids and refuses a viewer, an empty list, 201 ids and bad ids before posting anything", async () => {
    const viewer = loadToolkit("viewer");
    await expect(viewer.CS.markRead([1])).rejects.toThrow("not_allowed");
    expect(viewer.posted.filter((m) => m.op === "act")).toEqual([]);
    vi.restoreAllMocks();

    const owner = loadToolkit("owner");
    const ids = (n: number) => Array.from({ length: n }, (_, i) => i + 1);
    for (const bad of [
      [],
      ids(201),
      [0],
      [-1],
      [1.5],
      ["1"],
      "1",
      null,
      undefined,
      [Number.MAX_SAFE_INTEGER + 2],
    ]) {
      await expect(owner.CS.markRead(bad)).rejects.toThrow("invalid_args");
    }
    expect(owner.posted.filter((m) => m.op === "act")).toEqual([]);
    const full = owner.CS.markRead(ids(200));
    await vi.advanceTimersByTimeAsync(0);
    expect(owner.posted.filter((m) => m.op === "act")).toHaveLength(1);
    void full.catch(() => {});
  });

  it("maps a missing count to 0 and passes the relay's refusals through", async () => {
    const { CS, posted, deliver } = loadToolkit("owner");
    const first = CS.markRead([1]);
    await vi.advanceTimersByTimeAsync(0);
    deliver({
      op: "acted",
      aid: (posted.find((m) => m.op === "act") as { aid: string }).aid,
      ok: true,
      result: {},
    });
    await expect(first).resolves.toEqual({ marked: 0 });
    const second = CS.markRead([2]);
    await vi.advanceTimersByTimeAsync(0);
    const acts = posted.filter((m) => m.op === "act") as { aid: string }[];
    deliver({ op: "acted", aid: acts[1].aid, ok: false, error: "not_found" });
    await expect(second).rejects.toThrow("not_found");
  });
});
