// SPDX-License-Identifier: MIT
import { expect, test, type Frame } from "../support/quarantine-playwright";
import { createServer, type Server } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { createServer as createViteServer, type ViteDevServer } from "vite";
import { viewerScript } from "../support/page-shim";
let server: Server, vite: ViteDevServer, url: string;
test.beforeAll(async () => {
  const shim = viewerScript("LINK_INTERCEPTOR_JS");
  const dir = resolve("src-tauri/src/http/routes/dashboard_kit");
  const assets = new Map<string, { body: Buffer; type: string }>();
  for (const [name, type] of [
    ["editor.js", "text/javascript"],
    ["editor.css", "text/css"],
    ["tokens.css", "text/css"],
    ["kit.css", "text/css"],
    ["fonts/geist.woff2", "font/woff2"],
  ])
    assets.set("/kit/v1/" + name, { body: await readFile(resolve(dir, name)), type });
  server = createServer((req, res) => {
    const asset = assets.get(req.url!.split("?")[0]);
    if (asset) {
      res.setHeader("content-type", asset.type);
      res.end(asset.body);
      return;
    }
    res.setHeader("content-type", "text/html");
    res.setHeader(
      "content-security-policy",
      "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:",
    );
    res.end(`<!doctype html><link rel="stylesheet" href="/kit/v1/tokens.css"><link rel="stylesheet" href="/kit/v1/kit.css"><link rel="stylesheet" href="/kit/v1/editor.css"><script>window.__codesenseProxy={token:'editor'};${shim}</script><div id="editor"></div><script type="module">
      import {createEditor} from '/kit/v1/editor.js';
      window.createEditor=createEditor;window.changes=[];window.links=[];window.menu=null;window.sends=[];
      window.api=createEditor(document.getElementById('editor'),{markdown:'# Notes\\n\\n- [ ] Task',onChange:md=>changes.push(md),onSelection:info=>window.lastSelection=info,onLinkClick:href=>links.push(href),onSend:paste=>sends.push(paste),suggestions:{link:{items:q=>[{key:'note',label:'Other note',kind:'note',link:{kind:'note',target:'abc'}}],onState:s=>window.menu=s},slash:{onState:s=>window.menu=s}}});
      window.ready=true;
    </script>`);
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  vite = await createViteServer({
    configFile: false,
    root: process.cwd(),
    server: { host: "localhost", port: 0 },
    resolve: { alias: { "@": resolve("src") } },
    esbuild: { jsx: "automatic" },
    logLevel: "error",
  });
  await vite.listen();
  url = `http://localhost:${(vite.httpServer!.address() as { port: number }).port}/tests/frame-security/fixtures/editor-theme.html?port=${(server.address() as { port: number }).port}`;
});
test.afterAll(async () => {
  await vite?.close();
  await new Promise<void>((done) => (server ? server.close(() => done()) : done()));
});
async function mount(page: import("@playwright/test").Page): Promise<Frame> {
  await page.goto(url);
  const frame = await (await page.waitForSelector("iframe")).contentFrame();
  await frame!.waitForFunction(() => (window as any).ready === true);
  await expect
    .poll(() =>
      frame!.evaluate(() =>
        getComputedStyle(document.documentElement).getPropertyValue("--cs-bg").trim(),
      ),
    )
    .toBe("#fff");
  return frame!;
}
// The editor's Mod key follows the page's reported platform, which device emulation sets.
async function mod(frame: Frame) {
  return frame.evaluate(() => (/Mac|iP(hone|[oa]d)/.test(navigator.platform) ? "Meta" : "Control"));
}
async function markdown(frame: Frame) {
  return frame.evaluate(() => (window as any).api.getMarkdown());
}
test("committed editor types, undoes and toggles a task under the frame CSP", async ({ page }) => {
  const frame = await mount(page);
  await frame.locator(".scratch-note-editor h1").click();
  await page.keyboard.press("End");
  await page.keyboard.type(" updated");
  expect(await markdown(frame)).toContain("# Notes updated");
  await page.keyboard.press(`${await mod(frame)}+z`);
  expect(await markdown(frame)).toContain("# Notes\n");
  await frame.locator("input[type=checkbox]").check();
  expect(await markdown(frame)).toContain("- [x] Task");
  await page.keyboard.press(`${await mod(frame)}+Enter`);
  expect(await frame.evaluate(() => (window as any).sends)).toEqual([false]);
});
test("menus and code language edits consume Escape; links activate once", async ({ page }) => {
  const frame = await mount(page);
  await frame.evaluate(() => (window as any).api.setMarkdown(""));
  await frame.locator(".scratch-note-editor").click();
  await page.keyboard.type("/");
  await expect.poll(() => frame.evaluate(() => !!(window as any).menu)).toBe(true);
  await page.keyboard.press("ArrowDown");
  expect(await frame.evaluate(() => (window as any).menu.selectedIndex)).toBe(1);
  await page.keyboard.press("Escape");
  await expect.poll(() => frame.evaluate(() => (window as any).menu)).toBeNull();
  await frame.evaluate(() => (window as any).api.setMarkdown(""));
  await frame.locator(".scratch-note-editor").click();
  await page.keyboard.type("[[");
  await expect.poll(() => frame.evaluate(() => !!(window as any).menu)).toBe(true);
  await page.keyboard.press("Enter");
  expect(await markdown(frame)).toContain("[Other note](cs:note:abc)");
  await frame.evaluate(() => (window as any).api.setMarkdown("```typescript\nconst x = 1;\n```"));
  await frame.locator(".scratch-code-block-lang-label").click();
  await page.keyboard.press("Escape");
  await expect(frame.locator(".scratch-code-block-lang-input")).toHaveCount(0);
  expect(
    await page.evaluate(() =>
      (window as any).events.filter((e: any) => e.type === "codesense:key-escape"),
    ),
  ).toEqual([]);
  await frame.evaluate(() => (window as any).api.setMarkdown("[Note](cs:note:abc)"));
  await frame.locator("a").click();
  expect(await frame.evaluate(() => (window as any).links)).toEqual(["cs:note:abc"]);
  expect(
    await page.evaluate(() =>
      (window as any).events.filter((e: any) => e.type === "codesense:external-link"),
    ),
  ).toEqual([]);
});
test("paste-as-code uses the paste event without reading the clipboard", async ({ page }) => {
  const frame = await mount(page);
  await frame.evaluate(() => {
    (window as any).api.setMarkdown("");
    Object.defineProperty(navigator, "clipboard", {
      value: {
        readText() {
          throw new Error("must use paste event");
        },
      },
      configurable: true,
    });
  });
  await frame.locator(".scratch-note-editor").click();
  await page.keyboard.press(`${await mod(frame)}+Shift+v`);
  await frame.evaluate(() => {
    const data = new DataTransfer();
    data.setData("text/plain", "**literal**\n\tcode");
    document
      .querySelector(".scratch-note-editor")!
      .dispatchEvent(
        new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }),
      );
  });
  expect(await markdown(frame)).toBe("```plaintext\n**literal**\n\tcode\n```");
});
test("light tinted viewer ink stays neutral, dark theme and Geist follow the host", async ({
  page,
}) => {
  const frame = await mount(page);
  expect(
    await frame.locator(".scratch-note-editor").evaluate((el) => getComputedStyle(el).color),
  ).toBe("rgb(31, 35, 40)");
  const body = await frame.locator(".scratch-note-editor").evaluate((el) => ({
    size: getComputedStyle(el).fontSize,
    line: getComputedStyle(el).lineHeight,
  }));
  expect(body).toEqual({ size: "14px", line: "22.4px" });
  await frame.evaluate(() => (window as any).api.setMarkdown("```plaintext\ncode\n```"));
  expect(
    await frame
      .locator(".scratch-code-block-content")
      .evaluate((el) => getComputedStyle(el).padding),
  ).toBe("9px 10px 10px");
  await expect(frame.locator(".scratch-code-block-lang-label")).toHaveText("prompt");
  await frame.evaluate(() => document.fonts.ready);
  expect(await frame.evaluate(() => document.fonts.check('11px "Geist Variable"'))).toBe(true);
  await page.evaluate(() => (window as any).setScheme(true));
  await expect
    .poll(() => frame.locator(".scratch-note-editor").evaluate((el) => getComputedStyle(el).color))
    .toBe("rgb(230, 237, 243)");
  expect(
    await frame.locator(".scratch-note-editor").evaluate((el) => getComputedStyle(el).colorScheme),
  ).toBe("dark");
});

