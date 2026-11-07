// SPDX-License-Identifier: MIT
import { expect, test, type Locator, type Page } from "@playwright/test";
import type { Workspace } from "@/shared/stores/viewer-store";

const SIZES = ["small", "normal", "large"] as const;

/** Converge on the rendered lane across the titlebar's responsive regimes. */
async function targetStripWidth(page: Page, chip: Locator, target: number, tolerance: number) {
  const atWidth = async (width: number) => {
    await page.setViewportSize({ width, height: 850 });
    await page.evaluate(
      () =>
        new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        ),
    );
    return chip.evaluate((el) => parseFloat(getComputedStyle(el.parentElement!).width));
  };
  let lo = 600,
    hi = 2400;
  expect(await atWidth(lo)).toBeLessThan(target);
  expect(await atWidth(hi)).toBeGreaterThan(target);
  let best = { viewport: lo, error: Infinity };
  while (lo <= hi) {
    const viewport = Math.floor((lo + hi) / 2);
    const measured = await atWidth(viewport);
    const error = Math.abs(measured - target);
    if (error < best.error) best = { viewport, error };
    if (measured < target) lo = viewport + 1;
    else hi = viewport - 1;
  }
  expect(Math.abs((await atWidth(best.viewport)) - target)).toBeLessThan(tolerance);
}

async function boot(page: Page, width: number) {
  await page.setViewportSize({ width, height: 850 });
  await page.goto("/?agents=team");
  await page.waitForFunction(() => Boolean(window.__stores?.useViewerStore));
  await page.locator("[data-workspace-bar] [data-tray-chip]").first().waitFor();
  await page.evaluate(() =>
    window.__stores!.useViewerStore.setState({ workspaceTraySeenCoachmark: true }),
  );
}

/** Seed the actual store; the browser still renders the production tray/portal. */
async function seed(page: Page, placement: "shared" | "console", size: (typeof SIZES)[number]) {
  return page.evaluate(
    ({ placement, size }) => {
      const store = window.__stores!.useViewerStore;
      const state = store.getState();
      const team = Object.keys(state.workspaces).find((key) => state.workspaces[key].length)!;
      const original = state.workspaces[team];
      const extras: Workspace[] = Array.from({ length: 12 }, (_, i) => ({
        id: `generic:/tmp/fx122-${i}`,
        kind: "generic",
        cwd: `/tmp/fx122-${i}-a-long-workspace-label-that-must-fit-the-active-lane`,
        status: "ready",
      }));
      extras[0] = {
        id: "root:fx122/repo",
        kind: "root",
        cwd: "/tmp/fx122-repo",
        sourceRepoSlug: "fx122/repo",
        status: "ready",
      };
      extras[1] = {
        id: "epic:fx122",
        kind: "epic",
        cwd: "/tmp/fx122-epic",
        epicDirPath: "fx122-epic",
        status: "ready",
      };
      extras[2] = {
        ...extras[2],
        id: "generic:/tmp/fx122-remote",
        cwd: "",
        location: {
          kind: "machine",
          machineId: "ci-runner",
          requestId: "fx122-geometry-fixture",
          cwd: "/tmp/fx122-remote",
        },
      };
      const minimized: Workspace = {
        id: "generic:/tmp/fx122-minimized",
        kind: "generic",
        cwd: "/tmp/fx122-minimized-long-workspace",
        status: "ready",
      };
      store.setState({
        workspaces: { ...state.workspaces, [team]: [...original, ...extras] },
        softClosedWorkspaces: {
          ...state.softClosedWorkspaces,
          [team]: { [minimized.id]: { workspace: minimized, layoutMode: placement } },
        },
        activeWorkspace: { ...state.activeWorkspace, [team]: original[0].id },
        trayMode: { ...state.trayMode, [team]: "flat" },
        settings: { ...state.settings, workspaceBarSize: size },
      });
      store.getState().setLayoutMode(placement);
      return { team, original: original.map((w) => w.id), minimized: minimized.id };
    },
    { placement, size },
  );
}

async function geometry(tray: Locator) {
  return tray.evaluate((el) => {
    const chips = Array.from(el.querySelectorAll<HTMLElement>("[data-tray-chip]"));
    const area = chips[0].parentElement!;
    const box = area.getBoundingClientRect();
    const rects = chips.map((chip) => ({
      id: chip.dataset.trayChip!,
      active: chip.dataset.active === "true",
      compact: chip.dataset.compact === "true",
      rect: chip.getBoundingClientRect().toJSON(),
    }));
    const tops = rects.map((c) => c.rect.top);
    return {
      rowSpread: Math.max(...tops) - Math.min(...tops),
      wrap: getComputedStyle(area).flexWrap,
      overflow: getComputedStyle(area).overflowX,
      area: box.toJSON(),
      rects,
      clipped: rects
        .filter((c) => c.rect.left < box.left - 0.5 || c.rect.right > box.right + 0.5)
        .map((c) => c.id),
      pageOverflow: document.documentElement.scrollWidth > window.innerWidth,
    };
  });
}

async function activeFits(tray: Locator) {
  await expect
    .poll(async () => {
      const m = await geometry(tray);
      const active = m.rects.find((c) => c.active)!;
      return (
        !active.compact &&
        active.rect.left >= m.area.left - 1 &&
        active.rect.right <= m.area.right + 1
      );
    })
    .toBe(true);
}

/** Controls must have an unobscured hit point, not merely a DOM node. */
async function controlsReachable(page: Page, tray: Locator) {
  for (const id of ["workspace-tray-launcher", "workspace-tray-mode-toggle"]) {
    const control = page.getByTestId(id);
    await expect(control).toBeVisible();
    await control.click({ trial: true });
  }
  const controls = await page.getByTestId("title-bar").evaluate((el) =>
    Array.from(el.querySelectorAll<HTMLElement>("button"))
      // Workspace/epic choices deliberately scroll and remain reachable via
      // their overflow menus. Pinned window/identity/mode controls must hit.
      .filter(
        (b) =>
          !b.closest("[data-tray-chip], [data-epic-rail-pill]") &&
          b.getBoundingClientRect().width > 0,
      )
      .map((b) => {
        const r = b.getBoundingClientRect();
        const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        return {
          name: b.getAttribute("aria-label") ?? b.textContent,
          reachable: b.contains(hit),
          inside: r.left >= 0 && r.right <= window.innerWidth + 0.5,
        };
      }),
  );
  expect(controls.length).toBeGreaterThan(0);
  expect(controls.filter((c) => !c.reachable || !c.inside)).toEqual([]);
  const m = await geometry(tray);
  expect(m.pageOverflow).toBe(false);
}

for (const width of [600, 1824]) {
  for (const placement of ["shared", "console"] as const) {
    for (const size of SIZES) {
      test(`${placement}, ${width}px, ${size}: measured overflow, full active fit and controls`, async ({
        page,
      }) => {
        await boot(page, width);
        const fixture = await seed(page, placement, size);
        const tray = page.getByTestId("workspace-tray");
        await expect(tray.locator("[data-tray-chip]")).toHaveCount(17);
        await activeFits(tray);
        const m = await geometry(tray);
        expect(m.rowSpread).toBeLessThanOrEqual(1);
        expect(m.wrap).toBe("nowrap");
        expect(m.overflow).toBe("auto");
        expect(m.rects.some((c) => c.compact)).toBe(true);
        const overflow = tray.getByTestId("workspace-tray-overflow");
        await expect(overflow).toHaveText(new RegExp(`\\+${m.clipped.length}\\b`));
        await overflow.click();
        const rows = page.locator('[data-testid^="workspace-tray-hidden-row-"]');
        await expect(rows).toHaveCount(m.clipped.length);
        // Select a measured clipped item; neither count nor position is assumed.
        const target = m.clipped.find(
          (id) => id.startsWith("generic:/tmp/fx122-") && !id.endsWith("remote"),
        )!;
        expect(target).toBeTruthy();
        const row = page.getByTestId(`workspace-tray-hidden-row-${target}`);
        if (size === "small") {
          await row.focus();
          await page.keyboard.press("Enter");
        } else {
          await row.click();
        }
        await expect(page.getByTestId("workspace-tray-overflow-menu")).toBeHidden();
        await expect(tray.locator(`[data-tray-chip="${target}"]`)).toHaveAttribute(
          "data-active",
          "true",
        );
        await activeFits(tray);
        // Late labels and a bar-size/viewport change must keep active identity visible.
        await page.evaluate(
          ({ team, target }) => {
            const store = window.__stores!.useViewerStore;
            store.setState((s) => ({
              workspaces: {
                ...s.workspaces,
                [team]: s.workspaces[team].map((w) =>
                  w.id === target ? { ...w, cwd: `${w.cwd}-late-label-arrived` } : w,
                ),
              },
            }));
          },
          { team: fixture.team, target },
        );
        await activeFits(tray);
        await expect(tray.locator(`[data-tray-chip="${target}"] [data-ws-row]`)).toHaveAttribute(
          "aria-label",
          /late-label-arrived/,
        );
        await page.setViewportSize({ width: width + 120, height: 850 });
        await activeFits(tray);
        await page.evaluate(() =>
          window.__stores!.useViewerStore.getState().updateSettings({ workspaceBarSize: "large" }),
        );
        await activeFits(tray);
        await page.setViewportSize({ width, height: 850 });
        await page.evaluate(
          (workspaceBarSize) =>
            window.__stores!.useViewerStore.getState().updateSettings({ workspaceBarSize }),
          size,
        );
        await activeFits(tray);
        await controlsReachable(page, tray);
        await test.info().attach("rectangles", {
          body: JSON.stringify(await geometry(tray), null, 2),
          contentType: "application/json",
        });
        await page.screenshot({ path: test.info().outputPath("single-row.png") });
      });
    }
  }
}

