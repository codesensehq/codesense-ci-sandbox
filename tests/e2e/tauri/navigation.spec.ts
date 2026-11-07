// SPDX-License-Identifier: MIT
import { expect, test } from "../../support/fixtures/tauri";

const COMMAND_PALETTE_PLACEHOLDER = "Jump to a team, repo, or epic — or run an action…";

test.describe("@P1 @E2E @Tauri Navigation", () => {
  test("Cmd+Shift+P opens the global command palette", async ({ tauriApp }) => {
    await tauriApp.keyboard.press("Meta+Shift+KeyP");
    await expect(tauriApp.getByPlaceholder(COMMAND_PALETTE_PLACEHOLDER)).toBeVisible();
    await expect(tauriApp.getByTestId("quick-workspace-dialog")).toHaveCount(0);
  });

  test("Escape closes the command palette", async ({ tauriApp }) => {
    await tauriApp.keyboard.press("Meta+Shift+KeyP");
    const palette = tauriApp.getByPlaceholder(COMMAND_PALETTE_PLACEHOLDER);
    await expect(palette).toBeVisible();

    await tauriApp.keyboard.press("Escape");
    await expect(palette).toBeHidden();
  });
});
