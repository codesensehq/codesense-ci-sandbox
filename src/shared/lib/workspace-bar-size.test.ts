// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import {
  DEFAULT_WORKSPACE_BAR_SIZE,
  measuredZoom,
  resolveWorkspaceBarSize,
  workspaceBarZoom,
} from "./workspace-bar-size";

describe("workspace bar size", () => {
  it("defaults to the larger, original bar (Normal = the size before #2094)", () => {
    expect(DEFAULT_WORKSPACE_BAR_SIZE).toBe("normal");
    expect(workspaceBarZoom("normal")).toBe(1);
  });

  it("Small and Large are a step either side (14px and 20px labels): one factor scales the whole bar", () => {
    expect(workspaceBarZoom("small")).toBe(0.875);
    expect(workspaceBarZoom("large")).toBe(1.25);
  });

  it("an unknown or missing persisted value falls back to Normal", () => {
    expect(resolveWorkspaceBarSize(undefined)).toBe("normal");
    expect(resolveWorkspaceBarSize("huge")).toBe("normal");
    expect(resolveWorkspaceBarSize(3)).toBe("normal");
    expect(resolveWorkspaceBarSize("small")).toBe("small");
    expect(workspaceBarZoom("huge")).toBe(1);
  });

  it("measures an element's effective zoom from its drawn width over its layout width", () => {
    const el = document.createElement("div");
    Object.defineProperty(el, "offsetWidth", { get: () => 200 });
    el.getBoundingClientRect = () => ({ width: 150 }) as DOMRect;
    expect(measuredZoom(el)).toBe(0.75);
  });

  it("an element with no layout (unmounted, or a test DOM) measures as unscaled", () => {
    const el = document.createElement("div");
    expect(measuredZoom(el)).toBe(1);
    expect(measuredZoom(null)).toBe(1);
  });
});
