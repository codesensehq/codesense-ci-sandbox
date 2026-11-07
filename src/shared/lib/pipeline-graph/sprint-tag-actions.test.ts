// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@/tauri/commands", () => ({
  applyMetadataEdit: vi.fn(() => Promise.resolve()),
}));

const toastSuccess = vi.fn();
const toastError = vi.fn();
vi.mock("sonner", () => ({
  toast: {
    success: (...args: unknown[]) => toastSuccess(...args),
    error: (...args: unknown[]) => toastError(...args),
  },
}));

import { applyMetadataEdit } from "@/tauri/commands";
import { sprintToastLabel, applySprintChange, bulkTagStories } from "./sprint-tag-actions";

const applyMock = vi.mocked(applyMetadataEdit);

beforeEach(() => {
  applyMock.mockClear();
  applyMock.mockResolvedValue(undefined);
  toastSuccess.mockClear();
  toastError.mockClear();
});

describe("sprintToastLabel (pure)", () => {
  it("add (untagged → sprint) reads 'Added to …'", () => {
    expect(sprintToastLabel(null, "2026-08-A")).toBe("Added to 2026-08-A");
  });
  it("move (tagged → other sprint) reads 'Moved to …'", () => {
    expect(sprintToastLabel("2026-07-B", "2026-08-A")).toBe("Moved to 2026-08-A");
  });
  it("remove (tagged → null) reads 'Removed from sprint'", () => {
    expect(sprintToastLabel("2026-08-A", null)).toBe("Removed from sprint");
  });
});

describe("applySprintChange (single)", () => {
  it("writes, refreshes, and shows an undo toast whose action applies the inverse", async () => {
    const refresh = vi.fn();
    await applySprintChange("/repo/s.md", null, "2026-08-A", refresh);

    expect(applyMock).toHaveBeenCalledWith("/repo/s.md", [
      { kind: "setField", field: "sprint", value: "2026-08-A" },
    ]);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(toastSuccess).toHaveBeenCalledTimes(1);

    // Fire the toast's Undo action → applies the inverse (ClearField).
    const opts = toastSuccess.mock.calls[0][1] as { action: { onClick: () => void } };
    applyMock.mockClear();
    refresh.mockClear();
    opts.action.onClick();
    await Promise.resolve();
    await Promise.resolve();
    expect(applyMock).toHaveBeenCalledWith("/repo/s.md", [{ kind: "clearField", field: "sprint" }]);
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("on failure toasts an error and does NOT refresh (state unchanged)", async () => {
    const refresh = vi.fn();
    applyMock.mockRejectedValueOnce(new Error("read-only"));
    await applySprintChange("/repo/s.md", "2026-07-B", "2026-08-A", refresh);
    expect(refresh).not.toHaveBeenCalled();
    expect(toastSuccess).not.toHaveBeenCalled();
    expect(toastError).toHaveBeenCalledTimes(1);
  });
});

describe("bulkTagStories (epic)", () => {
  it("tags every entry, refreshes ONCE, and reports all succeeded", async () => {
    const refresh = vi.fn();
    const res = await bulkTagStories(
      [
        { filePath: "/a.md", prevSprint: null },
        { filePath: "/b.md", prevSprint: null },
      ],
      "2026-08-A",
      refresh,
    );
    expect(res.succeeded).toEqual(["/a.md", "/b.md"]);
    expect(res.failed).toEqual([]);
    expect(applyMock).toHaveBeenCalledTimes(2);
    expect(refresh).toHaveBeenCalledTimes(1); // one rescan at the end, not per-file
    expect(toastSuccess).toHaveBeenCalledTimes(1);
  });

  it("a story already in the target sprint is a no-op success (no write)", async () => {
    const refresh = vi.fn();
    const res = await bulkTagStories(
      [{ filePath: "/a.md", prevSprint: "2026-08-A" }],
      "2026-08-A",
      refresh,
    );
    expect(res.succeeded).toEqual(["/a.md"]);
    expect(applyMock).not.toHaveBeenCalled();
    // Nothing actually changed → no undo toast.
    expect(toastSuccess).not.toHaveBeenCalled();
  });

  it("aggregates partial failure honestly and never half-applies silently", async () => {
    const refresh = vi.fn();
    applyMock
      .mockResolvedValueOnce(undefined) // /a.md ok
      .mockRejectedValueOnce(new Error("locked")) // /b.md fails
      .mockResolvedValueOnce(undefined); // /c.md ok
    const res = await bulkTagStories(
      [
        { filePath: "/a.md", prevSprint: null },
        { filePath: "/b.md", prevSprint: null },
        { filePath: "/c.md", prevSprint: null },
      ],
      "2026-08-A",
      refresh,
    );
    expect(res.succeeded).toEqual(["/a.md", "/c.md"]);
    expect(res.failed).toEqual([{ filePath: "/b.md", error: "locked" }]);
    expect(refresh).toHaveBeenCalledTimes(1);
    // Summary toast names the failure count; undo reverts only the 2 that changed.
    expect(toastSuccess).toHaveBeenCalledTimes(1);
    const opts = toastSuccess.mock.calls[0][1] as { action: { onClick: () => void } };
    applyMock.mockClear();
    opts.action.onClick();
    await Promise.resolve();
    expect(applyMock).toHaveBeenCalledTimes(2); // only /a.md + /c.md reverted
  });

  it("all-failed toasts an error and returns no successes", async () => {
    const refresh = vi.fn();
    applyMock.mockRejectedValue(new Error("nope"));
    const res = await bulkTagStories(
      [{ filePath: "/a.md", prevSprint: null }],
      "2026-08-A",
      refresh,
    );
    expect(res.succeeded).toEqual([]);
    expect(refresh).not.toHaveBeenCalled();
    expect(toastError).toHaveBeenCalledTimes(1);
    expect(toastSuccess).not.toHaveBeenCalled();
  });
});
