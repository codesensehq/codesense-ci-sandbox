// SPDX-License-Identifier: MIT
/**
 * SW-LN-35: the "Terminal shell" settings row — shows the detected login shell
 * and stores an override that is pushed to Rust on blur.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";

const { terminalShellInfo, setTerminalShellOverride } = vi.hoisted(() => ({
  terminalShellInfo: vi.fn(),
  setTerminalShellOverride: vi.fn(),
}));

vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "system", setTheme: vi.fn() }),
}));
vi.mock("@/tauri/commands", () => ({
  detectTools: vi.fn().mockResolvedValue({ editors: [], terminals: [] }),
  checkForUpdate: vi.fn().mockResolvedValue(null),
  terminalShellInfo,
  setTerminalShellOverride,
}));
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("0.0.0-test"),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));

import { GeneralSection } from "./general-section";

const detectedBash = {
  path: "/bin/bash",
  kind: "bash",
  source: "env",
  detectedPath: "/bin/bash",
  overridePath: null,
  overrideRejected: false,
  unsupportedShell: null,
};

describe("GeneralSection — terminal shell row (SW-LN-35)", () => {
  beforeEach(() => {
    terminalShellInfo.mockReset().mockResolvedValue(detectedBash);
    setTerminalShellOverride.mockReset();
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, terminalShell: undefined },
    });
  });

  it("shows the detected shell", async () => {
    render(<GeneralSection query="terminal shell" />);
    await waitFor(() =>
      expect(screen.getByTestId("settings-terminal-shell-status").textContent).toContain(
        "Detected /bin/bash",
      ),
    );
    const input = screen.getByTestId("settings-terminal-shell") as HTMLInputElement;
    expect(input.placeholder).toBe("Detected: /bin/bash");
    expect(input.value).toBe("");
  });

  it("stores the override on blur and pushes it to Rust", async () => {
    setTerminalShellOverride.mockResolvedValue({
      ...detectedBash,
      path: "/opt/homebrew/bin/fish",
      kind: "fish",
      source: "override",
      overridePath: "/opt/homebrew/bin/fish",
    });
    render(<GeneralSection query="terminal shell" />);
    const input = screen.getByTestId("settings-terminal-shell") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "  /opt/homebrew/bin/fish " } });
    fireEvent.blur(input);

    expect(useViewerStore.getState().settings.terminalShell).toBe("/opt/homebrew/bin/fish");
    await waitFor(() =>
      expect(setTerminalShellOverride).toHaveBeenCalledWith("/opt/homebrew/bin/fish"),
    );
    await waitFor(() =>
      expect(screen.getByTestId("settings-terminal-shell-status").textContent).toBe(
        "New terminals use /opt/homebrew/bin/fish.",
      ),
    );
  });

  it("says when the override is ignored", async () => {
    terminalShellInfo.mockResolvedValue({
      ...detectedBash,
      overridePath: "/usr/bin/nu",
      overrideRejected: true,
    });
    render(<GeneralSection query="terminal shell" />);
    await waitFor(() =>
      expect(screen.getByTestId("settings-terminal-shell-status").textContent).toBe(
        "/usr/bin/nu isn't an executable zsh, bash, or fish. New terminals use /bin/bash.",
      ),
    );
  });

  it("clearing the field returns to the detected shell", async () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, terminalShell: "/bin/bash" },
    });
    setTerminalShellOverride.mockResolvedValue(detectedBash);
    render(<GeneralSection query="terminal shell" />);
    const input = screen.getByTestId("settings-terminal-shell") as HTMLInputElement;
    expect(input.value).toBe("/bin/bash");
    fireEvent.change(input, { target: { value: "" } });
    fireEvent.blur(input);
    expect(useViewerStore.getState().settings.terminalShell).toBeUndefined();
    await waitFor(() => expect(setTerminalShellOverride).toHaveBeenCalledWith(undefined));
  });

  it("renders without a backend", () => {
    terminalShellInfo.mockImplementation(() => {
      throw new Error("no tauri");
    });
    render(<GeneralSection query="terminal shell" />);
    expect(screen.getByTestId("settings-terminal-shell-status").textContent).toContain(
      "login shell",
    );
  });
});

it("keeps a dirty draft when a committed setting arrives from outside", () => {
  useViewerStore.setState({
    settings: { ...useViewerStore.getState().settings, terminalShell: "/bin/zsh" },
  });
  render(<GeneralSection query="terminal shell" />);
  const input = screen.getByTestId("settings-terminal-shell");
  fireEvent.change(input, { target: { value: "Pending local input" } });
  act(() => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, terminalShell: "/bin/bash" },
    });
  });
  expect(input).toHaveValue("Pending local input");
});
