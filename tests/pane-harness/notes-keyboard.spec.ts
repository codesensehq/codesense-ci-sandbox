// SPDX-License-Identifier: MIT
import type { Locator } from "@playwright/test";
import { expect, test, type MountedPane } from "./harness";

const PANE = "src/features/shell/right-pane/__fixtures__/notes-pane.html";
const blocks = [
  { name: "list", body: "- First\n- Second", selector: "li:last-child > p" },
  { name: "task list", body: "- [ ] First\n- [ ] Second", selector: "li:last-child p" },
  { name: "code", body: "```plaintext\nSecond\n```", selector: ".scratch-code-block-content" },
];

async function selectText(target: Locator) {
  await target.click();
  await target.evaluate((node) => {
    const range = document.createRange();
    range.selectNodeContents(node);
    const selection = window.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);
  });
}

async function pressPlainEscape(pane: MountedPane) {
  await pane.frame.evaluate(() => {
    window.addEventListener("keydown", (event) => {
      if (event.key === "Escape") (window as any).plainEscapePrevented = event.defaultPrevented;
    });
  });
  await pane.page.keyboard.press("Escape");
  await expect
    .poll(() => pane.frame.evaluate(() => (window as any).plainEscapePrevented))
    .toBe(false);
}

test("Notes prose Tab and Shift+Tab move focus without changing the note", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records: [{ id: "alpha", title: "Alpha", body: "Prose" }],
    width: 760,
  });
  const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
  await expect(editor).toHaveText("Prose");
  for (const key of ["Tab", "Shift+Tab"]) {
    await editor.click();
    await page.keyboard.press(key);
    await expect(editor).not.toBeFocused();
    await expect(editor).toHaveText("Prose");
  }
  await expect(editor).toHaveAccessibleDescription(/Escape.*Tab.*leave/i);
});

for (const block of blocks) {
  test(`Notes ${block.name} keeps native indentation`, async ({ mountPane, page }) => {
    const pane = await mountPane(PANE, {
      records: [{ id: "alpha", title: "Alpha", body: block.body }],
      width: 760,
    });
    const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
    await editor.locator(block.selector).click();
    await page.keyboard.press("End");
    await page.keyboard.press("Tab");
    await expect(editor).toBeFocused();
    if (block.name === "code") {
      await expect.poll(() => editor.locator(block.selector).textContent()).toBe("Second  ");
    } else {
      await expect(editor.locator("li li")).toContainText("Second");
      await page.keyboard.press("Shift+Tab");
      await expect(editor.locator("li li")).toHaveCount(0);
      await expect(editor).toBeFocused();
    }
  });

  for (const key of ["Tab", "Shift+Tab"]) {
    test(`Notes ${block.name} Escape then ${key} leaves the editor`, async ({
      mountPane,
      page,
    }) => {
      const pane = await mountPane(PANE, {
        records: [{ id: "alpha", title: "Alpha", body: block.body }],
        width: 760,
      });
      const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
      await selectText(editor.locator(block.selector));
      await expect(pane.frame.locator("#selection-menu")).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(pane.frame.locator("#selection-menu")).toBeHidden();
      await expect(editor).toBeFocused();
      await page.keyboard.press(key);
      await expect(editor).not.toBeFocused();
      await expect(editor.locator("li li")).toHaveCount(0);
      await expect(editor).toContainText("Second");
    });
  }
}

for (const block of blocks.filter((block) => block.name !== "code")) {
  for (const key of ["Tab", "Shift+Tab"]) {
    test(`Notes ${block.name} suggestion Escape then ${key} leaves the editor`, async ({
      mountPane,
      page,
    }) => {
      const pane = await mountPane(PANE, {
        records: [{ id: "alpha", title: "Alpha", body: block.body }],
        width: 760,
      });
      const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
      await editor.locator(block.selector).click();
      await page.keyboard.press("End");
      await page.keyboard.type(" /");
      const suggestions = pane.frame.getByRole("listbox", { name: "Insert a block" });
      await expect(suggestions).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(suggestions).toBeHidden();
      await expect(editor).toBeFocused();
      await page.keyboard.press(key);
      await expect(editor).not.toBeFocused();
      await expect(editor.locator("li li")).toHaveCount(0);
      await expect(editor).toContainText("Second /");
    });
  }
}