test("selection hooks report geometry and refreshed marks", async ({ page }) => {
  const frame = await mount(page);
  await frame.locator("h1").click();
  await page.keyboard.press("Meta+ArrowLeft");
  await page.keyboard.press("Meta+Shift+ArrowRight");
  await expect.poll(() => frame.evaluate(() => (window as any).lastSelection?.text)).toBe("Notes");
  expect(await frame.evaluate(() => (window as any).lastSelection.rect.width)).toBeGreaterThan(0);
  await frame.evaluate(() => (window as any).api.commands.toggleBold());
  expect(await frame.evaluate(() => (window as any).lastSelection.marks.bold)).toBe(true);
  await frame.evaluate(() => (window as any).api.setMarkdown("Next"));
  expect(await frame.evaluate(() => (window as any).lastSelection)).toBeNull();
});

for (const scenario of [
  { name: "slash without a page menu", query: "/", mode: "unwired" },
  { name: "link candidates without a page menu", query: "[[", mode: "items-only" },
  { name: "link menu without matches", query: "[[missing", mode: "empty-link" },
  { name: "slash menu without matches", query: "/zzzz", mode: "empty-slash" },
]) {
  test(`editing keys fall through for ${scenario.name}`, async ({ page }) => {
    const frame = await mount(page);
    await frame.evaluate((mode) => {
      const w = window as any;
      w.api.destroy();
      const element = document.getElementById("editor")!;
      element.replaceChildren();
      const link = {
        items: () =>
          mode === "items-only"
            ? [{ key: "note", label: "Other", kind: "note", link: { kind: "note", target: "abc" } }]
            : [],
      };
      w.api = w.createEditor(element, {
        suggestions:
          mode === "unwired"
            ? undefined
            : mode === "items-only"
              ? { link }
              : { link: { ...link, onState: () => {} }, slash: { onState: () => {} } },
      });
      w.keyResults = [];
      addEventListener("keydown", (event) =>
        w.keyResults.push({ key: event.key, prevented: event.defaultPrevented }),
      );
    }, scenario.mode);
    const root = frame.locator(".scratch-note-editor");
    await root.click();
    await page.keyboard.type(scenario.query);
    await page.keyboard.press("Escape");
    expect(
      await frame.evaluate(
        () => (window as any).keyResults.findLast((e: any) => e.key === "Escape").prevented,
      ),
    ).toBe(true);
    expect(
      await page.evaluate(() =>
        (window as any).events.filter((e: any) => e.type === "codesense:key-escape"),
      ),
    ).toEqual([]);
    await frame.evaluate(() => (window as any).api.setMarkdown(""));
    await root.click();
    await page.keyboard.type(scenario.query);
    await page.keyboard.press("Enter");
    await page.keyboard.type("after");
    await expect(root.locator("p")).toHaveCount(2);
    await expect(root.locator("p").last()).toHaveText("after");
    await expect(root.locator('[data-type="taskList"]')).toHaveCount(0);
    await frame.evaluate(() => (window as any).api.setMarkdown(""));
    await root.click();
    await page.keyboard.type(scenario.query);
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("ArrowUp");
    expect(
      await frame.evaluate(() =>
        (window as any).keyResults
          .filter((e: any) => e.key.startsWith("Arrow"))
          .map((e: any) => e.prevented),
      ),
    ).toEqual([false, false]);
  });
}

