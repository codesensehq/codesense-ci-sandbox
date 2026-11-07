// SPDX-License-Identifier: MIT
import { dirname, join } from "node:path";
import type { Frame } from "@playwright/test";
import { test, expect } from "../pane-harness/harness";
import { agentRow } from "./checks";
import { writeTrailFixture } from "../../scripts/story-trail-conformance.mjs";
const fixture = writeTrailFixture(
  join(
    dirname(process.env.STORY_TRAIL_TEST_FIXTURE ?? "/tmp/cde19-pr4-lines/trail.html"),
    "lines.html",
  ),
  { lines: true },
);
const paint = (frame: Frame) =>
  frame.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
const opener = 'tr[data-row-focus="file:src/app.ts"] [data-lines-path]';
async function pick(mountPane: any, mode = "exact") {
  const pane = await mountPane(fixture, {
      width: 1280,
      height: 800,
      sessions: [agentRow("lead", "Builder")],
    }),
    frame: Frame = pane.frame;
  await expect(frame.locator(opener)).toBeVisible();
  await frame.evaluate((value) => {
    (window as any).whyLineMode = value;
  }, mode);
  await frame.locator(opener).click();
  await expect(frame.locator("#drill-panel")).toBeVisible();
  return { pane, frame };
}

test("pick opens exact ranges grouped by session with their turn links", async ({ mountPane }) => {
  const { pane, frame } = await pick(mountPane);
  await expect(frame.locator("#panel-title")).toHaveText("Who changed lines 2–5");
  await expect(frame.locator("[name=linesStart]")).toHaveValue("2");
  await expect(frame.locator("[name=linesEnd]")).toHaveValue("5");
  await expect(frame.locator("[data-lines-summary]")).toHaveText(
    "This story changed 3 of these 4 lines.",
  );
  const group = frame.locator('[data-lines-group="session:lead"]');
  await expect(group.locator("b")).toHaveText("Builder");
  await expect(group.locator("[data-lines-range]")).toHaveCount(2);
  await expect(group.locator('[data-turn="4"]')).toHaveText("Open conversation at turn 4");
  await expect(frame.locator("#drill-panel [data-close]")).toBeFocused();
  expect(await frame.evaluate(() => (window as any).whyLineCalls)).toEqual([
    { repo: "example/project", rev: "b".repeat(40), path: "src/app.ts", start: 2, end: 5 },
  ]);
  await pane.close();
});

test("associated ranges list the candidate sessions only", async ({ mountPane }) => {
  const { pane, frame } = await pick(mountPane, "associated");
  await expect(frame.locator("[data-lines-associated]")).toContainText(
    "Who changed each line isn’t recorded yet",
  );
  await expect(frame.locator("[data-lines-associated]")).toContainText(
    "One of them may have changed lines 2–5.",
  );
  await expect(frame.locator("[data-lines-candidate]")).toHaveCount(1);
  await expect(frame.locator("[data-lines-candidate]")).toContainText("Builder · associated");
  await expect(frame.locator("[data-lines-summary]")).toBeHidden();
  await expect(frame.locator("#drill-panel [data-lines-group]")).toHaveCount(0);
  await expect(frame.locator("#drill-panel [data-turn]")).toHaveCount(0);
  await pane.close();
});

test("Escape and Close return focus to the Files row control", async ({ mountPane }) => {
  const { pane, frame } = await pick(mountPane);
  await expect(frame.locator("#drill-panel [data-close]")).toBeFocused();
  await pane.page.keyboard.press("Escape");
  await expect(frame.locator("#drill-panel")).toBeHidden();
  await expect(frame.locator(opener)).toBeFocused();
  await frame.locator(opener).click();
  await expect(frame.locator("#drill-panel")).toBeVisible();
  await frame.locator("#drill-panel [data-close]").click();
  await expect(frame.locator("#drill-panel")).toBeHidden();
  await expect(frame.locator(opener)).toBeFocused();
  expect(await frame.evaluate(() => location.hash)).toBe("#story/SV-X");
  await pane.close();
});

