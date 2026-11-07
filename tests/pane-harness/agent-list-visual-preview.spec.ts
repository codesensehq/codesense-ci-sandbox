// SPDX-License-Identifier: MIT
import { AGENT_LIST_PANE } from "./agent-list-fixtures";
import { test, expect } from "./harness";
import { agentListCaptureFixtures } from "./agent-list-fixtures";
import { previewWorld } from "./agent-list-visual-fixtures";
const LIVE = AGENT_LIST_PANE;
const themes = ["codesense", "nord"];
const schemes = (process.env.PREVIEW_SCHEMES || "light,dark").split(",") as ("light" | "dark")[];
// Every agent row must sit inside its virtual item: no text or box spills into a neighbour.
async function spills(frame: any) {
  return frame.evaluate(() => {
    const out: string[] = [];
    for (const item of document.querySelectorAll(".al-item")) {
      const box = item.getBoundingClientRect();
      for (const el of item.querySelectorAll(
        ".al-row, .al-lines, .al-l1, .al-ask, .al-name, .al-state",
      )) {
        const r = el.getBoundingClientRect();
        if (
          r.width &&
          (r.bottom > box.bottom + 0.5 || r.top < box.top - 0.5 || r.right > box.right + 0.5)
        )
          out.push(
            `${(item as HTMLElement).dataset.rowId} ${el.className} ${Math.round(r.bottom - box.bottom)}`,
          );
      }
      const name = item.querySelector(".al-name");
      if (name && name.getBoundingClientRect().width < 8)
        out.push(`${(item as HTMLElement).dataset.rowId} name-collapsed`);
    }
    return out;
  });
}
const worlds: [string, () => any][] = [
  ["teams", () => previewWorld({ arrange: "teams" })],
  ["manager", () => previewWorld({ arrange: "manager" })],
  ["status", () => previewWorld({ arrange: "status" })],
  ["capture", () => ({ ...agentListCaptureFixtures(), width: 236, height: 700 })],
];
for (const id of themes)
  for (const scheme of schemes)
    for (const [name, world] of worlds) {
      test(`${id} ${scheme} ${name}`, async ({ mountPane }, testInfo) => {
        const pane = await mountPane(LIVE, { ...world(), theme: { id: id as any, scheme } });
        const vars = await pane.page.evaluate(
          async ({ id, scheme }) => {
            const { themeTokens } = await import("/src/themes/index.ts");
            return themeTokens(id as any, scheme);
          },
          { id, scheme },
        );
        await pane.setTheme({ id: id as any, scheme, vars });
        await expect(pane.frame.locator(".al-row").first()).toBeVisible();
        const hover = pane.frame.locator(".al-row").nth(name === "capture" ? 1 : 4);
        await hover.hover();
        const bad = await spills(pane.frame);
        expect(bad).toEqual([]);
        if (name !== "capture") {
          const word = (n: string) =>
            pane.frame.evaluate((who) => {
              const row = [...document.querySelectorAll(".al-row")].find(
                (r) => r.querySelector(".al-name")?.textContent === who,
              );
              return row
                ? `${row.querySelector(".al-state")?.textContent}|${row.querySelector(".al-dot svg")?.getAttribute("data-mark")}`
                : "missing";
            }, n);
          const ac7 = {
            "Codex dev (idle, 2)": await word("Codex dev"),
            "Docs reviewer (stalled, 1)": await word("Docs reviewer"),
            "Reviewer (paused, 1)": await word("Reviewer"),
            "Perf soak (offline, 3)": await word("Perf soak"),
            "Docs writer (crashed, 2)": await word("Docs writer"),
            "Contrast check (ended, 1)": await word("Contrast check"),
          };
          expect(ac7["Codex dev (idle, 2)"]).toContain("Working");
          expect(ac7["Docs reviewer (stalled, 1)"]).toContain("Working");
          expect(ac7["Reviewer (paused, 1)"]).not.toContain("Working");
          expect(ac7["Perf soak (offline, 3)"]).toContain("Offline");
          expect(ac7["Docs writer (crashed, 2)"]).not.toContain("Working");
          expect(ac7["Contrast check (ended, 1)"]).not.toContain("Working");
        }
        const overflow = await pane.frame.evaluate(
          () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
        );
        expect(overflow).toBe(false);
        await pane.page
          .locator("iframe")
          .first()
          .screenshot({
            path: testInfo.outputPath(`${id}-${scheme}-${name}.png`),
            animations: "disabled",
          });
      });
    }
