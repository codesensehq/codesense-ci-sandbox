// SPDX-License-Identifier: MIT
import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import { test, expect, type MountedPane, type PaneFixtures } from "./harness";
import {
  builder,
  INBOX_PANE,
  messageViewFixtures,
  scrollBubbleTo,
} from "./inbox-message-view-fixtures";

async function shortThread(
  mountPane: (path: string, fixtures: PaneFixtures) => Promise<MountedPane>,
  reacted = false,
  long = false,
) {
  const now = Date.now();
  const pane = await mountPane(INBOX_PANE, {
    ...messageViewFixtures(false),
    width: 420,
    height: 620,
    inbox: [
      ...Array.from({ length: 24 }, (_, i) =>
        inboxFixtureRow(
          100 + i,
          long && i === 12
            ? Array.from(
                { length: 30 },
                (_, n) =>
                  `Long reply paragraph ${n}: keeping the conversation readable while scrolling.`,
              ).join("\n\n")
            : "OK",
          {
            from: "builder",
            fromName: "Builder",
            conversation: "builder",
            unread: false,
            contentType: "markdown",
            ...(reacted && i === 0
              ? {
                  primaryFile: "notes/a.md",
                  reactions: [{ emoji: "👍", count: 1, reactors: ["human"] }],
                }
              : {}),
            createdAtMs: now - (24 - i) * 6 * 60_000,
          },
          { org: "fixture", team: "dev" },
        ),
      ),
    ],
  });
  await pane.page.setViewportSize({ width: 1000, height: 800 });
  await pane.frame.locator(".ib-row").click();
  await expect(pane.frame.locator(".ib-msg")).toHaveCount(24);
  return pane;
}

async function align(pane: MountedPane, id: number, offset: number) {
  const body = pane.frame.locator(`[data-message-id="${id}"] .bd`);
  await body.evaluate((el, offset) => {
    const log = document.getElementById("log")!;
    log.scrollTop += el.getBoundingClientRect().top - log.getBoundingClientRect().top - offset;
  }, offset);
  await body.hover();
  return { body, bar: pane.frame.locator(`[data-message-id="${id}"] .ib-react`) };
}

test("the strip anchors to the bubble's outer edge above mid-thread and flips below at the top", async ({
  mountPane,
}) => {
  const pane = await shortThread(mountPane);
  for (const [id, offset, placement] of [
    [100, 4, "below"],
    [112, 180, "above"],
  ] as const) {
    const { body, bar } = await align(pane, id, offset);
    await expect(bar).toHaveCSS("opacity", "1");
    const rects = await body.evaluate((el) => ({
      text: el.querySelector("p")!.getBoundingClientRect().toJSON(),
      bubble: el.getBoundingClientRect().toJSON(),
      message: el.closest(".ib-msg")!.getBoundingClientRect().toJSON(),
      bar: el.querySelector(".ib-react")!.getBoundingClientRect().toJSON(),
    }));
    expect(rects.bar.bottom <= rects.text.top || rects.bar.top >= rects.text.bottom).toBe(true);
    // Theirs: the strip starts 4px outside the bubble's left (outer) edge.
    expect(Math.abs(rects.bar.left - (rects.bubble.left - 4))).toBeLessThanOrEqual(1);
    if (placement === "above") expect(rects.bar.bottom).toBeLessThanOrEqual(rects.bubble.top - 5);
    else expect(rects.bar.top).toBeGreaterThanOrEqual(rects.message.bottom);
    for (const button of await bar.getByRole("button").all())
      await expect(button).toBeInViewport({ ratio: 1 });
  }
});

test("the strip waits for hover intent so a pointer crossing the thread opens nothing", async ({
  mountPane,
}) => {
  const pane = await shortThread(mountPane);
  const bar = pane.frame.locator('[data-message-id="112"] .ib-react');
  await (await scrollBubbleTo(pane, 112, 180)).hover();
  await pane.page.waitForTimeout(120);
  await expect(bar).not.toHaveAttribute("data-open", "true");
  await expect(bar).toHaveAttribute("data-open", "true");
});

test("the strip is six quick emoji and All emoji, which opens the full picker and Escape closes it", async ({
  mountPane,
}) => {
  const pane = await shortThread(mountPane);
  const { bar } = await align(pane, 112, 180);
  await expect(bar.getByRole("button")).toHaveText(["👍", "✅", "👀", "❤️", "😂", "🙏", ""]);
  await expect(bar.getByRole("button").first()).toHaveAttribute("aria-label", "thumbs up");
  const more = bar.getByRole("button", { name: "All emoji" });
  await expect(more).toHaveAttribute("aria-haspopup", "dialog");
  await more.click();
  await expect(more).toHaveAttribute("aria-expanded", "true");
  const picker = pane.frame.getByRole("dialog", { name: "All emoji" });
  await expect(picker).toBeVisible();
  await expect(picker.getByRole("searchbox", { name: "Search emoji" })).toBeFocused();
  await pane.upsert("sessions", { ...builder, activity: "popover remains open" });
  await expect(picker).toBeVisible();
  await expect(more).toHaveAttribute("aria-expanded", "true");
  await pane.page.keyboard.press("Escape");
  await expect(picker).toBeHidden();
  await expect(more).toHaveAttribute("aria-expanded", "false");
  await expect(more).toBeFocused();
  await pane.page.keyboard.press("Escape");
  await expect(bar).toHaveCSS("opacity", "0");
  await expect(pane.frame.locator("#log")).toBeFocused();
});

