// SPDX-License-Identifier: MIT
import { expect, test } from "../pane-harness/harness";

const PANE = "src/features/shell/right-pane/__fixtures__/notes-pane.html";
const records = [{ id: "alpha", title: "Alpha", body: "A paragraph" }];

test("Notes row rename survives the host text menu and commits after Paste", async ({
  mountPane,
  page,
}) => {
  const { frame } = await mountPane(PANE, { records, width: 760, height: 680 });
  await expect(frame.getByRole("textbox", { name: "Note title" })).toHaveValue("Alpha");
  await page.evaluate(() => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { readText: async () => "Pasted row title" },
    });
  });
  await frame.getByRole("button", { name: "Actions for Alpha" }).click();
  await frame.getByRole("button", { name: "Rename", exact: true }).click();
  const input = frame.getByRole("textbox", { name: "Rename note" });
  await input.fill("");
  await input.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  await expect.poll(() => frame.evaluate(() => (window as any).codesense.textMenuOpen)).toBe(true);
  await expect(input).toBeAttached();
  await page.getByRole("menuitem", { name: /^Paste/ }).click();
  await expect(input).toHaveValue("Pasted row title");
  await expect(input).toBeFocused();
  await expect.poll(() => frame.evaluate(() => (window as any).codesense.textMenuOpen)).toBe(false);
  await input.press("Enter");
  await expect
    .poll(() =>
      page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).fields.title),
    )
    .toBe("Pasted row title");

  await frame.getByRole("button", { name: "Actions for Pasted row title" }).click();
  await frame.getByRole("button", { name: "Rename", exact: true }).click();
  await input.fill("Cancelled title");
  await input.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(input).toBeFocused();
  await input.press("Escape");
  await expect(input).not.toBeAttached();
  await expect(frame.getByRole("textbox", { name: "Note title" })).toHaveValue("Pasted row title");

  await frame.getByRole("button", { name: "Actions for Pasted row title" }).click();
  await frame.getByRole("button", { name: "Rename", exact: true }).click();
  await input.fill("Blurred row title");
  await frame.getByRole("textbox", { name: "Note title" }).click();
  await expect
    .poll(() =>
      page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).fields.title),
    )
    .toBe("Blurred row title");
});

test("Notes title defers its blur save while the host text menu is open", async ({
  mountPane,
  page,
}) => {
  const { frame } = await mountPane(PANE, { records, width: 760, height: 680 });
  const input = frame.getByRole("textbox", { name: "Note title" });
  await expect(input).toHaveValue("Alpha");
  await input.fill("Draft title");
  await input.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  await expect.poll(() => frame.evaluate(() => (window as any).codesense.textMenuOpen)).toBe(true);
  expect(
    await page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).fields.title),
  ).toBe("Alpha");
  await page.keyboard.press("Escape");
  await expect(input).toBeFocused();
  await expect.poll(() => frame.evaluate(() => (window as any).codesense.textMenuOpen)).toBe(false);
  await input.press("Enter");
  await expect
    .poll(() =>
      page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).fields.title),
    )
    .toBe("Draft title");
});

for (const kind of ["title", "row rename"] as const) {
  test(`Notes ${kind} commits on a normal host blur`, async ({ mountPane, page }) => {
    const { frame } = await mountPane(PANE, { records, width: 760, height: 680 });
    await expect(frame.getByRole("textbox", { name: "Note title" })).toHaveValue("Alpha");
    if (kind === "row rename") {
      await frame.getByRole("button", { name: "Actions for Alpha" }).click();
      await frame.getByRole("button", { name: "Rename", exact: true }).click();
    }
    const input = frame.getByRole("textbox", {
      name: kind === "title" ? "Note title" : "Rename note",
    });
    await input.fill("Saved on host blur");
    await page.getByRole("textbox", { name: "terminal", exact: true }).click();
    await expect
      .poll(() =>
        page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).fields.title),
      )
      .toBe("Saved on host blur");
  });

  test(`Notes flush and close saves a focused ${kind}`, async ({ mountPane, page }) => {
    const pane = await mountPane(PANE, { records, width: 760, height: 680 });
    const { frame } = pane;
    await expect(frame.getByRole("textbox", { name: "Note title" })).toHaveValue("Alpha");
    if (kind === "row rename") {
      await frame.getByRole("button", { name: "Actions for Alpha" }).click();
      await frame.getByRole("button", { name: "Rename", exact: true }).click();
    }
    const input = frame.getByRole("textbox", {
      name: kind === "title" ? "Note title" : "Rename note",
    });
    await input.fill("Saved before teardown");
    await expect(input).toBeFocused();
    await pane.flush();
    await pane.close();
    await expect
      .poll(() =>
        page.evaluate(async () => (await (window as any).noteFixture.read("alpha")).fields.title),
      )
      .toBe("Saved before teardown");
  });
}