type ContentCase = {
  placement: "shared" | "console";
  width: number;
  size: (typeof SIZES)[number];
  identity: "local" | "archived" | "remote";
  presentation: string;
  hiddenCountBoundary?: 100;
};
const contentCases: ContentCase[] = (["shared", "console"] as const).flatMap((placement) =>
  [600, 1824].flatMap((width) =>
    SIZES.flatMap((size) =>
      (["local", "archived", "remote"] as const).map((identity) => ({
        placement,
        width,
        size,
        identity,
        presentation: "plain",
      })),
    ),
  ),
);
for (const placement of ["shared", "console"] as const) {
  for (const presentation of ["glyph", "setup", "numbered", "combined"] as const) {
    for (const size of presentation === "combined" ? SIZES : (["normal"] as const)) {
      contentCases.push({ placement, width: 600, size, identity: "remote", presentation });
    }
  }
  for (const width of [600, 1824]) {
    for (const size of SIZES) {
      for (const profile of ["plain", "combined"]) {
        contentCases.push({
          placement,
          width,
          size,
          identity: "remote",
          presentation: `hidden-${profile}`,
        });
      }
    }
  }
  for (const profile of ["glyph", "setup", "numbered"]) {
    contentCases.push({
      placement,
      width: 600,
      size: "normal",
      identity: "remote",
      presentation: `hidden-${profile}`,
    });
  }
}
// Reuse the complete supported narrow profiles and the same host/Restore/paint
// journey. The original cases still receive ten; only these receive 99→100.
contentCases.push(
  ...contentCases
    .filter(
      (c) => c.placement === "console" && c.width === 600 && c.presentation.startsWith("hidden-"),
    )
    .map((c): ContentCase => ({ ...c, hiddenCountBoundary: 100 })),
);
for (const {
  placement,
  width,
  size,
  identity,
  presentation,
  hiddenCountBoundary,
} of contentCases) {
  test(`${placement}, ${width}px, ${size}, ${identity}, ${presentation}${hiddenCountBoundary ? ", 99→100→99" : ""}: active story contents survive late crew and sticky operation`, async ({
    page,
  }) => {
    // The longest journey: every extra at once, swept through nine real strip widths before and
    // after the Hidden-tabs round trips. Its geometry checks are the others'; it needs more time.
    if (presentation === "hidden-combined") test.slow();
    const resizeErrors: string[] = [];
    page.on("pageerror", (e) => {
      if (/ResizeObserver loop/.test(e.message)) resizeErrors.push(e.message);
    });
    const hiddenProfile = presentation.startsWith("hidden-");
    const profile = presentation.replace(/^hidden-/, "");
    await boot(page, width);
    const { team } = await seed(page, placement, size);
    if (["glyph", "setup"].includes(profile)) {
      await page.evaluate(async (team) => {
        const { useAgentListProjectionStore } =
          await import("/src/features/viewer/lib/agent-list-projection.ts");
        const lead = useAgentListProjectionStore
          .getState()
          .sessions!.find((r) => r.sessionId === "sess-login-lead")!;
        window
          .__stores!.useViewerStore.getState()
          .setAgentIdentity(team, lead.handle, { face: "GL" });
      }, team);
    }
    await page.evaluate(
      async ({ team, identity }) => {
        if (identity === "remote") {
          const store = window.__stores!.useViewerStore;
          store.setState((s) => ({
            workspaces: {
              ...s.workspaces,
              [team]: s.workspaces[team].map((w) =>
                w.id === "BUG-142"
                  ? {
                      ...w,
                      cwd: "",
                      location: {
                        kind: "machine",
                        machineId: "ci-runner",
                        requestId: "fx122-active-remote",
                        handle: "machine://ci-runner/fx122-story",
                        cwd: "/remote/fx122-story",
                      },
                    }
                  : w,
              ),
            },
          }));
        }
        const { activateAndFocusTrayWorkspace } =
          await import("/src/features/viewer/lib/focus-cascade.ts");
        activateAndFocusTrayWorkspace(team, "BUG-142");
      },
      { team, identity },
    );
    const tray = page.getByTestId("workspace-tray");
    const chip = page.locator('[data-tray-chip="BUG-142"]');
    const crew = page.getByTestId("workspace-tab-BUG-142-crew");
    // On the one-row bar the lead's face stands for the crew, and the count says how many.
    // At its tightest (a strip under 240px) the face gives way; the tally keeps the count.
    await expect(crew.locator(".ag-crew-face")).toHaveCount(0);
    await expect(page.getByTestId("workspace-tab-BUG-142-count")).toHaveText("4");
    await activeFits(tray);
    const contents = () =>
      chip.evaluate((el) => {
        const card = el.getBoundingClientRect();
        const button = el.querySelector<HTMLButtonElement>("[data-ws-row]")!;
        const label = button.querySelector<HTMLElement>(".truncate")!;
        const rect = (n: Element) => n.getBoundingClientRect().toJSON();
        const visible = Array.from(
          el.querySelectorAll<HTMLElement>("button, svg, .ag-avatar, .ag-face, [data-mark]"),
        ).filter((n) => n.getBoundingClientRect().width > 0 && !n.closest(".sr-only"));
        const operation = el.querySelector<HTMLElement>('[data-testid="operation-chip"]');
        const status = el.querySelector<HTMLElement>(
          '[data-testid="workspace-tab-BUG-142-ai-status-dot"]',
        )!;
        const crew = el.querySelector<HTMLElement>('[data-testid="workspace-tab-BUG-142-crew"]')!;
        const hit = (n: HTMLElement) => {
          const r = n.getBoundingClientRect();
          return n.contains(document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2));
        };
        const parts = Object.entries({
          label,
          status,
          crew,
          lead: crew.querySelector<HTMLElement>(".ag-crew-lead"),
          role: crew.querySelector<HTMLElement>(".ag-crew-lead .ag-piece"),
          followers: crew.querySelector<HTMLElement>(".ag-crew-faces"),
          more: crew.querySelector<HTMLElement>(".ag-crew-more"),
          identity: el.querySelector<HTMLElement>('[data-testid="workspace-tab-BUG-142-machine"]'),
          machine: el.querySelector<HTMLElement>(
            '[data-testid="workspace-tab-BUG-142-machine-mark"]',
          ),
          archive: el.querySelector<HTMLElement>('[data-testid="workspace-tab-BUG-142-archived"]'),
          glyph: el.querySelector<HTMLElement>('[data-testid="workspace-tab-BUG-142-repo-glyph"]'),
          setup: el.querySelector<HTMLElement>('[data-testid="workspace-tab-BUG-142-setup"]'),
          tag: crew.querySelector<HTMLElement>(".ag-crew-lead .ag-face-tag"),
          ...Object.fromEntries(
            Array.from(crew.querySelectorAll<HTMLElement>(".ag-crew-face .ag-face-tag")).map(
              (node, i) => [`tag-follower-${i}`, node],
            ),
          ),
          close: el.querySelector<HTMLElement>('[data-testid="workspace-tab-BUG-142-close"]'),
          operation,
          operationIcon: operation?.querySelector<HTMLElement>("svg"),
          hidden: el.querySelector<HTMLElement>('[data-testid="tray-chip-hidden-BUG-142"]'),
          shortcut: el.querySelector<HTMLElement>("[data-tray-shortcut]"),
          stage: el.querySelector<HTMLElement>('button[aria-label^="Change status"]'),
          hiddenEye: el.querySelector<HTMLElement>(".cs-workspace-hidden-eye"),
          hiddenCount: el.querySelector<HTMLElement>(".cs-workspace-hidden-count"),
          hiddenMark: el.querySelector<HTMLElement>(
            '[data-testid^="tray-chip-hidden-status-"], [data-testid^="tray-chip-hidden-ended-"]',
          ),
        }).filter(
          (entry): entry is [string, HTMLElement] =>
            Boolean(entry[1]) && entry[1]!.getBoundingClientRect().width > 0,
        );
        const allocations = parts.map(([name, node]) => {
          let parent = name.startsWith("tag") || name === "role" ? crew : node.parentElement!;
          while (getComputedStyle(parent).display === "contents") parent = parent.parentElement!;
          const r = node.getBoundingClientRect();
          const p = parent.getBoundingClientRect();
          const halo = node.querySelector<SVGCircleElement>(".cs-mark-halo");
          const svg = halo?.ownerSVGElement;
          const painted =
            halo && svg
              ? (() => {
                  const box = svg.getBoundingClientRect();
                  const scale = box.width / svg.viewBox.baseVal.width;
                  const radius =
                    halo.r.baseVal.value + parseFloat(getComputedStyle(halo).strokeWidth) / 2;
                  return {
                    left: box.left + (halo.cx.baseVal.value - radius) * scale,
                    right: box.left + (halo.cx.baseVal.value + radius) * scale,
                    top: box.top + (halo.cy.baseVal.value - radius) * scale,
                    bottom: box.top + (halo.cy.baseVal.value + radius) * scale,
                  };
                })()
              : (() => {
                  // Existing role/tag outer shadows are paint, not border geometry.
                  const shadow =
                    name.startsWith("tag") || name === "role"
                      ? getComputedStyle(node)
                          .boxShadow.split(/,(?![^()]*\))/)
                          .find((s) => !s.includes("inset"))
                      : undefined;
                  const lengths = shadow?.match(/-?[\d.]+px/g)?.map(parseFloat) ?? [];
                  const scale = card.width / parseFloat(getComputedStyle(el).width);
                  const [x = 0, y = 0, blur = 0, spread = 0] = lengths;
                  const extent = (blur + spread) * scale;
                  return {
                    left: r.left + x * scale - extent,
                    right: r.right + x * scale + extent,
                    top: r.top + y * scale - extent,
                    bottom: r.bottom + y * scale + extent,
                  };
                })();

          return {
            name,
            rect: r.toJSON(),
            parent: p.toJSON(),
            painted,
            paintInside:
              painted.left >= p.left - 0.5 &&
              painted.right <= p.right + 0.5 &&
              painted.top >= p.top - 0.5 &&
              painted.bottom <= p.bottom + 0.5 &&
              painted.left >= card.left - 0.5 &&
              painted.right <= card.right + 0.5 &&
              painted.top >= card.top - 0.5 &&
              painted.bottom <= card.bottom + 0.5,
            hit: hit(node),
            inside:
              r.left >= p.left - 0.5 &&
              r.right <= p.right + 0.5 &&
              r.top >= p.top - 0.5 &&
              r.bottom <= p.bottom + 0.5,
          };
        });
        const overlaps = parts.flatMap(([a, node], i) =>
          parts.slice(i + 1).flatMap(([b, other]) => {
            if (node.contains(other) || other.contains(node)) return [];
            // The ⌘ number and the close × share the end slot on purpose: one at rest, one on hover.
            if ([a, b].includes("shortcut") && [a, b].includes("close")) return [];
            const r = node.getBoundingClientRect();
            const s = other.getBoundingClientRect();
            return Math.min(r.right, s.right) - Math.max(r.left, s.left) > 0.5 &&
              Math.min(r.bottom, s.bottom) - Math.max(r.top, s.top) > 0.5
              ? [[a, b]]
              : [];
          }),
        );
        return {
          card: card.toJSON(),
          faceShown: getComputedStyle(crew).display !== "none",
          logicalIdentityWidth: parseFloat(
            getComputedStyle(el.querySelector(".cs-workspace-pill-identity") ?? label).width,
          ),
          logicalCardHeight: parseFloat(getComputedStyle(el).height),
          logicalContainerWidth: parseFloat(getComputedStyle(el.parentElement!).width),
          label: rect(label),
          outside: visible
            .filter((n) => {
              const r = n.getBoundingClientRect();
              return r.left < card.left - 1 || r.right > card.right + 1;
            })
            .map((n) => ({
              tag: n.tagName,
              cls: n.className.baseVal ?? n.className,
              rect: rect(n),
            })),
          labelHit: hit(label),
          allocations,
          overlaps,
          status: { ...rect(status), hit: hit(status) },
          crew: { ...rect(crew), hit: hit(crew) },
          operation: operation
            ? {
                rect: rect(operation),
                hit: operation.contains(
                  document.elementFromPoint(
                    operation.getBoundingClientRect().left +
                      operation.getBoundingClientRect().width / 2,
                    operation.getBoundingClientRect().top +
                      operation.getBoundingClientRect().height / 2,
                  ),
                ),
              }
            : null,
        };
      });
    const leaveTooltip = async () => {
      await page.keyboard.press("Escape");
      // Click clear chrome to leave both pointer and focus tooltips, without
      // locator auto-scrolling a terminal canvas across its tab controls.
      expect(
        await page.evaluate(
          () =>
            !document
              .elementFromPoint(1, 1)
              ?.closest('button, a, input, [data-slot="tooltip-trigger"]'),
        ),
      ).toBe(true);
      await page.mouse.click(1, 1);
      await page.keyboard.press("Escape");
      await expect(page.locator('[data-slot="tooltip-content"]')).toHaveCount(0);
    };
    const usable = async () => {
      const receivingTray = (await page.getByTestId("workspace-tray-children").count())
        ? page.getByTestId("workspace-tray-children")
        : tray;
      await activeFits(receivingTray);
      await leaveTooltip();
      const main = chip.locator("[data-ws-row]");
      const clearPoint = await main.evaluate((el) => {
        const r = el.getBoundingClientRect();
        for (let y = r.top + 1; y < r.bottom - 1; y += 1)
          for (let x = r.left + 1; x < r.right - 1; x += 1) {
            const hit = document.elementFromPoint(x, y);
            if (hit && el.contains(hit) && !hit.closest('[data-slot="tooltip-trigger"]'))
              return { x, y };
          }
        return null;
      });
      expect(clearPoint).not.toBeNull();
      await page.mouse.move(clearPoint!.x, clearPoint!.y);
      await expect(page.locator('[data-slot="tooltip-content"]')).toHaveCount(0);
      await expect.poll(async () => (await contents()).label.width).toBeGreaterThanOrEqual(32);
      await expect.poll(async () => (await contents()).outside).toEqual([]);
      const m = await contents();
      if (
        m.allocations.some(
          (a) =>
            !a.inside ||
            (!a.name.startsWith("tag") &&
              ![
                "operationIcon",
                "tag",
                "shortcut",
                "hiddenEye",
                "hiddenCount",
                "role",
                "followers",
                "more",
              ].includes(a.name) &&
              !a.hit),
        ) ||
        m.overlaps.length > 0
      ) {
        await test.info().attach("allocation-failure", {
          body: JSON.stringify(m, null, 2),
          contentType: "application/json",
        });
      }
      expect(m.allocations.filter((a) => !a.inside)).toEqual([]);
      expect(m.overlaps).toEqual([]);
      expect(m.allocations.filter((a) => !a.paintInside)).toEqual([]);
      expect(await chip.locator("button button").count()).toBe(0);
      expect(m.logicalCardHeight).toBeLessThanOrEqual(40);
      if (identity === "remote") expect(m.logicalIdentityWidth).toBeGreaterThanOrEqual(47);
      expect(m.status.width).toBeGreaterThan(0);
      if (m.faceShown) {
        expect(m.crew.width).toBeGreaterThan(0);
        expect(m.crew.hit).toBe(true);
      }
      expect(m.labelHit).toBe(true);
      expect(m.status.hit).toBe(true);
      expect(
        m.allocations.every(
          (a) =>
            a.name.startsWith("tag") ||
            [
              "operationIcon",
              "tag",
              "shortcut",
              "hiddenEye",
              "hiddenCount",
              "role",
              "followers",
              "more",
            ].includes(a.name) ||
            a.hit,
        ),
      ).toBe(true);
      const close = page.getByTestId("workspace-tab-BUG-142-close");
      // A locator trial click calls scrollIntoView before checking hits. With
      // CSS zoom that can move an already visible card. Receive the actual
      // native target at center/insets without introducing an artificial scroll;
      // disposable-workspace cases below execute the real Close gesture.
      expect(
        await close.evaluate((el) => {
          const r = el.getBoundingClientRect();
          return [
            [0.5, 0.5],
            [0.25, 0.25],
            [0.75, 0.75],
          ].every(([x, y]) =>
            el.contains(document.elementFromPoint(r.left + r.width * x, r.top + r.height * y)),
          );
        }),
      ).toBe(true);
      await activeFits(receivingTray);
      return m;
    };
    const before = await usable();
    await page.evaluate(async () => {
      const { useAgentListProjectionStore } =
        await import("/src/features/viewer/lib/agent-list-projection.ts");
      const lead = useAgentListProjectionStore
        .getState()
        .sessions!.find((r) => r.sessionId === "sess-login-lead")!;
      // The mock backend projects it to the app like a teammate's launch would arrive.
      (
        window as unknown as { __mockAgents: { addSession(row: unknown): void } }
      ).__mockAgents.addSession({
        ...lead,
        handle: "BUG-142#late-shell-agent",
        sessionId: "sess-login-shell",
        displayName: "Late crew arrival",
        launchedBy: lead.handle,
      });
    });
    await expect(page.getByTestId("workspace-tab-BUG-142-count")).toHaveText("5");
    // The pill is described by its crew's words and then by the keys that move it.
    await expect(chip.locator("[data-ws-row]")).toHaveAccessibleDescription(
      /5 agents, led by Login timeout lead/,
    );
    const grown = await usable();
    const setArchive = async (archived: boolean) => {
      await page.evaluate(async (archived) => {
        const { useWorkspaceArchiveStore } =
          await import("/src/shared/stores/workspace-archive-store.ts");
        useWorkspaceArchiveStore.getState().applyChange({
          workspaceId: "BUG-142",
          archivedAtMs: archived ? Date.now() : null,
        });
      }, archived);
      if (archived)
        await expect(page.getByTestId("workspace-tab-BUG-142-archived")).toHaveAttribute(
          "aria-label",
          "Workspace archived",
        );
      else await expect(page.getByTestId("workspace-tab-BUG-142-archived")).toHaveCount(0);
    };
    if (identity === "archived") await setArchive(true);
    const beforeOperation = await usable();
    await page.evaluate(async () => {
      const { useContextOperationsStore } =
        await import("/src/features/shell/stores/context-operations-store.ts");
      const { reducePrepareProgress } =
        await import("/src/features/agents/lib/agent-prepare-progress.ts");
      const store = window.__stores!.useViewerStore.getState();
      const ws = Object.values(store.workspaces)
        .flat()
        .find((w) => w.id === "BUG-142")!;
      useContextOperationsStore.getState().apply(
        reducePrepareProgress(undefined, {
          workspace: ws.location?.kind === "machine" ? ws.location.handle! : ws.cwd,
          agent: "explicit",
          operationId: "fx122-long-sticky",
          operationKind: "publish",
          phase: "ready",
          files: 123456,
          total: 999999,
          bytes: 0,
          note: "Published 123456 change(s); 876543 conflict(s) not written.",
        }),
      );
    });
    const operation = chip.getByTestId("operation-chip");
    await expect(operation).toHaveAttribute(
      "aria-label",
      /Published 123,456 changes.*876,543 conflicts.*Show details/,
    );
    const supported: Record<string, Awaited<ReturnType<typeof contents>>> = {};
    if (profile !== "plain") {
      await setArchive(true);
      if (["glyph", "combined"].includes(profile))
        // The team's source repos load on their own schedule: wait for one of its own.
        await expect
          .poll(() =>
            page.evaluate(async (team) => {
              const { useOrgStore } = await import("/src/shared/stores/org-store.ts");
              const { parseTeamKey } = await import("/src/shared/lib/team-key.ts");
              const { orgSlug, teamName } = parseTeamKey(team);
              const prefix = `workspace:${orgSlug}:${teamName}:`;
              return Object.keys(useOrgStore.getState().workspaceRepos).filter((k) =>
                k.startsWith(prefix),
              ).length;
            }, team),
          )
          .toBeGreaterThan(0);
      await page.evaluate(
        async ({ team, profile }) => {
          const store = window.__stores!.useViewerStore;
          if (["glyph", "combined"].includes(profile)) {
            const { useOrgStore } = await import("/src/shared/stores/org-store.ts");
            const { parseTeamKey } = await import("/src/shared/lib/team-key.ts");
            const { syntheticRepoKey } = await import("/src/shared/lib/build-synthetic-repo.ts");
            const { orgSlug, teamName } = parseTeamKey(team);
            const repos = useOrgStore.getState().workspaceRepos;
            const base = Object.values(repos).find((r) =>
              r.repoPath.startsWith(`workspace:${orgSlug}:${teamName}:`),
            );
            if (!base || !teamName) throw new Error("Missing real mock team repo");
            const next = { ...repos };
            for (const repo of ["first-service", "second-service"]) {
              const repoPath = syntheticRepoKey(orgSlug, teamName, { owner: "fx122", repo });
              next[repoPath] = { ...base, repoPath, sourceRepoSlug: `fx122/${repo}` };
            }
            useOrgStore.setState({ workspaceRepos: next });
            store.setState((s) => ({
              workspaces: {
                ...s.workspaces,
                [team]: s.workspaces[team].map((w) =>
                  w.id === "BUG-142" ? { ...w, sourceRepoSlug: "fx122/first-service" } : w,
                ),
              },
            }));
          }
          if (["setup", "combined"].includes(profile))
            store.getState().failAgentRun(team, "BUG-142", "FX receiver", "Disk full");
          if (["numbered", "combined"].includes(profile)) {
            const { useAgentListProjectionStore } =
              await import("/src/features/viewer/lib/agent-list-projection.ts");
            const rows = useAgentListProjectionStore.getState().sessions!;
            for (const id of ["sess-login-lead", "sess-login-shell"]) {
              const row = rows.find((r) => r.sessionId === id)!;
              store.getState().setAgentIdentity(team, row.handle, { face: "FX" });
            }
          }
        },
        { team, profile },
      );
      await expect
        .poll(() =>
          page.evaluate(
            (team) =>
              window
                .__stores!.useViewerStore.getState()
                .workspaces[team].find((w) => w.id === "BUG-142")!.status,
            team,
          ),
        )
        .toBe("ready");
      if (["glyph", "combined"].includes(profile))
        await expect(page.getByTestId("workspace-tab-BUG-142-repo-glyph")).toHaveText("FR");
      if (["setup", "combined"].includes(profile))
        await expect(page.getByTestId("workspace-tab-BUG-142-setup")).toHaveAttribute(
          "data-tone",
          "failed",
        );
      if (["numbered", "combined"].includes(profile))
        await expect(crew.locator(".ag-crew-lead .ag-face-tag")).toHaveText(/^[0-9]+$/);
      supported.failed = await usable();
      if (["setup", "combined"].includes(profile)) {
        const setup = page.getByTestId("workspace-tab-BUG-142-setup");
        const hoverSetup = async () => {
          // Enter once at the verified mark center. A second locator.hover after
          // the bubble opens can ask Playwright to auto-scroll its trigger away
          // from the bubble, moving the strip without a user scroll gesture.
          const setupBox = (await setup.boundingBox())!;
          const point = { x: setupBox.x + setupBox.width / 2, y: setupBox.y + setupBox.height / 2 };
          expect(
            await setup.evaluate(
              (el, point) => el.contains(document.elementFromPoint(point.x, point.y)),
              point,
            ),
          ).toBe(true);
          await page.mouse.move(point.x, point.y);
        };
        await leaveTooltip();
        await hoverSetup();
        await expect(
          page.getByRole("tooltip", {
            name: "Couldn't prepare FX receiver — disk full",
            exact: true,
          }),
        ).toContainText("Couldn't prepare FX receiver — disk full");
        await page.evaluate(async () => {
          const store = window.__stores!.useViewerStore;
          const { reducePrepareProgress } =
            await import("/src/features/agents/lib/agent-prepare-progress.ts");
          const ws = Object.values(store.getState().workspaces)
            .flat()
            .find((w) => w.id === "BUG-142")!;
          const state = reducePrepareProgress(undefined, {
            workspace: ws.location!.kind === "machine" ? ws.location!.handle! : ws.cwd,
            agent: "FX receiver",
            phase: "transfer",
            files: 4,
            total: 10,
            bytes: 100,
          });
          store.getState().applyAgentPrepareProgress(state);
        });
        await expect(setup).toHaveAttribute("data-tone", "busy");
        supported.running = await usable();
        await leaveTooltip();
        await hoverSetup();
        await expect(
          page.getByRole("tooltip", { name: "Preparing FX receiver", exact: true }),
        ).toContainText("Preparing FX receiver");
      }
      if (profile === "combined") {
        // Receive both sides of the existing284cqw decoration boundary,
        // and the full-mark allocation boundary, through real pane widths.
        if (placement === "shared")
          await page.evaluate(() =>
            window.__stores!.useViewerStore.setState({ rightPaneOpen: true, rightPaneRatio: 0.4 }),
          );
        await page.evaluate(
          () =>
            new Promise<void>((resolve) =>
              requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
            ),
        );
        for (const target of [270, 300, 400]) {
          await targetStripWidth(page, chip, target, 3);
          supported[`cqw-${target}`] = await usable();
        }
        await page.evaluate(() =>
          window.__stores!.useViewerStore.setState({ rightPaneOpen: false }),
        );
        await page.setViewportSize({ width, height: 850 });
      }
    }
    if (hiddenProfile) {
      const hidden = await page.evaluate(async (team) => {
        const { selectWorkspaceTabs, selectHiddenTerminalTabs } =
          await import("/src/shared/stores/viewer-store.ts");
        const { hideTab } = await import("/src/features/viewer/lib/terminal-close.ts");
        const s = window.__stores!.useViewerStore.getState();
        const shown = selectWorkspaceTabs(s, team, "BUG-142");
        const tab =
          shown.find((t) => t.sessionId === "sess-login-shell") ??
          shown.find((t) => t.sessionId !== "sess-login-lead");
        if (!tab || shown.length < 2)
          throw new Error("Need real shown nonlead tab and retained shown member");
        if (selectHiddenTerminalTabs(s, team, "BUG-142").length)
          throw new Error("Red starts with no user-hidden tabs");
        hideTab(team, "BUG-142", tab.id);
        return {
          tab: { id: tab.id, sessionId: tab.sessionId, name: tab.name },
          before: shown.map((t) => ({ id: t.id, sessionId: t.sessionId, name: t.name })),
        };
      }, team);
      await expect(page.getByTestId("tray-chip-hidden-BUG-142")).toHaveAttribute(
        "aria-label",
        /^1 hidden tab — restore/,
      );
      const after = await page.evaluate(async (team) => {
        const { selectWorkspaceTabs, selectHiddenTerminalTabs } =
          await import("/src/shared/stores/viewer-store.ts");
        const s = window.__stores!.useViewerStore.getState();
        return {
          shown: selectWorkspaceTabs(s, team, "BUG-142").map((t) => ({
            id: t.id,
            sessionId: t.sessionId,
            name: t.name,
          })),
          hidden: selectHiddenTerminalTabs(s, team, "BUG-142").map((t) => ({
            id: t.id,
            sessionId: t.sessionId,
            name: t.name,
          })),
          layout: s.terminalLayout,
        };
      }, team);
      expect(after.hidden).toContainEqual(hidden.tab);
      expect(after.shown.length).toBeGreaterThan(0);
      expect(after.shown.some((t) => t.sessionId === "sess-login-lead")).toBe(true);
      await test.info().attach("actual-hide-membership", {
        body: JSON.stringify({ hidden, after }, null, 2),
        contentType: "application/json",
      });
      await test.info().attach("hide-one-geometry", {
        body: JSON.stringify(await contents(), null, 2),
        contentType: "application/json",
      });
      await page.screenshot({ path: test.info().outputPath("hide-one-before-oracles.png") });
    }
    const hiddenReceiving: Record<string, unknown> = {};
    if (hiddenProfile) {
      const badge = page.getByTestId("tray-chip-hidden-BUG-142");
      const membership = () =>
        page.evaluate(async (team) => {
          const { selectWorkspaceTabs, selectHiddenTerminalTabs } =
            await import("/src/shared/stores/viewer-store.ts");
          const s = window.__stores!.useViewerStore.getState();
          return {
            shown: selectWorkspaceTabs(s, team, "BUG-142").map((t) => ({
              id: t.id,
              sessionId: t.sessionId,
              name: t.name,
            })),
            hidden: selectHiddenTerminalTabs(s, team, "BUG-142").map((t) => ({
              id: t.id,
              sessionId: t.sessionId,
              name: t.name,
            })),
            lives: s.terminalLives,
          };
        }, team);
      const restore = async (gesture: "pointer" | "Enter" | "Space") => {
        let viewportToRestore: { width: number; height: number } | undefined;
        const before = await membership();
        const tab = before.hidden[before.hidden.length - 1];
        expect(tab).toBeTruthy();
        await page.evaluate(() => {
          const w = window as unknown as {
            __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
            __fxRestoreCalls?: string[];
            __fxOriginalInvoke?: (cmd: string, args?: unknown) => Promise<unknown>;
          };
          const original = w.__TAURI_INTERNALS__.invoke;
          w.__fxOriginalInvoke = original;
          w.__fxRestoreCalls = [];
          w.__TAURI_INTERNALS__.invoke = (cmd, args) => {
            w.__fxRestoreCalls!.push(cmd);
            return original(cmd, args);
          };
        });

        if (await badge.isVisible()) {
          if (gesture === "pointer") await badge.click();
          else {
            await badge.focus();
            await page.keyboard.press(gesture);
          }
        } else {
          // At its tightest step (about one pill wide) the strip sheds the badge; the pill's own
          // menu lists the same Hidden tabs to restore.
          const pill = chip.locator("[data-ws-row]");
          if (gesture === "pointer") await pill.click({ button: "right" });
          else {
            await pill.focus();
            await page.keyboard.press("Shift+F10");
          }
        }
        const item = page.getByRole("menuitem", { name: `Restore “${tab.name}”`, exact: true });
        await expect(item).toBeVisible();
        if (hiddenCountBoundary) {
          const captureVisible = async (target: typeof item, phase: string) => {
            const geometry = await target.evaluate((el) => {
              const menu = el.closest<HTMLElement>('[role="menu"]')!;
              const rect = el.getBoundingClientRect();
              return {
                viewport: { width: window.innerWidth, height: window.innerHeight },
                menu: menu.getBoundingClientRect().toJSON(),
                item: rect.toJSON(),
                clientHeight: menu.clientHeight,
                scrollHeight: menu.scrollHeight,
                scrollTop: menu.scrollTop,
                overflowY: getComputedStyle(menu).overflowY,
                focused: document.activeElement === el,
                hit: el.contains(
                  document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2),
                ),
              };
            });
            await test
              .info()
              .attach(`boundary-restore-menu-${gesture}-${before.hidden.length}-${phase}`, {
                body: JSON.stringify(geometry, null, 2),
                contentType: "application/json",
              });
            expect(geometry.menu.top).toBeGreaterThanOrEqual(7.5);
            expect(geometry.menu.bottom).toBeLessThanOrEqual(geometry.viewport.height - 7.5);
            // scrollTop is whole pixels and the menu's rows are not, so a focused item at either
            // end can sit half a pixel past the menu's edge.
            expect(geometry.item.top).toBeGreaterThanOrEqual(geometry.menu.top - 0.5);
            expect(geometry.item.bottom).toBeLessThanOrEqual(geometry.menu.bottom + 0.5);
            expect(geometry.hit).toBe(true);
            return geometry;
          };
          if (gesture === "pointer") {
            await item.scrollIntoViewIfNeeded();
            await captureVisible(item, "pointer");
          } else {
            const menu = page.getByRole("menu");
            await page.keyboard.press("End");
            expect((await captureVisible(menu.locator(":focus"), "End")).focused).toBe(true);
            await page.keyboard.press("Home");
            expect((await captureVisible(menu.locator(":focus"), "Home")).focused).toBe(true);
            await page.keyboard.press("ArrowUp");
            expect((await captureVisible(menu.locator(":focus"), "wrap-last")).focused).toBe(true);
            await page.keyboard.press("ArrowDown");
            expect((await captureVisible(menu.locator(":focus"), "wrap-first")).focused).toBe(true);
            await page.keyboard.press("End");
            if (
              size === "normal" &&
              presentation === "hidden-plain" &&
              gesture === "Enter" &&
              before.hidden.length === 100
            ) {
              // R08-V1: resize this same open menu with its last item focused.
              // Width, cursor anchor and menu instance stay unchanged.
              const viewport = page.viewportSize()!;
              const openMenu = (await menu.elementHandle())!;
              const lastItem = (await menu.locator(":focus").elementHandle())!;
              expect(
                (await captureVisible(menu.locator(":focus"), "before-height-shrink")).focused,
              ).toBe(true);
              await page.setViewportSize({ width: viewport.width, height: 650 });
              await page.evaluate(
                () =>
                  new Promise<void>((resolve) =>
                    requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
                  ),
              );
              expect(
                await openMenu.evaluate(
                  (el) => el.isConnected && el === document.querySelector('[role="menu"]'),
                ),
              ).toBe(true);
              expect(await lastItem.evaluate((el) => el === document.activeElement)).toBe(true);
              const resized = await captureVisible(menu.locator(":focus"), "after-height-shrink");
              expect(resized.viewport).toEqual({ width: viewport.width, height: 650 });
              expect(resized.focused).toBe(true);
              viewportToRestore = viewport;
            }
            // Walk the actual existing menu order to the last hidden Restore;
            // no direct focus or off-screen locator activation.
            const items = menu.locator(
              '[role="menuitem"]:not([disabled]):not([aria-disabled="true"])',
            );
            const targetIndex = await items.evaluateAll(
              (elements, name) => elements.findIndex((el) => el.textContent?.trim() === name),
              `Restore “${tab.name}”`,
            );
            expect(targetIndex).toBeGreaterThanOrEqual(0);
            const steps = (await items.count()) - 1 - targetIndex;
            for (let i = 0; i < steps; i++) await page.keyboard.press("ArrowUp");
            await expect(item).toBeFocused();
            expect((await captureVisible(item, "ArrowUp")).focused).toBe(true);
          }
        }
        if (gesture === "pointer") await item.click();
        else {
          if (!hiddenCountBoundary) await item.focus();
          await page.keyboard.press(gesture);
        }
        await expect
          .poll(async () => (await membership()).hidden.length)
          .toBe(before.hidden.length - 1);
        const after = await membership();
        const calls = await page.evaluate(() => {
          const w = window as unknown as {
            __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
            __fxRestoreCalls: string[];
            __fxOriginalInvoke: (cmd: string, args?: unknown) => Promise<unknown>;
          };
          w.__TAURI_INTERNALS__.invoke = w.__fxOriginalInvoke;
          return w.__fxRestoreCalls;
        });
        expect(
          calls.filter((cmd) =>
            [
              "terminal_open_tab",
              "terminal_respawn_tab",
              "terminal_close_tab",
              "terminal_close_workspace",
            ].includes(cmd),
          ),
        ).toEqual([]);
        expect(calls).toContain("terminal_place_tab");
        expect(after.shown).toContainEqual(tab);
        expect(after.lives[tab.sessionId]).toBe(before.lives[tab.sessionId]);
        expect(after.shown.some((t) => t.sessionId === "sess-login-lead")).toBe(true);
        hiddenReceiving[`restore-${gesture}-${before.hidden.length}`] = { before, after, calls };
        if (viewportToRestore) {
          await expect(page.getByRole("menu")).toHaveCount(0);
          await page.setViewportSize(viewportToRestore);
        }
      };
      hiddenReceiving.one = await usable();
      await restore("pointer");
      await expect(badge).toHaveCount(0);
      hiddenReceiving.zero = await usable();
      // Legitimate disposable MOCK host sessions: the mock starts no process.
      // Hide-one changes membership through host events, never a count prop.
      const hiddenCount = hiddenCountBoundary ?? 10;
      const previousCount = hiddenCount - 1;
      const hostCreated = await page.evaluate(
        async ({ team, hiddenCount }) => {
          const { terminalOpenTab } = await import("/src/tauri/commands.ts");
          const created: string[] = [];
          for (let i = 0; i < hiddenCount; i++) {
            await terminalOpenTab({
              machineId: "local",
              workspace: `${team}|BUG-142`,
              session: `fx122-hidden-${i}`,
              name: `FX hidden ${i}`,
              placement: "tab",
              activate: false,
              spawn: { kind: "warm", targetCwd: "/mock/fx122", size: { cols: 80, rows: 24 } },
            });
            created.push(`fx122-hidden-${i}`);
          }
          return created;
        },
        { team, hiddenCount },
      );
      expect(new Set(hostCreated).size).toBe(hiddenCount);
      await expect
        .poll(
          async () =>
            (await membership()).shown.filter((t) => t.sessionId.startsWith("fx122-hidden-"))
              .length,
        )
        .toBe(hiddenCount);
      const hide = (from: number, to: number) =>
        page.evaluate(
          async ({ team, from, to }) => {
            const { selectWorkspaceTabs } = await import("/src/shared/stores/viewer-store.ts");
            const { hideTab } = await import("/src/features/viewer/lib/terminal-close.ts");
            for (let i = from; i < to; i++) {
              const tab = selectWorkspaceTabs(
                window.__stores!.useViewerStore.getState(),
                team,
                "BUG-142",
              ).find((t) => t.sessionId === `fx122-hidden-${i}`);
              if (!tab) throw new Error(`Missing host-created shown tab ${i}`);
              hideTab(team, "BUG-142", tab.id);
            }
          },
          { team, from, to },
        );
      await hide(0, previousCount);
      await expect(badge).toHaveAttribute(
        "aria-label",
        new RegExp(`^${previousCount} hidden tabs — restore`),
      );
      hiddenReceiving[hiddenCountBoundary ? "ninetyNine" : "nine"] = await usable();
      if (hiddenCountBoundary)
        await test.info().attach("hidden-boundary-before", {
          body: JSON.stringify(
            { hostCreated, geometry: await contents(), membership: await membership() },
            null,
            2,
          ),
          contentType: "application/json",
        });
      await hide(previousCount, hiddenCount);
      await expect(badge).toHaveAttribute(
        "aria-label",
        new RegExp(`^${hiddenCount} hidden tabs — restore`),
      );
      if (hiddenCountBoundary)
        await test.info().attach("hidden-boundary-at", {
          body: JSON.stringify(
            { geometry: await contents(), membership: await membership() },
            null,
            2,
          ),
          contentType: "application/json",
        });
      hiddenReceiving[hiddenCountBoundary ? "hundred" : "ten"] = await usable();
      await restore("Enter");
      await expect(badge).toHaveAttribute(
        "aria-label",
        new RegExp(`^${previousCount} hidden tabs — restore`),
      );
      hiddenReceiving[hiddenCountBoundary ? "ninetyNineAgain" : "nineAgain"] = await usable();
      await page.evaluate(async () => {
        const { useAgentStateStore } = await import("/src/shared/stores/agent-state-store.ts");
        useAgentStateStore
          .getState()
          .recordSignal("fx122-hidden-0", { tier: 1, signal: { kind: "ask-user" } });
        // A tab's mark reads the backend's status for its session, so the mock backend
        // projects the ask.
        const { useAgentListProjectionStore } =
          await import("/src/features/viewer/lib/agent-list-projection.ts");
        const lead = useAgentListProjectionStore
          .getState()
          .sessions!.find((r) => r.sessionId === "sess-login-lead")!;
        (
          window as unknown as { __mockAgents: { addSession(row: unknown): void } }
        ).__mockAgents.addSession({
          ...lead,
          handle: "fx122-hidden-0",
          sessionId: "fx122-hidden-0",
          displayName: "FX hidden 0",
          launchedBy: null,
          effectiveStatus: "awaiting-answer",
        });
      });
      // Shown with the badge; at the tightest step the badge (and so its mark) is shed.
      if (await badge.isVisible())
        await expect(page.getByTestId("tray-chip-hidden-status-BUG-142")).toBeVisible();
      else await expect(page.getByTestId("tray-chip-hidden-status-BUG-142")).toHaveCount(1);
      hiddenReceiving.loud = await usable();
      if (hiddenCountBoundary) {
        // Rehide the genuinely restored host tab, preserving admission/session.
        await hide(previousCount, hiddenCount);
        await expect(badge).toHaveAttribute("aria-label", /^100 hidden tabs — restore/);
        hiddenReceiving.hundredLoud = await usable();
      }
      if (profile === "combined" && width === 600) {
        if (placement === "shared")
          await page.evaluate(() =>
            window.__stores!.useViewerStore.setState({ rightPaneOpen: true, rightPaneRatio: 0.4 }),
          );
        for (const target of [283, 284, 285, 383, 384, 385]) {
          await targetStripWidth(page, chip, target, 0.6);
          hiddenReceiving[`loud-cqw-${target}`] = await usable();
        }
        await page.evaluate(() =>
          window.__stores!.useViewerStore.setState({ rightPaneOpen: false }),
        );
        await page.setViewportSize({ width, height: 850 });
        hiddenReceiving.loudRestored = await usable();
      }

      await page.evaluate(async () => {
        const { useAgentStateStore } = await import("/src/shared/stores/agent-state-store.ts");
        useAgentStateStore
          .getState()
          .recordPtyExit("fx122-hidden-0", { code: 0, signal: null, requested: false });
      });
      if (await badge.isVisible())
        await expect(page.getByTestId("tray-chip-hidden-ended-BUG-142")).toBeVisible();
      else await expect(page.getByTestId("tray-chip-hidden-ended-BUG-142")).toHaveCount(1);
      hiddenReceiving.ended = await usable();
      await restore("Space");
      hiddenReceiving.afterSpace = await usable();
      if (hiddenCountBoundary) {
        await expect(badge).toHaveAttribute("aria-label", /^99 hidden tabs — restore/);
        await hide(previousCount, hiddenCount);
        await expect(badge).toHaveAttribute("aria-label", /^100 hidden tabs — restore/);
        hiddenReceiving.hundredEnded = await usable();
        await restore("pointer");
        await expect(badge).toHaveAttribute("aria-label", /^99 hidden tabs — restore/);
        hiddenReceiving.ninetyNineFinal = {
          geometry: await usable(),
          membership: await membership(),
        };
        await page.screenshot({ path: test.info().outputPath("hidden-boundary-restored.png") });
      }
      if (profile === "combined" && width === 600) {
        await page.getByTestId("workspace-tray-mode-toggle").click();
        const rail = page.getByTestId("workspace-epic-rail");
        const children = page.getByTestId("workspace-tray-children");
        await expect(rail).toBeVisible();
        await expect(children).toBeVisible();
        expect(
          await children.evaluate((el) =>
            el.contains(document.querySelector('[data-tray-chip="BUG-142"]')),
          ),
        ).toBe(true);
        expect(await rail.evaluate((el) => !el.querySelector('[data-tray-chip="BUG-142"]'))).toBe(
          true,
        );
        hiddenReceiving.grouped = await usable();
        await page.getByTestId("workspace-tray-mode-toggle").click();
        await expect(children).toHaveCount(0);
        hiddenReceiving.flatRestored = await usable();
      }

      await test.info().attach("hidden-production-receiving", {
        body: JSON.stringify(hiddenReceiving, null, 2),
        contentType: "application/json",
      });
    }
    const settled = await usable();
    let archivedAfter = null;
    if (identity !== "local") {
      await setArchive(false);
      await usable();
      await setArchive(true);
      archivedAfter = await usable();
    }
    expect(settled.operation!.rect.width).toBeGreaterThanOrEqual(22);
    expect(settled.operation!.hit).toBe(true);
    await operation.evaluate((el) => {
      const events: unknown[] = [];
      (window as unknown as { __fxOperationEvents: unknown[] }).__fxOperationEvents = events;
      for (const type of ["pointerdown", "pointerup", "click"])
        document.addEventListener(
          type,
          (e) =>
            events.push({
              type,
              target: (e.target as Element).closest("button")?.getAttribute("data-testid"),
              tag: (e.target as Element).tagName,
              cls: (e.target as Element).getAttribute("class"),
              x: (e as PointerEvent).clientX,
              y: (e as PointerEvent).clientY,
              rect: el.getBoundingClientRect().toJSON(),
              scrolls: Array.from(document.querySelectorAll("[data-tray-chip]"))
                .slice(0, 1)
                .map((chip) => chip.parentElement!.scrollLeft),
              expanded: el.getAttribute("aria-expanded"),
            }),
          { capture: true },
        );
      new MutationObserver(() =>
        events.push({
          mutation: el.getAttribute("aria-expanded"),
          dialogs: document.querySelectorAll('[role="dialog"]').length,
        }),
      ).observe(el, { attributes: true, attributeFilter: ["aria-expanded"] });
    });
    await operation.click();
    await test.info().attach("operation-pointer-events", {
      body: JSON.stringify(
        await page.evaluate(
          () => (window as unknown as { __fxOperationEvents: unknown[] }).__fxOperationEvents,
        ),
      ),
      contentType: "application/json",
    });
    await expect(page.getByRole("dialog")).toBeVisible();
    await expect(page.getByTestId("operation-dismiss")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(operation).toHaveAttribute("aria-expanded", "false");
    await operation.focus();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("dialog")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog")).toBeHidden();
    await expect(operation).toHaveAttribute("aria-expanded", "false");
    await page.keyboard.press("Tab");
    await expect(chip.locator("[data-ws-row]")).toHaveAttribute("aria-label", /BUG-142/);
    if (identity === "remote") {
      await leaveTooltip();
      // No machine list in the mock knows "ci-runner", so the pill names it in words.
      await expect(chip.locator("[data-ws-row]")).toHaveAttribute(
        "aria-label",
        /on a remote machine/,
      );
      await expect(page.getByTestId("workspace-tab-BUG-142-machine-mark")).toBeVisible();
      await page.mouse.move(0, 0);
      await page.getByTestId("workspace-tab-BUG-142-machine").hover();
      await expect(page.getByRole("tooltip")).toContainText("BUG-142 · lives on a remote machine");
    }
    await controlsReachable(page, tray);
    await test.info().attach("contents", {
      body: JSON.stringify(
        { before, grown, beforeOperation, supported, hiddenReceiving, settled, archivedAfter },
        null,
        2,
      ),
      contentType: "application/json",
    });
    expect(resizeErrors).toEqual([]);
    await page.screenshot({ path: test.info().outputPath("contents.png") });
  });
}

