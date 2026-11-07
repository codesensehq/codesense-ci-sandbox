// SPDX-License-Identifier: MIT
/** Host-side isolation and recovery in both slot locations. */
import { readFileSync } from "node:fs";
import { AGENT_LIST_PANE, agentListFixtures } from "../pane-harness/agent-list-fixtures";
import { expect, test } from "../pane-harness/harness";
import {
  expectStandardFallback,
  expectThrowOnLoadFallback,
} from "../support/slot-fallback-assertions";

const fixture = (name: string) => `tests/conformance/fixtures/${name}.html`;

for (const slotId of ["right-pane", "dock"] as const) {
  test.describe(slotId, () => {
    const dock = slotId === "dock";
    test.beforeEach(async ({ page }, info) => {
      if (!dock) return;
      const name = info.title.includes("throws on load") ? "throws" : "spoof";
      const probe = readFileSync(fixture(name), "utf8").match(/<body[^>]*>([\s\S]*?)<\/body>/i)![1];
      const fault = probe.match(/<script>([\s\S]*?)<\/script>/i)?.[1];
      await page.route("**/pane/**", async (route) => {
        const response = await route.fetch();
        const body = await response.text();
        await route.fulfill({
          response,
          body:
            name === "throws"
              ? body.replace(
                  "const cs = window.codesense,",
                  fault + "\nconst cs = window.codesense,",
                )
              : body.replace(
                  "</body>",
                  `<div style="position:fixed;top:0;right:0;z-index:10000;background:Canvas">${probe}</div></body>`,
                ),
        });
      });
    });
    test("a page that throws on load falls back at once to the built-in with its view state", async ({
      mountPane,
    }) => {
      const { page } = await mountPane(
        dock ? AGENT_LIST_PANE : fixture("throws"),
        {
          ...(dock ? agentListFixtures(3) : {}),
          title: "Broken pane",
          surface: "notes",
          ui: dock ? { selected: "s1" } : { conversation: "#qa" },
        },
        { awaitFrame: false },
      );
      await expectThrowOnLoadFallback(page, {
        name: "Broken pane",
        builtIn: dock ? "Agents list" : "Notes",
        message: "chart failed",
        selection: dock ? "s1" : "#qa",
      });
    });

    test.describe("the confirmation dialog belongs to the host", () => {
      // Policy: the confirm override is this fixture's configuration only; the default stays open.
      const spoofable = {
        ...(dock ? agentListFixtures(3) : {}),
        title: "Spoof",
        confirm: ["routineCancel"],
      };

      test("above an inert frame, Cancel first, armed after 500 ms, and a page cannot answer it", async ({
        mountPane,
      }) => {
        const pane = await mountPane(dock ? AGENT_LIST_PANE : fixture("spoof"), spoofable);
        const { page, frame } = pane;
        await expect(page.locator("iframe")).toHaveAttribute("title", "Spoof");
        expect(new URL(frame.url()).origin).not.toBe(new URL(page.url()).origin);
        await frame.getByRole("button", { name: "Cancel run" }).click();
        const dialog = page.getByTestId("verb-confirmation");
        await expect(dialog).toBeVisible();
        await expect(page.getByTestId("verb-confirmation-cancel")).toBeFocused();
        // Confirm ignores clicks until it has been open 500 ms.
        await page.getByTestId("verb-confirmation-confirm").click({ force: true });
        await expect(dialog).toBeVisible();
        await expect(dialog).toContainText(
          `Requested by ${dock ? "agent-list-pane.html" : "spoof.html"} · private`,
        );
        expect(await page.locator("iframe").evaluate((el) => (el as HTMLIFrameElement).inert)).toBe(
          true,
        );
        const box = (await dialog.boundingBox())!;
        expect(
          await page.evaluate(
            ([x, y]) =>
              !!document.elementFromPoint(x, y)?.closest('[data-testid="verb-confirmation"]'),
            [box.x + box.width / 2, box.y + box.height / 2],
          ),
        ).toBe(true);
        const spoof = await frame.evaluate(() => (window as any).spoof());
        expect(spoof).toEqual({
          error: expect.stringContaining("not allowlisted"),
          reached: false,
        });
        await page.waitForTimeout(300);
        await expect(dialog).toBeVisible();
        expect(await pane.answers()).toEqual([]);
        await page.waitForTimeout(300);
        await page.getByTestId("verb-confirmation-confirm").click();
        await expect(dialog).toBeHidden();
        expect(await pane.answers()).toEqual([{ confirmId: expect.any(String), allow: true }]);
        await expect
          .poll(() => frame.evaluate(() => (window as any).result))
          .toMatchObject({ status: "confirmed" });
      });

      test("a maximized dashboard keeps the host strip while the dialog is open", async ({
        mountPane,
      }) => {
        const pane = await mountPane(dock ? AGENT_LIST_PANE : fixture("spoof"), spoofable);
        await pane.maximize(true);
        await expect(pane.page.getByTestId("overlay")).toHaveAttribute("data-fullscreen", "true");
        await pane.frame.getByRole("button", { name: "Cancel run" }).click();
        await expect(pane.page.getByTestId("verb-confirmation")).toBeVisible();
        // A modal hides the background from accessibility queries, not from the screen.
        const strip = pane.page.getByTestId("overlay-strip");
        await expect(strip).toBeVisible();
        await expect(strip).toContainText("Spoof · private");
        await expect(
          strip.getByRole("button", { name: "Exit", includeHidden: true }),
        ).toBeVisible();
        expect(
          await pane.page.locator("iframe").evaluate((el) => (el as HTMLIFrameElement).inert),
        ).toBe(true);
      });
    });
  });
}

test("the shim's automatic ready is not live: a page that never readies falls back", async ({
  mountPane,
  page,
}) => {
  await page.clock.install();
  await mountPane(fixture("never-ready"), {
    title: "Never ready",
    surface: "notes",
    ui: { conversation: "#qa" },
  });
  await page.clock.fastForward(3500);
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  await page.clock.fastForward(26500);
  await expectStandardFallback(page, {
    name: "Never ready",
    builtIn: "Notes",
    detail: "timed out waiting for codesense.ready()",
    selection: "#qa",
  });
});
