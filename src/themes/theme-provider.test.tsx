// SPDX-License-Identifier: MIT
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppThemeProvider } from "./theme-provider";
import { AppearanceSection } from "@/features/settings/components/appearance-section";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { themeTokens } from "./index";
import { useTerminalThemeConfig } from "./use-terminal-theme";
vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "light", resolvedTheme: "light", setTheme: vi.fn() }),
}));
afterEach(() => {
  cleanup();
  document.documentElement.removeAttribute("style");
  document.getElementById("app-theme-values")?.remove();
  document.documentElement.removeAttribute("data-surface");
});
function TerminalProbe() {
  const config = useTerminalThemeConfig();
  return <output data-testid="terminal-background">{config.theme.background}</output>;
}
it("selects through Settings, applies native CSS, publishes once, and respects the terminal override", () => {
  useViewerStore.setState({
    settings: {
      ...useViewerStore.getState().settings,
      appTheme: "codesense",
      terminalTheme: "match-app",
    },
  });
  const changed = vi.fn();
  const consumer = liveHub.attach("provider-test", "another-team", () => {}, true, true, changed);
  render(
    <>
      <AppThemeProvider />
      <AppearanceSection query="" />
      <TerminalProbe />
    </>,
  );
  changed.mockClear();
  fireEvent.click(
    within(screen.getByRole("group", { name: "Theme" })).getByRole("radio", { name: "Nord" }),
  );
  expect(
    getComputedStyle(document.documentElement).getPropertyValue("--theme-surface-canvas-bg"),
  ).toBe(themeTokens("nord", "light")["--theme-surface-canvas-bg"]);
  expect(changed).toHaveBeenCalledTimes(1);
  expect(screen.getByTestId("terminal-background")).toHaveTextContent(
    themeTokens("nord", "light")["--theme-terminal-bg"],
  );
  fireEvent.click(
    within(screen.getByRole("group", { name: "Terminal colours" })).getByRole("radio", {
      name: "Dracula",
    }),
  );
  const bg = screen.getByTestId("terminal-background").textContent;
  fireEvent.click(
    within(screen.getByRole("group", { name: "Theme" })).getByRole("radio", { name: "GitHub" }),
  );
  expect(screen.getByTestId("terminal-background")).toHaveTextContent(bg!);
  expect(
    getComputedStyle(document.documentElement).getPropertyValue("--theme-surface-terminal-bg"),
  ).toBe(bg);
  consumer.dispose();
  act(() =>
    useViewerStore.setState({
      settings: {
        ...useViewerStore.getState().settings,
        appTheme: "codesense",
        terminalTheme: "match-app",
      },
    }),
  );
});