for (const placement of ["shared", "console"] as const) {
  for (const gesture of ["wheel", "touchstart"] as const) {
    for (const keyboard of [false, true]) {
      test(`${placement}, ${gesture}, ${keyboard ? "keyboard" : "pointer"}: manual browsing survives measurements; same-active selection resumes follow`, async ({
        page,
      }) => {
        await boot(page, 600);
        const { team } = await seed(page, placement, "normal");
        await page.evaluate(async (team) => {
          const { activateAndFocusTrayWorkspace } =
            await import("/src/features/viewer/lib/focus-cascade.ts");
          activateAndFocusTrayWorkspace(team, "BUG-151");
        }, team);
        const tray = page.getByTestId("workspace-tray");
        await activeFits(tray);
        await tray.locator('[data-tray-chip="BUG-151"]').evaluate((el, gesture) => {
          const area = el.parentElement!;
          area.dispatchEvent(new Event(gesture, { bubbles: true }));
          area.scrollLeft = area.scrollWidth;
        }, gesture);
        await expect
          .poll(async () => (await geometry(tray)).clipped.includes("BUG-151"))
          .toBe(true);
        await page.evaluate((team) => {
          const store = window.__stores!.useViewerStore;
          store.setState((s) => ({
            workspaces: {
              ...s.workspaces,
              [team]: [
                ...s.workspaces[team],
                {
                  id: "generic:/tmp/fx122-added",
                  kind: "generic",
                  cwd: "/tmp/added",
                  status: "ready",
                },
              ],
            },
          }));
        }, team);
        await expect(tray.locator('[data-tray-chip="generic:/tmp/fx122-added"]')).toHaveCount(1);
        // Browsing must remain at the distant end across each measurement-key change.
        await page.waitForTimeout(100);
        expect((await geometry(tray)).clipped).toContain("BUG-151");
        await page.evaluate((team) => {
          const store = window.__stores!.useViewerStore;
          store.setState((s) => ({
            workspaces: {
              ...s.workspaces,
              [team]: s.workspaces[team].filter((w) => w.id !== "generic:/tmp/fx122-added"),
            },
          }));
          store.getState().updateSettings({ workspaceBarSize: "large" });
        }, team);
        await page.waitForTimeout(100);
        expect((await geometry(tray)).clipped).toContain("BUG-151");
        const before = await geometry(tray);
        await tray.getByTestId("workspace-tray-overflow").click();
        const row = page.getByTestId("workspace-tray-hidden-row-BUG-151");
        if (keyboard) {
          await row.focus();
          await page.keyboard.press("Enter");
        } else await row.click();
        await expect(page.getByTestId("workspace-tray-overflow-menu")).toBeHidden();
        await expect(
          page.locator('[data-pane-id="BUG-151"] .xterm textarea').first(),
        ).toBeFocused();
        await activeFits(tray);
        await page.evaluate((team) => {
          window.__stores!.useViewerStore.setState((s) => ({
            workspaces: {
              ...s.workspaces,
              [team]: s.workspaces[team].map((w) =>
                w.id === "BUG-151"
                  ? {
                      ...w,
                      storyId: "Late active workspace identity after explicit activation",
                    }
                  : w.id === "BUG-142"
                    ? { ...w, storyId: "Late preceding workspace identity grows" }
                    : w,
              ),
            },
          }));
        }, team);
        await activeFits(tray);
        await expect(tray.locator('[data-tray-chip="BUG-151"] [data-ws-row]')).toHaveAttribute(
          "aria-label",
          /Late active workspace identity/,
        );
        await test.info().attach("manual-follow", {
          body: JSON.stringify({ before, after: await geometry(tray) }, null, 2),
          contentType: "application/json",
        });
      });
    }
  }

  test(`${placement}: few entries need no overflow; empty tray keeps controls`, async ({
    page,
  }) => {
    await boot(page, 1824);
    const { team } = await seed(page, placement, "normal");
    await page.evaluate((team) => {
      const store = window.__stores!.useViewerStore;
      store.setState((s) => ({
        workspaces: { [team]: s.workspaces[team].slice(0, 2) },
        softClosedWorkspaces: {},
      }));
    }, team);
    const tray = page.getByTestId("workspace-tray");
    await expect(tray.locator("[data-tray-chip]")).toHaveCount(2);
    await expect(tray.getByTestId("workspace-tray-overflow")).toHaveCount(0);
    await expect(tray.locator("[data-compact]")).toHaveCount(0);
    await page.evaluate((team) => {
      window.__stores!.useViewerStore.setState({ workspaces: { [team]: [] } });
    }, team);
    await expect(tray.locator("[data-tray-chip]")).toHaveCount(0);
    await page.getByTestId("workspace-tray-launcher").click({ trial: true });
    await page.getByTestId("workspace-tray-mode-toggle").click({ trial: true });
  });
}

