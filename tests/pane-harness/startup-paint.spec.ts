// SPDX-License-Identifier: MIT
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { PNG } from "pngjs";
import { test, expect } from "./harness";
test.setTimeout(60_000);

test.afterEach(async ({ page }, info) => {
  if (info.status === info.expectedStatus) return;
  const strip = page.getByTestId("slot-fallback-strip");
  if (await strip.count()) {
    await strip
      .getByRole("button", { name: "Details", exact: true })
      .click()
      .catch(() => {});
    console.log("STARTUP_FALLBACK_DETAILS", await strip.innerText());
  }
  const directory = `/tmp/sv-plg-28/startup-paint/${info.project.name}-${info.title.replace(/[^a-z0-9]+/gi, "-")}-repeat-${info.repeatEachIndex}`;
  mkdirSync(directory, { recursive: true });
  await page.screenshot({ path: `${directory}/failure.png` }).catch(() => {});
  writeFileSync(
    `${directory}/host-calls.json`,
    JSON.stringify(
      await page
        .evaluate(
          () =>
            (window as any).calls?.filter(([command]: [string]) => command.includes("log")) ?? [],
        )
        .catch(() => []),
      null,
      2,
    ),
  );
});

const panes = [
  ["src/features/shell/slots/__fixtures__/agent-list-pane.html", "dock"],
  ["src/features/shell/slots/builtin/files-tree.html", "file-nav"],
  ["src/features/shell/slots/builtin/files-tree.html", "right-pane"],
  ["src/features/shell/right-pane/__fixtures__/inbox-pane.html", "right-pane"],
  ["src/features/shell/right-pane/__fixtures__/notes-pane.html", "right-pane"],
] as const;

for (const id of ["codesense", "dracula", "github", "nord", "catppuccin", "confluence"] as const) {
  for (const mode of ["dark", "light"] as const) {
    test(`${id}/${mode}: first frame matches the enclosing surface before pane readiness`, async ({
      page,
      mountPane,
      fakeHost,
    }, info) => {
      const captures = `/tmp/sv-plg-28/startup-paint/${info.project.name}-${id}-${mode}-repeat-${info.repeatEachIndex}`;
      mkdirSync(captures, { recursive: true });
      // Delay application startup without leaving WebKit's screenshot waiting on a
      // parser-blocking network response. The parse-time observer still checks the
      // palette before kit or authored content can paint.
      await page.route("**/kit/v1/kit.js", async (route) => {
        const response = await route.fetch();
        await route.fulfill({
          response,
          body:
            `(function () {
            const cs = window.codesense, pending = [];
            let released = false;
            for (const name of ["ready", "subscribe"]) {
              const original = cs[name];
              cs[name] = function (...args) {
                if (released) return original.apply(this, args);
                return new Promise((resolve, reject) => pending.push(() => {
                  try { resolve(original.apply(cs, args)); } catch (error) { reject(error); }
                }));
              };
            }
            window.__releaseStartupProbe = () => {
              released = true;
              pending.splice(0).forEach(run => run());
            };
          })();\n` + (await response.text()),
        });
      });
      const originErrors: string[] = [];
      page.on("console", (message) => {
        if (message.text().includes("Unable to post message")) originErrors.push(message.text());
        if (message.text().includes("STARTUP_FALLBACK_LOG")) console.log(message.text());
      });
      await page.addInitScript(() => {
        if (parent === window) return;
        const snapshot = () => {
          if (!document.querySelector("#codesense-initial-theme")) return;
          const s = getComputedStyle(document.documentElement);
          if (!s.getPropertyValue("--cs-bg").trim()) return;
          (window as any).__firstPanePaint = {
            bg: s.backgroundColor,
            token: s.getPropertyValue("--cs-bg").trim(),
            scheme: s.colorScheme,
          };
          observer.disconnect();
        };
        // A parser blocked in <head> may defer rAF until its script is released.
        // Observe the injected style at parse time, then separately verify the first rAF.
        const observer = new MutationObserver(snapshot);
        observer.observe(document, { childList: true, subtree: true });
        requestAnimationFrame(() => {
          const s = getComputedStyle(document.documentElement);
          (window as any).__firstPaneAnimationFrame = {
            bg: s.backgroundColor,
            token: s.getPropertyValue("--cs-bg").trim(),
            scheme: s.colorScheme,
          };
        });
      });
      await page.goto(fakeHost.url("tests/pane-harness/fixtures/pane-host.html"));
      const vars = await page.evaluate(
        async ([theme, scheme]) => {
          // @ts-expect-error Vite serves this module in the browser.
          const { themeTokens } = await import("/src/themes/index.ts");
          return themeTokens(theme, scheme) as Record<string, string>;
        },
        [id, mode],
      );
      for (const [file, slotId] of panes) {
        const surface = slotId === "file-nav" || slotId === "dock" ? "sidebar" : "viewer";
        const expected = vars[`--theme-surface-${surface}-bg`];
        const mounted = await mountPane(
          file,
          { slotId, theme: { id, scheme: mode, vars } },
          { awaitFrame: false },
        );
        await page.evaluate(async () => {
          // @ts-expect-error Vite serves this module in the browser.
          const { logger } = await import("/src/shared/lib/logger.ts");
          const warn = logger.warn;
          logger.warn = (tag: string, message: string) => {
            if (tag === "slot-fallback") console.info("STARTUP_FALLBACK_LOG", message);
            warn(tag, message);
          };
        });
        console.log("STARTUP_STAGE", info.project.name, id, mode, file, "mounted", Date.now());
        await expect.poll(() => !!mounted.currentFrame()).toBe(true);
        const frame = mounted.currentFrame()!;
        await expect
          .poll(() => frame.evaluate(() => (window as any).__firstPanePaint))
          .toBeTruthy();
        const paint = await frame.evaluate(() => (window as any).__firstPanePaint);
        expect(paint.token).toBe(expected);
        expect(paint.scheme).toBe(mode);
        expect(paint.bg).toBe(
          await page.evaluate((hex) => {
            const el = document.createElement("div");
            el.style.color = hex;
            document.body.append(el);
            const rgb = getComputedStyle(el).color;
            el.remove();
            return rgb;
          }, expected),
        );
        console.log(
          "STARTUP_STAGE",
          file,
          "before-screenshot",
          Date.now(),
          await frame.evaluate(() => ({
            readyState: document.readyState,
            kit: !!(window as any).codesense?.kit,
            messages: (window as any).__harness?.messages ?? null,
          })),
        );

        await frame.waitForFunction(() => document.readyState === "complete");
        await expect(page.getByTestId("pane-startup-placeholder")).toBeVisible();
        const captureStarted = Date.now();
        await page.screenshot({ path: `${captures}/${file.split("/").pop()}-${slotId}.png` });
        expect(Date.now() - captureStarted).toBeLessThan(10_000);
        await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
        console.log("STARTUP_STAGE", file, "after-screenshot", Date.now());
        await page.getByRole("button", { name: "Pane pane options", exact: true }).click();
        await expect(
          page.getByRole("menuitem", { name: "Reload pane", exact: true }),
        ).toBeVisible();
        await page.keyboard.press("Escape");
        await frame.evaluate(() => (window as any).__releaseStartupProbe());
        await frame.waitForFunction(() => !!(window as any).codesense?.kit);
        await expect(page.getByTestId("pane-startup-placeholder")).toHaveCount(0);
        await expect
          .poll(() => frame.evaluate(() => (window as any).__firstPaneAnimationFrame))
          .toBeTruthy();
        expect(await frame.evaluate(() => (window as any).__firstPaneAnimationFrame)).toEqual(
          paint,
        );
        await expect
          .poll(() =>
            frame.evaluate(() => document.body && getComputedStyle(document.body).backgroundColor),
          )
          .toBe(paint.bg);
        await page.screenshot({
          path: `${captures}/ready-${file.split("/").pop()}-${slotId}.png`,
        });
        await mounted.close();
      }
      expect(originErrors).toEqual([]);
    });
  }
}