test("a passive refresh cannot move or swallow a held panel click", async ({ mountPane }) => {
  const { pane, frame } = await pick(mountPane);
  const target = frame.locator('#drill-panel [data-turn="4"]');
  await expect(target).toBeVisible();
  await target.evaluate((el) => {
    const w = window as any;
    w.heldNode = el;
    w.heldClicks = 0;
    el.addEventListener("click", () => w.heldClicks++);
  });
  const box = (await target.boundingBox())!,
    before = await frame.evaluate(() => (window as any).whyLineCalls.length);
  await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await pane.page.mouse.down();
  await frame.evaluate(() => {
    (window as any).whyLineMode = "empty";
  });
  await pane.upsert("sessions", agentRow("lead", "Builder refreshed"));
  await expect
    .poll(() => frame.evaluate(() => (window as any).whyLineCalls.length))
    .toBeGreaterThan(before);
  await paint(frame);
  expect(
    await target.evaluate(
      (el) =>
        el === (window as any).heldNode &&
        el.isConnected &&
        el.getAttribute("aria-disabled") === null,
    ),
  ).toBe(true);
  expect((await target.boundingBox())!.y).toBe(box.y);
  await pane.page.mouse.up();
  await paint(frame);
  expect(await frame.evaluate(() => (window as any).heldClicks)).toBe(1);
  await expect
    .poll(() => frame.evaluate(() => (window as any).openedFiles))
    .toEqual([{ path: "story-trail.html#session/lead/turn/4", newTab: true }]);
  await expect(frame.locator("#drill-panel [data-lines-range]")).toHaveCount(0);
  await expect(frame.locator("#lines-form [type=submit]")).not.toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await pane.close();
});

test("Open conversation at turn N lands on that turn, highlighted", async ({ mountPane }) => {
  const { pane, frame } = await pick(mountPane);
  await frame.locator('#drill-panel [data-turn="6"]').click();
  await expect
    .poll(() => frame.evaluate(() => (window as any).openedFiles))
    .toEqual([{ path: "story-trail.html#session/lead/turn/6", newTab: true }]);
  const hash = (await frame.evaluate(() => (window as any).openedFiles[0].path)).split("#")[1];
  const element = await frame.frameElement(),
    base = frame.url().split("#")[0];
  await element.evaluate((node: HTMLIFrameElement, url) => {
    node.contentWindow!.location.href = url;
    if (node.getAttribute("src") !== url) node.src = url;
  }, `${base}#${hash}`);
  await expect(frame.locator("#turn-6")).toHaveClass(/highlight/);
  await expect(frame.locator(".turn.highlight")).toHaveCount(1);
  await expect(frame.locator("#turn-6")).toBeInViewport();
  await pane.close();
});

test("Close during a pending read makes no further read and raises no error", async ({
  mountPane,
}) => {
  const pane = await mountPane(fixture, {
      width: 1280,
      height: 800,
      sessions: [agentRow("lead", "Builder")],
    }),
    frame: Frame = pane.frame,
    errors: string[] = [];
  pane.page.on("pageerror", (error) => errors.push(error.message));
  await expect(frame.locator(opener)).toBeVisible();
  await frame.evaluate(() => {
    (window as any).holdWhyLine = true;
  });
  await frame.locator(opener).click();
  await expect.poll(() => frame.evaluate(() => (window as any).heldWhyLines.length)).toBe(1);
  await pane.page.keyboard.press("Escape");
  await expect(frame.locator("#drill-panel")).toBeHidden();
  await expect(frame.locator(opener)).toBeFocused();
  await frame.evaluate(() => (window as any).heldWhyLines[0]());
  await paint(frame);
  await paint(frame);
  expect(
    await frame.evaluate(() => [
      (window as any).whyLineCalls.length,
      (window as any).rangeReads.length,
    ]),
  ).toEqual([1, 1]);
  expect(errors).toEqual([]);
  await expect(frame.locator("#drill-panel")).toBeHidden();
  await expect(frame.locator(opener)).toBeFocused();
  await pane.close();
});

test("a newer pick owns the panel while the older read is pending", async ({ mountPane }) => {
  const pane = await mountPane(fixture, {
      width: 1280,
      height: 800,
      sessions: [agentRow("lead", "Builder")],
    }),
    frame: Frame = pane.frame;
  await expect(frame.locator(opener)).toBeVisible();
  await frame.evaluate(() => {
    (window as any).holdWhyLine = true;
  });
  await frame.locator(opener).click();
  await expect.poll(() => frame.evaluate(() => (window as any).heldWhyLines.length)).toBe(1);
  await frame.locator('tr[data-row-focus="file:tests/app.test.ts"] [data-lines-path]').click();
  await expect.poll(() => frame.evaluate(() => (window as any).heldWhyLines.length)).toBe(2);
  await frame.evaluate(() => (window as any).heldWhyLines[0]());
  await paint(frame);
  await expect(frame.locator("[data-lines-file]")).toHaveText("tests/app.test.ts");
  await expect(frame.locator("#drill-panel [data-lines-range]")).toHaveCount(0);
  await frame.evaluate(() => (window as any).heldWhyLines[1]());
  await expect(frame.locator("#drill-panel [data-lines-range]")).toHaveCount(2);
  await expect(frame.locator("[name=linesStart]")).toHaveValue("10");
  expect(
    await frame.evaluate(() =>
      (window as any).whyLineCalls.map((call: any) => [call.path, call.start, call.end]),
    ),
  ).toEqual([
    ["src/app.ts", 2, 5],
    ["tests/app.test.ts", 10, 12],
  ]);
  await expect(frame.locator("#drill-panel [data-close]")).toBeFocused();
  await pane.close();
});
