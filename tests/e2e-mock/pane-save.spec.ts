// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { shimSource, viewerScript } from "../support/page-shim";
import { expect, test, type Frame, type Page } from "@playwright/test";

function pane(
  version: number,
  options: {
    wait?: boolean;
    broken?: boolean;
    snapshot?: "oversize" | "late";
    flush?: boolean;
    stream?: boolean;
    autofocus?: boolean;
    slowRetirement?: boolean;
  } = {},
) {
  return `<!doctype html><html><body style="margin:0;background:var(--cs-bg);color:var(--cs-fg)">
  <strong id="version">Version ${version}</strong><input aria-label="Draft"><div id="scroll" style="height:70px;overflow:auto"><div style="height:800px">Scroll content</div></div><output id="rows"></output>
  <script>
    window.inputEvents=[]; window.updates=0; window.captures=0;
    const input=document.querySelector('input'), scroll=document.querySelector('#scroll');
    input.addEventListener('input',()=>window.inputEvents.push(input.value));
    ${options.autofocus ? "input.focus();" : ""}
    codesense.onBeforeReload?.(()=>{window.captures++;return ${options.snapshot === "oversize" ? "({text:'é'.repeat(32768)})" : options.snapshot === "late" ? "new Promise(resolve=>setTimeout(()=>resolve({text:'late'}),300))" : "({draft:input.value,scroll:scroll.scrollTop})"};});
    ${
      options.flush
        ? `let pending; input.addEventListener('input',()=>{clearTimeout(pending);pending=setTimeout(save,1000)});
      function save(){clearTimeout(pending);return codesense.setUiState('ui:right-pane',{draft:input.value})}
      codesense.onFlush(save);`
        : ""
    }
    ${options.slowRetirement ? "let flushes=0;codesense.onFlush(()=>{if(++flushes>=2){window.retirementStarted=true;return new Promise(()=>{});}});" : ""}
    async function start(){
      const state=await (codesense.reloadState?.()??Promise.resolve(null)); window.restored=state;
      if(state){input.value=state.draft||'';scroll.scrollTop=state.scroll||0;}
      ${options.flush ? `const ui=await codesense.subscribe('ui:right-pane',{onChange:s=>{input.value=s.snapshot[0]?.draft||'';window.savedDraft=input.value}});` : ""}
      ${options.stream ? `await codesense.subscribe('sessions',{onChange:s=>{document.querySelector('#rows').textContent=s.snapshot[0]?.displayName||'';window.updates++}});` : ""}
      ${options.broken ? "throw new Error('broken save')" : "codesense.ready();"}
    }
    window.start=start;
    ${options.wait ? "" : "start();"}
  </script></body></html>`;
}

async function mount(page: Page, id: string, html = pane(1)) {
  await page.request.put(`/__mock-panes/default/${id}`, { data: html });
  await page.goto("/?harness=live-hub");
  await page.waitForFunction(() => !!(window as any).__liveHubMount);
  await page.evaluate((id) => (window as any).__liveHubMount(1, true, true, id), id);
  await expect(page.locator("[data-slot-id]")).not.toHaveAttribute("aria-busy", "true");
  await page.evaluate(() => {
    const internals = (window as any).__TAURI_INTERNALS__;
    const invoke = internals.invoke;
    (window as any).__hostCalls = [];
    internals.invoke = (command: string, args: unknown) => {
      (window as any).__hostCalls.push({ command, args });
      return invoke(command, args);
    };
  });
  const frame = await current(page, 1);
  return frame;
}
async function current(page: Page, version: number) {
  let found: Frame | undefined;
  await expect
    .poll(async () => {
      for (const frame of page.frames().filter((f) => f.url().includes("/pane/"))) {
        try {
          if ((await frame.locator("#version").textContent()) === `Version ${version}`)
            found = frame;
        } catch {
          /* frame retired during lookup */
        }
      }
      return !!found;
    })
    .toBe(true);
  return found!;
}
async function save(page: Page, id: string, html: string) {
  await page.request.put(`/__mock-panes/default/${id}`, { data: html });
}
async function sample(page: Page) {
  await page.evaluate(() => {
    const result = { samples: 0, blank: 0, swaps: 0, previous: "", maxFrames: 0 };
    (window as any).__saveSamples = result;
    (window as any).__stopSaveSamples = false;
    function tick() {
      if ((window as any).__stopSaveSamples) return;
      const frames = [...document.querySelectorAll<HTMLIFrameElement>("[data-slot-id] iframe")];
      result.samples++;
      result.maxFrames = Math.max(result.maxFrames, frames.length);
      const shown = frames.filter((frame) => {
        if (!frame.getBoundingClientRect().height) return false;
        for (let el: HTMLElement | null = frame; el; el = el.parentElement) {
          const s = getComputedStyle(el);
          if (s.opacity === "0" || s.visibility === "hidden" || s.display === "none") return false;
        }
        return true;
      });
      if (!shown.length) result.blank++;
      const identity = shown.at(-1)?.src || "";
      if (identity && result.previous && result.previous !== identity) result.swaps++;
      if (identity) result.previous = identity;
      requestAnimationFrame(tick);
    }
    tick();
  });
}
async function samples(page: Page) {
  return page.evaluate(() => {
    (window as any).__stopSaveSamples = true;
    return (window as any).__saveSamples;
  });
}

