// SPDX-License-Identifier: MIT
import { AGENT_LIST_PANE } from "./agent-list-fixtures";
// SV-AGP-9 step 8: skeleton, empty and drag states of the pane under test.
import { test, expect } from "./harness";
import { previewWorld } from "./agent-list-visual-fixtures";
const LIVE = AGENT_LIST_PANE;
for (const scheme of ["light", "dark"] as const) {
  test(`skeleton ${scheme}`, async ({ page, mountPane }, testInfo) => {
    await page.route("**/pane/**", async (route) => {
      const response = await route.fetch();
      const body = (await response.text()).replace(
        "const cs = window.codesense,",
        `const gate = new Promise(r => window.releaseSnapshot = r); const sub = window.codesense.subscribe;
         window.codesense.subscribe = async (...a) => { await gate; return sub(...a); }; const cs = window.codesense,`,
      );
      await route.fulfill({ response, body });
    });
    const pane = await mountPane(LIVE, { ...previewWorld(), height: 420, theme: { scheme } });
    const loading = pane.frame.getByRole("status", { name: "Loading agents" });
    await expect(loading.locator(".al-skeleton-card")).toHaveCount(2);
    await pane.page
      .locator("iframe")
      .first()
      .screenshot({ path: testInfo.outputPath(`skeleton-${scheme}.png`), animations: "disabled" });
  });
  test(`empty ${scheme}`, async ({ mountPane }, testInfo) => {
    const w = previewWorld();
    w.sessions = [];
    w.topics.agentList = w.topics.agentList.filter((r: any) => !r.sessionRef);
    const pane = await mountPane(LIVE, { ...w, height: 360, theme: { scheme } });
    await expect(pane.frame.getByText("No agents running", { exact: true })).toBeVisible();
    await pane.page
      .locator("iframe")
      .first()
      .screenshot({ path: testInfo.outputPath(`empty-${scheme}.png`) });
  });
  test(`drag ${scheme}`, async ({ mountPane }, testInfo) => {
    const w = previewWorld({ arrange: "teams" });
    w.topics["ui:dock"][0].folds = Object.fromEntries(
      ["main", "SV-AGP-9", "SV-INP-13", "SV-DOC-5"].map((id) => [
        `#ws-fold:codesense:riverside|${id}`,
        true,
      ]),
    );
    const pane = await mountPane(LIVE, { ...w, height: 520, theme: { scheme } });
    const head = pane.frame.locator(".al-ws-head", { hasText: "SV-AGP-9" });
    await expect(head).toBeVisible();
    const b = (await head.boundingBox())!;
    const target = (await pane.frame
      .locator(".al-ws-head", { hasText: "SV-DOC-5" })
      .boundingBox())!;
    await pane.page.mouse.move(b.x + 60, b.y + b.height / 2);
    await pane.page.mouse.down();
    await pane.page.mouse.move(b.x + 64, b.y + b.height / 2 + 12, { steps: 4 });
    await pane.page.mouse.move(target.x + 60, target.y + 4, { steps: 8 });
    await expect(pane.frame.locator(".al-ws-drag-preview")).toBeVisible();
    await expect(pane.frame.locator(".al-drop-line")).toBeVisible();
    await pane.page
      .locator("iframe")
      .first()
      .screenshot({ path: testInfo.outputPath(`drag-${scheme}.png`), animations: "disabled" });
    await pane.page.mouse.up();
  });
}
// Every icon is a real kit or authored glyph: kit.icon marks unknown names data-missing and draws "?".
const BUILTIN = AGENT_LIST_PANE;
for (const [name, file] of [["builtin", BUILTIN]] as const)
  for (const arrange of ["teams", "manager"])
    test(`icons are real glyphs ${name} ${arrange}`, async ({ mountPane }) => {
      const pane = await mountPane(file, { ...previewWorld({ arrange }), width: 300 });
      await expect(pane.frame.locator(".al-row").first()).toBeVisible();
      const report = await pane.frame.evaluate(() => {
        const missing = [...document.querySelectorAll("svg[data-missing]")].map((s) =>
          s.getAttribute("data-missing"),
        );
        const drawn = (sel: string) =>
          [...document.querySelectorAll(sel)].every((el) =>
            el.querySelector("svg path, svg circle, svg rect, svg line"),
          );
        return {
          missing: [...new Set(missing)],
          quick: document.querySelectorAll(".al-quick").length,
          messages: document.querySelectorAll(".al-messages").length,
          actionsDrawn: drawn(".al-quick, .al-messages"),
          chevronsDrawn: drawn(".al-fchev, .al-ws-grip, .al-fold"),
        };
      });
      console.log(`ICONS ${name} ${arrange}:`, JSON.stringify(report));
      {
        expect(report.missing).toEqual([]);
        expect(report.messages).toBeGreaterThan(0);
        expect(report.actionsDrawn && report.chevronsDrawn).toBe(true);
      }
    });
// Every icon button's glyph sits on its row's text centre line (within 1px), in both views.
for (const arrange of ["teams", "manager"])
  test(`icons align with their row text ${arrange}`, async ({ mountPane }) => {
    const pane = await mountPane(LIVE, { ...previewWorld({ arrange }), width: 236 });
    await expect(pane.frame.locator(".al-row").first()).toBeVisible();
    const off = await pane.frame.evaluate(() => {
      const mid = (el: Element | null) => {
        if (!el) return NaN;
        const r = el.getBoundingClientRect();
        return r.top + r.height / 2;
      };
      const out: string[] = [];
      const check = (scope: Element, textSel: string, iconSel: string, label: string) => {
        const text = mid(scope.querySelector(textSel));
        for (const btn of scope.querySelectorAll(iconSel)) {
          const glyph = btn.querySelector("svg") || btn;
          if (!glyph.getBoundingClientRect().height) continue; // display:none in this view
          const d = mid(glyph) - text;
          if (!(Math.abs(d) <= 1))
            out.push(`${label} ${(btn as HTMLElement).className || btn.tagName} ${d.toFixed(1)}`);
        }
      };
      const head = document.querySelector(".al-head")!;
      check(head, "#al-title", ".al-icon, #al-arrange-chevron", "header");
      for (const t of document.querySelectorAll(".al-team"))
        check(t, ".al-team-name", ".al-launch, .al-team-funnel, .al-fchev", "team");
      for (const c of document.querySelectorAll(".al-ws-card"))
        check(c, ".al-ws-name", ".al-launch, .al-fchev, .al-ws-grip, .al-ws-machine", "workspace");
      for (const r of document.querySelectorAll(".al-row")) {
        const l1 = r.querySelector(".al-l1")!;
        check(r, ".al-name", ".al-acts > button, .al-fold", "row");
        check(l1, ".al-name", ".al-via, .al-marker, .al-subs", "row-l1");
      }
      return out;
    });
    console.log(`ALIGN ${arrange}:`, JSON.stringify(off.slice(0, 12)), off.length);
    expect(off).toEqual([]);
  });
