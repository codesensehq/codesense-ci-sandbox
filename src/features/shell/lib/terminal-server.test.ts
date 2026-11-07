// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

import type { IncompatibleTerminalServer } from "@/tauri/commands";
import {
  holdForTerminalServer,
  isTerminalServerRefusal,
  heldCopy,
  noticeCopy,
  resetTerminalServerForTests,
  restart,
  restartCopy,
  terminals,
  useTerminalServer,
} from "./terminal-server";

const commands = vi.hoisted(() => ({
  terminalServerStatus: vi.fn(),
  restartTerminalServer: vi.fn(),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  ...commands,
}));

describe("terminal-server copy", () => {
  it("counts terminals", () => {
    expect(terminals(1)).toBe("1 terminal");
    expect(terminals(3)).toBe("3 terminals");
  });

  it("names the restart's cost for every count", () => {
    expect(restartCopy(3)).toMatchObject({
      action: "Restart terminal server (closes 3 terminals)",
      destructive: true,
    });
    expect(restartCopy(1).action).toBe("Restart terminal server (closes 1 terminal)");
    expect(restartCopy(0)).toEqual({
      action: "Restart terminal server",
      destructive: false,
      sentence: "It isn't running any terminals, so nothing is lost.",
    });
    expect(restartCopy(null).action).toBe("Restart terminal server (closes its terminals)");
  });

  it("words the notice for every count", () => {
    expect(noticeCopy(3)).toBe(
      "The terminal server from another CodeSense version is still running your 3 terminals. Restart it to open new ones. This closes those 3.",
    );
    expect(noticeCopy(1)).toContain("your terminal.");
    expect(noticeCopy(0)).toBe(
      "The terminal server from another CodeSense version is still running. Restart it to open new terminals.",
    );
    expect(noticeCopy(null)).toContain("This closes them.");
  });

  it("a held terminal says it is waiting, and starts on its own once the server restarts", () => {
    expect(heldCopy(1)).toBe("This terminal will start once the terminal server restarts.");
    expect(heldCopy(2)).toBe("These 2 terminals will start once the terminal server restarts.");
  });
});

describe("a terminal refused by an incompatible terminal server", () => {
  const server: IncompatibleTerminalServer = {
    liveSessions: 2,
    serverProtocol: 4,
    appProtocol: 5,
    reason: "restart terminal server (ends 2 sessions): it speaks protocol 4, this app 5",
  };
  const current = () => {
    const { result } = renderHook(() => useTerminalServer());
    return result.current;
  };

  beforeEach(() => {
    vi.clearAllMocks();
    resetTerminalServerForTests();
  });

  it("is told apart from any other failure to open", () => {
    expect(isTerminalServerRefusal(server.reason)).toBe(true);
    expect(isTerminalServerRefusal(new Error("spawn failed: no such file"))).toBe(false);
    expect(isTerminalServerRefusal(undefined)).toBe(false);
  });

  it("re-offers the restart, and starts the terminal once it restarted", async () => {
    commands.terminalServerStatus.mockResolvedValue(server);
    commands.restartTerminalServer.mockResolvedValue(2);
    const retry = vi.fn();
    holdForTerminalServer(retry);
    await waitFor(() => expect(current()).toMatchObject({ server, open: true }));
    expect(retry).not.toHaveBeenCalled();
    await act(async () => {
      await restart();
    });
    expect(retry).toHaveBeenCalledOnce();
  });

  it("starts the terminal at once when the server was already restarted", async () => {
    commands.terminalServerStatus.mockResolvedValue(null);
    const retry = vi.fn();
    holdForTerminalServer(retry);
    await waitFor(() => expect(retry).toHaveBeenCalledOnce());
    expect(current().open).toBe(false);
  });

  it("tries the terminal again shortly when the status cannot be read", async () => {
    vi.useFakeTimers();
    commands.terminalServerStatus.mockRejectedValue(new Error("ipc"));
    const retry = vi.fn();
    holdForTerminalServer(retry);
    await vi.advanceTimersByTimeAsync(4999);
    expect(retry).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(retry).toHaveBeenCalledOnce();
    vi.useRealTimers();
  });

  it("never starts a terminal whose hold was cancelled", async () => {
    commands.terminalServerStatus.mockResolvedValue(server);
    commands.restartTerminalServer.mockResolvedValue(null);
    const retry = vi.fn();
    const cancel = holdForTerminalServer(retry);
    await waitFor(() => expect(current().open).toBe(true));
    cancel();
    await act(async () => {
      await restart();
    });
    expect(retry).not.toHaveBeenCalled();
  });
});
