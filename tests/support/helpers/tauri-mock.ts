// SPDX-License-Identifier: MIT
/**
 * Tauri invoke mock helper for Vitest integration tests.
 *
 * Usage in test files:
 *   import { createTauriMock, mockCommand } from "../support/helpers/tauri-mock";
 *
 *   beforeEach(() => {
 *     createTauriMock();
 *   });
 */
import { vi } from "vitest";

type CommandHandler = (...args: unknown[]) => unknown;

const commandHandlers = new Map<string, CommandHandler>();

/**
 * Sets up the Tauri invoke mock. Call in beforeEach.
 * All commands return undefined by default — use mockCommand() to override.
 */
export function createTauriMock() {
  commandHandlers.clear();

  vi.mock("@tauri-apps/api/core", () => ({
    invoke: vi.fn((cmd: string, args?: unknown) => {
      const handler = commandHandlers.get(cmd);
      if (handler) return Promise.resolve(handler(args));
      return Promise.resolve(undefined);
    }),
  }));
}

/**
 * Mock a specific Tauri command to return a value.
 *
 * @example
 *   mockCommand("check_path_exists", true);
 *   mockCommand("load_app_state", '{"repos":[]}');
 *   mockCommand("detect_tools", () => ({ terminals: [], editors: [] })); // lazy handler form
 */
export function mockCommand(command: string, returnValue: unknown | CommandHandler) {
  if (typeof returnValue === "function") {
    commandHandlers.set(command, returnValue as CommandHandler);
  } else {
    commandHandlers.set(command, () => returnValue);
  }
}

/**
 * Mock a command to reject with an error.
 */
export function mockCommandError(command: string, errorMessage: string) {
  commandHandlers.set(command, () => {
    throw new Error(errorMessage);
  });
}
