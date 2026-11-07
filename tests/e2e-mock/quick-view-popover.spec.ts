// SPDX-License-Identifier: MIT
import { expect, test, type Page } from "@playwright/test";
import type { AppTheme } from "../../src/themes";

async function setTheme(page: Page, theme: AppTheme, scheme: "light" | "dark") {
  await page.evaluate(
    ({ theme, scheme }) => {
      window
        .__stores!.useViewerStore.getState()
        .updateSettings({ appTheme: theme, terminalTheme: "match-app" });
      window.dispatchEvent(new StorageEvent("storage", { key: "theme", newValue: scheme }));
    },
    { theme, scheme },
  );
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          `${document.documentElement.dataset.appTheme}/${document.documentElement.style.colorScheme}`,
      ),
    )
    .toBe(`${theme}/${scheme}`);
}

// The mock world supplies terminals; seed the same projection the Rust host normally pushes.
test.beforeEach(async ({ page }) => {
  await page.goto("/?agents=teams");
  await page.waitForFunction(() => !!window.__stores?.useViewerStore);
  await page.evaluate(async () => {
    const { agentTeamSessions, agentTeamTabs } = await new Function(
      "return import('/src/app/mock/handlers/agents-team.ts')",
    )();
    const { useAgentListProjectionStore } = await new Function(
      "return import('/src/features/viewer/lib/agent-list-projection.ts')",
    )();
    const { useOrgStore } = await new Function(
      "return import('/src/shared/stores/org-store.ts')",
    )();
    const org = useOrgStore.getState();
    const team = `${org.activeOrgSlug}:${org.activeTeamName}`;
    const rows = agentTeamSessions("?agents=team").map((r: { sessionId: string }) => ({
      ...r,
      effectiveStatus: "tool-running",
      subagents: r.sessionId === "sess-login-lead" ? 2 : 0,
    }));
    const tabs = agentTeamTabs("?agents=team");
    useAgentListProjectionStore.setState({
      sessions: rows,
      references: rows.map(
        (r: { sessionId: string; handle: string; story?: string }, i: number) => ({
          id: r.sessionId,
          sessionRef: r.sessionId,
          inboxRef: r.handle,
          teamKey: team,
          parentId: null,
          parentSource: null,
          order: i,
          workspaceRef:
            tabs.find((tab: { session: string }) => tab.session === r.sessionId)?.workspaceId ??
            null,
          storyRef: r.story ?? null,
          workspaceRank: i,
        }),
      ),
      partial: false,
      teamOrder: [team],
      homeTeam: team,
    });
  });
});

for (const theme of ["codesense", "nord"] as const) {
  for (const scheme of ["light", "dark"] as const) {
    test(`quick view card ${theme} ${scheme}`, async ({ page }) => {
      await page.setViewportSize({ width: 1280, height: 800 });
      const row = page.locator('.ag-row[data-agent-handle="BUG-142#dev"]').first();
      await expect(row).toBeVisible();
      await setTheme(page, theme, scheme);
      await row.hover();
      await row.getByTestId("agent-row-cli").click();
      const quick = page.getByTestId("agent-quick-send");
      await expect(quick).toBeVisible();
      await expect(quick.getByRole("button", { name: "Open tab" })).toBeVisible();
      await expect(quick.locator(".ag-face")).toBeVisible();
      const bounds = await page.locator("[data-workspace-bar]").boundingBox();
      const card = (await quick.boundingBox())!;
      expect(card.y + card.height).toBeLessThanOrEqual(bounds!.y - 7);
      await page.screenshot({
        path: `test-results/quick-view-${theme}-${scheme}.png`,
        animations: "disabled",
      });
      await quick.getByRole("button", { name: "Close", exact: true }).click();
      await expect(quick).toBeHidden();
      await expect(row.getByTestId("agent-row-cli")).toBeFocused();
    });
  }
}

