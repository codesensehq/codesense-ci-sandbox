// SPDX-License-Identifier: MIT
import { mkdirSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

/**
 * Review captures of the CLI workspace pills in the bottom tray (the CLI pane beside the viewer)
 * and the title bar (the full-screen CLI), flat and grouped, at a roomy and a crowded width, in
 * the frontend mock backend's `?agents=teams` world. Asserts that the pills drew on one line (each
 * inside its strip's row, all on the same top), and that across the strip's width steps no pill
 * outgrows its box and the ⌘ number and the close × never show together at rest.
 */

const OUT_DIR = process.env.TRAY_PILLS_OUT ?? "test-results/tray-pills-review";
test.use({ deviceScaleFactor: 2 });

async function boot(
  page: Page,
  width: number,
  layout: "shared" | "console",
  mode: "flat" | "grouped",
) {
  await page.setViewportSize({ width, height: 900 });
  await page.goto("/?agents=teams");
  await page.waitForFunction(() => Boolean(window.__stores?.useViewerStore));
  await page.locator("[data-tray-chip]").first().waitFor();
  await page.evaluate(
    ({ layout, mode }) => {
      const store = window.__stores!.useViewerStore;
      const teams = Object.keys(store.getState().workspaces);
      store.setState((st) => ({
        workspaceTraySeenCoachmark: true,
        trayMode: { ...st.trayMode, ...Object.fromEntries(teams.map((t) => [t, mode])) },
      }));
      store.getState().setLayoutMode(layout);
    },
    { layout, mode },
  );
  await page.mouse.move(0, 0);
  await page.waitForTimeout(500);
}

for (const layout of ["shared", "console"] as const) {
  for (const mode of ["flat", "grouped"] as const) {
    for (const width of [1440, 1040]) {
      test(`tray pills — ${layout} ${mode} ${width}`, async ({ page }) => {
        mkdirSync(OUT_DIR, { recursive: true });
        await boot(page, width, layout, mode);
        const chips = page.locator("[data-tray-chip]");
        await expect(chips.first()).toBeVisible();
        // One line: every pill sits inside its strip's row, no taller than it, all on one top.
        const rows = await chips.evaluateAll((els) =>
          els
            .filter((el) => el.getBoundingClientRect().width > 0)
            .map((el) => {
              const r = el.getBoundingClientRect();
              const strip = el.parentElement!.getBoundingClientRect();
              return {
                top: r.top,
                bottom: r.bottom,
                height: r.height,
                stripTop: strip.top,
                stripBottom: strip.bottom,
              };
            }),
        );
        console.log(`${layout} ${mode} ${width} pill heights`, rows.map((r) => r.height).join(","));
        expect(rows.length).toBeGreaterThan(0);
        for (const r of rows) {
          expect(r.top).toBeGreaterThanOrEqual(r.stripTop - 0.5);
          expect(r.bottom).toBeLessThanOrEqual(r.stripBottom + 0.5);
          expect(r.height).toBeLessThanOrEqual(r.stripBottom - r.stripTop + 0.5);
          expect(Math.abs(r.top - rows[0]!.top)).toBeLessThanOrEqual(1);
        }
        const clip =
          layout === "shared"
            ? {
                x: 0,
                y: mode === "grouped" ? 800 : 840,
                width,
                height: mode === "grouped" ? 100 : 60,
              }
            : { x: 0, y: 0, width, height: mode === "grouped" ? 100 : 56 };
        await page.screenshot({ path: `${OUT_DIR}/${layout}-${mode}-${width}.png`, clip });
        // Hover a pill (its close control and full-name tooltip).
        await chips.nth(1).hover();
        await page.waitForTimeout(700);
        await page.screenshot({
          path: `${OUT_DIR}/${layout}-${mode}-${width}-hover.png`,
          clip: {
            ...clip,
            y: layout === "shared" ? clip.y - 44 : clip.y,
            height: clip.height + 44,
          },
        });
      });
    }
  }
}

for (const layout of ["shared", "console"] as const) {
  test(`tray pills — states (remote, minimized, crowded) — ${layout}`, async ({ page }) => {
    mkdirSync(OUT_DIR, { recursive: true });
    await boot(page, 1280, layout, "flat");
    await page.evaluate(() => {
      const store = window.__stores!.useViewerStore;
      const state = store.getState();
      const team = Object.keys(state.workspaces).find((k) => state.workspaces[k].length)!;
      const extras = [
        {
          id: "generic:/tmp/review-remote",
          kind: "generic" as const,
          cwd: "",
          status: "ready" as const,
          location: {
            kind: "machine" as const,
            machineId: "ci-runner",
            requestId: "review-remote",
            cwd: "/tmp/review-remote",
          },
        },
        ...Array.from({ length: 5 }, (_, i) => ({
          id: `generic:/tmp/review-${i}`,
          kind: "generic" as const,
          cwd: `/tmp/review-${i}-a-longer-workspace-name`,
          status: "ready" as const,
        })),
      ];
      const minimized = {
        id: "generic:/tmp/review-minimized",
        kind: "generic" as const,
        cwd: "/tmp/review-minimized",
        status: "ready" as const,
      };
      store.setState((s) => ({
        workspaces: { ...s.workspaces, [team]: [...s.workspaces[team], ...extras] },
        softClosedWorkspaces: {
          ...s.softClosedWorkspaces,
          [team]: { [minimized.id]: { workspace: minimized } },
        },
      }));
    });
    // Focus the remote workspace, so its pill (machine mark and ring) is the full, active one.
    await page.evaluate(async () => {
      const store = window.__stores!.useViewerStore;
      const team = Object.keys(store.getState().workspaces).find((k) =>
        store.getState().workspaces[k].some((w) => w.id === "generic:/tmp/review-remote"),
      )!;
      const { activateAndFocusTrayWorkspace } =
        await import("/src/features/viewer/lib/focus-cascade.ts");
      activateAndFocusTrayWorkspace(team, "generic:/tmp/review-remote");
    });
    await page.mouse.move(0, 0);
    await page.waitForTimeout(600);
    const clip =
      layout === "shared"
        ? { x: 0, y: 840, width: 1280, height: 60 }
        : { x: 0, y: 0, width: 1280, height: 56 };
    await page.screenshot({ path: `${OUT_DIR}/${layout}-states.png`, clip });
    // Keyboard focus on a pill: its ring, and the close × in its end slot.
    await page.keyboard.press("Shift");
    await page.locator('[data-tray-chip="generic:/tmp/review-4"] [data-ws-row]').focus();
    await page.waitForTimeout(400);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-states-focus.png`, clip });
    if (process.env.PILL_PROBE)
      console.log(
        "PROBE",
        await page.locator('[data-tray-chip="generic:/tmp/review-0"]').evaluate((el) =>
          JSON.stringify(
            Array.from(el.querySelectorAll<HTMLElement>("*"))
              .slice(0, 14)
              .map((n) => [
                n.className.toString().slice(0, 40),
                Math.round(n.getBoundingClientRect().width),
              ]),
          ),
        ),
      );
  });
}

test("tray pills — light theme (confluence)", async ({ page }) => {
  mkdirSync(OUT_DIR, { recursive: true });
  await boot(page, 1440, "shared", "flat");
  await page.evaluate(() => {
    const store = window.__stores!.useViewerStore;
    store.getState().updateSettings({ viewerTheme: "confluence" });
  });
  await page.mouse.move(0, 0);
  await page.waitForTimeout(600);
  await page.screenshot({
    path: `${OUT_DIR}/shared-light.png`,
    clip: { x: 0, y: 840, width: 1440, height: 60 },
  });
});

test("tray pills — no pill outgrows its box between the strip's width steps", async ({ page }) => {
  await boot(page, 1280, "shared", "flat");
  await page.evaluate(() => {
    const store = window.__stores!.useViewerStore;
    const state = store.getState();
    const team = Object.keys(state.workspaces).find((k) => state.workspaces[k].length)!;
    const remote = {
      id: "generic:/tmp/steps-remote",
      kind: "generic" as const,
      cwd: "",
      status: "ready" as const,
      location: {
        kind: "machine" as const,
        machineId: "ci-runner",
        requestId: "steps-remote",
        cwd: "/tmp/steps-remote-with-a-long-name",
      },
    };
    store.setState((s) => ({
      workspaces: { ...s.workspaces, [team]: [...s.workspaces[team], remote] },
    }));
    // The remote pill is the full, active one: the widest a pill gets.
    store.getState().activateTrayWorkspace(team, remote.id);
  });
  await page.mouse.move(0, 0);
  await page.waitForTimeout(600);
  // Across and between the container steps (384, 320, 284, 240, 200px), in 4px steps.
  for (let w = 420; w >= 160; w -= 4) {
    const bad = await page.evaluate(async (w) => {
      const area = document.querySelector("[data-tray-chip]")!.parentElement!;
      area.style.flex = "none";
      area.style.width = `${w}px`;
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
      return Array.from(area.querySelectorAll<HTMLElement>(":scope > [data-tray-chip]")).flatMap(
        (chip) => {
          const pill = chip.querySelector<HTMLElement>(".cs-workspace-pill-one-line");
          const overflow = Math.max(
            chip.scrollWidth - chip.clientWidth,
            pill ? pill.scrollWidth - pill.clientWidth : 0,
          );
          const end = chip.querySelector(".cs-workspace-pill-end");
          const hint = end?.querySelector(".cs-tray-shortcut");
          const close = end?.querySelector("[data-testid$='-close']");
          const both =
            !!hint &&
            !!close &&
            Number(getComputedStyle(hint).opacity) > 0 &&
            Number(getComputedStyle(close).opacity) > 0;
          return overflow > 1 || both ? [{ id: chip.dataset.trayChip, overflow, both }] : [];
        },
      );
    }, w);
    expect(bad, `strip ${w}px`).toEqual([]);
  }
});

test("tray pills — a many-parts pill never outgrows its cap on a wide strip", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?agents=teams");
  await page.waitForFunction(() => Boolean(window.__stores?.useViewerStore));
  await page.locator("[data-tray-chip]").first().waitFor();
  await page.evaluate(() => {
    const store = window.__stores!.useViewerStore;
    const state = store.getState();
    const team = Object.keys(state.workspaces).find((k) => state.workspaces[k].length)!;
    const remote = {
      id: "generic:/tmp/steps-remote",
      kind: "generic" as const,
      cwd: "",
      status: "ready" as const,
      location: {
        kind: "machine" as const,
        machineId: "ci-runner",
        requestId: "steps-remote",
        cwd: "/tmp/steps-remote-with-a-long-name-that-goes-on-and-on-and-on-forever",
      },
    };
    store.setState((s) => ({
      workspaceTraySeenCoachmark: true,
      workspaces: { ...s.workspaces, [team]: [...s.workspaces[team], remote] },
    }));
    const local = {
      id: "generic:/tmp/probe-op",
      kind: "generic" as const,
      cwd: "/tmp/probe-op-with-a-rather-long-folder-name-for-the-pill",
      status: "ready" as const,
    };
    store.setState((s) => ({
      workspaces: { ...s.workspaces, [team]: [...s.workspaces[team], local] },
    }));
    store.getState().activateTrayWorkspace(team, remote.id);
  });
  await page.evaluate(async () => {
    const ops = await import("/src/features/shell/stores/context-operations-store.ts");
    for (const ws of [
      "/tmp/probe-op-with-a-rather-long-folder-name-for-the-pill",
      "/tmp/steps-remote-with-a-long-name-that-goes-on-and-on-and-on-forever",
    ]) {
      ops.useContextOperationsStore.getState().apply(
        {
          workspace: ws,
          agent: "s-1",
          operationId: "probe-" + ws.length,
          operationKind: "hydrate",
          phase: "transfer",
          files: 3,
          bytes: 1000,
          total: 10,
          settled: false,
          note: "a long operation note",
        },
        Date.now(),
      );
    }
  });
  await page.mouse.move(0, 0);
  await page.waitForTimeout(800);
  const out: unknown[] = [];
  for (let w = 1200; w >= 380; w -= 20) {
    const bad = await page.evaluate(async (w) => {
      const area = document.querySelector("[data-tray-chip]")!.parentElement!;
      area.style.flex = "none";
      area.style.width = `${w}px`;
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
      return Array.from(area.querySelectorAll<HTMLElement>(":scope > [data-tray-chip]")).flatMap(
        (chip) => {
          const pill = chip.querySelector<HTMLElement>(".cs-workspace-pill-one-line");
          const overflow = Math.max(
            chip.scrollWidth - chip.clientWidth,
            pill ? pill.scrollWidth - pill.clientWidth : 0,
          );
          return overflow > 1 ? [{ id: chip.dataset.trayChip, overflow }] : [];
        },
      );
    }, w);
    if (bad.length) out.push({ w, bad });
  }
  expect(out).toEqual([]);
});
