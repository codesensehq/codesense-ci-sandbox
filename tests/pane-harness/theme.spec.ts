// SPDX-License-Identifier: MIT
import type { AppTheme } from "../../src/themes";
import { kitRoles } from "../../src/themes/kit";
import { hex } from "./colors";
import { expect, test, type MountedPane } from "./harness";

const PANE = "tests/pane-harness/fixtures/sample-pane.html";

async function colors(pane: MountedPane) {
  return hex(
    pane.frame,
    await pane.frame.locator("body").evaluate((el) => {
      const style = getComputedStyle(el);
      return [style.backgroundColor, style.color];
    }),
  );
}

async function palette(pane: MountedPane, id: AppTheme, scheme: "light" | "dark") {
  return pane.page.evaluate(
    async ({ id, scheme }) => {
      // @ts-expect-error -- browser import served by the fixture Vite host.
      const { themeTokens } = await import("/src/themes/index.ts");
      return themeTokens(id, scheme) as Record<string, string>;
    },
    { id, scheme },
  );
}

for (const slotId of ["right-pane", "file-nav"] as const) {
  const surface = slotId === "file-nav" ? "sidebar" : "viewer";
  const roles = kitRoles(surface);
  for (const scheme of ["dark", "light"] as const) {
    test(`${slotId} mounts and switches distinct ${scheme} theme palettes inside the frame`, async ({
      mountPane,
    }) => {
      const mounted: string[][] = [];
      for (const id of ["dracula", "github"] as const) {
        const pane = await mountPane(PANE, { slotId, theme: { id, scheme } });
        const expected = async (themeId: AppTheme) => {
          const tokens = await palette(pane, themeId, scheme);
          return hex(pane.frame, [tokens[`--theme-${roles.bg}`], tokens[`--theme-${roles.fg}`]]);
        };
        await expect.poll(() => colors(pane)).toEqual(await expected(id));
        mounted.push(await colors(pane));
        const boot = (await pane.probe()).boot;
        const next = id === "dracula" ? "github" : "dracula";
        await pane.setTheme({ id: next, scheme });
        await expect.poll(() => colors(pane)).toEqual(await expected(next));
        expect((await pane.probe()).boot).toBe(boot);
      }
      expect(mounted[0][0]).not.toBe(mounted[1][0]);
      expect(mounted[0][1]).not.toBe(mounted[1][1]);
    });
  }
}

test("an omitted theme id uses CodeSense tokens and preserves fixture overrides", async ({
  mountPane,
}) => {
  const override = "#123456";
  const pane = await mountPane(PANE, {
    theme: { scheme: "dark", vars: { "--theme-surface-viewer-bg": override } },
  });
  const tokens = await palette(pane, "codesense", "dark");
  await expect
    .poll(() => colors(pane))
    .toEqual(await hex(pane.frame, [override, tokens["--theme-text-viewer-primary"]]));
  await pane.setTheme({
    id: "github",
    scheme: "light",
    vars: { "--theme-surface-viewer-bg": override },
  });
  await expect
    .poll(() => colors(pane))
    .toEqual(
      await hex(pane.frame, [
        override,
        (await palette(pane, "github", "light"))["--theme-text-viewer-primary"],
      ]),
    );
});
