// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { terminalLayoutMemberSnapshots } from "./commands";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

it("preserves stamped live and historical member contracts for guarded C consumers", async () => {
  const privateSnapshot = { serverName: "private-sentinel" };
  const localSnapshot = { serverName: "local" };
  const replies = [
    { mode: "live", privateAuthorityEpoch: "1", snapshot: privateSnapshot },
    { mode: "historical", privateAuthorityEpoch: "1", frozen: { snapshot: privateSnapshot } },
    { mode: "live", snapshot: localSnapshot },
  ];
  vi.mocked(invoke).mockResolvedValue(replies);
  expect(await terminalLayoutMemberSnapshots("boot")).toEqual(replies);
});
