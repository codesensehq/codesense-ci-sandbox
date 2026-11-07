// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
const read = vi.hoisted(() =>
  vi
    .fn()
    .mockResolvedValue({ stories: {}, reasons: [], pending: [], indexing: { done: 0, total: 0 } }),
);
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  storyTotals: read,
}));
import { HTML_BRIDGE_ALLOWLIST as bridge } from "./html-bridge";
it("SV-PBF-287: storyTotals forwards the ids and frame identity to the Tauri reader", async () => {
  const result = await bridge.storyTotals({ storyIds: ["SV-29", "SV-30"] }, {
    docPath: "/team/page.html",
    frame: { frameId: "child" },
  } as never);
  expect(read).toHaveBeenCalledWith({ storyIds: ["SV-29", "SV-30"] }, "child", "/team/page.html");
  expect(result).toEqual({
    stories: {},
    reasons: [],
    pending: [],
    indexing: { done: 0, total: 0 },
  });
});
it("SV-PBF-287: storyTotals refuses unknown fields and ids that are not strings", async () => {
  expect(() => bridge.storyTotals({ storyIds: ["SV-29"], limit: 5 })).toThrow(
    "storyTotals: unknown field",
  );
  expect(() => bridge.storyTotals({ storyIds: "SV-29" })).toThrow(
    "storyTotals: storyIds must be a list of story ids",
  );
  expect(() => bridge.storyTotals({ storyIds: [29] })).toThrow(
    "storyTotals: storyIds must be a list of story ids",
  );
});
