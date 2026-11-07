// SPDX-License-Identifier: MIT
import { expect, test } from "../../support/fixtures/tauri";

test.describe("@P1 @E2E @Tauri Launch floor", () => {
  test("authenticated launch reaches the team landing floor", async ({ tauriApp }) => {
    // SV-PLG-46: an authenticated launch opens the active team's own view straight away, source
    // repo or not (the store write opens it; no empty-state commit). The "Add a source repo to …"
    // empty state is only the floor for an active team that is not open.
    await expect(tauriApp.getByText(/^Open a file from the sidebar/)).toBeVisible();
    await expect(tauriApp.getByRole("button", { name: "Start an agent" })).toBeVisible();
    await expect(tauriApp.getByRole("heading", { name: /^Add a source repo to / })).toHaveCount(0);
    await expect(tauriApp.getByRole("button", { name: /^Test Workspace/ })).toBeVisible();
    await expect(tauriApp.getByRole("heading", { name: "Something went wrong" })).toHaveCount(0);
    await expect(tauriApp.getByTestId("status-bar-repo")).toHaveCount(0);
  });

  test("retired repository and workspace dialogs stay absent", async ({ tauriApp }) => {
    await tauriApp.keyboard.press("Meta+Shift+KeyP");
    await expect(tauriApp.getByTestId("quick-repo-dialog")).toHaveCount(0);
    await expect(tauriApp.getByTestId("quick-workspace-dialog")).toHaveCount(0);
  });
});