for (const placement of ["shared", "console"] as const) {
  for (const size of SIZES) {
    test(`${placement}, ${size}: grouped rail stays separate, shortcut follows real terminal focus`, async ({
      page,
    }) => {
      await boot(page, 600);
      const { team } = await seed(page, placement, size);
      await page.getByTestId("workspace-tray-mode-toggle").click();
      const rail = page.getByTestId("workspace-epic-rail");
      const children = page.getByTestId("workspace-tray-children");
      await expect(rail.locator("[data-epic-rail-pill]")).toHaveCount(3);
      if (placement === "console") {
        await expect(page.getByTestId("console-switcher-slot")).toContainText("Fx122 Epic");
        expect(
          await children.evaluate((el) => !!el.closest('[data-testid="console-switcher-slot"]')),
        ).toBe(false);
      }
      await rail.locator('[data-epic-rail-pill="fx122-epic"]').click();
      await expect(children.locator('[data-tray-chip="epic:fx122"]')).toHaveAttribute(
        "data-active",
        "true",
      );
      await activeFits(children);
      // Grouped shortcuts use the selected child strip's published order.
      // Restore the existing Ungrouped rail, then jump to its real host session.
      await rail.locator('[data-epic-rail-pill=""]').click();
      const index = await page.evaluate(
        (team) =>
          window.__stores!.useViewerStore.getState().trayOrderByTeam[team].indexOf("BUG-142"),
        team,
      );
      expect(index).toBeGreaterThanOrEqual(0);
      expect(index).toBeLessThan(9);
      await page.keyboard.press(`Meta+${index + 1}`);
      const chip = children.locator('[data-tray-chip="BUG-142"]');
      await expect(chip).toHaveAttribute("data-active", "true");
      await expect(chip).not.toHaveAttribute("data-compact");
      await expect(page.getByTestId("workspace-tab-BUG-142-crew")).toBeVisible();
      await expect(page.locator('[data-pane-id="BUG-142"] .xterm textarea').first()).toBeFocused();
      await activeFits(children);
      const m = await geometry(children);
      expect(m.rowSpread).toBeLessThanOrEqual(1);
      const tops = await rail
        .locator("[data-epic-rail-pill]")
        .evaluateAll((els) => els.map((el) => el.getBoundingClientRect().top));
      expect(Math.max(...tops) - Math.min(...tops)).toBeLessThanOrEqual(1);
      await controlsReachable(page, children);
      await page.screenshot({ path: test.info().outputPath("grouped.png") });
    });
  }
}

