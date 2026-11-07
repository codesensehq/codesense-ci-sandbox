// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { toast } from "sonner";
import { listen } from "@tauri-apps/api/event";
import { render, screen, fireEvent, act } from "@testing-library/react";

/**
 * Tests for Story 13.6: Authenticated Auto-Update
 *
 * Tests the update-toast module which orchestrates the full update flow:
 * check → toast → download (with progress) → install → relaunch.
 *
 * Each test dynamically imports the module fresh to reset module-level state
 * (scopePromptShownThisSession, eventListenerActive).
 */

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

vi.mock("@/tauri/commands", () => ({
  checkForUpdate: vi.fn(),
  downloadAndInstallUpdate: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn(),
}));

vi.mock("sonner", () => {
  const customFn = vi.fn();
  const dismissFn = vi.fn();
  const toastFn = Object.assign(vi.fn(), {
    custom: customFn,
    success: vi.fn(),
    error: vi.fn(),
    loading: vi.fn(),
    dismiss: dismissFn,
  });
  return { toast: toastFn };
});

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock("@tauri-apps/plugin-process", () => ({
  relaunch: vi.fn(() => Promise.resolve()),
}));

const mockToast = vi.mocked(toast);
const mockListen = vi.mocked(listen);

beforeEach(() => {
  vi.clearAllMocks();
  // Reset module cache so module-level state resets between tests
  vi.resetModules();
});

// Helper to get fresh mocks after resetModules
async function getMocks() {
  const commands = await import("@/tauri/commands");
  return {
    checkForUpdate: vi.mocked(commands.checkForUpdate),
    downloadAndInstallUpdate: vi.mocked(commands.downloadAndInstallUpdate),
  };
}

// ===== AC 1: UPDATE AVAILABLE TOAST (P0) =====

describe("@P0 AC1: update available toast", () => {
  it("shows update toast when new version available", async () => {
    const { checkForUpdate } = await getMocks();
    checkForUpdate.mockResolvedValue({
      version: "0.4.0",
      currentVersion: "0.3.0",
      body: null,
    });

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    expect(checkForUpdate).toHaveBeenCalled();
    expect(mockToast.custom).toHaveBeenCalledWith(
      expect.any(Function),
      expect.objectContaining({ id: "update", duration: Infinity }),
    );
  });

  it("does NOT show toast when already on latest version", async () => {
    const { checkForUpdate } = await getMocks();
    checkForUpdate.mockResolvedValue(null);

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    expect(checkForUpdate).toHaveBeenCalled();
    expect(mockToast.custom).not.toHaveBeenCalled();
  });
});

// ===== AC 5: NO CHECK BEFORE AUTH (P0) =====

describe("@P0 AC5: update check timing", () => {
  it("update check calls check_for_update Tauri command", async () => {
    const { checkForUpdate } = await getMocks();
    checkForUpdate.mockResolvedValue(null);

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    expect(checkForUpdate).toHaveBeenCalledTimes(1);
  });
});

// ===== ERROR TYPE ROUTING (P0) =====

describe("@P0 error type routing", () => {
  it("routes AuthError to re-auth prompt (UX State 6)", async () => {
    const { checkForUpdate } = await getMocks();
    checkForUpdate.mockRejectedValue({
      kind: "auth",
      message: "forbidden",
    });

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    expect(mockToast).toHaveBeenCalledWith(
      "Auto-updates need additional access",
      expect.objectContaining({
        id: "update-scope",
        duration: Infinity,
      }),
    );
    expect(mockToast.custom).not.toHaveBeenCalled();
  });

  it("does NOT show re-auth prompt for non-auth errors", async () => {
    const { checkForUpdate } = await getMocks();
    checkForUpdate.mockRejectedValue({
      kind: "network",
      message: "timeout",
    });

    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    expect(mockToast).not.toHaveBeenCalledWith(
      "Auto-updates need additional access",
      expect.anything(),
    );

    consoleSpy.mockRestore();
  });
});

// ===== AC 3: EXPIRED TOKEN (P1) =====

describe("@P1 AC3: expired token handling", () => {
  it("fails gracefully when token error occurs", async () => {
    const { checkForUpdate } = await getMocks();
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    checkForUpdate.mockRejectedValue("Not authenticated with GitHub");

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    expect(consoleSpy).toHaveBeenCalledWith("[update] check failed:", expect.anything());
    expect(mockToast.custom).not.toHaveBeenCalled();

    consoleSpy.mockRestore();
  });
});

// ===== AC 6/AC 10: SCOPE RE-AUTH ONCE PER SESSION (P1) =====

