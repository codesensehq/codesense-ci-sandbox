// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
const commands = vi.hoisted(() => ({
  routineResult: vi.fn(),
  routineRunNow: vi.fn(),
  routineCancelResult: vi.fn(),
}));
vi.mock("@/tauri/commands", () => commands);
import {
  HTML_BRIDGE_ALLOWLIST,
  isReadActionVerb,
  isSpawnVerb,
  isManagementVerb,
} from "./html-bridge";

describe("routine dashboard bridge", () => {
  beforeEach(() => vi.clearAllMocks());
  it("repeated cached reads never launch and forward their key", async () => {
    commands.routineResult.mockResolvedValue({ snapshot: null });
    for (let i = 0; i < 3; i++) await HTML_BRIDGE_ALLOWLIST.routineResult({ key: "a/b/c/d" });
    expect(commands.routineResult).toHaveBeenCalledTimes(3);
    expect(commands.routineResult).toHaveBeenCalledWith("a/b/c/d");
    expect(commands.routineRunNow).not.toHaveBeenCalled();
    expect(isReadActionVerb("routineResult")).toBe(true);
    expect(isReadActionVerb("routineStatus")).toBe(true);
  });
  it("explicit refresh forwards variables and cancellation forwards only routine key", async () => {
    await HTML_BRIDGE_ALLOWLIST.routineRefresh({ key: "a/b/c/d", variables: { repo: "example" } });
    expect(commands.routineRunNow).toHaveBeenCalledExactlyOnceWith("a/b/c/d", { repo: "example" });
    commands.routineRunNow.mockClear();
    await HTML_BRIDGE_ALLOWLIST.routineRefresh({ key: "a/b/c/d" }, {
      docPath: "/parent/shared.html",
      frame: { frameId: "child-frame" },
    } as never);
    expect(commands.routineRunNow).toHaveBeenCalledExactlyOnceWith(
      "a/b/c/d",
      undefined,
      "child-frame",
      "/parent/shared.html",
    );
    await HTML_BRIDGE_ALLOWLIST.routineCancel({ key: "a/b/c/d", runId: "untrusted" });
    expect(commands.routineCancelResult).toHaveBeenCalledExactlyOnceWith("a/b/c/d");
    expect(isSpawnVerb("routineRefresh")).toBe(true);
    expect(isManagementVerb("routineCancel")).toBe(true);
    expect(() =>
      HTML_BRIDGE_ALLOWLIST.routineRefresh({ key: "a/b/c/d", variables: { bad: 3 } }),
    ).toThrow();
  });
});
