// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { resolveTerminalTheme } from "./terminal";
it("keeps explicit person palettes and resolves both family modes", () => {
  expect(resolveTerminalTheme("dracula", "codesense", "light")).toBe(
    resolveTerminalTheme("dracula", "nord", "dark"),
  );
  expect(resolveTerminalTheme("match-app", "codesense", "light").theme.background).toBe(
    resolveTerminalTheme("match-app", "codesense", "dark").theme.background,
  );
  expect(resolveTerminalTheme("match-app", "nord", "light").theme.background).not.toBe(
    resolveTerminalTheme("match-app", "nord", "dark").theme.background,
  );
});