for (const hold of ["keyboard", "drag"] as const) {
  test(`quick view bounds during a ${hold} resize hold and window shrink`, async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    const row = page.locator('.ag-row[data-agent-handle="BUG-142#dev"]').first();
    await row.hover();
    await row.getByTestId("agent-row-cli").click();
    const quick = page.getByTestId("agent-quick-send");
    const grip = quick.getByRole("separator", { name: "Resize popup" });
    if (hold === "keyboard") {
      await grip.focus();
      await page.keyboard.press("ArrowRight");
    } else {
      const corner = (await grip.boundingBox())!;
      await page.mouse.move(corner.x + corner.width / 2, corner.y + corner.height / 2);
      await page.mouse.down();
      await page.mouse.move(corner.x + corner.width / 2 + 16, corner.y + corner.height / 2);
    }
    const initial = (await quick.boundingBox())!;
    const saved = await page.evaluate(() =>
      localStorage.getItem("codesense.agents.quickView.size"),
    );

    const checkBounds = async () => {
      const geometry = await quick.evaluate(async (dialog) => {
        const { appPopoverBounds } = await new Function(
          "return import('/src/shared/lib/popover-bounds.ts')",
        )();
        const rect = dialog.getBoundingClientRect();
        return {
          left: rect.left,
          right: rect.right,
          top: rect.top,
          bottom: rect.bottom,
          width: rect.width,
          bounds: appPopoverBounds(),
        };
      });
      expect(geometry.left).toBeGreaterThanOrEqual(geometry.bounds.left + 7);
      expect(geometry.right).toBeLessThanOrEqual(geometry.bounds.right - 7);
      expect(geometry.top).toBeGreaterThanOrEqual(geometry.bounds.top + 7);
      expect(geometry.bottom).toBeLessThanOrEqual(geometry.bounds.bottom - 7);
      expect(
        await page.evaluate(() => localStorage.getItem("codesense.agents.quickView.size")),
      ).toBe(saved);
      return geometry;
    };

    await page.setViewportSize({ width: 800, height: 500 });
    const shrunk = await checkBounds();
    expect(shrunk.left).toBeCloseTo(initial.x, 0);
    expect(shrunk.width).toBeCloseTo(shrunk.bounds.right - initial.x - 8, 0);
    expect(shrunk.width).toBeGreaterThanOrEqual(380);

    await page.setViewportSize({ width: Math.floor(initial.x + 380 - 40), height: 500 });
    const shifted = await checkBounds();
    expect(shifted.width).toBeCloseTo(380, 0);
    expect(shifted.left).toBeLessThan(initial.x);

    await page.setViewportSize({ width: 1280, height: 800 });
    const restored = await checkBounds();
    expect(restored.width).toBeCloseTo(initial.width, 0);
    if (hold === "drag") await page.mouse.up();
  });
}

