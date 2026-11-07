// SPDX-License-Identifier: MIT
import { afterEach, expect, it } from "vitest";
import { registerTerminalSection, useTerminalSectionRegistry } from "./terminal-section-registry";
import { registerRightPaneHost, useRightPaneHostRegistry } from "./right-pane-host-registry";

afterEach(() => {
  useTerminalSectionRegistry.setState({ section: undefined });
  useRightPaneHostRegistry.setState({ hosts: {} });
});

it("the terminal section has one slot and obsolete cleanup cannot unregister its replacement", () => {
  const old = document.createElement("div");
  const next = document.createElement("div");
  const unregisterOld = registerTerminalSection(old);
  const unregisterNext = registerTerminalSection(next);
  expect(useTerminalSectionRegistry.getState().section).toBe(next);
  unregisterOld();
  expect(useTerminalSectionRegistry.getState().section).toBe(next);
  unregisterNext();
  expect(useTerminalSectionRegistry.getState().section).toBeUndefined();
});

it("right-pane hosts have one slot per team and obsolete cleanup preserves the current hosts", () => {
  const old = document.createElement("div");
  const next = document.createElement("div");
  const other = document.createElement("div");
  const unregisterOld = registerRightPaneHost("acme:alpha", old);
  const unregisterOther = registerRightPaneHost("acme:beta", other);
  const unregisterNext = registerRightPaneHost("acme:alpha", next);
  unregisterOld();
  expect(useRightPaneHostRegistry.getState().hosts).toEqual({
    "acme:alpha": next,
    "acme:beta": other,
  });
  unregisterNext();
  expect(useRightPaneHostRegistry.getState().hosts).toEqual({ "acme:beta": other });
  unregisterOther();
  expect(useRightPaneHostRegistry.getState().hosts).toEqual({});
});
