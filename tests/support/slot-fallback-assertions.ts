// SPDX-License-Identifier: MIT
/**
 * What a slot shows when its dashboard never goes live: the standard strip over the built-in,
 * with the slot's shared view state. Shared by the chromium mock-app spec and the WebKit
 * conformance host spec, so both hosts are held to the same assertions.
 */
import { expect, type Page } from "@playwright/test";

export async function expectStandardFallback(
  page: Page,
  expected: {
    /** The dashboard title the strip names. */
    name: string;
    /** The slot's actual built-in surface. */
    builtIn?: string;
    /** Text the strip's Details shows. */
    detail: string | RegExp;
    /** The built-in's `ui:<slotId>` selection, when the test seeded one. */
    selection?: string;
    timeout?: number;
  },
) {
  const strip = page.getByTestId("slot-fallback-strip");
  await expect(strip).toBeVisible({ timeout: expected.timeout ?? 32_000 });
  await expect(strip).toContainText(
    `${expected.name} pane didn't load — showing the built-in ${expected.builtIn ?? "Inbox"}.`,
  );
  if (expected.selection !== undefined)
    await expect(page.getByTestId("native-selection")).toHaveText(expected.selection);
  await page.getByRole("button", { name: "Details", exact: true }).click();
  await expect(strip).toContainText(expected.detail);
}

/** A page that throws before it is live falls back at once, with its error, and no frame. */
export async function expectThrowOnLoadFallback(
  page: Page,
  expected: { name: string; message: string; selection?: string; builtIn?: string },
) {
  await expectStandardFallback(page, {
    name: expected.name,
    detail: "The page hit an error while loading.",
    builtIn: expected.builtIn,
    selection: expected.selection,
    timeout: 2_500,
  });
  await expect(page.getByTestId("slot-fallback-strip")).toContainText(expected.message);
  await expect(page.locator("iframe")).toHaveCount(0);
}