for (const placement of ["shared", "console"] as const) {
  test(`${placement}: overflow restores the same minimized host sessions and typing focus`, async ({
    page,
  }) => {
    await boot(page, 600);
    const { team } = await seed(page, placement, "normal");
    const before = await page.evaluate(async (team) => {
      const path = "/src/shared/stores/viewer-store.ts";
      const { selectWorkspaceTabs } = await import(/* @vite-ignore */ path);
      const store = window.__stores!.useViewerStore;
      const sessions: string[] = selectWorkspaceTabs(store.getState(), team, "BUG-142").map(
        (t: { sessionId: string }) => t.sessionId,
      );
      store.getState().closeWorkspace(team, "BUG-142");
      return sessions;
    }, team);
    expect(before.length).toBeGreaterThan(0);
    const chip = page.locator('[data-tray-chip="BUG-142"]');
    await expect(chip).toHaveAttribute("data-minimized", "true");
    await page.getByTestId("workspace-tray-overflow").click();
    await page.getByTestId("workspace-tray-hidden-row-BUG-142").click();
    await expect(chip).toHaveAttribute("data-minimized", "false");
    await expect(chip).toHaveAttribute("data-active", "true");
    await activeFits(page.getByTestId("workspace-tray"));
    const after = await page.evaluate(async (team) => {
      const path = "/src/shared/stores/viewer-store.ts";
      const { selectWorkspaceTabs } = await import(/* @vite-ignore */ path);
      return selectWorkspaceTabs(window.__stores!.useViewerStore.getState(), team, "BUG-142").map(
        (t: { sessionId: string }) => t.sessionId,
      );
    }, team);
    expect(after).toEqual(before);
    await expect(page.locator('[data-pane-id="BUG-142"] .xterm textarea').first()).toBeFocused();
  });
}

