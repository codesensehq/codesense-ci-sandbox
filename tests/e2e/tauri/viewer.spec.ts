// SPDX-License-Identifier: MIT
import { test, expect } from "../../support/fixtures/tauri";

/**
 * Viewer E2E Tests (with Tauri IPC mocks)
 *
 * T.6: the file-tree / markdown-render tests required entering viewer
 * mode via the picker → repo-row click flow. With `RepoPickerView`
 * retired and the viewer now mounted per-team via `<TeamWorkspaceView
 * teamKey={tk} />`, reaching the viewer in e2e needs a full attach
 * + team-pick flow that's expensive to drive at this layer. Those
 * file-tree / markdown tests retire here and are covered at the unit
 * level by `markdown-viewer.test.tsx` + `team-workspace-view.test.tsx`.
 *
 * The surviving test asserts the global Cmd+, settings shortcut, which
 * still works from the empty state.
 *
 * Tags: @P1 @E2E @Tauri
 */
test.describe("@P1 @E2E @Tauri Viewer chrome", () => {
  test("settings panel opens on Cmd+, (works from team-workspace empty state)", async ({
    tauriApp,
  }) => {
    // T.6: the Cmd+, settings shortcut is global and fires regardless
    // of whether a workspace is focused (pre-T-6 it was gated on
    // selectedRepoPath).
    await tauriApp.keyboard.press("Meta+,");

    const settingsHeading = tauriApp.getByRole("heading", { name: "Settings" });
    await expect(settingsHeading).toBeVisible({ timeout: 5_000 });
    const generalHeading = tauriApp.getByRole("heading", { name: "General" });
    await expect(generalHeading).toBeVisible();
    const searchInput = tauriApp.getByPlaceholder("Search settings...");
    await expect(searchInput).toBeVisible();
  });
});
