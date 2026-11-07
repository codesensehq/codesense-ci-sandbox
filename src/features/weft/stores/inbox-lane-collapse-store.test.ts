// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";

import { resolveLaneCollapsed, useInboxLaneCollapseStore } from "./inbox-lane-collapse-store";

describe("resolveLaneCollapsed", () => {
  it("uses the lane default when the user has never toggled it", () => {
    expect(resolveLaneCollapsed({}, "running", true)).toBe(true);
    expect(resolveLaneCollapsed({}, "needs-you", false)).toBe(false);
  });

  it("an explicit user choice overrides the default", () => {
    expect(resolveLaneCollapsed({ running: false }, "running", true)).toBe(false);
    expect(resolveLaneCollapsed({ "needs-you": true }, "needs-you", false)).toBe(true);
  });
});

describe("useInboxLaneCollapseStore", () => {
  beforeEach(() => {
    globalThis.localStorage?.clear();
    useInboxLaneCollapseStore.setState({ collapsed: {} });
  });

  it("records an explicit toggle and persists it across a fresh load", () => {
    useInboxLaneCollapseStore.getState().setCollapsed("done", true);
    expect(useInboxLaneCollapseStore.getState().collapsed.done).toBe(true);
    // Persisted to localStorage — a remount reads it back (survives unmount).
    const raw = globalThis.localStorage?.getItem("codesense.inbox-lane-collapse");
    expect(raw).toBeTruthy();
    expect(JSON.parse(raw as string).done).toBe(true);
  });
});