for (const placement of ["shared", "console"] as const) {
  for (const gesture of ["pointer", "Enter", "Space"] as const) {
    test(`${placement}, ${gesture}: independent native Close receives default End and Hide on disposable mock fixtures`, async ({
      page,
    }) => {
      await boot(page, 600);
      const { team } = await seed(page, placement, "normal");
      const tray = page.getByTestId("workspace-tray");
      const endId = "generic:/tmp/fx122-3";
      await page.evaluate(
        async ({ team, endId }) => {
          const { terminalOpenTab } = await import("/src/tauri/commands.ts");
          const { activateAndFocusTrayWorkspace } =
            await import("/src/features/viewer/lib/focus-cascade.ts");
          await terminalOpenTab({
            machineId: "local",
            workspace: `${team}|${endId}`,
            session: "fx122-close-idle",
            name: "FX idle fixture",
            descriptor: {
              displayName: "FX idle fixture",
              kind: "generic",
              org: "acme",
              team: "default",
              worktreePath: "/mock/fx122",
            },
            placement: "tab",
            activate: true,
            spawn: { kind: "warm", targetCwd: "/mock/fx122", size: { cols: 80, rows: 24 } },
          });
          activateAndFocusTrayWorkspace(team, endId);
        },
        { team, endId },
      );
      const close = async (id: string, verb: "End" | "Hide") => {
        await activeFits(tray);
        const button = page.getByTestId(`workspace-tab-${id}-close`);
        await button.focus();
        await expect(button).toHaveAttribute("aria-label", new RegExp(`^${verb} workspace `));
        expect(await button.evaluate((el) => el.tagName)).toBe("BUTTON");
        expect(await tray.locator("button button").count()).toBe(0);
        if (gesture === "pointer") {
          const box = (await button.boundingBox())!;
          const point = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
          expect(
            await button.evaluate(
              (el, p) => el.contains(document.elementFromPoint(p.x, p.y)),
              point,
            ),
          ).toBe(true);
          await page.mouse.click(point.x, point.y);
        } else {
          expect(
            await button.evaluate(
              (el) => el.matches(":focus-visible") && getComputedStyle(el).outlineStyle !== "none",
            ),
          ).toBe(true);
          await page.keyboard.press(gesture);
        }
        await expect(page.getByRole("dialog")).toHaveCount(0);
      };
      await close(endId, "End");
      await expect(tray.locator(`[data-tray-chip="${endId}"]`)).toHaveCount(0);
      expect(
        await page.evaluate(async (team) => {
          const { selectWorkspaceTabs } = await import("/src/shared/stores/viewer-store.ts");
          return selectWorkspaceTabs(
            window.__stores!.useViewerStore.getState(),
            team,
            "generic:/tmp/fx122-3",
          ).length;
        }, team),
      ).toBe(0);
      const before = await page.evaluate(async (team) => {
        const { selectWorkspaceTabs } = await import("/src/shared/stores/viewer-store.ts");
        const { activateAndFocusTrayWorkspace } =
          await import("/src/features/viewer/lib/focus-cascade.ts");
        activateAndFocusTrayWorkspace(team, "BUG-151");
        const { hideTab } = await import("/src/features/viewer/lib/terminal-close.ts");
        const tab = selectWorkspaceTabs(
          window.__stores!.useViewerStore.getState(),
          team,
          "BUG-151",
        ).find((t) => t.sessionId === "sess-login-crosscheck")!;
        hideTab(team, "BUG-151", tab.id);
        return selectWorkspaceTabs(window.__stores!.useViewerStore.getState(), team, "BUG-151").map(
          (t) => t.sessionId,
        );
      }, team);
      await expect(page.getByTestId("tray-chip-hidden-BUG-151")).toHaveAttribute(
        "aria-label",
        /^1 hidden tab — restore/,
      );
      const shownBefore = await page.evaluate(async (team) => {
        const { selectWorkspaceTabs } = await import("/src/shared/stores/viewer-store.ts");
        return selectWorkspaceTabs(window.__stores!.useViewerStore.getState(), team, "BUG-151").map(
          (t) => t.sessionId,
        );
      }, team);
      expect(shownBefore.length).toBeGreaterThan(1);
      await close("BUG-151", "Hide");
      const chip = tray.locator('[data-tray-chip="BUG-151"]');
      await expect(chip).toHaveAttribute("data-minimized", "true");
      await expect(page.getByTestId("workspace-tab-BUG-151-close")).toHaveCount(0);
      // Existing whole-workspace Hide folds its Hidden tabs back into the tile.
      await expect(page.getByTestId("tray-chip-hidden-BUG-151")).toHaveCount(0);
      await expect(page.getByTestId("workspace-tab-BUG-151-terminal-count")).toHaveAttribute(
        "aria-label",
        `${before.length} running terminals`,
      );
      // Hide-one also works on the retained minimized host membership: receive
      // the badge and named shown count together without a fabricated count.
      await page.evaluate(async (team) => {
        const { selectWorkspaceTabs } = await import("/src/shared/stores/viewer-store.ts");
        const { hideTab } = await import("/src/features/viewer/lib/terminal-close.ts");
        const tab = selectWorkspaceTabs(
          window.__stores!.useViewerStore.getState(),
          team,
          "BUG-151",
        ).find((t) => t.sessionId === "sess-login-crosscheck")!;
        hideTab(team, "BUG-151", tab.id);
      }, team);
      await expect(page.getByTestId("tray-chip-hidden-BUG-151")).toHaveAttribute(
        "aria-label",
        /^1 hidden tab — restore/,
      );
      await expect(page.getByTestId("workspace-tab-BUG-151-terminal-count")).toHaveAttribute(
        "aria-label",
        `${before.length} running terminals`,
      );
      await chip.evaluate((el) => {
        const area = el.parentElement!;
        area.dispatchEvent(new Event("wheel", { bubbles: true }));
        const box = area.getBoundingClientRect();
        const zoom = box.width / area.offsetWidth;
        area.scrollLeft += (el.getBoundingClientRect().left - box.left) / zoom;
      });
      const minimizedGeometry = await chip.evaluate((el) => {
        const card = el.getBoundingClientRect();
        const area = el.parentElement!.getBoundingClientRect();
        return {
          card: card.toJSON(),
          area: area.toJSON(),
          parts: ["tray-chip-hidden-BUG-151", "workspace-tab-BUG-151-terminal-count"].map((id) => {
            const node = el.querySelector<HTMLElement>(`[data-testid="${id}"]`)!;
            const r = node.getBoundingClientRect();
            let parent = node.parentElement!;
            while (getComputedStyle(parent).display === "contents") parent = parent.parentElement!;
            const slot = parent.getBoundingClientRect();
            return {
              id,
              shown: r.width > 0,
              rect: r.toJSON(),
              slot: slot.toJSON(),
              inside:
                r.left >= slot.left - 0.5 &&
                r.right <= slot.right + 0.5 &&
                r.top >= slot.top - 0.5 &&
                r.bottom <= slot.bottom + 0.5 &&
                r.left >= area.left - 0.5 &&
                r.right <= area.right + 0.5,
              hit: node.contains(
                document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2),
              ),
            };
          }),
        };
      });
      await test.info().attach("minimized-hidden-count-allocation", {
        body: JSON.stringify(minimizedGeometry),
        contentType: "application/json",
      });
      // The full-screen CLI's title-bar strip is about one pill wide: there the badge is shed (its
      // tabs stay in the pill's menu). Whatever is shown fits and takes its own hits.
      expect(
        minimizedGeometry.parts.every(
          (p) => (p.id.startsWith("tray-chip-hidden-") && !p.shown) || (p.inside && p.hit),
        ),
      ).toBe(true);
      expect(minimizedGeometry.parts.find((p) => p.id.endsWith("-terminal-count"))!.shown).toBe(
        true,
      );
      await chip.locator("[data-ws-row]").click();
      await expect(chip).toHaveAttribute("data-minimized", "false");
      const after = await page.evaluate(async (team) => {
        const { selectWorkspaceTabs } = await import("/src/shared/stores/viewer-store.ts");
        return selectWorkspaceTabs(window.__stores!.useViewerStore.getState(), team, "BUG-151").map(
          (t) => t.sessionId,
        );
      }, team);
      expect(after).toEqual(shownBefore);
      await activeFits(tray);
      await test.info().attach("native-close-receiving", {
        body: JSON.stringify({ placement, gesture, endId, before, shownBefore, after }),
        contentType: "application/json",
      });
    });
  }
}

