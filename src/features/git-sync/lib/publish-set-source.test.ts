// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import { VENDOR_NAME_RE } from "@/shared/components/supervised-agent-session";

import {
  PUBLISH_SET_FIXTURE,
  GS_STATUS_FIXTURE,
  GS_STATUS_WAITING_CHECKS_FIXTURE,
  INCOMING_FIXTURE,
} from "./fixtures";
import type { PublishSet } from "./types";

const invokeGetPublishSet = vi.fn();
vi.mock("@/tauri/commands", () => ({
  getPublishSet: (...args: unknown[]) => invokeGetPublishSet(...args),
}));

import { getPublishSet } from "./publish-set-source";

const EMPTY: PublishSet = { buckets: [], general: { count: 0, files: [] } };

describe("getPublishSet seam (SW-GS-2 — repointed at the real command)", () => {
  beforeEach(() => {
    invokeGetPublishSet.mockReset();
  });

  it("fetches over the explicitly-supplied org root", async () => {
    invokeGetPublishSet.mockResolvedValue(PUBLISH_SET_FIXTURE);
    const set = await getPublishSet("/org/root");
    // SW-GS-19/13: workspaceRef stays undefined; the trailing prNumber +
    // commitSha + pathPrefix are undefined for the unscoped working-tree source.
    expect(invokeGetPublishSet).toHaveBeenCalledWith(
      "/org/root",
      undefined,
      undefined,
      undefined,
      undefined,
    );
    expect(set).toBe(PUBLISH_SET_FIXTURE);
  });

  it("threads a team pathPrefix through as the 5th arg", async () => {
    invokeGetPublishSet.mockResolvedValue(PUBLISH_SET_FIXTURE);
    await getPublishSet("/org/root", undefined, undefined, "_team/default/content");
    expect(invokeGetPublishSet).toHaveBeenCalledWith(
      "/org/root",
      undefined,
      undefined,
      undefined,
      "_team/default/content",
    );
  });

  it.each([null, undefined, ""])(
    "returns empty WITHOUT a store-scan fallback when org root is %p",
    async (root) => {
      const set = await getPublishSet(root);
      expect(set).toEqual(EMPTY);
      expect(invokeGetPublishSet).not.toHaveBeenCalled();
    },
  );
});

describe("agent-agnostic labels (data side)", () => {
  it("no vendor string appears in any fixture label/branch/status", () => {
    const haystack = JSON.stringify([
      PUBLISH_SET_FIXTURE,
      GS_STATUS_FIXTURE,
      GS_STATUS_WAITING_CHECKS_FIXTURE,
      INCOMING_FIXTURE,
    ]);
    expect(VENDOR_NAME_RE.test(haystack)).toBe(false);
  });
});