test("save keeps a painted old pane until its candidate is ready; scroll and draft round-trip", async ({
  page,
}) => {
  const old = await mount(page, "save-state");
  await old.getByRole("textbox").fill("pre-save text");
  await old.locator("#scroll").evaluate((el) => (el.scrollTop = 230));
  await sample(page);
  await save(page, "save-state", pane(2, { wait: true }));
  const candidate = await current(page, 2);
  await expect(page.locator("[data-pane-retained]")).not.toHaveCSS("opacity", "0");
  await expect(page.locator("[data-pane-retained]")).toHaveClass(/pointer-events-none/);
  await page.waitForTimeout(200);
  await candidate.evaluate(() => (window as any).start());
  await expect(candidate.getByRole("textbox")).toHaveValue("pre-save text");
  expect(await candidate.locator("#scroll").evaluate((el) => el.scrollTop)).toBe(230);
  await expect.poll(() => old.isDetached()).toBe(true);
  const result = await samples(page);
  expect(result.samples).toBeGreaterThan(5);
  expect(result).toMatchObject({ blank: 0, swaps: 1, maxFrames: 2 });
});

test("broken save discards the candidate, shows a strip and unfreezes the old input", async ({
  page,
}) => {
  const old = await mount(page, "save-broken");
  await sample(page);
  await save(page, "save-broken", pane(2, { broken: true }));
  await expect(page.getByTestId("slot-fallback-strip")).toContainText(
    "keeping the previous version",
  );
  await expect(page.locator("[data-slot-id] iframe")).toHaveCount(1);
  const logs = await page.evaluate(() =>
    (window as any).__hostCalls.filter((c: any) => c.command === "plugin:log|log"),
  );
  expect(
    logs.some(
      (c: any) =>
        JSON.stringify(c.args).includes("slot-fallback") &&
        JSON.stringify(c.args).includes("instance") &&
        JSON.stringify(c.args).includes("script"),
    ),
  ).toBe(true);
  await old.getByRole("textbox").fill("still interactive");
  await expect(old.getByRole("textbox")).toHaveValue("still interactive");
  expect(await samples(page)).toMatchObject({ blank: 0, swaps: 0 });
});

test("three saves in 100ms create one candidate and one swap", async ({ page }) => {
  const old = await mount(page, "save-burst");
  let mints = 0;
  page.on("request", (r) => {
    if (r.url().endsWith("/__mock-frames") && r.method() === "POST") mints++;
  });
  await sample(page);
  for (const version of [2, 3, 4]) {
    await save(page, "save-burst", pane(version));
    await page.waitForTimeout(20);
  }
  await current(page, 4);
  await expect.poll(() => old.isDetached()).toBe(true);
  expect(mints).toBe(1);
  expect(await samples(page)).toMatchObject({ blank: 0, swaps: 1, maxFrames: 2 });
});

