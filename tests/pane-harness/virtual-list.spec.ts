// SPDX-License-Identifier: MIT
import { watchRow, rowStability } from "../support/row-stability";
import { expect, test } from "./harness";

const PANE = "tests/pane-harness/fixtures/virtual-list-pane.html";
const ROWS = Array.from({ length: 5000 }, (_, i) => ({
  id: `r${String(i).padStart(4, "0")}`,
  label: `Row ${i}`,
}));
// 300 px viewport of 20 px rows, plus one partial row and 8 rows of overscan each side.
const MAX_ROWS = 15 + 1 + 16;

test("a 5,000-row list keeps the DOM to the window and moves the window as it scrolls", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, { topics: { rows: ROWS } });
  const { frame } = pane;
  await expect(frame.locator(".row").first()).toHaveText("Row 0");
  expect(await frame.locator(".row").count()).toBeLessThanOrEqual(MAX_ROWS);
  expect((await pane.windows("rows"))[0]).toEqual([0, 24]);

  await frame.evaluate(() => (document.getElementById("list")!.scrollTop = 2000 * 20));
  await expect(frame.locator('.row[data-id="r2000"]')).toBeVisible();
  expect(await frame.locator(".row").count()).toBeLessThanOrEqual(MAX_ROWS);
  expect(await frame.locator('.row[data-id="r0000"]').count()).toBe(0);
  const windows = await pane.windows("rows");
  expect(windows.at(-1)).toEqual([1992, 2024]);
  for (const [from, to] of windows) expect(to - from).toBeLessThanOrEqual(MAX_ROWS);

  // An upsert to one visible row re-renders that row only.
  await frame.evaluate(() => {
    (window as any).before = [...document.querySelectorAll(".row")];
    (window as any).rendersBefore = (window as any).renders;
  });
  await pane.upsert("rows", { id: "r2005", label: "Row 2005 (edited)" });
  await expect(frame.locator('.row[data-id="r2005"]')).toHaveText("Row 2005 (edited)");
  const kept = await frame.evaluate(() => {
    const now = new Set(document.querySelectorAll(".row"));
    const before: Element[] = (window as any).before;
    return {
      others: before
        .filter((el) => (el as HTMLElement).dataset.id !== "r2005")
        .every((el) => now.has(el)),
      renders: (window as any).renders - (window as any).rendersBefore,
    };
  });
  expect(kept).toEqual({ others: true, renders: 1 });
});

test("after scrolling away and back, DOM and Tab order follow row order", async ({ mountPane }) => {
  const pane = await mountPane(PANE, { topics: { rows: ROWS } });
  const { frame } = pane;
  await expect(frame.locator(".row").first()).toHaveText("Row 0");
  // A short scroll, so the windows overlap and rows 2–23 stay mounted throughout.
  await frame.evaluate(() => (document.getElementById("list")!.scrollTop = 10 * 20));
  await expect.poll(() => pane.windows("rows").then((w) => w.at(-1))).toEqual([2, 34]);
  await expect(frame.locator('.row[data-id="r0033"]')).toBeAttached();
  await frame.evaluate(() => (document.getElementById("list")!.scrollTop = 0));
  await expect.poll(() => pane.windows("rows").then((w) => w.at(-1))).toEqual([0, 24]);
  await expect(frame.locator('.row[data-id="r0000"]')).toBeVisible();
  const ids = await frame
    .locator(".row")
    .evaluateAll((rows) => rows.map((r) => (r as HTMLElement).dataset.id!));
  expect(ids).toEqual([...ids].sort());
  // Focus-in lands on the list (the first focusable control); Tab then walks the rows.
  await frame.locator("#list").focus();
  await expect(frame.locator("#list")).toBeFocused();
  for (const id of ["r0000", "r0001", "r0002"]) {
    await pane.page.keyboard.press("Tab");
    await expect(frame.locator(`.row[data-id="${id}"]`)).toBeFocused();
  }
});

test("inserting an earlier row retains shifted nodes and updates their indexes", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, { topics: { rows: ROWS } });
  const { frame } = pane;
  await expect(frame.locator('.row[data-id="r0010"]')).toHaveAttribute("data-index", "10");
  await frame.evaluate(() => {
    (window as any).before = new Map(
      [...document.querySelectorAll<HTMLElement>(".row")].map((el) => [el.dataset.id, el]),
    );
  });
  await pane.upsert("rows", { id: "r0004a", label: "Inserted" });
  await expect(frame.locator('.row[data-id="r0004a"]')).toHaveAttribute("data-index", "5");
  await expect(frame.locator('.row[data-id="r0010"]')).toHaveAttribute("data-index", "11");
  const result = await frame.evaluate(() => {
    const before: Map<string, Element> = (window as any).before;
    const rows = [...document.querySelectorAll<HTMLElement>(".row")];
    return {
      kept: ["r0000", "r0004"].map(
        (id) => rows.find((el) => el.dataset.id === id) === before.get(id),
      ),
      moved: rows.find((el) => el.dataset.id === "r0010") === before.get("r0010"),
      indexes: rows.every((el, k) => el.dataset.index === String(k)),
    };
  });
  expect(result).toEqual({ kept: [true, true], moved: true, indexes: true });
});

