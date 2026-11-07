// SPDX-License-Identifier: MIT
import { expect, test } from "./harness";
const PANE = "src/features/shell/right-pane/__fixtures__/notes-pane.html";
const records = [
  {
    id: "alpha",
    title: "Alpha",
    body: "# First heading\n\nFirst paragraph\n\nLast paragraph",
    pinned: true,
  },
  { id: "beta", title: "Beta", body: "## Destination\n\nSecond note" },
];
async function mod(frame: import("@playwright/test").Frame) {
  return frame.evaluate(() => (/Mac|iP(hone|[oa]d)/.test(navigator.platform) ? "Meta" : "Control"));
}

test("Notes types, autosaves, merges an MCP edit and flushes the final keystroke", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records,
    width: 760,
    height: 680,
    theme: { scheme: "light" },
  });
  const { frame } = pane;
  await expect(frame.getByRole("textbox", { name: "Note title" })).toHaveValue("Alpha");
  const paragraphs = frame.locator(".scratch-note-editor > p");
  await paragraphs.last().click();
  await page.keyboard.press("End");
  await page.keyboard.type(" typed");
  await expect
    .poll(() => page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).body))
    .toContain("Last paragraph typed");
  await paragraphs.first().click();
  await page.keyboard.press("End");
  await page.keyboard.type(" before");
  await expect(frame.locator("#save-state")).toHaveText("Saved");
  await page.keyboard.type(" continued");
  await expect(paragraphs.first()).toContainText("First paragraph before continued");
  await page.evaluate(() =>
    (window as any).noteFixture.write(
      "alpha",
      "# First heading\n\nAgent first\n\nLast paragraph typed",
    ),
  );
  await expect(frame.locator(".scratch-note-editor")).toContainText("Agent first");
  await paragraphs.last().click();
  await page.keyboard.press("End");
  await page.keyboard.type(" final");
  await pane.close();
  await expect
    .poll(() => page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).body))
    .toContain("typed final");
});

