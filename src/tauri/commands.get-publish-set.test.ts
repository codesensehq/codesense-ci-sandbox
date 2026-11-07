// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { PublishSet } from "@/features/git-sync/lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import { getPublishSet } from "./commands";

describe("getPublishSet wrapper (SW-GS-2 AC-3)", () => {
  beforeEach(() => invokeMock.mockReset());

  it("invokes get_publish_set with camelCase args and returns the PublishSet", async () => {
    const fixture: PublishSet = {
      buckets: [],
      general: { count: 1, files: [{ path: "a.md", status: "modified" }] },
    };
    invokeMock.mockResolvedValue(fixture);

    const result = await getPublishSet("/org/root", "ws1");

    expect(invokeMock).toHaveBeenCalledWith("get_publish_set", {
      repoPath: "/org/root",
      workspaceRef: "ws1",
    });
    // The wrapper's return type IS SW-GS-1's locked view-model — single source.
    expect(result).toEqual(fixture);
  });

  it("omits workspaceRef when not supplied", async () => {
    invokeMock.mockResolvedValue({ buckets: [], general: { count: 0, files: [] } });
    await getPublishSet("/org/root");
    expect(invokeMock).toHaveBeenCalledWith("get_publish_set", {
      repoPath: "/org/root",
      workspaceRef: undefined,
    });
  });
});
