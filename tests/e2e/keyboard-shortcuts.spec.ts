// SPDX-License-Identifier: MIT
import { expect, test } from "../support/fixtures/tauri";

const COMMAND_PALETTE_PLACEHOLDER = "Jump to a team, repo, or epic — or run an action…";

test.describe("@P1 @E2E Keyboard Shortcuts", () => {
  test("Cmd+Shift+P opens the command palette and Escape closes it", async ({ tauriApp }) => {
    await tauriApp.keyboard.press("Meta+Shift+KeyP");
    const palette = tauriApp.getByPlaceholder(COMMAND_PALETTE_PLACEHOLDER);
    await expect(palette).toBeVisible();

    await tauriApp.keyboard.press("Escape");
    await expect(palette).toBeHidden();
  });

  test("Cmd+, opens Settings and Cmd+W closes it", async ({ tauriApp }) => {
    await tauriApp.keyboard.press("Meta+Comma");
    const settings = tauriApp.getByTestId("settings-panel");
    await expect(settings).toBeVisible();

    await tauriApp.keyboard.press("Meta+KeyW");
    await expect(settings).toBeHidden();
  });

  test("rapid global shortcuts leave the app shell stable", async ({ tauriApp }) => {
    await tauriApp.keyboard.press("Meta+Shift+KeyP");
    await tauriApp.keyboard.press("Escape");
    await tauriApp.keyboard.press("Meta+Comma");
    await tauriApp.keyboard.press("Escape");

    await expect(tauriApp.getByRole("button", { name: /^Test Workspace/ })).toBeVisible();
    // Escape does not close the Settings tab (Cmd+W does), so the shell is left
    // showing a working Settings view, not an error boundary.
    await expect(tauriApp.getByTestId("settings-panel")).toBeVisible();
    await expect(tauriApp.getByRole("heading", { name: "Something went wrong" })).toHaveCount(0);
  });
});
