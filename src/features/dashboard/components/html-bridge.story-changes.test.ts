// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
const read = vi.hoisted(() => vi.fn().mockResolvedValue({ totals: { sessions: 2 } }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  storyChanges: read,
}));
import { HTML_BRIDGE_ALLOWLIST as bridge } from "./html-bridge";
it("storyChanges forwards story and frame identity to the Tauri reader", async () => {
  const result = await bridge.storyChanges({ storyId: "SV-29" }, {
    docPath: "/team/page.html",
    frame: { frameId: "child" },
  } as never);
  expect(read).toHaveBeenCalledWith({ storyId: "SV-29" }, "child", "/team/page.html");
  expect(result).toEqual({ totals: { sessions: 2 } });
});
it("storyChanges forwards commit paging arguments", async () => {
  await bridge.storyChanges({ storyId: "SV-29", limit: 1, cursor: "sc:fixture:sha" }, {
    docPath: "/team/page.html",
    frame: { frameId: "child" },
  } as never);
  expect(read).toHaveBeenCalledWith(
    { storyId: "SV-29", limit: 1, cursor: "sc:fixture:sha" },
    "child",
    "/team/page.html",
  );
});

it("storyChanges forwards only, and rejects an empty or unknown one", async () => {
  read.mockClear();
  await bridge.storyChanges({ storyId: "SV-29", only: ["tasks", "reviews"] });
  expect(read).toHaveBeenCalledWith(
    { storyId: "SV-29", only: ["tasks", "reviews"] },
    undefined,
    undefined,
  );
  for (const only of [[], ["nope"], "tasks", [1]]) {
    expect(() => bridge.storyChanges({ storyId: "SV-29", only })).toThrow(/invalid only/);
  }
  expect(read).toHaveBeenCalledTimes(1);
});

it("storyChanges forwards opt-in ranges", async () => {
  await bridge.storyChanges({ storyId: "SV-29", include: ["ranges"] });
  expect(read).toHaveBeenCalledWith(
    { storyId: "SV-29", include: ["ranges"] },
    undefined,
    undefined,
  );
});
