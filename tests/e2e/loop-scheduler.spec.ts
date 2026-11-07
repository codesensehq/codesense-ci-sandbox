// SPDX-License-Identifier: MIT
import { test, expect } from "@playwright/test";

/**
 * SW-GT-31 · Loop scheduler live tick — AC-5 motivating-case E2E (the GT-9 AC-2 proof).
 *
 * A review-phase `mode: loop` step watches the story's PR: on new input it dispatches its
 * repo-scoped fix prompt headless, pushes, re-checks, and exits when the validator reports
 * merged/approved — visible as a first-class worker (run monitor + node worker badge) with
 * live iteration state in the GT-11 gate drawer, leaving zero orphaned PTYs and no renderer
 * frame-time regression.
 *
 * DEFERRED — NOT LIVE-VERIFIED. This harness drives the Vite dev server WITHOUT the Tauri
 * backend (see app-launch.spec.ts), so it cannot start the scheduler thread, open a headless
 * workspace, or watch a real PR. Standing this up requires the full Tauri build + a real open
 * PR fixture; it is the one AC that could not ride the unit suite (exactly as GT-9 shipped its
 * trusted core with the E2E deferred). Left as `fixme` so the scenario is a tracked placeholder
 * the live pass fills in — the trusted core + FE render below it are unit-tested and green.
 */
test.describe("@P2 @E2E SW-GT-31 loop scheduler (deferred — needs Tauri backend + live PR)", () => {
  test.fixme("review-phase mode:loop watches the PR, dispatches fixes, and exits on validator pass", async ({
    page,
  }) => {
    // Live seam (requires the Tauri backend):
    //  1. Seed a review-phase `mode: loop` step and admit the story.
    //  2. Assert the scheduler seeds the arm and ticks: gate drawer shows "iteration N".
    //  3. Push a new PR comment → assert a headless fix dispatch (no macOS terminal).
    //  4. Make the validator report merged/approved → assert the loop exits, records a
    //     Passed verdict, and the story advances past the loop node (conjunction gate).
    //  5. Assert zero orphaned PTYs remain after exit.
    await page.goto("/");
    await expect(page.locator("#root")).toBeAttached();
  });
});