for (const explicit of [false, true]) {
  test(`heading navigation uses ${explicit ? "the explicit" : "the nearest"} scroll container`, async ({
    page,
  }) => {
    const frame = await mount(page);
    await frame.evaluate((explicit) => {
      const w = window as any;
      w.api.destroy();
      const element = document.getElementById("editor")!;
      element.replaceChildren();
      const container = document.createElement("div");
      container.id = "scroll-port";
      container.style.cssText = "height:200px;overflow:auto";
      element.before(container);
      container.append(element);
      const paragraphs = Array.from({ length: 30 }, (_, i) => `Paragraph ${i}.`).join("\n\n");
      w.api = w.createEditor(element, {
        markdown: `# Top\n\n${paragraphs}\n\n## Bottom\n\n${paragraphs}`,
        scrollContainer: explicit ? container : undefined,
      });
      w.api.scrollToHeading("bottom");
    }, explicit);
    const result = await frame.evaluate(() => {
      const container = document.getElementById("scroll-port")!;
      const heading = document.querySelector("h2")!;
      return {
        scroll: container.scrollTop,
        offset: heading.getBoundingClientRect().top - container.getBoundingClientRect().top,
        pageScroll: document.scrollingElement!.scrollTop,
      };
    });
    expect(result.scroll).toBeGreaterThan(500);
    expect(Math.abs(result.offset)).toBeLessThan(2);
    expect(result.pageScroll).toBe(0);
  });
}

