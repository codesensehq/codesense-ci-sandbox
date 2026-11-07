// SPDX-License-Identifier: MIT
// @vitest-environment node
import vm from "node:vm";
import { TextEncoder } from "node:util";
import { afterEach, expect, it, vi } from "vitest";
import { viewerScript } from "../../../../tests/support/page-shim";

function shim() {
  const posted: any[] = [];
  const listeners: Array<(event: any) => void> = [];
  const parent = { postMessage: (m: unknown) => posted.push(m) };
  const warn = vi.fn();
  const window: any = {
    parent,
    __codesenseProxy: { token: "reload-test" },
    addEventListener: (name: string, cb: any) => {
      if (name === "message") listeners.push(cb);
    },
  };
  vm.runInNewContext(viewerScript("LINK_INTERCEPTOR_JS"), {
    window,
    document: { addEventListener() {} },
    console: { warn },
    setTimeout,
    clearTimeout,
    performance,
    TextEncoder,
  });
  return {
    cs: window.codesense,
    posted,
    warn,
    deliver: (data: unknown) => listeners.forEach((cb) => cb({ source: parent, data })),
  };
}
afterEach(() => vi.useRealTimers());
it("reloadState resolves the host handoff once, including null on a cold load", async () => {
  const s = shim();
  const state = s.cs.reloadState();
  s.deliver({ type: "codesense:reload-state", state: { scroll: 120, draft: "typed" } });
  expect(await state).toEqual({ scroll: 120, draft: "typed" });
  s.deliver({ type: "codesense:reload-state", state: null });
  expect(await s.cs.reloadState()).toEqual({ scroll: 120, draft: "typed" });
  const cold = shim();
  cold.deliver({ type: "codesense:reload-state", state: null });
  expect(await cold.cs.reloadState()).toBeNull();
});
it("captures JSON through an opt-in hook and supports removing the hook", async () => {
  const s = shim();
  const off = s.cs.onBeforeReload(() => ({ scroll: 500 }));
  s.deliver({ type: "codesense:before-reload", id: 1 });
  await vi.waitFor(() =>
    expect(s.posted).toContainEqual(
      expect.objectContaining({
        type: "codesense:before-reload-result",
        id: 1,
        state: { scroll: 500 },
      }),
    ),
  );
  off();
  s.deliver({ type: "codesense:before-reload", id: 2 });
  expect(s.posted).toContainEqual(
    expect.objectContaining({ type: "codesense:before-reload-result", id: 2, state: null }),
  );
});
it.each(["oversize", "late"])(
  "drops and warns for a %s snapshot, without waiting on the hook",
  async (mode) => {
    vi.useFakeTimers();
    const s = shim();
    s.cs.onBeforeReload(() =>
      mode === "oversize" ? { text: "é".repeat(32768) } : new Promise(() => {}),
    );
    s.deliver({ type: "codesense:before-reload", id: 1 });
    await vi.advanceTimersByTimeAsync(101);
    expect(s.warn).toHaveBeenCalledWith(
      expect.stringContaining(mode === "oversize" ? "64" : "100"),
    );
    expect(s.posted).toContainEqual(
      expect.objectContaining({
        type: "codesense:before-reload-result",
        id: 1,
        state: null,
        warning: expect.any(String),
      }),
    );
  },
);