for (const mode of ["oversize", "late"] as const)
  test(`${mode} snapshot is dropped and logged in both frame and host, swap proceeds`, async ({
    page,
  }) => {
    const warnings: string[] = [];
    page.on("console", (m) => {
      if (m.type() === "warning" || m.text().includes("pane-reload")) warnings.push(m.text());
    });
    const old = await mount(page, `save-${mode}`, pane(1, { snapshot: mode }));
    await save(page, `save-${mode}`, pane(2));
    const candidate = await current(page, 2);
    await expect.poll(() => old.isDetached()).toBe(true);
    expect(await candidate.evaluate(() => (window as any).restored)).toBeNull();
    expect(warnings.filter((s) => s.includes("beforeReload"))).toHaveLength(1);
    const logs = await page.evaluate(() =>
      (window as any).__hostCalls.filter((c: any) => c.command === "plugin:log|log"),
    );
    expect(
      logs.some(
        (c: any) =>
          JSON.stringify(c.args).includes("[pane-reload]") &&
          JSON.stringify(c.args).includes("beforeReload"),
      ),
    ).toBe(true);
  });

test("typing before a save is flushed ahead of candidate reads; typing during the frozen window is blocked while real hub deltas stream", async ({
  page,
}) => {
  const old = await mount(page, "save-stream", pane(1, { flush: true, stream: true }));
  await expect(old.locator("#rows")).toHaveText("Session 0");
  await page.evaluate(() => {
    (window as any).__stream = setInterval(() => (window as any).__liveHubFixture.fire(), 100);
  });
  await old.getByRole("textbox").pressSequentially("draft before save");
  let mints = 0;
  const released: string[] = [];
  page.on("request", (r) => {
    if (r.url().endsWith("/__mock-frames") && r.method() === "POST") mints++;
    if (r.url().includes("/__mock-frames/") && r.method() === "DELETE") released.push(r.url());
  });
  const oldHost = new URL(old.url()).hostname;
  const oldFrameId = oldHost.slice(2).split(".")[0];
  expect(await page.evaluate(() => (window as any).__liveHubFrameIds())).toContain(oldFrameId);
  await sample(page);
  await save(page, "save-stream", pane(2, { wait: true, flush: true, stream: true }));
  await expect(page.locator("[data-pane-retained]")).toHaveClass(/pointer-events-none/);
  await page.keyboard.type(" must be blocked");
  expect(await old.getByRole("textbox").inputValue()).toBe("draft before save");
  const candidate = await current(page, 2);
  await candidate.evaluate(() => (window as any).start());
  await expect(candidate.getByRole("textbox")).toHaveValue("draft before save");
  expect(await candidate.evaluate(() => (window as any).savedDraft)).toBe("draft before save");
  await expect.poll(() => old.isDetached()).toBe(true);
  await expect
    .poll(() => page.evaluate(() => (window as any).__liveHubFrameIds()))
    .not.toContain(oldFrameId);
  const before = await candidate.evaluate(() => (window as any).updates);
  await expect
    .poll(() => candidate.evaluate(() => (window as any).updates))
    .toBeGreaterThan(before + 3);
  await page.waitForTimeout(1100);
  expect(mints).toBe(1);
  expect(released.some((s) => s.includes(oldHost.slice(2).split(".")[0]))).toBe(true);
  expect(await samples(page)).toMatchObject({ blank: 0, swaps: 1, maxFrames: 2 });
  await page.evaluate(() => clearInterval((window as any).__stream));
  await page.evaluate(() => (window as any).__liveHubMount(0));
  await expect.poll(() => candidate.isDetached()).toBe(true);
});

test("a slow candidate uses the absolute 30s deadline and returns input to the old pane", async ({
  page,
}) => {
  const old = await mount(page, "save-deadline");
  await sample(page);
  const started = Date.now();
  await save(page, "save-deadline", pane(2, { wait: true }));
  await current(page, 2);
  await expect(page.getByText("Updating…")).toBeVisible({ timeout: 2000 });
  await expect(page.getByTestId("slot-fallback-strip")).toBeVisible({ timeout: 32000 });
  expect(Date.now() - started).toBeLessThan(32000);
  await expect(page.getByText("Updating…")).toHaveCount(0);
  const logs = await page.evaluate(() =>
    (window as any).__hostCalls.filter((c: any) => c.command === "plugin:log|log"),
  );
  expect(
    logs.some(
      (c: any) =>
        JSON.stringify(c.args).includes("slot-fallback") &&
        JSON.stringify(c.args).includes("instance") &&
        JSON.stringify(c.args).includes("timeout"),
    ),
  ).toBe(true);
  await old.getByRole("textbox").fill("after timeout");
  await expect(old.getByRole("textbox")).toHaveValue("after timeout");
  expect(await samples(page)).toMatchObject({ blank: 0, swaps: 0 });
});

