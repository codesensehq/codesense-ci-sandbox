// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { warmTerminalTabIds } from "./terminal-warm-tabs";

const tabs = (count: number) => Array.from({ length: count }, (_, i) => ({ id: String(i) }));

describe("warm terminal panes", () => {
  it("keeps current, neighbours, and last used, in stable tab order", () => {
    expect(warmTerminalTabIds(tabs(30), "15", "2", true)).toEqual(["2", "14", "15", "16"]);
  });
  it.each([
    [0, undefined, undefined, []],
    [1, "0", "0", ["0"]],
    [2, "0", "1", ["0", "1"]],
    [6, "0", "4", ["0", "1", "4"]],
    [6, "5", "1", ["1", "4", "5"]],
    [6, "3", "2", ["2", "3", "4"]],
    [6, "3", "closed", ["2", "3", "4"]],
    [6, "closed", "2", []],
  ] as const)(
    "handles %s tabs with selection %s and previous %s",
    (count, active, previous, expected) => {
      expect(warmTerminalTabIds(tabs(count), active, previous, true)).toEqual(expected);
    },
  );
  it("keeps no panes for hidden or minimized tiles", () => {
    expect(warmTerminalTabIds(tabs(30), "15", "2", false)).toEqual([]);
  });
  it("excludes offline neighbours and previous tabs but keeps an active offline notice", () => {
    const remote = [
      { id: "a", origin: { offlineSince: 1 } },
      { id: "b" },
      { id: "c", origin: { offlineSince: 2 } },
      { id: "d", origin: { offlineSince: 3 } },
    ];
    expect(warmTerminalTabIds(remote, "b", "d", true)).toEqual(["b"]);
    expect(warmTerminalTabIds(remote, "a", "d", true)).toEqual(["a", "b"]);
  });
});
