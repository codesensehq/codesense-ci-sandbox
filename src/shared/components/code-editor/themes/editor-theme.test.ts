// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { useViewerStore } from "@/shared/stores/viewer-store";
import { resolveAppTheme } from "./editor-theme";

describe("resolveAppTheme — folded light/dark resolver", () => {
  it("explicit prop overrides next-themes", () => {
    expect(resolveAppTheme("light", "dark")).toBe("light");
    expect(resolveAppTheme("dark", "light")).toBe("dark");
  });
  it("auto follows next-themes, defaulting to light", () => {
    expect(resolveAppTheme("auto", "dark")).toBe("dark");
    expect(resolveAppTheme("auto", "light")).toBe("light");
    expect(resolveAppTheme("auto", undefined)).toBe("light");
  });
});

describe("viewer-store app theme", () => {
  it("has one default and roundtrips the app preference", () => {
    expect(useViewerStore.getState().settings.appTheme).toBe("codesense");
    useViewerStore.getState().updateSettings({ appTheme: "dracula" });
    expect(useViewerStore.getState().settings.appTheme).toBe("dracula");
    useViewerStore.getState().updateSettings({ appTheme: "codesense" });
  });
});