for (const scenario of [
  {
    name: "bare centre dark",
    file: "centre-dark.html",
    pixel: { chromium: [18, 18, 18], webkit: [30, 30, 30] },
  },
  {
    name: "bare centre light",
    file: "centre-light.html",
    pixel: { chromium: [255, 255, 255], webkit: [255, 255, 255] },
  },
  {
    name: "hosted message child dark",
    file: "message-dark.html",
    pixel: { chromium: [13, 17, 23], webkit: [13, 17, 23] },
  },
] as const) {
  test(`${scenario.name}: first observable frame uses viewer response theme`, async ({
    page,
  }, info) => {
    const bytes = readFileSync(
      new URL(`./fixtures/startup-paint/${scenario.file}`, import.meta.url),
    );
    await page.route("http://viewer-fixture.test/**", (route) =>
      route.fulfill({ status: 200, contentType: "text/html; charset=utf-8", body: bytes }),
    );
    await page.setContent('<iframe title="themed viewer document"></iframe>');
    const iframe = page.locator("iframe");
    const loaded = iframe.evaluate(
      (element) =>
        new Promise<void>((resolve) =>
          element.addEventListener("load", () => resolve(), { once: true }),
        ),
    );
    await iframe.evaluate((element, file) => {
      (element as HTMLIFrameElement).src = `http://viewer-fixture.test/${file}`;
    }, scenario.file);
    await loaded;

    // The iframe screenshot immediately after load is the earliest observable pixel sample in
    // this harness. The route serves exact viewer.rs response fixtures, but is not a native WKWebView.
    const screenshot = await iframe.screenshot();
    const png = PNG.sync.read(screenshot);
    const offset = (Math.floor(png.height / 2) * png.width + Math.floor(png.width / 2)) * 4;
    const expectedPixel = scenario.pixel[info.project.name as "chromium" | "webkit"];
    expect([...png.data.subarray(offset, offset + 3)]).toEqual([...expectedPixel]);

    const captures = "/tmp/sv-plg-39-startup-paint";
    mkdirSync(captures, { recursive: true });
    writeFileSync(
      `${captures}/${info.project.name}-${scenario.file.replace(/\.html$/, ".png")}`,
      screenshot,
    );
  });
}