for (const placement of ["shared", "console"] as const) {
  test(`${placement}: kind/phase guards, assembly fallback, and empty ready operation use the same composition`, async ({
    page,
  }) => {
    await boot(page, 600);
    const { team } = await seed(page, placement, "normal");
    const tray = page.getByTestId("workspace-tray");
    const activate = (id: string) =>
      page.evaluate(
        async ({ team, id }) => {
          const { activateAndFocusTrayWorkspace } =
            await import("/src/features/viewer/lib/focus-cascade.ts");
          activateAndFocusTrayWorkspace(team, id);
        },
        { team, id },
      );
    const receive = async (id: string) => {
      await activeFits(tray);
      const chip = tray.locator(`[data-tray-chip="${id}"]`);
      expect(await chip.locator("button button").count()).toBe(0);
      const m = await chip.evaluate((el) => {
        const card = el.getBoundingClientRect();
        const parts = Array.from(
          el.querySelectorAll<HTMLElement>(
            'button, .cs-workspace-pill-label, [data-testid="operation-chip-assembly-failed"], [data-testid$="-creating"], [data-testid$="-error"], [data-testid$="-setup"]',
          ),
        ).filter((n) => n.getBoundingClientRect().width > 0);
        return {
          height: parseFloat(getComputedStyle(el).height),
          parts: parts.map((n) => {
            const r = n.getBoundingClientRect();
            let parent = n.parentElement!;
            while (getComputedStyle(parent).display === "contents") parent = parent.parentElement!;
            const p = parent.getBoundingClientRect();
            return {
              name: n.getAttribute("aria-label") ?? n.dataset.testid ?? n.className,
              rect: r.toJSON(),
              slot: p.toJSON(),
              inside:
                r.left >= p.left - 0.5 &&
                r.right <= p.right + 0.5 &&
                r.top >= p.top - 0.5 &&
                r.bottom <= p.bottom + 0.5 &&
                r.left >= card.left - 0.5 &&
                r.right <= card.right + 0.5,
            };
          }),
        };
      });
      expect(m.height).toBeLessThanOrEqual(40);
      expect(m.parts.filter((p) => !p.inside)).toEqual([]);
      await test
        .info()
        .attach(`kind-phase-${id}`, { body: JSON.stringify(m), contentType: "application/json" });
      return chip;
    };
    const receiveMenu = async (target: Locator) => {
      await target.click({ button: "right" });
      await expect(page.getByRole("menu", { name: "Workspace actions", exact: true })).toHaveCount(
        1,
      );
      await page.keyboard.press("Escape");
      await expect(page.getByRole("menu", { name: "Workspace actions", exact: true })).toHaveCount(
        0,
      );
    };
    for (const id of ["root:fx122/repo", "epic:fx122", "generic:/tmp/fx122-3", "BUG-142"]) {
      await activate(id);
      const chip = await receive(id);
      await expect(chip.getByTestId("operation-chip")).toHaveCount(0);
      expect(
        await chip
          .locator(".cs-workspace-pill-operation-one-line")
          .evaluateAll((nodes) => nodes.every((n) => getComputedStyle(n).display === "none")),
      ).toBe(true);
      if (id.startsWith("root:")) {
        await expect(page.getByTestId(`workspace-tab-${id}-close`)).toHaveCount(0);
        for (const target of [
          chip.locator(".cs-workspace-pill-state"),
          chip.locator("[data-ws-row]"),
        ]) {
          await target.click({ button: "right" });
          await expect(
            page.getByRole("menu", { name: "Workspace actions", exact: true }),
          ).toHaveCount(0);
        }
      }
      if (id === "BUG-142") {
        await receiveMenu(chip.getByTestId("workspace-tab-BUG-142-ai-status-dot"));
        await receiveMenu(chip.getByRole("button", { name: /^Change status/ }));
        await receiveMenu(chip.locator("[data-ws-row]"));
      }
    }
    await activate("epic:fx122");
    await page.evaluate(async () => {
      const { useSurfaceSyncStore } =
        await import("/src/features/shell/stores/surface-sync-store.ts");
      useSurfaceSyncStore
        .getState()
        .markAssemblyFailure("/tmp/fx122-epic", "Owned mock assembly refusal");
    });
    const epic = await receive("epic:fx122");
    await expect(epic.getByTestId("operation-chip-assembly-failed")).toHaveAttribute(
      "title",
      "Owned mock assembly refusal",
    );
    // Drive real retry admission and refusal, holding only this fixture's mock
    // story-read transport. No native worktree/setup is opened or changed.
    await activate("BUG-142");
    await page.evaluate((team) => {
      const store = window.__stores!.useViewerStore;
      store.setState((s) => ({
        workspaces: {
          ...s.workspaces,
          [team]: s.workspaces[team].map((w) =>
            w.id === "BUG-142" ? { ...w, sourceRepoSlug: "fx122/mock-retry" } : w,
          ),
        },
      }));
      const w = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
        };
        __fxRetryReject: (e: Error) => void;
        __fxRetry: Promise<void>;
        __fxRetryOriginal: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
      };
      const original = w.__TAURI_INTERNALS__.invoke;
      w.__fxRetryOriginal = original;
      const gate = new Promise<unknown>((_, reject) => {
        w.__fxRetryReject = reject;
      });
      w.__TAURI_INTERNALS__.invoke = (cmd, args) =>
        cmd === "read_file" && String(args?.path).endsWith("/stories/BUG-142.md")
          ? gate
          : original(cmd, args);
      w.__fxRetry = store.getState().retryWorkspaceCreate(team, "BUG-142");
    }, team);
    await expect(page.getByTestId("workspace-tab-BUG-142-setup")).toBeVisible();
    const creating = await receive("BUG-142");
    await receiveMenu(creating.getByTestId("workspace-tab-BUG-142-setup"));
    await expect(creating.locator('button[aria-label^="Change status"]')).toHaveCount(0);
    await expect(creating.getByTestId("operation-chip")).toHaveCount(0);
    await page.evaluate(async () => {
      const w = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
        };
        __fxRetryReject: (e: Error) => void;
        __fxRetry: Promise<void>;
        __fxRetryOriginal: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
      };
      w.__fxRetryReject(new Error("Owned mock registration refusal"));
      await w.__fxRetry;
      w.__TAURI_INTERNALS__.invoke = w.__fxRetryOriginal;
    });
    await expect(page.getByTestId("workspace-tab-BUG-142-error")).toBeVisible();
    const error = await receive("BUG-142");
    await expect(error.locator('button[aria-label^="Change status"]')).toHaveCount(0);
    await expect(error.getByTestId("operation-chip")).toHaveCount(0);
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await controlsReachable(page, tray);
  });
}