test("a flipped strip clears its own reactions and attachment chips", async ({ mountPane }) => {
  const pane = await shortThread(mountPane, true);
  const { body, bar } = await align(pane, 100, 4);
  await expect(bar).toHaveCSS("opacity", "1");
  const boxes = await body.evaluate((el) => ({
    strip: el.querySelector(".ib-react")!.getBoundingClientRect().toJSON(),
    foot: el.closest(".ib-msg")!.querySelector(".foot")!.getBoundingClientRect().toJSON(),
  }));
  await expect(pane.frame.locator('[data-message-id="100"] .rx-chip')).toBeVisible();
  const chips = await body.evaluate((el) =>
    el.querySelector(".ib-rx")!.getBoundingClientRect().toJSON(),
  );
  expect(boxes.strip.bottom <= boxes.foot.top || boxes.strip.top >= boxes.foot.bottom).toBe(true);
  expect(boxes.strip.bottom <= chips.top || boxes.strip.top >= chips.bottom).toBe(true);
});

test("a strip near the bottom of the log never covers the composer", async ({ mountPane }) => {
  const pane = await shortThread(mountPane, false, true);
  const body = pane.frame.locator('[data-message-id="112"] .bd');
  const point = await body.evaluate((el) => {
    const log = document.getElementById("log")!;
    log.scrollTop += el.getBoundingClientRect().bottom - log.getBoundingClientRect().bottom + 8;
    const box = el.getBoundingClientRect();
    return { x: box.left + box.width / 2, y: log.getBoundingClientRect().bottom - 20 };
  });
  const frame = (await (await pane.frame.frameElement()).boundingBox())!;
  await pane.page.mouse.move(frame.x + point.x, frame.y + point.y);
  await expect(body.locator(".ib-react")).toHaveAttribute("data-open", "true");
  const boxes = await body.evaluate((el) => ({
    strip: el.querySelector(".ib-react")!.getBoundingClientRect().toJSON(),
    log: document.getElementById("log")!.getBoundingClientRect().toJSON(),
    composer: document.getElementById("composer")!.getBoundingClientRect().toJSON(),
  }));
  expect(boxes.strip.bottom).toBeLessThanOrEqual(boxes.log.bottom);
  expect(boxes.strip.bottom).toBeLessThanOrEqual(boxes.composer.top);
});

test("pointer travel from a continuation to its strip keeps the reaction target", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, {
    ...messageViewFixtures(false),
    width: 900,
    height: 700,
  });
  await pane.page.setViewportSize({ width: 1100, height: 850 });
  await pane.frame.locator(".ib-row").click();
  const { bar } = await align(pane, 2, 120);
  await expect(bar).toHaveCSS("opacity", "1");
  const box = (await bar.boundingBox())!;
  await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, { steps: 20 });
  await pane.page.waitForTimeout(250);
  await expect(pane.frame.locator('[data-message-id="2"] .ib-react')).toHaveAttribute(
    "data-open",
    "true",
  );
  await bar.getByRole("button", { name: "check mark button", exact: true }).click();
  await expect
    .poll(async () => (await pane.verbs()).find((v) => v.verb === "react")?.args)
    .toMatchObject({ messageId: 2, emoji: "✅", remove: false });
});

test("leaving a pending hover target closes the previous reaction strip", async ({ mountPane }) => {
  const pane = await shortThread(mountPane);
  const { bar } = await align(pane, 112, 140);
  await expect(bar).toHaveCSS("opacity", "1");
  const next = pane.frame.locator('[data-message-id="113"] .bd');
  const box = (await next.boundingBox())!;
  const log = (await pane.frame.locator("#log").boundingBox())!;
  await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await pane.page.mouse.move(log.x + log.width - 18, box.y + box.height / 2);
  await expect(bar).toHaveCSS("opacity", "0");
});

test("keyboard-focused reactions remain visible when another message is hovered", async ({
  mountPane,
}) => {
  const pane = await shortThread(mountPane);
  const { bar } = await align(pane, 112, 140);
  const button = bar.getByRole("button").first();
  await button.focus();
  await pane.frame.locator('[data-message-id="113"] .bd').hover();
  await pane.page.waitForTimeout(300);
  await expect(button).toBeFocused();
  await expect(bar).toHaveCSS("opacity", "1");
});

test("the strip and the full picker stay inside the thread in the wide layout", async ({
  mountPane,
}) => {
  const now = Date.now();
  const pane = await mountPane(INBOX_PANE, {
    ...messageViewFixtures(false),
    width: 1000,
    height: 700,
    inbox: Array.from({ length: 6 }, (_, i) =>
      inboxFixtureRow(
        300 + i,
        "OK",
        {
          from: "builder",
          fromName: "Builder",
          conversation: "builder",
          unread: false,
          contentType: "markdown",
          createdAtMs: now - (6 - i) * 6 * 60_000,
        },
        { org: "fixture", team: "dev" },
      ),
    ),
  });
  await pane.page.setViewportSize({ width: 1000, height: 700 });
  await pane.frame.locator(".ib-row").click();
  const { bar } = await align(pane, 303, 200);
  await bar.getByRole("button", { name: "All emoji" }).click();
  const picker = pane.frame.getByRole("dialog", { name: "All emoji" });
  await expect(picker).toBeVisible();
  const [strip, log, box] = await Promise.all([
    bar.evaluate((el) => el.getBoundingClientRect().toJSON()),
    pane.frame.locator("#log").evaluate((el) => el.getBoundingClientRect().toJSON()),
    picker.evaluate((el) => el.getBoundingClientRect().toJSON()),
  ]);
  for (const r of [strip, box]) {
    expect(r.left).toBeGreaterThanOrEqual(log.left);
    expect(r.right).toBeLessThanOrEqual(log.right);
  }
});