test("quick view chrome contrast across all six themes in light and dark", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const row = page.locator('.ag-row[data-agent-handle="BUG-142#dev"]').first();
  await expect(row).toBeVisible();
  await row.hover();
  await row.getByTestId("agent-row-cli").click();
  const quick = page.getByTestId("agent-quick-send");
  await expect(quick).toBeVisible();
  for (const theme of [
    "codesense",
    "dracula",
    "github",
    "nord",
    "catppuccin",
    "confluence",
  ] as const) {
    for (const scheme of ["light", "dark"] as const) {
      await setTheme(page, theme, scheme);
      const checks = await quick.evaluate((dialog) => {
        const ctx = document
          .createElement("canvas")
          .getContext("2d", { willReadFrequently: true })!;
        const parse = (css: string) => {
          ctx.clearRect(0, 0, 1, 1);
          ctx.fillStyle = css;
          ctx.fillRect(0, 0, 1, 1);
          return [...ctx.getImageData(0, 0, 1, 1).data].map((v, i) => (i === 3 ? v / 255 : v));
        };
        const over = (a: number[], b: number[]) =>
          [0, 1, 2].map((i) => a[i] * a[3] + b[i] * (1 - a[3])).concat(1);
        const lum = (rgb: number[]) =>
          rgb
            .slice(0, 3)
            .map((v) => v / 255)
            .map((v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4))
            .reduce((a, v, i) => a + v * [0.2126, 0.7152, 0.0722][i], 0);
        const ratio = (a: number[], b: number[]) =>
          (Math.max(lum(a), lum(b)) + 0.05) / (Math.min(lum(a), lum(b)) + 0.05);
        const background = (el: Element | null): number[] => {
          if (!el) return [255, 255, 255, 1];
          const c = parse(getComputedStyle(el).backgroundColor);
          return c[3] === 1 ? c : over(c, background(el.parentElement));
        };
        const results: { what: string; ratio: number; min: number }[] = [];
        const walker = document.createTreeWalker(dialog, NodeFilter.SHOW_TEXT);
        while (walker.nextNode()) {
          const node = walker.currentNode;
          const el = node.parentElement!;
          if (
            !node.textContent?.trim() ||
            el.closest(".ag-quick-send-terminal,.sr-only,[hidden],svg")
          )
            continue;
          const bg = background(el);
          results.push({
            what: node.textContent.trim(),
            ratio: ratio(over(parse(getComputedStyle(el).color), bg), bg),
            min: 4.5,
          });
        }
        const style = getComputedStyle(dialog);
        const bg = background(dialog);
        const edge = parse(style.borderTopColor);
        results.push({ what: "decorative frame edge", ratio: ratio(edge, bg), min: 1.5 });
        const head = dialog.querySelector(".ag-quick-send-head")!;
        results.push({
          what: "decorative header separator",
          ratio: ratio(parse(getComputedStyle(head).borderBottomColor), bg),
          min: 1.5,
        });
        const probe = document.createElement("span");
        dialog.append(probe);
        for (const token of ["--theme-terminal-bg", "--theme-surface-canvas-bg"]) {
          probe.style.background = `var(${token})`;
          results.push({
            what: `edge against ${token}`,
            ratio: ratio(edge, parse(getComputedStyle(probe).backgroundColor)),
            min: 1.5,
          });
        }
        probe.remove();
        // Icons and control/focus boundaries carry meaning; frame edges and separators decorate.
        for (const el of dialog.querySelectorAll(
          ".ag-quick-send-head button, .ag-quick-send-head svg, .ag-quick-send-grip, .cs-mark",
        )) {
          const cs = getComputedStyle(el);
          const backdrop = background(el);
          const control = el.matches("button");
          results.push({
            what: `${el.getAttribute("aria-label") ?? el.textContent ?? el.tagName} ${control ? "control boundary" : "icon/mark"}`,
            ratio: ratio(parse(control ? cs.borderTopColor : cs.color), backdrop),
            min: 3,
          });
        }
        const well = dialog.querySelector<HTMLElement>(".ag-quick-send-terminal");
        if (well) {
          const ring = document.createElement("span");
          ring.style.background = "var(--ag-terminal-focus-ring)";
          well.append(ring);
          results.push({
            what: "terminal focus ring",
            ratio: ratio(parse(getComputedStyle(ring).backgroundColor), background(well)),
            min: 3,
          });
          ring.remove();
        }
        return results;
      });
      for (const check of checks)
        expect
          .soft(check.ratio, `${theme}/${scheme}: ${check.what}`)
          .toBeGreaterThanOrEqual(check.min);
      console.log(
        `${theme}/${scheme}: ${checks.length} contrast checks, ${checks.filter((check) => check.ratio < check.min).length} failures`,
      );
    }
  }
});

test("a parent's header shares the row's Working word and compact subagent count", async ({
  page,
}) => {
  const row = page.locator('.ag-row[data-agent-handle="BUG-142#lead"]').first();
  await expect(row).toBeVisible();
  await row.hover();
  await row.getByTestId("agent-row-cli").click();
  const quick = page.getByTestId("agent-quick-send");
  await expect(quick.locator(".ag-quick-send-state")).toHaveText("Working2");
  await expect(quick.getByRole("img", { name: "2 subagents running" })).toBeVisible();
});

test("CodeSense light keeps a visible focus ring over an explicitly selected Nord terminal", async ({
  page,
}) => {
  await setTheme(page, "codesense", "light");
  await page.evaluate(() =>
    window.__stores!.useViewerStore.getState().updateSettings({ terminalTheme: "nord" }),
  );
  const row = page.locator('.ag-row[data-agent-handle="BUG-142#dev"]').first();
  await row.hover();
  await row.getByTestId("agent-row-cli").click();
  const terminal = page.getByTestId("agent-quick-send").locator(".ag-quick-send-terminal");
  await expect(terminal).toHaveCSS("background-color", "rgb(46, 52, 64)");
  await expect(terminal.locator("textarea")).toBeFocused();
  const contrast = await terminal.evaluate(async (well) => {
    const { contrastRatio } = await new Function(
      "return import('/src/shared/lib/color-contrast.ts')",
    )();
    const ctx = document.createElement("canvas").getContext("2d")!;
    const hex = (css: string) => {
      ctx.fillStyle = css;
      ctx.fillRect(0, 0, 1, 1);
      return (
        "#" +
        [...ctx.getImageData(0, 0, 1, 1).data]
          .slice(0, 3)
          .map((v) => v.toString(16).padStart(2, "0"))
          .join("")
      );
    };
    const style = getComputedStyle(well);
    return contrastRatio(
      hex(style.getPropertyValue("--ag-terminal-focus-ring").trim()),
      hex(style.backgroundColor),
    );
  });
  expect(contrast).toBeGreaterThanOrEqual(3);
});
