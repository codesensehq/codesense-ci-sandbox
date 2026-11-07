// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { migrateThemeSettings } from "./settings";
it("maps untouched defaults to CodeSense and drops retired keys", () => {
  expect(
    migrateThemeSettings({ viewerTheme: "dracula", editorTheme: "default", contentWidth: "wide" }),
  ).toEqual({ appTheme: "codesense", contentWidth: "wide" });
});
it.each([
  ["github-dark", "github"],
  ["nord", "nord"],
  ["catppuccin", "catppuccin"],
  ["confluence", "confluence"],
])("carries explicit %s to %s", (old, next) => {
  expect(migrateThemeSettings({ viewerTheme: old }).appTheme).toBe(next);
});
it("preserves an app choice and editor choices", () => {
  expect(migrateThemeSettings({ appTheme: "github", viewerTheme: "nord" }).appTheme).toBe("github");
  expect(migrateThemeSettings({ viewerTheme: "dracula", editorTheme: "catppuccin" }).appTheme).toBe(
    "catppuccin",
  );
});
