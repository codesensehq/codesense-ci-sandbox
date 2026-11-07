// SPDX-License-Identifier: MIT
import { expect, test, type Page } from "@playwright/test";

/**
 * Real-browser guards for the bottom workspace bar at every "Workspace bar
 * size": the whole bar (both rows in grouped mode) sits inside the window, no
 * chip or control is cut by the row it sits in, the parts scale together, and a
 * crowded bar still goes compact and counts what doesn't fit.
 *
 * `SHOTS=<dir>` also saves the screenshots the PR shows.
 */

type Size = "small" | "normal" | "large";
const SIZES: Size[] = ["small", "normal", "large"];
const SHOTS = process.env.SHOTS;

async function boot(page: Page, width = 1400, height = 850) {
  await page.setViewportSize({ width, height });
  await page.goto("/?agents=team");
  await expect(page.locator("[data-workspace-bar] [data-tray-chip]").first()).toBeVisible();
  // The dev shim exposes the stores once its dynamic imports settle.
  await page.waitForFunction(() => Boolean(window.__stores?.useViewerStore));
  // Dismiss the first-run coachmark so it doesn't sit over the screenshots.
  await page.evaluate(() =>
    window.__stores!.useViewerStore.setState({ workspaceTraySeenCoachmark: true }),
  );
}

async function setSize(page: Page, size: Size) {
  await page.evaluate((workspaceBarSize) => {
    const s = window.__stores!.useViewerStore;
    s.getState().updateSettings({ workspaceBarSize });
  }, size);
  await expect(page.locator(`[data-bar-size="${size}"]`)).toBeVisible();
}

async function setGrouped(page: Page, grouped: boolean) {
  const toggle = page.getByTestId("workspace-tray-mode-toggle");
  if ((await toggle.getAttribute("aria-pressed")) !== String(grouped)) await toggle.click();
  await expect(toggle).toHaveAttribute("aria-pressed", String(grouped));
}

/** Everything the "fully visible" claim rests on, measured in the page. */
async function measure(page: Page) {
  return page.evaluate(() => {
    const bar = document.querySelector<HTMLElement>("[data-workspace-bar]")!;
    const b = bar.getBoundingClientRect();
    const strips = Array.from(bar.querySelectorAll<HTMLElement>('[role="toolbar"]'));
    const cut: string[] = [];
    for (const strip of strips) {
      const s = strip.getBoundingClientRect();
      // Every chip, pill and control must sit inside its strip's box vertically.
      strip
        .querySelectorAll<HTMLElement>("[data-tray-chip], [data-epic-rail-pill], button")
        .forEach((el) => {
          const r = el.getBoundingClientRect();
          if (r.height === 0) return;
          if (r.top < s.top - 0.5 || r.bottom > s.bottom + 0.5) {
            cut.push(
              `${el.dataset.testid ?? el.dataset.trayChip ?? el.tagName} ${r.top}-${r.bottom} in ${s.top}-${s.bottom}`,
            );
          }
        });
      // No strip scrolls vertically (overflow-y hidden would clip what overhangs).
      strip.querySelectorAll<HTMLElement>("*").forEach((el) => {
        // (sr-only text is clipped to 1px by design.)
        if (el.classList.contains("sr-only")) return;
        if (el.scrollHeight > el.clientHeight + 1 && getComputedStyle(el).overflowY === "hidden") {
          cut.push(`overflowing ${el.className.toString().slice(0, 40)}`);
        }
      });
    }
    const chip = bar.querySelector<HTMLElement>("[data-tray-chip]")!;
    const label = chip.querySelector<HTMLElement>("button span.truncate")!;
    return {
      innerHeight: window.innerHeight,
      barTop: b.top,
      barBottom: b.bottom,
      barHeight: b.height,
      stripsHeight: strips.reduce((h, s) => h + s.getBoundingClientRect().height, 0),
      chipHeight: chip.getBoundingClientRect().height,
      // Drawn size: font size × the drawn/layout ratio (no reliance on currentCSSZoom).
      labelPx:
        parseFloat(getComputedStyle(label).fontSize) *
        (label.getBoundingClientRect().height / label.offsetHeight),
      cut,
    };
  });
}

async function shot(
  page: Page,
  name: string,
  clip?: { x: number; y: number; width: number; height: number },
) {
  if (!SHOTS) return;
  await page.screenshot({ path: `${SHOTS}/${name}.png`, clip });
}

