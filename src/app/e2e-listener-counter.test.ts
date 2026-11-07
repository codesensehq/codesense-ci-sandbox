// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";
import {
  __resetListenerCounterForTest,
  installListenerCounter,
  liveListenerCount,
} from "./e2e-listener-counter";

// Patch a throwaway prototype, never the global EventTarget, so the counter
// under test cannot leak into other suites.
class FakeTarget extends EventTarget {}

afterEach(() => {
  __resetListenerCounterForTest();
});

describe("e2e-listener-counter", () => {
  it("counts adds up and removes down", () => {
    installListenerCounter(FakeTarget.prototype);
    const target = new FakeTarget();
    const handler = () => {};
    expect(liveListenerCount()).toBe(0);
    target.addEventListener("x", handler);
    target.addEventListener("y", handler);
    expect(liveListenerCount()).toBe(2);
    target.removeEventListener("x", handler);
    expect(liveListenerCount()).toBe(1);
  });

  it("never drops below zero on an unmatched remove", () => {
    installListenerCounter(FakeTarget.prototype);
    new FakeTarget().removeEventListener("nope", () => {});
    expect(liveListenerCount()).toBe(0);
  });

  it("is idempotent — a second install does not double-count", () => {
    installListenerCounter(FakeTarget.prototype);
    installListenerCounter(FakeTarget.prototype);
    const target = new FakeTarget();
    target.addEventListener("x", () => {});
    expect(liveListenerCount()).toBe(1);
  });

  it("exposes the read hook on globalThis for browser.execute sampling", () => {
    installListenerCounter(FakeTarget.prototype);
    const read = (globalThis as unknown as { __CODESENSE_E2E_LISTENERS__?: () => number })
      .__CODESENSE_E2E_LISTENERS__;
    expect(typeof read).toBe("function");
    new FakeTarget().addEventListener("x", () => {});
    expect(read?.()).toBe(1);
  });

  it("still forwards to the real listener (handler fires)", () => {
    installListenerCounter(FakeTarget.prototype);
    const target = new FakeTarget();
    let fired = false;
    target.addEventListener("ping", () => {
      fired = true;
    });
    target.dispatchEvent(new Event("ping"));
    expect(fired).toBe(true);
  });

  it("reset restores the original methods (no lingering wrap)", () => {
    const original = FakeTarget.prototype.addEventListener;
    installListenerCounter(FakeTarget.prototype);
    expect(FakeTarget.prototype.addEventListener).not.toBe(original);
    __resetListenerCounterForTest();
    expect(FakeTarget.prototype.addEventListener).toBe(original);
  });
});
