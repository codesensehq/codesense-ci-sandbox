// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

const { commandsRegister, setActive, commandsList, commandsRun } = vi.hoisted(() => ({
  commandsRegister: vi.fn(),
  setActive: vi.fn(),
  commandsList: vi.fn(),
  commandsRun: vi.fn(),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  commandsRegister: (...args: unknown[]) => commandsRegister(...args),
  commandsList: (...args: unknown[]) => commandsList(...args),
  commandsRun: (...args: unknown[]) => commandsRun(...args),
}));
vi.mock("../lib/dashboard-commands", () => ({
  setActiveDashboardCommands: (...args: unknown[]) => setActive(...args),
}));

import type { FrameRecord } from "@/tauri/commands";
import { runBridgeCommand } from "./html-bridge";

const frame = { origin: "http://f-1.localhost:9", token: "t" } as FrameRecord;

describe("commands.register", () => {
  it("sets the calling dashboard's active set from what Rust accepted", async () => {
    commandsRegister.mockResolvedValue({ dashboard: "dashboard:team:a:b/inbox", active: ["next"] });
    await expect(
      runBridgeCommand("commands.register", { ids: ["next"] }, { frame }),
    ).resolves.toEqual({ active: ["next"] });
    expect(commandsRegister).toHaveBeenCalledWith(frame.origin, frame.token, ["next"]);
    expect(setActive).toHaveBeenCalledWith("dashboard:team:a:b/inbox", ["next"]);
  });

  it("rejects an undeclared id with Rust's error and changes nothing", async () => {
    setActive.mockClear();
    commandsRegister.mockRejectedValue('undeclared command "delete"');
    await expect(
      runBridgeCommand("commands.register", { ids: ["delete"] }, { frame }),
    ).rejects.toBe('undeclared command "delete"');
    expect(setActive).not.toHaveBeenCalled();
  });

  it("requires a list of ids", async () => {
    await expect(runBridgeCommand("commands.register", { ids: "next" }, { frame })).rejects.toThrow(
      "ids must be an array",
    );
  });
});

describe("commands.list and commands.run", () => {
  it("answer from the host registry as the person", async () => {
    const rows = [{ id: "d/archive", label: "Archive", humanOnly: true }];
    commandsList.mockResolvedValue(rows);
    await expect(runBridgeCommand("commands.list", {}, { frame })).resolves.toBe(rows);
    commandsRun.mockResolvedValue(undefined);
    await expect(
      runBridgeCommand("commands.run", { id: "d/archive", args: { n: 1 } }, { frame }),
    ).resolves.toEqual({ requested: true });
    expect(commandsRun).toHaveBeenCalledWith("d/archive", { n: 1 });
  });

  it("needs a command id and passes the host's refusal through", async () => {
    await expect(runBridgeCommand("commands.run", {}, { frame })).rejects.toThrow(
      "id must be a command id",
    );
    commandsRun.mockRejectedValue('unavailable: "d/archive" isn\'t active');
    await expect(runBridgeCommand("commands.run", { id: "d/archive" }, { frame })).rejects.toBe(
      'unavailable: "d/archive" isn\'t active',
    );
  });
});