test("Notes clicking a list caret after Escape clears the escape and Tab indents", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records: [{ id: "alpha", title: "Alpha", body: "- First\n- Second" }],
    width: 760,
  });
  const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
  const item = editor.locator("li:last-child > p");
  await item.click();
  await page.keyboard.press("End");
  await page.keyboard.type(" /");
  const suggestions = pane.frame.getByRole("listbox", { name: "Insert a block" });
  await expect(suggestions).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(suggestions).toBeHidden();
  await expect(editor).toBeFocused();
  await item.click({ position: { x: 5, y: 5 } });
  await page.keyboard.press("Tab");
  await expect(editor).toBeFocused();
  await expect(editor.locator("li li")).toContainText("Second /");
});

test("Notes pointer controls still choose suggestions and format selected text", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records: [{ id: "alpha", title: "Alpha", body: "Words" }],
    width: 760,
  });
  const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
  await editor.locator("p").click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("/");
  const suggestions = pane.frame.getByRole("listbox", { name: "Insert a block" });
  await suggestions.getByText("Heading", { exact: true }).click();
  await expect(suggestions).toBeHidden();
  await page.keyboard.type("Heading text");
  await selectText(editor.locator("h1"));
  await pane.frame
    .locator("#selection-menu")
    .getByRole("button", { name: "Bold", exact: true })
    .click();
  await expect(editor.locator("h1 strong")).toHaveText("Heading text");
});

test("Notes typing after Escape clears the escape and Tab still indents", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records: [{ id: "alpha", title: "Alpha", body: "```plaintext\nSecond\n```" }],
    width: 760,
  });
  const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
  await selectText(editor.locator(".scratch-code-block-content"));
  await expect(pane.frame.locator("#selection-menu")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(editor).toBeFocused();
  await page.keyboard.type("Typed");
  await page.keyboard.press("Tab");
  await expect(editor).toBeFocused();
  await expect
    .poll(() => editor.locator(".scratch-code-block-content").textContent())
    .toBe("Typed  ");
});

for (const block of blocks.filter((block) => block.name !== "task list")) {
  for (const key of ["Tab", "Shift+Tab"]) {
    test(`Notes ${block.name} plain Escape reaches the host before ${key}`, async ({
      mountPane,
      page,
    }) => {
      const pane = await mountPane(PANE, {
        records: [{ id: "alpha", title: "Alpha", body: block.body }],
        width: 760,
      });
      const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
      await editor.locator(block.selector).click();
      await expect(editor).toBeFocused();
      await expect(pane.frame.locator("#selection-menu")).toBeHidden();
      await expect(pane.frame.locator("#suggestions")).toBeHidden();
      await pressPlainEscape(pane);
      await expect(page.locator("#terminal")).toBeFocused();
      await page.keyboard.press(key);
      await expect(editor).not.toBeFocused();
      await expect(page.locator("#terminal")).not.toBeFocused();
      await expect(editor.locator("li li")).toHaveCount(0);
      await expect(editor).toContainText("Second");
    });
  }

  test(`Notes ${block.name} plain Escape exits fullscreen to the opener`, async ({
    mountPane,
    page,
  }) => {
    const pane = await mountPane(PANE, {
      records: [{ id: "alpha", title: "Alpha", body: block.body }],
      width: 760,
    });
    await page.locator("#opener").focus();
    await pane.maximize(true);
    await expect(pane.frame.locator("#notes")).toHaveAttribute("data-presentation", "overlay");
    const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
    await editor.locator(block.selector).click();
    await expect(pane.frame.locator("#selection-menu")).toBeHidden();
    await expect(pane.frame.locator("#suggestions")).toBeHidden();
    await pressPlainEscape(pane);
    await expect(pane.frame.locator("#notes")).toHaveAttribute("data-presentation", "docked");
    await expect(page.locator("#opener")).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(page.locator("#terminal")).toBeFocused();
    await expect(editor).not.toBeFocused();
  });
}

test("Notes suggestion Escape keeps first refusal and plain Escape exits fullscreen", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(PANE, {
    records: [{ id: "alpha", title: "Alpha", body: "Prose" }],
    width: 760,
  });
  await page.locator("#opener").focus();
  await pane.maximize(true);
  const editor = pane.frame.locator('[contenteditable][aria-label="Note"]');
  await editor.locator("p").click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("/");
  await expect(pane.frame.getByRole("listbox", { name: "Insert a block" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(pane.frame.getByRole("listbox", { name: "Insert a block" })).toBeHidden();
  await expect(pane.frame.locator("#notes")).toHaveAttribute("data-presentation", "overlay");
  await expect(editor).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(pane.frame.locator("#notes")).toHaveAttribute("data-presentation", "docked");
  await expect(page.locator("#opener")).toBeFocused();
});
