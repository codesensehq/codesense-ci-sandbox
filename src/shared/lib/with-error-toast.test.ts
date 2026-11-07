// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

const toastError = vi.fn();
vi.mock("sonner", () => ({
  toast: {
    error: (msg: string, opts?: unknown) => (opts ? toastError(msg, opts) : toastError(msg)),
  },
}));

const logWarn = vi.fn();
vi.mock("./logger", () => ({ logger: { warn: (...a: unknown[]) => logWarn(...a) } }));

import { SAFE_ERROR_REASON } from "./error-message";
import { toastError as toastErrorFn, toastFailure, withErrorToast } from "./with-error-toast";

beforeEach(() => {
  toastError.mockClear();
});

describe("toastError", () => {
  it("builds `${prefix}${message}` from an Error", () => {
    toastErrorFn("Failed to save: ", new Error("disk full"));
    expect(toastError).toHaveBeenCalledWith("Failed to save: disk full");
  });

  it("stringifies a non-Error thrown value", () => {
    toastErrorFn("Boom: ", "raw string");
    expect(toastError).toHaveBeenCalledWith("Boom: raw string");
  });
});

describe("withErrorToast", () => {
  it("toasts and swallows a rejection (no unhandled rejection)", async () => {
    // Returns void synchronously; the swallow happens on the microtask queue.
    withErrorToast(Promise.reject(new Error("nope")), "Failed: ");
    await Promise.resolve();
    await Promise.resolve();
    expect(toastError).toHaveBeenCalledWith("Failed: nope");
  });

  it("does not toast on a resolved promise", async () => {
    withErrorToast(Promise.resolve("ok"), "Failed: ");
    await Promise.resolve();
    await Promise.resolve();
    expect(toastError).not.toHaveBeenCalled();
  });

  it("returns void (fire-and-forget, not a chainable promise)", () => {
    const ret = withErrorToast(Promise.resolve(), "Failed: ");
    expect(ret).toBeUndefined();
  });
});

describe("toastFailure (SW-FU-B8)", () => {
  it("says what failed as the title and a safe reason as the description", () => {
    toastFailure("Couldn't track acme/web", new Error("invalid: bad slug"));
    expect(toastError).toHaveBeenCalledWith("Couldn't track acme/web", { description: "bad slug" });
  });

  it("never shows raw error text, and logs it (the safe reason points at the log)", () => {
    toastFailure("Couldn't back up", new Error("failed to open: (os error 2)"));
    expect(toastError).toHaveBeenCalledWith("Couldn't back up", { description: SAFE_ERROR_REASON });
    expect(logWarn).toHaveBeenCalledWith(
      "error",
      expect.stringContaining("Couldn't back up: failed to open: (os error 2)"),
    );
  });

  it("offers one next action when given", () => {
    const onClick = vi.fn();
    toastFailure("Couldn't back up", "Network error", { label: "Try again", onClick });
    expect(toastError).toHaveBeenCalledWith("Couldn't back up", {
      description: "Network error",
      action: { label: "Try again", onClick },
    });
  });

  it("toastError reads the reason through the safe path", () => {
    toastErrorFn("Failed: ", new Error("x::y broke"));
    expect(toastError).toHaveBeenCalledWith(`Failed: ${SAFE_ERROR_REASON}`);
  });
});
