// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

const toastSuccess = vi.fn();
const toastError = vi.fn();
vi.mock("sonner", () => ({
  toast: {
    success: (...a: unknown[]) => toastSuccess(...a),
    error: (...a: unknown[]) => toastError(...a),
  },
}));

import { toastWithUndo, runUndo } from "./undo-toast";

beforeEach(() => {
  toastSuccess.mockClear();
  toastError.mockClear();
});

describe("toastWithUndo (SW-SB-16)", () => {
  it("fires a success toast whose Undo action invokes the callback", () => {
    const undo = vi.fn();
    toastWithUndo("Moved to Done", undo);
    expect(toastSuccess).toHaveBeenCalledTimes(1);
    expect(toastSuccess.mock.calls[0][0]).toBe("Moved to Done");
    const opts = toastSuccess.mock.calls[0][1] as {
      action: { label: string; onClick: () => void };
    };
    expect(opts.action.label).toBe("Undo");
    opts.action.onClick();
    expect(undo).toHaveBeenCalledTimes(1);
  });
});

describe("runUndo (SW-SB-16)", () => {
  it("reverts then refreshes on success (no error toast)", async () => {
    const refresh = vi.fn();
    await runUndo(() => Promise.resolve(), refresh, "Couldn't undo");
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(toastError).not.toHaveBeenCalled();
  });

  it("toasts the error and never throws when the revert rejects", async () => {
    const refresh = vi.fn();
    await expect(
      runUndo(() => Promise.reject(new Error("read-only")), refresh, "Couldn't undo"),
    ).resolves.toBeUndefined();
    expect(refresh).not.toHaveBeenCalled();
    expect(toastError).toHaveBeenCalledTimes(1);
    expect(String(toastError.mock.calls[0][0])).toContain("read-only");
  });
});
