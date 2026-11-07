// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useViewerStore } from "./viewer-store";
import { scrollbackLines } from "@/shared/terminal/terminal-constants";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  listNotes: vi.fn().mockResolvedValue([]),
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

import * as commands from "@/tauri/commands";

async function loadSettings(settings: Record<string, unknown>) {
  vi.mocked(commands.loadAppState).mockResolvedValue(
    JSON.stringify({ viewer: { schemaVersion: 8, settings } }),
  );
  await useViewerStore.getState().loadPersistedState();
  return useViewerStore.getState().settings;
}

/** The history length a new terminal gets from the loaded settings. */
const newTerminalScrollback = () =>
  scrollbackLines(useViewerStore.getState().settings.terminalScrollback);

describe("terminal history: default vs saved", () => {
  beforeEach(() => {
    useViewerStore.setState({ stateLoaded: false });
  });

  it("a settings file with no scrollback key opens new terminals at 5,000 lines", async () => {
    const settings = await loadSettings({ terminalTheme: "nord" });
    expect(settings.terminalScrollback).toBeUndefined();
    expect(newTerminalScrollback()).toBe(5_000);
  });

  it("a saved value is kept, 10,000 included", async () => {
    await loadSettings({ terminalScrollback: 10_000 });
    expect(newTerminalScrollback()).toBe(10_000);
  });

  it("the default is never written back as if the user had chosen it", async () => {
    await loadSettings({ terminalTheme: "nord" });
    vi.mocked(commands.saveAppState).mockClear();
    useViewerStore.getState().updateSettings({ terminalTheme: "dracula" });
    await vi.waitFor(() => expect(commands.saveAppState).toHaveBeenCalled());
    const saved = JSON.parse(vi.mocked(commands.saveAppState).mock.calls.at(-1)![0] as string);
    expect(saved.viewer.settings).not.toHaveProperty("terminalScrollback");
  });
});
