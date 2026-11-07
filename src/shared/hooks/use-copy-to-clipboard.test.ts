// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useCopyToClipboard } from "./use-copy-to-clipboard";

const toastError = vi.fn();
vi.mock("sonner", () => ({ toast: { error: (...args: unknown[]) => toastError(...args) } }));

describe("useCopyToClipboard", () => {
  const writeText = vi.fn<(t: string) => Promise<void>>();

  beforeEach(() => {
    writeText.mockReset().mockResolvedValue(undefined);
    toastError.mockReset();
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
      writable: true,
    });
  });

  it("writes the value and exposes it as copiedValue", async () => {
    const { result } = renderHook(() => useCopyToClipboard());
    act(() => result.current.handleCopy("hello"));
    expect(writeText).toHaveBeenCalledWith("hello");
    await waitFor(() => expect(result.current.copiedValue).toBe("hello"));
  });

  it("does not throw and reports failure when the Clipboard API is unavailable", () => {
    Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
    const { result } = renderHook(() => useCopyToClipboard());
    expect(() => act(() => result.current.handleCopy("x"))).not.toThrow();
    expect(toastError).toHaveBeenCalled();
    expect(result.current.copiedValue).toBeNull();
  });

  it("reports failure and leaves copiedValue null when the write rejects", async () => {
    writeText.mockRejectedValue(new Error("not focused"));
    const { result } = renderHook(() => useCopyToClipboard());
    act(() => result.current.handleCopy("x"));
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(result.current.copiedValue).toBeNull();
  });
});