test("hover attribution appends the chip to the hovered block instead of the caret block", async ({
  page,
}) => {
  const frame = await mount(page);
  await frame.evaluate(() => (window as any).api.setMarkdown("Caret line\n\nHovered line"));
  await frame.locator(".scratch-note-editor > p").first().click();
  // The editor adopts a clicked caret on the browser's async selectionchange.
  await expect
    .poll(() => frame.evaluate(() => (window as any).api.commands.sendTarget().text))
    .toBe("Caret line");
  const target = await frame.evaluate(() => {
    const rect = document.querySelectorAll(".scratch-note-editor > p")[1].getBoundingClientRect();
    return (window as any).api.blockAt({ x: rect.left + 5, y: rect.top + rect.height / 2 });
  });
  expect(target).toMatchObject({ text: "Hovered line", pos: expect.any(Number) });
  expect(target.pos).toBeGreaterThan(0);
  expect(
    await frame.evaluate(
      (pos) => (window as any).api.commands.appendAgentRef(pos, { handle: "dev", name: "Ricky" }),
      target.pos,
    ),
  ).toBe(true);
  expect(await markdown(frame)).toBe("Caret line\n\nHovered line [@Ricky](cs:agent:dev)");
  expect(
    await frame.locator(".nt-agent-chip").evaluate((el) => getComputedStyle(el).backgroundColor),
  ).not.toBe("rgba(0, 0, 0, 0)");
});

test("heading navigation stays aligned on first and repeated jumps in an already-scrolled page", async ({
  page,
}) => {
  const frame = await mount(page);
  await frame.evaluate(() => {
    const paragraphs = Array.from({ length: 40 }, (_, i) => `Paragraph ${i}.`).join("\n\n");
    (window as any).api.setMarkdown(`# Top\n\n${paragraphs}\n\n## Middle\n\n${paragraphs}`);
    window.scrollTo(0, 300);
  });
  expect(await frame.evaluate(() => window.scrollY)).toBe(300);
  let previousScroll = 0;
  for (let jump = 0; jump < 2; jump++) {
    const result = await frame.evaluate(() => {
      (window as any).api.scrollToHeading("middle");
      return {
        scroll: window.scrollY,
        top: document.querySelector("h2")!.getBoundingClientRect().top,
      };
    });
    expect(result.scroll).toBeGreaterThan(300);
    expect(Math.abs(result.top)).toBeLessThan(2);
    if (jump > 0) expect(Math.abs(result.scroll - previousScroll)).toBeLessThanOrEqual(1);
    previousScroll = result.scroll;
  }
});

test("page menus choose the hovered row with Tab and selection can become a link query", async ({
  page,
}) => {
  const frame = await mount(page);
  const root = frame.locator(".scratch-note-editor");
  await frame.evaluate(() => (window as any).api.setMarkdown(""));
  await root.click();
  await page.keyboard.type("/");
  await frame.evaluate(() => (window as any).menu.setIndex(2));
  expect(await frame.evaluate(() => (window as any).menu.selectedIndex)).toBe(2);
  await page.keyboard.press("Tab");
  await expect(root.locator("h2")).toHaveCount(1);
  await frame.evaluate(() => (window as any).api.setMarkdown(""));
  await root.click();
  await page.keyboard.type("[[");
  await frame.evaluate(() => (window as any).menu.setIndex(0));
  await page.keyboard.press("Tab");
  expect(await markdown(frame)).toContain("[Other note](cs:note:abc)");
  await frame.evaluate(() => {
    (window as any).api.setMarkdown("Words here");
    (window as any).api.focus("start");
  });
  await page.keyboard.type("First ");
  expect(await markdown(frame)).toBe("First Words here");
  await frame.evaluate(() => (window as any).api.focus("end"));
  await root.click();
  await page.keyboard.press(`${await mod(frame)}+a`);
  await expect
    .poll(() => frame.evaluate(() => (window as any).lastSelection?.text))
    .toBe("First Words here");
  await frame.evaluate(() => (window as any).api.commands.linkSelection());
  await expect(root).toHaveText("[[First Words here");
  await expect
    .poll(() => frame.evaluate(() => (window as any).menu?.items[0]?.label))
    .toBe("Other note");
  await page.keyboard.press("Tab");
  expect(await markdown(frame)).toContain("[Other note](cs:note:abc)");
});
