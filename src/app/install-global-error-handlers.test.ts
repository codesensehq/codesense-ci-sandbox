// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

const errorSpy = vi.fn<(tag: string, msg: string) => void>();
vi.mock("@/shared/lib/logger", () => ({
  logger: { error: (tag: string, msg: string) => errorSpy(tag, msg) },
}));

import {
  installGlobalErrorHandlers,
  unhandledRejectionCount,
} from "./install-global-error-handlers";

beforeEach(() => {
  errorSpy.mockReset();
});

describe("installGlobalErrorHandlers", () => {
  // NOTE: the module-level `installed` guard persists for the file's lifetime,
  // so this idempotency check must run first — it also performs the one-time
  // registration the later tests rely on.
  it("registers each window listener exactly once and is idempotent on repeat calls", () => {
    const addSpy = vi.spyOn(window, "addEventListener");
    installGlobalErrorHandlers();
    installGlobalErrorHandlers(); // second call must be a no-op (StrictMode/HMR)
    const errorAdds = addSpy.mock.calls.filter(([type]) => type === "error");
    const rejectionAdds = addSpy.mock.calls.filter(([type]) => type === "unhandledrejection");
    expect(errorAdds).toHaveLength(1);
    expect(rejectionAdds).toHaveLength(1);
    addSpy.mockRestore();
  });

  it("forwards window 'error' events to logger.error with message + location + stack", () => {
    window.dispatchEvent(
      new ErrorEvent("error", {
        message: "kaboom-abc",
        filename: "chunk.ts",
        lineno: 12,
        colno: 34,
        error: new Error("kaboom-abc"),
      }),
    );
    expect(errorSpy).toHaveBeenCalledWith("window.onerror", expect.stringContaining("kaboom-abc"));
    expect(errorSpy).toHaveBeenCalledWith(
      "window.onerror",
      expect.stringContaining("chunk.ts:12:34"),
    );
  });

  it("forwards unhandled promise rejections to logger.error", () => {
    // happy-dom doesn't construct PromiseRejectionEvent; synthesize the shape.
    const event = new Event("unhandledrejection") as PromiseRejectionEvent;
    Object.defineProperty(event, "reason", { value: new Error("rejected-xyz") });
    window.dispatchEvent(event);
    expect(errorSpy).toHaveBeenCalledWith(
      "unhandledrejection",
      expect.stringContaining("rejected-xyz"),
    );
  });

  it("serializes object rejection reasons instead of [object Object]", () => {
    const event = new Event("unhandledrejection") as PromiseRejectionEvent;
    Object.defineProperty(event, "reason", { value: { code: "E_SYNC", detail: "boom-obj" } });
    window.dispatchEvent(event);
    expect(errorSpy).toHaveBeenCalledWith(
      "unhandledrejection",
      expect.stringContaining("boom-obj"),
    );
    expect(errorSpy).not.toHaveBeenCalledWith("unhandledrejection", "[object Object]");
  });

  it("keeps a string fingerprint for an undefined rejection reason", () => {
    // JSON.stringify(undefined) === undefined — must not surface a non-string.
    const event = new Event("unhandledrejection") as PromiseRejectionEvent;
    Object.defineProperty(event, "reason", { value: undefined });
    window.dispatchEvent(event);
    expect(errorSpy).toHaveBeenCalledWith("unhandledrejection", "undefined");
  });

  // RF-perf-5 (AC-4): each unhandled rejection increments the count the heartbeat
  // forwards to the gated memory sampler.
  it("increments unhandledRejectionCount on each rejection", () => {
    const before = unhandledRejectionCount();
    const fire = () => {
      const event = new Event("unhandledrejection") as PromiseRejectionEvent;
      Object.defineProperty(event, "reason", { value: new Error("count-me") });
      window.dispatchEvent(event);
    };
    fire();
    fire();
    expect(unhandledRejectionCount()).toBe(before + 2);
  });
});