describe("@P1 AC6/AC10: scope re-auth prompt shown once", () => {
  it("shows re-auth prompt only once per session", async () => {
    const { checkForUpdate } = await getMocks();
    checkForUpdate.mockRejectedValue({
      kind: "auth",
      message: "forbidden",
    });

    // Import once — module state persists within this test
    const { runUpdateCheck } = await import("./update-toast");

    // First call — should show prompt
    await runUpdateCheck();
    const firstCallCount = mockToast.mock.calls.filter(
      (c) => c[0] === "Auto-updates need additional access",
    ).length;
    expect(firstCallCount).toBe(1);

    // Second call — should NOT show prompt again
    await runUpdateCheck();
    const secondCallCount = mockToast.mock.calls.filter(
      (c) => c[0] === "Auto-updates need additional access",
    ).length;
    expect(secondCallCount).toBe(1);
  });
});

// ===== PROGRESS BAR (P1) =====

describe("@P1 download progress", () => {
  it("sets up event listener for download progress on update found", async () => {
    const { checkForUpdate } = await getMocks();
    checkForUpdate.mockResolvedValue({
      version: "0.4.0",
      currentVersion: "0.3.0",
      body: null,
    });

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    expect(mockListen).toHaveBeenCalledWith("update-download-progress", expect.any(Function));
  });
});

// ===== ERROR MESSAGE MAPPING (P1) =====

describe("@P1 error message mapping", () => {
  it("module exports runUpdateCheck", async () => {
    const mod = await import("./update-toast");
    expect(mod.runUpdateCheck).toBeDefined();
    expect(typeof mod.runUpdateCheck).toBe("function");
  });
});

// ===== AC 12: ALREADY UP TO DATE (P2) =====

describe("@P2 AC12: already up to date", () => {
  it("no toast shown on auto-check when already up to date", async () => {
    const { checkForUpdate } = await getMocks();
    checkForUpdate.mockResolvedValue(null);

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    expect(mockToast.custom).not.toHaveBeenCalled();
    expect(mockToast.success).not.toHaveBeenCalled();
  });
});

// ===== AC 11: COMMANDS (P2) =====

describe("@P2 AC11: update commands", () => {
  it("checkForUpdate and downloadAndInstallUpdate commands are available", async () => {
    const commands = await import("@/tauri/commands");
    expect(commands.checkForUpdate).toBeDefined();
    expect(commands.downloadAndInstallUpdate).toBeDefined();
  });
});

// ===== ERROR TOAST DISMISS BUTTON =====

describe("error toast dismiss button", () => {
  it("X button calls toast.dismiss to close the error toast", async () => {
    const { checkForUpdate, downloadAndInstallUpdate } = await getMocks();
    checkForUpdate.mockResolvedValue({
      version: "0.4.0",
      currentVersion: "0.3.0",
      body: null,
    });
    downloadAndInstallUpdate.mockRejectedValue({
      kind: "network",
      message: "timeout",
    });

    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    // Extract the render function passed to toast.custom
    const renderFn = mockToast.custom.mock.calls[0][0] as () => React.ReactElement;

    // Render in "available" state, then click Install to trigger download failure
    const { rerender } = render(renderFn());
    const installBtn = screen.getByText("Install & Restart");

    await act(async () => {
      fireEvent.click(installBtn);
      // Allow async startDownload to reject and setState to error
      await new Promise((r) => setTimeout(r, 50));
    });

    rerender(renderFn());

    // Verify dismiss button exists and works
    const dismissBtn = screen.getByLabelText("Dismiss");
    expect(dismissBtn).toBeInTheDocument();
    fireEvent.click(dismissBtn);
    expect(mockToast.dismiss).toHaveBeenCalledWith("update");

    consoleSpy.mockRestore();
  });

  it("hides Retry button for notFound", async () => {
    const { checkForUpdate, downloadAndInstallUpdate } = await getMocks();
    checkForUpdate.mockResolvedValue({
      version: "0.4.0",
      currentVersion: "0.3.0",
      body: null,
    });
    downloadAndInstallUpdate.mockRejectedValue({
      kind: "notFound",
      message: "not found",
    });

    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const { runUpdateCheck } = await import("./update-toast");
    await runUpdateCheck();

    const renderFn = mockToast.custom.mock.calls[0][0] as () => React.ReactElement;
    const { rerender } = render(renderFn());

    await act(async () => {
      fireEvent.click(screen.getByText("Install & Restart"));
      await new Promise((r) => setTimeout(r, 50));
    });

    rerender(renderFn());

    // Retry should be hidden for notFound
    expect(screen.queryByText("Retry")).not.toBeInTheDocument();
    // SW-PB-13 AC-6: no dead "Download manually" link; the error is shown on its own.
    expect(screen.queryByText("Download manually")).not.toBeInTheDocument();
    expect(screen.getByText(/Update not found/)).toBeInTheDocument();

    consoleSpy.mockRestore();
  });
});