test("failed recovery keeps the previous Notes live and its autosave keeps retained edits", async ({
  mountPane,
  page,
}) => {
  test.setTimeout(60_000);
  const pane = await mountPane(PANE, { records, surface: "notes", width: 760, height: 680 });
  const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
  await expect(editor).toContainText("Last paragraph");
  const originalGrant = await page.evaluate(() =>
    (window as any).__TAURI_INTERNALS__.invoke("resolve_frame_grant", { token: "notes-pane.html" }),
  );
  // Production mints distinct credentials. The basename-only fake otherwise
  // overwrites the retained frame's grant when the candidate is minted.
  await page.evaluate((originalGrant) => {
    const runtime = (window as any).__TAURI_INTERNALS__;
    const invoke = runtime.invoke;
    const grants = new Map([["notes-pane.html", originalGrant]]);
    runtime.invoke = async (command: string, args: any) => {
      if (command === "resolve_frame_grant" && grants.has(args.token))
        return grants.get(args.token);
      const result = await invoke(command, args);
      if (command !== "mint_frame") return result;
      const token = `instance:${result.frameId}`;
      grants.set(token, result.grant);
      (window as any).recoveryToken = token;
      return { ...result, token };
    };
  }, originalGrant);
  let release!: () => void;
  let candidateRequested!: () => void;
  const requested = new Promise<void>((resolve) => {
    candidateRequested = resolve;
  });
  const hold = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/pane/**", async (route) => {
    if (!route.request().isNavigationRequest()) return route.continue();
    const response = await route.fetch();
    candidateRequested();
    await hold;
    const html = await response.text();
    const proxy = 'window.__codesenseProxy={token:"notes-pane.html"}';
    expect(html).toContain(proxy);
    const token = await page.evaluate(() => (window as any).recoveryToken);
    await route.fulfill({
      response,
      body: html
        .replace(proxy, `window.__codesenseProxy={token:${JSON.stringify(token)}}`)
        .replace(
          "<title>Notes</title>",
          "<script>throw new Error('replacement failed before autosave')</script><title>Notes</title>",
        ),
    });
  });
  await page.evaluate(() => (window as any).control.refresh());
  await requested;
  await expect(page.locator("iframe")).toHaveCount(2);
  await page.clock.install();
  await page.clock.pauseAt(new Date());
  await editor.locator("p").last().click();
  await page.keyboard.press("End");
  await page.keyboard.type(" retained unsaved edit");
  await expect(editor).toContainText("Last paragraph retained unsaved edit");
  const read = () =>
    page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).body);
  expect(await read()).not.toContain("retained unsaved edit");
  release();
  await expect(page.getByTestId("slot-fallback-strip")).toBeVisible();
  // A failed replacement keeps the previous version live, so its own autosave keeps the edit.
  expect(pane.frame.isDetached()).toBe(false);
  await expect(editor).toContainText("Last paragraph retained unsaved edit");
  await page.clock.runFor(1_500);
  await expect.poll(read).toContain("Last paragraph retained unsaved edit");
  expect(pane.frame.isDetached()).toBe(false);
});

test("Notes menus choose with Tab, links open notes, intent jumps to a heading and layouts follow width", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records,
    width: 380,
    height: 650,
    theme: { scheme: "dark" },
  });
  const { frame } = pane;
  await expect(frame.getByRole("textbox", { name: "Note title" })).toHaveValue("Alpha");
  await frame.locator(".scratch-note-editor > p").last().click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("[[");
  await expect(frame.getByRole("listbox", { name: "Insert link to note or doc" })).toBeVisible();
  await page.keyboard.press("Tab");
  await frame.locator(".scratch-note-editor a").click();
  await expect(frame.getByRole("textbox", { name: "Note title" })).toHaveValue("Beta");
  await pane.sendIntent({ noteId: "alpha", heading: "first-heading" });
  await expect(frame.getByRole("textbox", { name: "Note title" })).toHaveValue("Alpha");
  await frame.getByRole("button", { name: "Show notes" }).click();
  await expect(frame.getByRole("textbox", { name: "Search notes" })).toBeVisible();
  await page.keyboard.press("Escape");
  await frame.getByRole("textbox", { name: "Note title" }).press(`${await mod(frame)}+o`);
  await expect(frame.locator("#palette-search")).toBeFocused();
  await page.keyboard.press("Escape");
  await frame.getByRole("textbox", { name: "Note title" }).press(`${await mod(frame)}+Shift+o`);
  await expect(frame.locator("#palette-search")).toHaveAttribute("placeholder", "Go to heading");
  await page.keyboard.press("Escape");
  await expect(frame.locator("#notes")).toHaveAttribute("data-layout", "narrow");
  await pane.setTheme({ scheme: "light" });
  await expect
    .poll(() =>
      frame.evaluate(() =>
        getComputedStyle(document.documentElement).getPropertyValue("--cs-color-scheme").trim(),
      ),
    )
    .toBe("light");
  await page.screenshot({ path: "test-results/notes-narrow-light.png" });
});

test("Notes exposes Retry and recovers a failed draft after the frame reloads", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, { records, width: 1120, height: 700 });
  const { frame } = pane;
  await expect(frame.getByRole("textbox", { name: "Note title" })).toHaveValue("Alpha");
  await page.evaluate(() => (window as any).noteFixture.fail(true));
  await frame.locator(".scratch-note-editor > p").last().click();
  await page.keyboard.press("End");
  await page.keyboard.type(" unsent");
  await expect(frame.getByRole("button", { name: "Retry", exact: true })).toBeVisible();
  await expect(frame.locator("#save-state")).toHaveText("Not saved");
  await expect
    .poll(() => page.evaluate(() => (window as any).noteFixture.prefs()))
    .toContain("unsent");
  await frame.evaluate(() => location.reload());
  await expect(frame.getByRole("button", { name: "Recover draft" })).toBeVisible();
  await frame.getByRole("button", { name: "Recover draft" }).click();
  await page.evaluate(() => (window as any).noteFixture.fail(false));
  await expect
    .poll(() => page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).body))
    .toContain("unsent");
  await frame.getByRole("button", { name: "Note history", exact: true }).click();
  await expect(frame.locator("#side-history")).toBeVisible();
  await expect(frame.locator("#history-sheet")).toBeHidden();
  await page.screenshot({ path: "test-results/notes-wide-dark.png" });
});

test("Notes selection tools follow the selected text while the note scrolls", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, {
    records: [
      {
        ...records[0],
        body:
          "# Long note\n\n" +
          Array.from({ length: 40 }, (_, i) => `Paragraph ${i} with text to select.`).join("\n\n"),
      },
    ],
    width: 760,
    height: 680,
  });
  const { frame } = pane;
  const target = frame.locator(".scratch-note-editor > p").nth(8);
  await target.scrollIntoViewIfNeeded();
  await target.click();
  await target.evaluate((node) => {
    const range = document.createRange();
    range.selectNodeContents(node);
    const selection = window.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);
  });
  const menu = frame.locator("#selection-menu");
  await expect(menu).toBeVisible();
  const top = await menu.evaluate((node) => node.getBoundingClientRect().top);
  await frame.locator("#editor-port").evaluate((node) => {
    node.scrollTop += 30;
  });
  await expect
    .poll(() => menu.evaluate((node) => node.getBoundingClientRect().top))
    .toBeLessThan(top - 20);
});

const runningAgent = {
  id: "ada",
  handle: "ada",
  sessionId: "ada",
  kind: "cli",
  alive: true,
  team: { org: "fixture", team: "dev" },
  rowNames: { primary: "Ada" },
  state: "idle",
};

test("Notes sends a line, selection and code through the live agent picker", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records: [
      {
        ...records[0],
        body: "A line to send\n\nSelect this passage\n\n```js\nconsole.log('hello')\n```",
      },
    ],
    sessions: [runningAgent],
    width: 800,
    height: 700,
  });
  const { frame } = pane;
  const first = frame.locator(".scratch-note-editor > p").first();
  await first.click();
  await page.keyboard.press(`${await mod(frame)}+Enter`);
  await expect(frame.getByRole("textbox", { name: "Send to agent" })).toBeFocused();
  await frame.locator("[data-agent=ada]").click();
  await expect(frame.locator(".nt-agent-chip-name")).toHaveText("Ada");
  await expect
    .poll(() => pane.verbs())
    .toContainEqual({
      verb: "sendPrompt",
      args: { handle: "ada", text: expect.stringContaining("A line to send"), submit: true },
    });
  const paragraph = frame.locator(".scratch-note-editor > p").nth(1);
  await paragraph.click();
  await page.keyboard.press("Home");
  await page.keyboard.press("Shift+End");
  await expect(frame.locator("#selection-menu")).toBeVisible();
  await frame.locator("#selection-menu").getByRole("button", { name: "Send", exact: true }).click();
  await frame.locator("[data-agent=ada]").click();
  await expect(frame.locator("#float")).toBeHidden();
  await frame.locator(".scratch-code-block-send").click();
  await expect(frame.locator("[data-agent=ada]")).toBeVisible();
  await frame.locator("[data-agent=ada]").click();
  await expect
    .poll(async () => (await pane.verbs()).filter((call) => call.verb === "sendPrompt").length)
    .toBe(3);
  await page.keyboard.press("Escape");
  await paragraph.click();
  await paragraph.hover();
  await expect(frame.locator("#line-send")).toBeVisible();
  await frame.locator("#line-send").click();
  await expect(frame.locator("#send-search")).toBeFocused();
  await page.screenshot({ path: "test-results/notes-send-picker.png" });
});

test("Notes uses the host row menu and genuine Escape exits an overlay to its opener", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, { records, width: 1000, height: 700 });
  const { frame } = pane;
  await expect(frame.locator("#note-title")).toHaveValue("Alpha");
  await frame.locator("[data-note-id=alpha]").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Unpin", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).fields.pinned),
    )
    .toBe(false);
  await page.locator("#opener").focus();
  await pane.maximize(true);
  await expect(frame.locator("#notes")).toHaveAttribute("data-presentation", "overlay");
  await expect(frame.locator("#notes")).toHaveAttribute("data-layout", "full");
  await expect(frame.locator(".nt-side")).toBeVisible();
  await page.screenshot({ path: "test-results/notes-overlay.png" });
  await frame.locator(".scratch-note-editor > p").first().click();
  await page.keyboard.press("Escape");
  await expect(frame.locator("#notes")).toHaveAttribute("data-presentation", "docked");
  await expect(page.locator("#opener")).toBeFocused();
});

test("Notes clears hover Send on a new note and on editing its cached line", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records,
    sessions: [runningAgent],
    width: 800,
    height: 700,
  });
  const { frame } = pane;
  const line = frame.locator(".scratch-note-editor > p").first();
  await line.click();
  await line.hover();
  await expect(frame.locator("#line-send")).toBeVisible();
  await page.keyboard.type("changed");
  await expect(frame.locator("#line-send")).toBeHidden();
  await page.mouse.move(1, 1);
  await line.hover();
  await expect(frame.locator("#line-send")).toBeVisible();
  await page.keyboard.press(`${await mod(frame)}+n`);
  await expect(frame.locator("#note-title")).toHaveValue("");
  await expect(frame.locator("#line-send")).toBeHidden();
  expect((await pane.verbs()).filter((call) => call.verb === "sendPrompt")).toHaveLength(0);
});

test("Notes relocates the sent line while another paragraph changes during delivery", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records,
    sessions: [runningAgent],
    width: 800,
    height: 700,
  });
  const { frame } = pane;
  await expect(frame.locator("#note-title")).toHaveValue("Alpha");
  await page.evaluate(() => (window as any).noteFixture.holdPrompt());
  const paragraphs = frame.locator(".scratch-note-editor > p");
  await paragraphs.last().click();
  await page.keyboard.press(`${await mod(frame)}+Enter`);
  await frame.locator("[data-agent=ada]").click();
  await expect
    .poll(async () => (await pane.verbs()).filter((call) => call.verb === "sendPrompt").length)
    .toBe(1);
  await page.keyboard.press("Escape");
  await expect(frame.locator(".scratch-note-editor")).toBeFocused();
  await paragraphs.first().click();
  await page.keyboard.type("An earlier edit ");
  await expect(paragraphs.first()).toContainText("An earlier edit");
  await page.evaluate(() => (window as any).noteFixture.releasePrompt());
  await expect(paragraphs.last().locator(".nt-agent-chip-name")).toHaveText("Ada");
  await expect(paragraphs.first()).toContainText("An earlier edit");
});

test("Notes closes an uncertain send and asks to check the terminal before sending again", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records,
    sessions: [runningAgent],
    width: 800,
    height: 700,
  });
  const { frame } = pane;
  await expect(frame.locator("#note-title")).toHaveValue("Alpha");
  await page.evaluate(() =>
    (window as any).noteFixture.promptResult({
      status: "unknown",
      submitted: null,
      machine: "remote",
    }),
  );
  await frame.locator(".scratch-note-editor > p").first().click();
  await page.keyboard.press(`${await mod(frame)}+Enter`);
  await frame.locator("[data-agent=ada]").click();
  await expect(frame.locator("#float")).toBeHidden();
  await expect(frame.locator("#notice")).toContainText("couldn't confirm it arrived");
  await expect(frame.locator("#notice")).toContainText("Check its terminal before sending again");
  await expect(frame.locator(".nt-agent-chip")).toHaveCount(0);
  expect((await pane.verbs()).filter((call) => call.verb === "sendPrompt")).toHaveLength(1);
});

test("Notes discards the hidden drawer layer when maximizing from a narrow pane", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, { records, width: 380, height: 700 });
  const { frame } = pane;
  await expect(frame.locator("#note-title")).toHaveValue("Alpha");
  await frame.getByRole("button", { name: "Show notes" }).click();
  await expect(frame.locator("#notes")).toHaveAttribute("data-drawer", "true");
  await expect(frame.locator("#list-search")).toBeFocused();
  await page.locator("#opener").focus();
  await expect(page.locator("#opener")).toBeFocused();
  await pane.maximize(true);
  await expect(frame.locator("#notes")).toHaveAttribute("data-layout", "full");
  await expect(frame.locator("#notes")).toHaveAttribute("data-drawer", "false");
  await frame.locator(".scratch-note-editor > p").first().click();
  await page.keyboard.press("Escape");
  await expect(frame.locator("#notes")).toHaveAttribute("data-presentation", "docked");
  await expect(page.locator("#opener")).toBeFocused();
});
