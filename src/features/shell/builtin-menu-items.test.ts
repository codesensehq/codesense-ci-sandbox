// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
import { builtinDisabledReason, runBuiltinMenuItem, validateMenuItems } from "./builtin-menu-items";

it("uses the native reasons for ended and offline sessions", () => {
  expect(builtinDisabledReason("builtin:stop", { row: { alive: false } })).toBe(
    "This session has ended.",
  );
  expect(
    builtinDisabledReason("builtin:stop", { row: { alive: true }, offlineMachine: "Build Mac" }),
  ).toBe("Build Mac is offline: it can be stopped once it reconnects.");
  expect(builtinDisabledReason("builtin:stop", { row: { alive: true } })).toBeUndefined();
  expect(builtinDisabledReason("builtin:close-tab", { row: { alive: true }, tab: null })).toBe(
    "This session has no terminal tab.",
  );
});
it("dispatches trusted handles and tab ids through host confirmation", async () => {
  const run = vi.fn().mockResolvedValue({});
  const target = { row: { handle: "agent-handle" }, tab: { tab: { id: "tab-id" } } };
  await runBuiltinMenuItem("builtin:stop", target, run);
  await runBuiltinMenuItem("builtin:close-tab", target, run);
  expect(run.mock.calls).toEqual([
    ["stopSession", { handle: "agent-handle" }, { confirmInHost: true }],
    ["closeTerminalTab", { handle: "tab-id" }, { confirmInHost: true }],
  ]);
});
it("rejects unknown nested built-ins but ignores overrides of a trusted submenu", () => {
  expect(() => validateMenuItems([{ id: "custom", submenu: ["builtin:relaunch"] }])).toThrow(
    'unknown built-in "builtin:relaunch"',
  );
  expect(() =>
    validateMenuItems([
      { id: "builtin:set-role", label: "Override", submenu: ["builtin:relaunch"] },
    ]),
  ).not.toThrow();
});

it("recognizes the native file dialog actions without requiring a session row", () => {
  expect(() => validateMenuItems(["builtin:rename", "builtin:delete"])).not.toThrow();
  expect(builtinDisabledReason("builtin:rename", null)).toBeUndefined();
  expect(builtinDisabledReason("builtin:delete", null)).toBeUndefined();
});
