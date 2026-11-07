// SPDX-License-Identifier: MIT
import { expect, test } from "../support/fixtures/tauri";

test.describe("@P1 @E2E App Launch", () => {
  test("loads the assembled app into the team landing without the root crash fallback", async ({
    tauriApp,
    tauriPageErrors,
  }) => {
    await expect(tauriApp).toHaveTitle("CodeSense");
    await expect(tauriApp.getByText(/^Open a file from the sidebar/)).toBeVisible();
    await expect(tauriApp.getByRole("button", { name: /^Test Workspace/ })).toBeVisible();
    await expect(tauriApp.getByRole("heading", { name: "Something went wrong" })).toHaveCount(0);
    expect(tauriPageErrors()).toEqual([]);
  });
});