async function barClip(page: Page, width?: number) {
  const vp = page.viewportSize()!;
  const box = (await page.locator("[data-workspace-bar]").boundingBox())!;
  const top = Math.max(0, box.y - 24);
  return { x: 0, y: top, width: width ?? vp.width, height: vp.height - top };
}

test.describe("workspace bar size", () => {
  for (const size of SIZES) {
    test(`${size}: the whole bar sits inside the window and nothing in it is cut`, async ({
      page,
    }) => {
      await boot(page);
      await setSize(page, size);
      const m = await measure(page);
      expect(m.cut).toEqual([]);
      expect(m.barBottom).toBeLessThanOrEqual(m.innerHeight + 0.5);
      // The box is as tall as its strips: nothing hangs below it.
      expect(Math.abs(m.barHeight - m.stripsHeight)).toBeLessThan(1);
      await shot(page, `size-${size}`, await barClip(page));
      await shot(page, `corner-${size}`, {
        x: 0,
        y: m.barTop - 30,
        width: 520,
        height: m.innerHeight - m.barTop + 30,
      });
    });
  }

  test("normal is the original, larger bar; small and large scale everything together", async ({
    page,
  }) => {
    await boot(page);
    const at: Record<string, Awaited<ReturnType<typeof measure>>> = {};
    for (const size of SIZES) {
      await setSize(page, size);
      at[size] = await measure(page);
    }
    // The one-line pill's name is 12px at Normal, never drawn smaller (Small keeps 12px), and
    // grows with Large.
    expect(at.normal.labelPx).toBeCloseTo(12, 0);
    expect(at.small.labelPx).toBeCloseTo(12, 0);
    expect(at.large.labelPx).toBeCloseTo(15, 0);
    // Pills (and the row) scale by the same factor as the labels.
    expect(at.small.chipHeight / at.normal.chipHeight).toBeCloseTo(0.875, 1);
    expect(at.large.chipHeight / at.normal.chipHeight).toBeCloseTo(1.25, 1);
    expect(at.small.barHeight).toBeLessThan(at.normal.barHeight);
    expect(at.large.barHeight).toBeGreaterThan(at.normal.barHeight);
  });

  for (const size of SIZES) {
    test(`${size}, grouped: both rows sit inside the window, nothing cut`, async ({ page }) => {
      await boot(page);
      await setSize(page, size);
      await setGrouped(page, true);
      const m = await measure(page);
      expect(m.cut).toEqual([]);
      expect(m.barBottom).toBeLessThanOrEqual(m.innerHeight + 0.5);
      expect(Math.abs(m.barHeight - m.stripsHeight)).toBeLessThan(1);
      await shot(page, `grouped-${size}`, await barClip(page));
      await setGrouped(page, false);
    });
  }

  for (const size of SIZES) {
    test(`${size}, narrow window: crowded chips go compact, the rest is counted, nothing cut`, async ({
      page,
    }) => {
      await boot(page, 600, 700);
      await setSize(page, size);
      const m = await measure(page);
      expect(m.cut).toEqual([]);
      expect(m.barBottom).toBeLessThanOrEqual(m.innerHeight + 0.5);
      // Crowded at every size: compact pills, the focused chip full.
      await expect(page.locator("[data-workspace-bar] [data-compact]").first()).toBeAttached();
      await shot(page, `narrow-${size}`, await barClip(page));
    });
  }

  test("a size change alone re-measures: Large to Small, the crowded bar goes back to full pills", async ({
    page,
  }) => {
    // Observe actual full rows before production compaction; return every
    // original rectangle unchanged and remove the probe before the final oracle.
    await page.addInitScript(() => {
      const w = window as unknown as {
        __fxCapacity: Record<
          string,
          {
            demand: number;
            available: number;
            margin: number;
            widths: { id: string; width: number }[];
          }
        >;
        __fxStopCapacity: () => void;
      };
      w.__fxCapacity = {};
      const original = HTMLElement.prototype.getBoundingClientRect;
      let measuring = false;
      HTMLElement.prototype.getBoundingClientRect = function () {
        const r = original.call(this);
        if (!measuring && this.matches("[data-tray-chip]")) {
          const lane = this.parentElement!;
          const chips = Array.from(lane.querySelectorAll<HTMLElement>("[data-tray-chip]"));
          if (chips.length && chips.every((chip) => !chip.hasAttribute("data-compact"))) {
            measuring = true;
            try {
              const box = original.call(lane);
              const rects = chips.map((chip) => ({
                id: chip.dataset.trayChip!,
                rect: original.call(chip),
              }));
              const size = document.querySelector<HTMLElement>("[data-bar-size]")!.dataset.barSize!;
              w.__fxCapacity[size] = {
                demand:
                  Math.max(box.left, ...rects.map((chip) => chip.rect.right)) -
                  Math.min(box.left, ...rects.map((chip) => chip.rect.left)),
                available: box.width,
                margin: 56 * (box.width / lane.offsetWidth),
                widths: rects.map((chip) => ({ id: chip.id, width: chip.rect.width })),
              };
            } finally {
              measuring = false;
            }
          }
        }
        return r;
      };
      w.__fxStopCapacity = () => {
        HTMLElement.prototype.getBoundingClientRect = original;
      };
    });
    await boot(page, 800, 700);
    // Retain every workspace/child and the 800px/zero oracle.
    // The original longer generic names remain in the pressure case below.
    const ids = await page.evaluate(() => {
      const store = window.__stores!.useViewerStore;
      const original = store.getState().workspaces;
      store.setState({
        workspaces: Object.fromEntries(
          Object.entries(original).map(([team, rows]) => [
            team,
            rows.map((ws, i) =>
              ws.kind === "generic"
                ? { ...ws, cwd: `/tmp/fx122-capacity/${i === 2 ? "p" : "s"}` }
                : ws,
            ),
          ]),
        ),
      });
      return Object.values(original)
        .flat()
        .map((ws) => ws.id);
    });
    await setSize(page, "large");
    await expect(page.locator("[data-workspace-bar] [data-compact]").first()).toBeAttached();
    // No viewport change: the size change alone must trigger the re-measure.
    await setSize(page, "small");
    const premise = await page.evaluate(() => {
      const w = window as unknown as {
        __fxCapacity: Record<
          string,
          {
            demand: number;
            available: number;
            margin: number;
            widths: { id: string; width: number }[];
          }
        >;
        __fxStopCapacity: () => void;
      };
      w.__fxStopCapacity();
      return w.__fxCapacity;
    });
    expect(premise.large.demand).toBeGreaterThan(premise.large.available);
    expect(premise.small.demand + premise.small.margin).toBeLessThan(premise.small.available);
    expect(premise.small.widths.map((chip) => chip.id).sort()).toEqual([...ids].sort());
    await test.info().attach("size-release-capacity-premise", {
      body: JSON.stringify(premise),
      contentType: "application/json",
    });
    await expect(page.locator("[data-workspace-bar] [data-compact]")).toHaveCount(0);
  });

  test("original800px generic-name pressure stays compact, uncut and usable at Small", async ({
    page,
  }) => {
    await boot(page, 800, 700);
    // The one-line pills are compact enough that this bar may no longer need its compact pills;
    // whichever it draws, nothing is cut and the focused pill stays whole.
    await setSize(page, "large");
    await setSize(page, "small");
    expect((await measure(page)).cut).toEqual([]);
    const active = page.locator('[data-workspace-bar] [data-tray-chip][data-active="true"]');
    await expect(active).not.toHaveAttribute("data-compact");
    expect(
      await active.evaluate((el) => {
        const card = el.getBoundingClientRect();
        const lane = el.parentElement!.getBoundingClientRect();
        const main = el.querySelector<HTMLElement>("[data-ws-row]")!;
        const r = main.getBoundingClientRect();
        return (
          card.left >= lane.left - 1 &&
          card.right <= lane.right + 1 &&
          main.contains(document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2))
        );
      }),
    ).toBe(true);
  });

  test("the chip menu opens at the pointer at a scaled size", async ({ page }) => {
    await boot(page);
    await setSize(page, "small");
    const pill = page.locator("[data-workspace-bar] [data-tray-chip] button").nth(1);
    const box = (await pill.boundingBox())!;
    const x = box.x + box.width / 2;
    const y = box.y + box.height / 2;
    await page.mouse.click(x, y, { button: "right" });
    const menu = page.getByRole("menu", { name: "Workspace actions" });
    await expect(menu).toBeVisible();
    const m = (await menu.boundingBox())!;
    // Clamped into the viewport, so it may move up from the pointer, never sideways off it.
    expect(Math.abs(m.x - x)).toBeLessThan(2);
    expect(m.y + m.height).toBeLessThanOrEqual(page.viewportSize()!.height + 0.5);
    await page.keyboard.press("Escape");
  });

  /** Drawn hit boxes of every bar control (chips excepted) and drawn badge text sizes. */
  async function controlsAndBadges(page: Page) {
    return page.evaluate(() => {
      const bar = document.querySelector<HTMLElement>("[data-workspace-bar]")!;
      const controls = Array.from(bar.querySelectorAll<HTMLElement>("button"))
        .filter((b) => !b.closest("[data-tray-chip]") && b.getBoundingClientRect().height > 0)
        .map((b) => {
          const r = b.getBoundingClientRect();
          return {
            name: b.getAttribute("aria-label") ?? b.dataset.testid ?? "",
            w: r.width,
            h: r.height,
          };
        });
      const drawnPx = (el: HTMLElement) =>
        parseFloat(getComputedStyle(el).fontSize) *
        (el.currentCSSZoom ?? el.getBoundingClientRect().width / (el.offsetWidth || 1));
      const badges = Array.from(
        bar.querySelectorAll<HTMLElement>(
          ".ag-face-tag, .ag-dock-n, .ag-dock-needs, [data-tray-chip] [data-tray-shortcut]",
        ),
      ).map((el, i) => ({ i, cls: el.className.toString().slice(0, 30), px: drawnPx(el) }));
      const chipH = Math.min(
        ...Array.from(bar.querySelectorAll<HTMLElement>("[data-tray-chip]")).map(
          (c) => c.getBoundingClientRect().height,
        ),
      );
      const shortcuts = Array.from(
        bar.querySelectorAll<HTMLElement>("[data-tray-chip] [data-tray-shortcut]"),
      ).map((el) => ({ text: el.textContent?.trim(), px: drawnPx(el) }));
      const expectedShortcuts = Array.from(bar.querySelectorAll("[data-tray-chip]"))
        .slice(0, 9)
        // The jump key as this platform names it: ⌘1 on a Mac, Ctrl+1 elsewhere.
        .map((_, i) => (/Mac/i.test(navigator.platform) ? `⌘${i + 1}` : `Ctrl+${i + 1}`));
      return { controls, badges, chipH, shortcuts, expectedShortcuts };
    });
  }

  for (const cliOpen of [true, false]) {
    test(`small${cliOpen ? "" : ", CLI closed"}: every control keeps a 24px hit target and badge text keeps its Normal size`, async ({
      page,
    }) => {
      await boot(page);
      if (!cliOpen) await page.keyboard.press("Meta+Backslash");
      if (!cliOpen) await expect(page.locator("[data-agents-toggle]")).toBeVisible();
      await setSize(page, "normal");
      const normal = await controlsAndBadges(page);
      await setSize(page, "small");
      const small = await controlsAndBadges(page);
      // Both CLI placements must actually include every supplied shortcut, not
      // pass a font comparison with empty arrays after a composition change.
      expect(normal.expectedShortcuts.length).toBeGreaterThan(0);
      expect(normal.shortcuts.map((s) => s.text)).toEqual(normal.expectedShortcuts);
      expect(small.shortcuts.map((s) => s.text)).toEqual(normal.expectedShortcuts);
      for (const [i, shortcut] of small.shortcuts.entries()) {
        expect
          .soft(shortcut.px, `shortcut ${shortcut.text}`)
          .toBeGreaterThanOrEqual(normal.shortcuts[i]!.px - 0.2);
      }
      expect(small.controls.length).toBeGreaterThan(0);
      for (const c of small.controls) {
        expect.soft(c.h, `${c.name} height`).toBeGreaterThanOrEqual(24 - 0.5);
        expect.soft(c.w, `${c.name} width`).toBeGreaterThanOrEqual(24 - 0.5);
      }
      expect(small.chipH).toBeGreaterThanOrEqual(24);
      expect(small.badges.length).toBe(normal.badges.length);
      for (const b of small.badges) {
        expect.soft(b.px, b.cls).toBeGreaterThanOrEqual(normal.badges[b.i]!.px - 0.2);
      }
    });
  }

  for (const height of [850, 300]) {
    test(`large, ${height}px window: the "+N" menu sits on its button and inside the window`, async ({
      page,
    }) => {
      await boot(page, 600, height);
      await setSize(page, "large");
      const plus = page.getByTestId("workspace-tray-overflow");
      await expect(plus).toBeVisible();
      await plus.click();
      const menu = page.getByTestId("workspace-tray-overflow-menu");
      await expect(menu).toBeVisible();
      const b = (await plus.boundingBox())!;
      const m = (await menu.boundingBox())!;
      const vp = page.viewportSize()!;
      // Above its button (the bar is at the bottom), right edges aligned (align="end").
      expect(m.y + m.height).toBeLessThanOrEqual(b.y + 0.5);
      expect(b.y - (m.y + m.height)).toBeLessThan(12);
      expect(Math.abs(m.x + m.width - (b.x + b.width))).toBeLessThan(2);
      // Inside the window; a list too long for it scrolls inside the menu.
      expect(m.y).toBeGreaterThanOrEqual(0);
      expect(m.x).toBeGreaterThanOrEqual(0);
      expect(m.x + m.width).toBeLessThanOrEqual(vp.width + 0.5);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/plus-menu-large-${height}.png` });
      await page.keyboard.press("Escape");
    });
  }

  for (const size of ["small", "large"] as const) {
    test(`${size}: a pill dragged onto a pane splits it; the ghost starts under the pointer`, async ({
      page,
    }) => {
      await boot(page);
      await setSize(page, size);
      const chip = page.locator("[data-workspace-bar] [data-tray-chip]").last();
      const dragged = (await chip.getAttribute("data-tray-chip"))!;
      const before = await page.evaluate(() =>
        JSON.stringify(window.__stores!.useViewerStore.getState().foregroundLayout),
      );
      const c = (await chip.boundingBox())!;
      const x0 = c.x + c.width / 2;
      const y0 = c.y + c.height / 2;
      await page.mouse.move(x0, y0);
      await page.mouse.down();
      // Out of the bar's band (in it, the drag reorders the pills and shows no ghost).
      await page.mouse.move(x0, y0 - 60, { steps: 6 });
      // The ghost appears at the pointer, not offset by the bar's zoom.
      const ghost = page.getByTestId("workspace-drag-ghost");
      await expect(ghost).toBeVisible();
      void dragged;
      const g = (await ghost.boundingBox())!;
      expect(Math.abs(g.x + g.width / 2 - x0)).toBeLessThan(Math.max(c.width, g.width) / 2 + 4);
      expect(Math.abs(g.y + g.height / 2 - (y0 - 60))).toBeLessThan(c.height);
      // Onto another tile's left edge zone.
      const zones = page.locator("[data-testid^='tile-zones-']").first();
      await expect(zones).toBeVisible();
      const left = zones.locator("[data-drop-side='left']");
      const z = (await left.boundingBox())!;
      await page.mouse.move(z.x + z.width / 2, z.y + z.height / 2, { steps: 12 });
      await expect(left).toHaveAttribute("data-drop-over", "true");
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/drag-${size}.png` });
      await page.mouse.up();
      const after = await page.evaluate(() =>
        JSON.stringify(window.__stores!.useViewerStore.getState().foregroundLayout),
      );
      expect(after).not.toBe(before);
    });
  }

  test("a very short window keeps the whole bar on screen (the panes above give way)", async ({
    page,
  }) => {
    await boot(page, 1200, 180);
    for (const size of SIZES) {
      await setSize(page, size);
      const m = await measure(page);
      expect(m.barBottom).toBeLessThanOrEqual(m.innerHeight + 0.5);
      expect(Math.abs(m.barHeight - m.stripsHeight)).toBeLessThan(1);
      expect(m.cut).toEqual([]);
    }
  });

  for (const size of ["small", "large"] as const) {
    test(`${size}: a control's tooltip anchors on the control`, async ({ page }) => {
      await boot(page);
      await setSize(page, size);
      const toggle = page.getByTestId("workspace-tray-mode-toggle");
      await toggle.hover();
      const tip = page.getByRole("tooltip").first();
      await expect(tip).toBeVisible();
      const b = (await toggle.boundingBox())!;
      // Opens above the bar control, close to it, overlapping it horizontally.
      const tipBox = (await page
        .locator("[data-radix-popper-content-wrapper]")
        .last()
        .boundingBox())!;
      expect(tipBox.y + tipBox.height).toBeLessThanOrEqual(b.y + 1);
      expect(b.y - (tipBox.y + tipBox.height)).toBeLessThan(16);
      expect(tipBox.x).toBeLessThan(b.x + b.width);
      expect(tipBox.x + tipBox.width).toBeGreaterThan(b.x - 1);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/tooltip-${size}.png` });
    });
  }
});