test("candidate autofocus does not move focus away from the frozen shown pane", async ({
  page,
}) => {
  const old = await mount(page, "save-focus");
  await old.getByRole("textbox").fill("focused draft");
  await expect(old.getByRole("textbox")).toBeFocused();
  await save(page, "save-focus", pane(2, { wait: true, autofocus: true }));
  const candidate = await current(page, 2);
  await expect(old.getByRole("textbox")).toBeFocused();
  await candidate.evaluate(() => (window as any).start());
  await expect.poll(() => old.isDetached()).toBe(true);
  await expect(candidate.getByRole("textbox")).toBeFocused();
});

test("candidate autofocus preserves focus moved outside the slot, including at swap", async ({
  page,
}) => {
  const old = await mount(page, "save-focus-outside");
  await old.getByRole("textbox").fill("draft");
  await page.evaluate(() => {
    const input = document.createElement("input");
    input.setAttribute("aria-label", "Outside");
    document.body.append(input);
  });
  await save(page, "save-focus-outside", pane(2, { wait: true }));
  const candidate = await current(page, 2);
  await page.getByRole("textbox", { name: "Outside" }).fill("outside focus");
  await candidate.getByRole("textbox").evaluate((input) => input.focus());
  await expect(page.getByRole("textbox", { name: "Outside" })).toBeFocused();
  await candidate.evaluate(() => (window as any).start());
  await expect.poll(() => old.isDetached()).toBe(true);
  await expect(page.getByRole("textbox", { name: "Outside" })).toBeFocused();
});

test("hiding during debounce defers reload; hiding a candidate and unmounting release its frames", async ({
  page,
}) => {
  const id = "save-hidden";
  const old = await mount(page, id);
  await save(page, id, pane(2, { wait: true }));
  await page.evaluate((id) => (window as any).__liveHubMount(1, false, true, id), id);
  await page.waitForTimeout(200);
  await expect(page.locator("[data-slot-id] iframe")).toHaveCount(1);
  await expect(page.locator("[data-pane-current]")).not.toHaveClass(/pointer-events-none/);
  await page.evaluate((id) => (window as any).__liveHubMount(1, true, true, id), id);
  const candidate = await current(page, 2);
  const frameId = new URL(candidate.url()).hostname.slice(2).split(".")[0];
  await page.evaluate((id) => (window as any).__liveHubMount(1, false, true, id), id);
  await expect.poll(() => candidate.isDetached()).toBe(true);
  await expect
    .poll(() => page.evaluate(() => (window as any).__liveHubFrameIds()))
    .not.toContain(frameId);
  expect(old.isDetached()).toBe(false);
  await page.evaluate((id) => (window as any).__liveHubMount(1, true, true, id), id);
  const next = await current(page, 2);
  expect(next).not.toBe(candidate);
  await page.evaluate(() => (window as any).__liveHubMount(0));
  await expect.poll(() => next.isDetached() && old.isDetached()).toBe(true);
  await expect.poll(() => page.evaluate(() => (window as any).__liveHubFrameIds())).toEqual([]);
});

test("a second save freezes and captures the shown attempt while its predecessor retires", async ({
  page,
}) => {
  const id = "save-during-retirement";
  const first = await mount(page, id, pane(1, { slowRetirement: true }));
  await first.getByRole("textbox").fill("version one");
  await save(page, id, pane(2, { wait: true }));
  const second = await current(page, 2);
  await second.evaluate(() => (window as any).start());
  await expect
    .poll(() => second.evaluate(() => (window as any).restored?.draft))
    .toBe("version one");
  await expect.poll(() => first.evaluate(() => (window as any).retirementStarted)).toBe(true);
  expect(first.isDetached()).toBe(false);
  await second.getByRole("textbox").fill("version two");
  await save(page, id, pane(3, { wait: true }));
  await expect.poll(() => second.evaluate(() => (window as any).captures)).toBe(1);
  await page.waitForTimeout(50);
  await page.keyboard.type(" should be blocked");
  const third = await current(page, 3);
  await third.evaluate(async () => {
    (window as any).restored = await (window as any).codesense.reloadState();
  });
  expect(await second.getByRole("textbox").inputValue()).toBe("version two");
  expect(await second.evaluate(() => (window as any).captures)).toBe(1);
  expect(await third.evaluate(() => (window as any).restored)).toEqual({
    draft: "version two",
    scroll: 0,
  });
});

