// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// SV-INP-48: CS.confirmHint in the hosted toolkit, run against a stand-in shell. It posts the
// confirm_hint op once the shell has said hello and does nothing else.

const SOURCE = readFileSync(
  join(process.cwd(), "src-tauri/src/dashboard_publish/toolkit/cs-live.js"),
  "utf8",
);

function loadToolkit(role: "owner" | "viewer") {
  const posted: Record<string, unknown>[] = [];
  const parent = {};
  Object.defineProperty(window, "parent", { value: parent, configurable: true });
  vi.spyOn(window, "postMessage").mockImplementation(((msg: Record<string, unknown>) => {
    posted.push(msg);
  }) as typeof window.postMessage);
  // One core per page: a test that loads the toolkit again starts from a fresh budget.
  delete (window as unknown as Record<string, unknown>).__csKitCore;
  const CS = new Function(`${SOURCE}\nreturn CS;`)() as { confirmHint: () => Promise<unknown> };
  window.dispatchEvent(
    new MessageEvent("message", {
      data: { cs: "live", v: 1, op: "hello", role, control: false },
      source: parent as Window,
    }),
  );
  return { CS, posted };
}

describe("CS.confirmHint", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it.each(["owner", "viewer"] as const)(
    "posts only the confirm_hint op and resolves with nothing for a %s",
    async (role) => {
      const { CS, posted } = loadToolkit(role);
      const done = CS.confirmHint();
      await vi.advanceTimersByTimeAsync(0);
      await expect(done).resolves.toBeUndefined();
      expect(posted).toEqual([{ cs: "live", v: 1, op: "confirm_hint" }]);
    },
  );
});