for (const placement of ["shared", "console"] as const) {
  test(`${placement}: actual portaled details preserve live/minimized selection, documents and focus`, async ({
    page,
  }) => {
    await boot(page, 1824);
    const team = await page.evaluate(async (placement) => {
      const store = window.__stores!.useViewerStore;
      const team = Object.keys(store.getState().workspaces).find((t) =>
        store.getState().workspaces[t].some((w) => w.id === "BUG-142"),
      )!;
      store.setState((s) => ({ trayMode: { ...s.trayMode, [team]: "flat" } }));
      store.getState().setLayoutMode(placement);
      const { useContextOperationsStore } =
        await import("/src/features/shell/stores/context-operations-store.ts");
      const ws = store.getState().workspaces[team].find((w) => w.id === "BUG-142")!;
      useContextOperationsStore.getState().apply(
        {
          workspace: ws.cwd,
          agent: "explicit",
          operationId: "fx122-portal",
          operationKind: "hydrate",
          phase: "failed",
          files: 0,
          bytes: 0,
          total: 1,
          settled: true,
          note: "Selectable FX operation receipt",
        },
        Date.now(),
      );
      return team;
    }, placement);
    const activate = (id: string) =>
      page.evaluate(
        async ({ team, id }) => {
          const { activateAndFocusTrayWorkspace } =
            await import("/src/features/viewer/lib/focus-cascade.ts");
          activateAndFocusTrayWorkspace(team, id);
        },
        { team, id },
      );
    const snapshot = () =>
      page.evaluate((team) => {
        const s = window.__stores!.useViewerStore.getState();
        return {
          active: s.activeWorkspace[team],
          shown: s.workspaces[team].map((w) => w.id),
          minimized: Object.keys(s.softClosedWorkspaces[team] ?? {}),
          documents: s.openTabs[team],
          activeDocument: s.activeTab[team],
        };
      }, team);
    const chip = page.locator('[data-tray-chip="BUG-142"]');
    const receipts = [];
    for (const minimized of [false, true]) {
      if (minimized)
        await page.evaluate(async (team) => {
          const { hideWorkspace } = await import("/src/features/viewer/lib/terminal-close.ts");
          hideWorkspace(team, "BUG-142");
        }, team);
      await activate("BUG-151");
      await activeFits(page.getByTestId("workspace-tray"));
      await expect(chip).toHaveAttribute("data-minimized", String(minimized));
      await chip.getByTestId("operation-chip").click();
      const details = page.getByTestId("operation-details-fx122-portal");
      const note = details.getByText("Selectable FX operation receipt", { exact: true });
      await expect(note).toBeVisible();
      const before = await snapshot();
      const focusStaysInDetails = () =>
        page.evaluate(() =>
          Boolean(document.activeElement?.closest('[data-slot="popover-content"]')),
        );
      await note.click();
      expect(await snapshot()).toEqual(before);
      expect(await focusStaysInDetails()).toBe(true);
      await note.dblclick();
      expect(await snapshot()).toEqual(before);
      expect(await focusStaysInDetails()).toBe(true);
      const r = (await note.boundingBox())!;
      await page.mouse.move(r.x + 3, r.y + r.height / 2);
      await page.mouse.down();
      await page.mouse.move(r.x + r.width - 3, r.y + r.height / 2, { steps: 8 });
      await page.mouse.up();
      expect(
        await page.evaluate(() => window.getSelection()?.toString().length ?? 0),
      ).toBeGreaterThan(0);
      expect(await snapshot()).toEqual(before);
      await note.click({ button: "right" });
      await expect(page.getByRole("menu", { name: "Workspace actions", exact: true })).toHaveCount(
        0,
      );
      expect(await snapshot()).toEqual(before);
      await page.keyboard.press("Escape");
      await expect(details).toHaveCount(0);
      await chip.locator("[data-ws-row]").click();
      await expect(chip).toHaveAttribute("data-active", "true");
      await expect(chip).toHaveAttribute("data-minimized", "false");
      await activeFits(page.getByTestId("workspace-tray"));
      await chip.getByTestId("operation-chip").click();
      await expect(details).toBeVisible();
      await page.keyboard.press("Escape");
      receipts.push({
        minimized,
        before,
        after: await snapshot(),
        focusPreserved: true,
        textSelectionReceived: true,
      });
    }
    await test.info().attach("portal-interaction-receiving", {
      body: JSON.stringify(receipts),
      contentType: "application/json",
    });
  });
}
