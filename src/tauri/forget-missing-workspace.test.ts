// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
import { forgetMissingWorkspace } from "./commands";

const h = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: h.invoke }));

it("serializes exactly the target fields accepted by both removal endpoints", async () => {
  h.invoke.mockResolvedValue({ status: "removed" });
  const rowTarget = { kind: "remote" as const, machineId: "devbox", workspaceHandle: "gone" };
  expect(await forgetMissingWorkspace(rowTarget)).toEqual({ status: "removed" });
  expect(h.invoke).toHaveBeenCalledWith("forget_missing_workspace", {
    target: { machineId: "devbox", workspaceHandle: "gone" },
  });
});
