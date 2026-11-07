// SPDX-License-Identifier: MIT
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useViewerStore } from "@/shared/stores/viewer-store";
const { notify, dismiss, setMode } = vi.hoisted(() => ({
  notify: vi.fn(),
  dismiss: vi.fn(),
  setMode: vi.fn(),
}));
vi.mock("sonner", () => ({ toast: Object.assign(notify, { dismiss }) }));
vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "system", resolvedTheme: "light", setTheme: setMode }),
}));
import { AppearanceSection } from "./appearance-section";
beforeEach(() => {
  vi.useFakeTimers();
  notify.mockClear();
  dismiss.mockClear();
  useViewerStore.setState({
    settings: {
      ...useViewerStore.getState().settings,
      appTheme: "codesense",
      terminalTheme: "match-app",
    },
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
it("offers one named theme picker, Mode and person-wide terminal colours", () => {
  render(<AppearanceSection query="" />);
  expect(screen.getByRole("group", { name: "Theme" }).querySelectorAll("input")).toHaveLength(6);
  expect(screen.getByRole("radio", { name: "CodeSense" })).toBeChecked();
  fireEvent.click(screen.getByRole("radio", { name: "Dark" }));
  expect(setMode).toHaveBeenCalledWith("dark");
  fireEvent.click(
    within(screen.getByRole("group", { name: "Theme" })).getByRole("radio", { name: "Dracula" }),
  );
  expect(useViewerStore.getState().settings.appTheme).toBe("dracula");
  expect(screen.getByText("Changed")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("radio", { name: /Solarized Dark/ }));
  expect(useViewerStore.getState().settings.terminalTheme).toBe("solarized-dark");
  fireEvent.click(screen.getByRole("button", { name: "Reset to CodeSense" }));
  expect(useViewerStore.getState().settings.appTheme).toBe("codesense");
});
it("coalesces browsing into one Undo that restores the origin and focus", () => {
  render(<AppearanceSection query="theme" />);
  fireEvent.click(
    within(screen.getByRole("group", { name: "Theme" })).getByRole("radio", { name: "Nord" }),
  );
  act(() => vi.advanceTimersByTime(400));
  fireEvent.click(
    within(screen.getByRole("group", { name: "Theme" })).getByRole("radio", { name: "GitHub" }),
  );
  act(() => vi.advanceTimersByTime(699));
  expect(notify).not.toHaveBeenCalled();
  act(() => vi.advanceTimersByTime(1));
  expect(notify).toHaveBeenCalledTimes(1);
  fireEvent.keyDown(document, { key: "z", metaKey: true });
  expect(useViewerStore.getState().settings.appTheme).toBe("codesense");
  expect(screen.getByRole("radio", { name: "CodeSense" })).toHaveFocus();
});
it("does not consume text-edit Undo or lose the origin when browsing resumes", () => {
  render(
    <>
      <input aria-label="Search" />
      <AppearanceSection query="" />
    </>,
  );
  fireEvent.click(
    within(screen.getByRole("group", { name: "Theme" })).getByRole("radio", { name: "Nord" }),
  );
  act(() => vi.advanceTimersByTime(700));
  fireEvent.keyDown(screen.getByRole("textbox"), { key: "z", ctrlKey: true });
  expect(useViewerStore.getState().settings.appTheme).toBe("nord");
  act(() => vi.advanceTimersByTime(4500));
  fireEvent.click(
    within(screen.getByRole("group", { name: "Theme" })).getByRole("radio", { name: "GitHub" }),
  );
  act(() => vi.advanceTimersByTime(700));
  act(() => notify.mock.calls.slice(-1)[0]![1].action.onClick());
  expect(useViewerStore.getState().settings.appTheme).toBe("codesense");
});
it("searching Mode does not match theme cards", () => {
  render(<AppearanceSection query="mode" />);
  expect(screen.getByRole("group", { name: "Mode" })).toBeInTheDocument();
  expect(screen.queryByRole("group", { name: "Theme" })).toBeNull();
});
it("a retained hidden Appearance pane does not handle Undo", () => {
  const { container } = render(
    <div style={{ display: "none" }}>
      <AppearanceSection query="theme" />
    </div>,
  );
  fireEvent.click(container.querySelector('input[name="app-theme"][value="nord"]')!);
  act(() => vi.advanceTimersByTime(700));
  expect(useViewerStore.getState().settings.appTheme).toBe("nord");
  fireEvent.keyDown(document, { key: "z", metaKey: true });
  expect(useViewerStore.getState().settings.appTheme).toBe("nord");
});
it("a toast Undo clicked after the pane's team is hidden undoes the theme without moving focus into it", () => {
  const { container } = render(
    <div data-testid="team">
      <AppearanceSection query="theme" />
    </div>,
  );
  const outside = document.createElement("button");
  document.body.append(outside);
  fireEvent.click(container.querySelector('input[name="app-theme"][value="nord"]')!);
  act(() => vi.advanceTimersByTime(700));
  const action = notify.mock.calls.at(-1)![1].action.onClick as () => void;
  (screen.getByTestId("team") as HTMLElement).style.display = "none";
  outside.focus();
  act(() => action());
  expect(useViewerStore.getState().settings.appTheme).toBe("codesense");
  expect(document.activeElement).toBe(outside);
  outside.remove();
});
