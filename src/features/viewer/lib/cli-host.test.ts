// SPDX-License-Identifier: MIT
import { afterEach, expect, it } from "vitest";
import { cliHostRoot } from "./cli-host";

afterEach(() => {
  document.body.innerHTML = "";
});
it("returns no focus root before the CLI mounts, even with a quick-view terminal", () => {
  document.body.innerHTML =
    '<div data-agent-row-popup><div class="xterm"><textarea></textarea></div></div>';
  expect(cliHostRoot()).toBeNull();
});
it("returns only the CLI host when another terminal is elsewhere", () => {
  document.body.innerHTML =
    '<div class="xterm"><textarea></textarea></div><div data-xterm-host></div>';
  expect(cliHostRoot()).toBe(document.querySelector("[data-xterm-host]"));
});