test("baseline HTML scroll and links match the merge-base shim", async ({ page }) => {
  const base = execFileSync("git", ["merge-base", "HEAD", "origin/main"], {
    encoding: "utf8",
  }).trim();
  const source = execFileSync("git", ["show", `${base}:src-tauri/src/http/routes/viewer.rs`], {
    encoding: "utf8",
  });
  const before = source.match(/const LINK_INTERCEPTOR_JS: &str = r#"([\s\S]*?)"#;/)![1];
  for (const shim of [before, viewerScript("LINK_INTERCEPTOR_JS")]) {
    await page.setContent(
      '<iframe id="baseline" sandbox="allow-scripts" style="height:250px;width:500px"></iframe>',
    );
    await page.locator("#baseline").evaluate((f, html) => {
      (f as HTMLIFrameElement).srcdoc = html;
    }, `<script>${shim}</script><a href="#end">End</a><div style="height:3000px">Before comparison</div><p id="end">End of comparison</p>`);
    const baseline = page.frames()[1];
    await baseline.getByRole("link", { name: "End", exact: true }).waitFor();
    await page.locator("#baseline").hover();
    await page.mouse.wheel(0, 500);
    await expect.poll(() => baseline.evaluate(() => window.scrollY)).toBeGreaterThan(0);
    await baseline.evaluate(() => window.scrollTo(0, 0));
    await baseline.getByRole("link", { name: "End", exact: true }).click();
    await expect.poll(() => baseline.evaluate(() => location.hash)).toBe("#end");
  }
});

test("Tab and Escape leave a frozen pane while typing remains blocked", async ({ page }) => {
  const id = "save-keyboard-navigation";
  const old = await mount(page, id);
  await old.getByRole("textbox").fill("draft");
  await page.evaluate(() => {
    const input = document.createElement("input");
    input.id = "outside";
    document.body.append(input);
    const slot = document.querySelector("[data-slot-id]")!;
    slot.setAttribute("data-fullscreen", "true");
    slot.addEventListener("codesense:frame-escape", () => input.focus());
  });
  await save(page, id, pane(2, { wait: true }));
  await current(page, 2);
  await page.keyboard.type(" blocked");
  await expect(old.getByRole("textbox")).toHaveValue("draft");
  await page.keyboard.press("Tab");
  await expect(old.getByRole("textbox")).not.toBeFocused();
  await old.getByRole("textbox").evaluate((input: HTMLInputElement) => input.focus());
  await page.keyboard.press("Escape");
  await expect(old.getByRole("textbox")).not.toBeFocused();
});

for (const attemptsFocus of [false, true]) {
  test(`focus in another iframe survives candidate ${attemptsFocus ? "focus suppression and " : ""}swap`, async ({
    page,
  }) => {
    const id = `save-other-frame-${attemptsFocus}`;
    const old = await mount(page, id);
    await old.getByRole("textbox").fill("draft");
    await save(page, id, pane(2, { wait: true }));
    const candidate = await current(page, 2);
    await page.evaluate(() => {
      const other = document.createElement("iframe");
      other.id = "other-pane";
      other.srcdoc = '<input aria-label="Other pane">';
      document.body.append(other);
    });
    const input = page.frameLocator("#other-pane").getByRole("textbox");
    await input.fill("other draft");
    if (attemptsFocus)
      await candidate.getByRole("textbox").evaluate((el: HTMLInputElement) => el.focus());
    await expect(input).toBeFocused();
    await candidate.evaluate(() => (window as any).codesense.ready());
    await expect.poll(() => old.isDetached()).toBe(true);
    await expect(input).toBeFocused();
  });
}

for (const broken of [false, true]) {
  test(`embedded message edits freeze and flush before replacement ${broken ? "fails and unfreezes" : "reads them"}`, async ({
    page,
  }) => {
    const id = `save-child-${broken}`;
    const grants = new Map<string, { token: string }>();
    page.on("response", async (response) => {
      if (
        new URL(response.url()).pathname === "/__mock-frames" &&
        response.request().method() === "POST"
      ) {
        const grant = await response.json();
        grants.set(grant.origin, grant);
      }
    });
    const childHtml = `<input aria-label="Message draft"><script>
      const input=document.querySelector('input');
      codesense.onFlush(async()=>{window.flushing=true;await new Promise(resolve=>window.finishFlush=resolve);await codesense.setPrefs({draft:input.value});window.flushed=true;});
      codesense.prefs().then(saved=>{input.value=saved.draft||'';codesense.ready();});
    </script>`;
    await page.route("**/message/**", async (route) => {
      const origin = new URL(route.request().url()).origin;
      await expect.poll(() => grants.has(origin)).toBe(true);
      const token = grants.get(origin)!.token;
      await route.fulfill({
        contentType: "text/html",
        body: `<script>window.__codesenseProxy={token:${JSON.stringify(token)}};</script><script>${shimSource().replaceAll("window.parent", "window.top")}</script>${childHtml}`,
      });
    });
    const parent = (
      version: number,
      bootChild: boolean,
    ) => `<!doctype html><html><body><strong id="version">Version ${version}</strong><script>
      window.addChild=async()=>{const child=await codesense.embedMessage({org:'acme',team:'dev',messageId:7});const frame=document.createElement('iframe');frame.setAttribute('sandbox','allow-scripts allow-same-origin');frame.src=child.url;document.body.append(frame);};
      (async()=>{await codesense.reloadState();${bootChild ? (broken ? "throw new Error('broken save');" : "await addChild();") : ""}codesense.ready();})();
    </script></body></html>`;
    const old = await mount(page, id, parent(1, false));
    await page.evaluate(() => {
      const internals = (window as any).__TAURI_INTERNALS__,
        invoke = internals.invoke;
      internals.invoke = (command: string, args: unknown) =>
        command === "agent_read_inbox"
          ? Promise.resolve({
              messages: [
                {
                  messageId: 7,
                  from: "Mia",
                  to: "human",
                  toHuman: true,
                  contentType: "html",
                  text: "<input>",
                },
              ],
            })
          : invoke(command, args);
    });
    await old.evaluate(() => (window as any).addChild());
    let child: Frame | undefined;
    await expect
      .poll(() => {
        child = page.frames().find((frame) => frame.url().includes("/message/"));
        return !!child;
      })
      .toBe(true);
    await child!.getByRole("textbox").fill("message draft");
    await save(page, id, parent(2, true));
    await child!.waitForFunction(() => (window as any).flushing === true);
    await page.keyboard.type(" blocked");
    await expect(child!.getByRole("textbox")).toHaveValue("message draft");
    expect(
      page.frames().filter((frame) => frame.url().includes("codesenseCandidate=1")),
    ).toHaveLength(0);
    await child!.evaluate(() => (window as any).finishFlush());
    if (broken) {
      await expect(page.getByText(/keeping the previous version/i).first()).toBeVisible();
      await child!.getByRole("textbox").fill("editable again");
      await expect(child!.getByRole("textbox")).toHaveValue("editable again");
      expect(old.isDetached()).toBe(false);
    } else {
      await current(page, 2);
      let next: Frame | undefined;
      await expect
        .poll(() => {
          next = page
            .frames()
            .find((frame) => frame !== child && frame.url().includes("/message/"));
          return !!next;
        })
        .toBe(true);
      await expect(next!.getByRole("textbox")).toHaveValue("message draft");
    }
  });
}

test("a frozen range blocks native arrows and authored modifier edits", async ({ page }) => {
  const id = "save-frozen-range";
  const html = `<!doctype html><html><body><strong id="version">Version 1</strong><input aria-label="Size" type="range" min="0" max="100" value="10"><script>
    const input=document.querySelector('input');
    input.addEventListener('keydown',e=>{if(e.altKey&&e.key==='ArrowRight')input.value='50'});
    codesense.onBeforeReload(()=>({size:input.value}));codesense.reloadState().then(()=>codesense.ready());
  </script></body></html>`;
  const old = await mount(page, id, html);
  await old.getByRole("slider").focus();
  await save(page, id, pane(2, { wait: true }));
  await current(page, 2);
  await page.keyboard.press("ArrowRight");
  await expect(old.getByRole("slider")).toHaveValue("10");
  await page.keyboard.press("Alt+ArrowRight");
  await expect(old.getByRole("slider")).toHaveValue("10");
});

test("candidate-owned embedded focus stays with the current owner through the swap", async ({
  page,
}) => {
  const id = "save-candidate-child-focus";
  const grants = new Map<string, { token: string }>();
  page.on("response", async (response) => {
    if (
      new URL(response.url()).pathname === "/__mock-frames" &&
      response.request().method() === "POST"
    ) {
      const grant = await response.json();
      grants.set(grant.origin, grant);
    }
  });
  await page.route("**/message/**", async (route) => {
    const origin = new URL(route.request().url()).origin;
    await expect.poll(() => grants.has(origin)).toBe(true);
    await route.fulfill({
      contentType: "text/html",
      body: `<script>window.__codesenseProxy={token:${JSON.stringify(grants.get(origin)!.token)}};</script><script>${shimSource().replaceAll("window.parent", "window.top")}</script><input aria-label="Child focus"><script>document.querySelector('input').focus();window.didFocus=true;codesense.ready();</script>`,
    });
  });
  const old = await mount(page, id);
  await old.getByRole("textbox").fill("old focused");
  await page.evaluate(() => {
    const internals = (window as any).__TAURI_INTERNALS__,
      invoke = internals.invoke;
    internals.invoke = (command: string, args: unknown) =>
      command === "agent_read_inbox"
        ? Promise.resolve({
            messages: [
              {
                messageId: 7,
                from: "Mia",
                to: "human",
                toHuman: true,
                contentType: "html",
                text: "<input>",
              },
            ],
          })
        : invoke(command, args);
  });
  const html = `<!doctype html><html><body><strong id="version">Version 2</strong><script>
    (async()=>{await codesense.reloadState();const child=await codesense.embedMessage({org:'acme',team:'dev',messageId:7});const f=document.createElement('iframe');f.setAttribute('sandbox','allow-scripts allow-same-origin');f.src=child.url;document.body.append(f);})();
  </script></body></html>`;
  await save(page, id, html);
  const candidate = await current(page, 2);
  let child: Frame | undefined;
  await expect
    .poll(() => {
      child = page.frames().find((frame) => frame.url().includes("/message/"));
      return !!child;
    })
    .toBe(true);
  await child!.waitForFunction(() => (window as any).didFocus === true);
  await expect(old.getByRole("textbox")).toBeFocused();
  await page.evaluate(() => {
    const f = document.createElement("iframe");
    f.id = "other-pane";
    f.srcdoc = '<input aria-label="Other pane">';
    document.body.append(f);
  });
  const other = page.frameLocator("#other-pane").getByRole("textbox");
  await other.fill("other draft");
  await child!.getByRole("textbox").evaluate((input: HTMLInputElement) => input.focus());
  await expect(other).toBeFocused();
  await candidate.evaluate(() => (window as any).codesense.ready());
  await expect.poll(() => old.isDetached()).toBe(true);
  await expect(other).toBeFocused();
});

test("an authored document reload reads persistent state after consuming the one-shot handoff", async ({
  page,
}) => {
  const id = "save-handoff-once";
  const old = await mount(page, id);
  await old.getByRole("textbox").fill("snapshot from version one");
  const html = `<!doctype html><html><body><strong id="version">Version 2</strong><input aria-label="Draft"><script>
    window.boot=Math.random();const input=document.querySelector('input');
    codesense.onBeforeReload(()=>({draft:input.value}));
    (async()=>{window.restored=await codesense.reloadState();const persisted=await codesense.prefs();input.value=window.restored?window.restored.draft:persisted.draft||'';codesense.ready();})();
  </script></body></html>`;
  await save(page, id, html);
  const second = await current(page, 2);
  await expect.poll(() => old.isDetached()).toBe(true);
  await expect(second.getByRole("textbox")).toHaveValue("snapshot from version one");
  const boot = await second.evaluate(() => (window as any).boot);
  await second.evaluate(async () => {
    await (window as any).codesense.setPrefs({ draft: "new persisted draft" });
    location.reload();
  });
  await second.waitForFunction(
    (previous) => (window as any).boot !== previous && "restored" in window,
    boot,
  );
  await expect(second.getByRole("textbox")).toHaveValue("new persisted draft");
  expect(await second.evaluate(() => (window as any).restored)).toBeNull();
});

test("a frozen shipped kit code block uses Tab for navigation without editing", async ({
  page,
}) => {
  await page.route("**/kit/v1/editor.js", (route) =>
    route.fulfill({
      contentType: "text/javascript",
      body: readFileSync("src-tauri/src/http/routes/dashboard_kit/editor.js", "utf8"),
    }),
  );
  const editorPane = (
    version: number,
    hold = false,
  ) => `<!doctype html><html><body><strong id="version">Version ${version}</strong><div id="editor"></div><button>After editor</button><script type="module">
    import {createEditor} from '/kit/v1/editor.js';
    window.ed=createEditor(document.querySelector('#editor'),{markdown:${JSON.stringify("```\noriginal code\n```")},onChange:value=>{window.edits=(window.edits||[]).concat(value)}});
    codesense.onBeforeReload(()=>({markdown:ed.getMarkdown()}));
    codesense.onFlush(()=>{window.flushed=ed.getMarkdown()});
    window.restored=await codesense.reloadState();if(window.restored)ed.setMarkdown(window.restored.markdown);
    window.booted=true;${hold ? "" : "codesense.ready();"}
  </script></body></html>`;
  const id = "save-kit-tab";
  const old = await mount(page, id, editorPane(1));
  await old.waitForFunction(() => (window as any).booted);
  const before = await old.evaluate(() => (window as any).ed.getMarkdown());
  await old.locator(".scratch-code-block-content").click();
  await page.keyboard.press("End");
  await page.keyboard.press("Tab");
  expect(await old.evaluate(() => (window as any).ed.getMarkdown())).not.toBe(before);
  await old.evaluate((value) => (window as any).ed.setMarkdown(value), before);
  await old.locator(".scratch-code-block-content").click();
  await page.keyboard.press("End");
  await save(page, id, editorPane(2, true));
  const next = await current(page, 2);
  await next.waitForFunction(() => (window as any).booted);
  await expect(old.locator("[contenteditable=true]")).toBeFocused();
  await page.keyboard.press("Tab");
  expect(await old.evaluate(() => (window as any).ed.getMarkdown())).toBe(before);
  expect(await old.evaluate(() => (window as any).flushed)).toBe(before);
  expect(await next.evaluate(() => (window as any).restored.markdown)).toBe(before);
  await expect(old.locator("[contenteditable=true]")).not.toBeFocused();
});

for (const snapshot of [false, true]) {
  test(`root window scroll stays at its saved position${snapshot ? " with an authored snapshot" : " without a hook"}`, async ({
    page,
  }) => {
    const rootPane = (
      version: number,
    ) => `<!doctype html><html><body style="margin:0"><strong id="version">Version ${version}</strong><div style="height:3000px">Scrollable pane</div><script>
      window.shown=new URLSearchParams(location.search).get('codesenseCandidate')!=='1';window.scrollSamples=[];
      addEventListener('message',event=>{if(event.source===parent&&event.data?.type==='codesense:presentation')window.shown=event.data.shown;});
      function sample(){if(window.shown)window.scrollSamples.push(scrollY);requestAnimationFrame(sample)}requestAnimationFrame(sample);
      ${snapshot ? "codesense.onBeforeReload(()=>({scroll:window.scrollY}));" : ""}
      (async()=>{window.restored=await codesense.reloadState();if(window.restored)window.scrollTo(0,window.restored.scroll);window.readyScroll=scrollY;codesense.ready();})();
    </script></body></html>`;
    const id = `save-window-scroll-${snapshot}`;
    const old = await mount(page, id, rootPane(1));
    await page.evaluate(() => {
      (window as any).__scrollReported = new Promise<void>((resolve) => {
        const onMessage = (event: MessageEvent) => {
          if (event.data?.type !== "codesense:scroll" || event.data.y !== 500) return;
          window.removeEventListener("message", onMessage);
          resolve();
        };
        window.addEventListener("message", onMessage);
      });
    });
    await old.evaluate(() => window.scrollTo(0, 500));
    await expect.poll(() => old.evaluate(() => scrollY)).toBe(500);
    // The viewer tracks opaque-frame root scroll on the shim's postMessage report.
    // Wait for that handoff before saving so the test exercises restore, not message ordering.
    await page.evaluate(async () => await (window as any).__scrollReported);
    await save(page, id, rootPane(2));
    const next = await current(page, 2);
    await expect.poll(() => old.isDetached()).toBe(true);
    await next.waitForFunction(() => (window as any).scrollSamples.length >= 8);
    expect(await next.evaluate(() => (window as any).restored)).toEqual(
      snapshot ? { scroll: 500 } : null,
    );
    expect(await next.evaluate(() => window.scrollY)).toBe(500);
    expect(await next.evaluate(() => [...new Set((window as any).scrollSamples)])).toEqual([500]);
  });
}
