// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

// Mock the helper before importing the SUT so module load picks up the mock.
const mockOpenAppSettings = vi.fn();
vi.mock("@/features/settings/lib/open-settings", () => ({
  openAppSettings: (...args: unknown[]) => mockOpenAppSettings(...args),
}));

// update-toast.tsx imports several Tauri commands and listens at module
// load — stub the surface so the import doesn't try to hit the IPC bridge.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@/tauri/commands", () => ({
  checkForUpdate: vi.fn().mockResolvedValue(null),
  downloadAndInstallUpdate: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), {
    error: vi.fn(),
    info: vi.fn(),
    success: vi.fn(),
    dismiss: vi.fn(),
    custom: vi.fn(),
  }),
}));

import { betaResendUrl, errorMessage, navigateToSettings } from "./update-toast";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

describe("update-toast — navigateToSettings (C.6d rewire)", () => {
  beforeEach(() => {
    mockOpenAppSettings.mockReset();
  });

  it("opens the Account section (re-auth flow lands where sign-in lives)", () => {
    navigateToSettings();
    expect(mockOpenAppSettings).toHaveBeenCalledTimes(1);
    expect(mockOpenAppSettings.mock.calls[0][0]).toBe("account");
  });

  it("forwards the clicked element as the focus-restore trigger (AC-11)", () => {
    const button = document.createElement("button");
    document.body.appendChild(button);
    navigateToSettings(button);
    expect(mockOpenAppSettings).toHaveBeenCalledTimes(1);
    expect(mockOpenAppSettings.mock.calls[0][1]).toBe(button);
    document.body.removeChild(button);
  });
});

describe("update-toast — errors are never a dead end (SW-PB-13 AC-6)", () => {
  const internal = { channel: "internal" as const, publicHost: "codesense.ai" };
  const beta = { channel: "beta" as const, publicHost: "codesense.ai" };
  const staging = { channel: "beta-staging" as const, publicHost: "staging.example.invalid" };

  it("internal builds show the error only", () => {
    expect(errorMessage({ kind: "network", message: "timeout" }, internal)).toBe(
      "Could not download the update. Check your connection and try again.",
    );
    expect(errorMessage({ kind: "unknown", message: "x" }, internal)).toBe(
      "Something went wrong. Try again.",
    );
  });

  it("beta builds point to the resend page on the channel's public host", () => {
    expect(betaResendUrl(beta)).toBe("https://codesense.ai/beta/resend");
    expect(errorMessage({ kind: "signature", message: "bad" }, beta)).toContain(
      "https://codesense.ai/beta/resend",
    );
    expect(errorMessage("boom", staging)).toContain("https://staging.example.invalid/beta/resend");
  });

  it("the dead releases link and its button are gone", () => {
    const source = readFileSync(resolve(__dirname, "update-toast.tsx"), "utf8");
    expect(source).not.toContain("/codesense/releases");
    expect(source).not.toContain("RELEASES_URL");
    expect(source).not.toContain("Download manually");
  });
});