test("a row added past the window still grows the list, and agentList windows follow order", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, {
    topics: {
      rows: ROWS,
      agentList: [
        { id: "b", order: 1 },
        { id: "a", order: 2 },
        { id: "c", order: 3 },
      ],
    },
  });
  const { frame } = pane;
  const height = () =>
    frame.evaluate(
      () => document.querySelector<HTMLElement>("[data-cs-virtual-list]")!.style.height,
    );
  await expect.poll(height).toBe(`${5000 * 20}px`);
  // Appended after the window: only the total changes for this subscription.
  await pane.upsert("rows", { id: "r9999", label: "Last" });
  await expect.poll(height).toBe(`${5001 * 20}px`);

  await frame.evaluate(() => {
    (window as any).agents = [];
    return (window as any).codesense.subscribe("agentList", {
      window: [0, 2],
      onChange: (s: { snapshot: { id: string }[] }) =>
        (window as any).agents.push(s.snapshot.map((row) => row.id)),
    });
  });
  await expect.poll(() => frame.evaluate(() => (window as any).agents.at(-1))).toEqual(["b", "a"]);
  await pane.upsert("agentList", { id: "c", order: 0 });
  await expect.poll(() => frame.evaluate(() => (window as any).agents.at(-1))).toEqual(["c", "b"]);
});

test("a pointed idle row survives five seconds of 100ms live updates", async ({ mountPane }) => {
  const pane = await mountPane(PANE, { topics: { rows: ROWS.slice(0, 15) } });
  const row = pane.frame.locator('[data-id="r0001"]');
  await expect(row).toBeVisible();
  await row.focus();
  await watchRow(pane.frame, row);
  const start = Date.now();
  for (let i = 0; Date.now() - start < 5000; i++) {
    await pane.upsert("rows", { ...ROWS[0], label: `Busy ${i}` });
    await pane.upsert("rows", { ...ROWS[1], heartbeat: i });
    await pane.page.waitForTimeout(100);
  }
  const result = await rowStability(pane.frame);
  console.log("topic virtualList stability", result);
  expect(result.frames).toBeGreaterThan(5);
  expect(result).toMatchObject({
    replacements: 0,
    pointerReplacements: 0,
    removals: 0,
    sameRow: true,
    samePointer: true,
  });
  await expect(row).toBeFocused();
});

for (const mode of ["local", "topic", "tree"] as const) {
  test(`${mode}: reordering retained rows keeps focus on the original button`, async ({
    mountPane,
  }) => {
    const a = { id: "a", order: 0 },
      b = { id: "b", order: 1 };
    const pane = await mountPane(PANE, { topics: { rows: [a, b], agentList: [a, b] } });
    await expect(pane.frame.locator(".row")).toHaveCount(2);
    await pane.frame.evaluate((mode) => {
      const w = window as any;
      w.list.destroy();
      const a = { id: "a", order: 0 },
        b = { id: "b", order: 1 };
      w.reorderList = w.codesense.kit.virtualList({
        container: document.getElementById("list"),
        ...(mode === "topic" ? { topic: "agentList" } : { rows: [a, b] }),
        ...(mode === "local" ? {} : { layoutKey: (row: any) => row.id }),
        tree: mode === "tree",
        rowHeight: 24,
        render(row: any) {
          const el = document.createElement("div");
          const button = document.createElement("button");
          button.textContent = row.id;
          el.append(button);
          return el;
        },
      });
    }, mode);
    const row = pane.frame.locator('[data-key="b"]');
    const button = row.getByRole("button", { name: "b", exact: true });
    await expect(button).toBeVisible();
    if (mode === "tree") await pane.frame.evaluate(() => (window as any).reorderList.focus("b"));
    await button.focus();
    await pane.frame.evaluate(() => {
      (window as any).focusedButton = document.activeElement;
      (window as any).beforeScroll = document.getElementById("list")!.scrollTop;
    });
    if (mode === "topic") await pane.upsert("agentList", { ...b, order: -1 });
    else
      await pane.frame.evaluate(() =>
        (window as any).reorderList.setRows([
          { id: "b", order: 1 },
          { id: "a", order: 0 },
        ]),
      );
    await expect(row).toHaveCSS("top", "0px");
    expect(await button.evaluate((el) => el === (window as any).focusedButton)).toBe(true);
    await expect(button).toBeFocused();
    expect(
      await pane.frame.evaluate(
        () => document.getElementById("list")!.scrollTop === (window as any).beforeScroll,
      ),
    ).toBe(true);
    if (mode === "tree") {
      await expect(row).toHaveAttribute("tabindex", "0");
      await expect(row).toHaveAttribute("aria-posinset", "1");
      await expect(pane.frame.locator('[role="treeitem"][tabindex="0"]')).toHaveCount(1);
      await pane.page.keyboard.press("ArrowDown");
      await expect(pane.frame.locator('[data-key="a"]')).toBeFocused();
    }
  });
}
