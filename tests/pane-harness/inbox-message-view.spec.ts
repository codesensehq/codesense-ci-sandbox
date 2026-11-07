// SPDX-License-Identifier: MIT
import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import { expect, test, type MountedPane } from "./harness";
import { builder, INBOX_PANE, MARKDOWN, messageViewFixtures } from "./inbox-message-view-fixtures";

const builderRow = (pane: MountedPane) =>
  pane.frame.locator(".ib-row", {
    has: pane.frame.locator(".who > .trunc", { hasText: /^Builder$/ }),
  });
const post = (pane: MountedPane, message: unknown) =>
  pane.page.evaluate(
    ({ message, url }) => {
      const frame = [...document.querySelectorAll("iframe")].find((f) => f.src === url)!;
      frame.contentWindow!.postMessage(message, new URL(frame.src).origin);
    },
    { message, url: pane.frame.url() },
  );

test("hovering grouped messages survives live session snapshots without flashing", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, messageViewFixtures(false));
  await builderRow(pane).click();
  const grouped = pane.frame.locator(".ib-msg.cont:not(.mine)").first();
  await expect(grouped).toBeVisible();
  // Opening schedules a read refresh; finish that initial render before testing live updates.
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "inboxRollup").length)
    .toBeGreaterThan(1);
  await pane.frame.evaluate(async () => {
    await document.fonts.ready;
    await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
  });
  // Observe the actual session stream before asserting the node after the redraw.
  await pane.frame.evaluate(async () => {
    const w = window as any;
    w.messageNode = document.querySelector(".ib-msg.cont:not(.mine)");
    w.messageLeaves = 0;
    w.messageRemovals = 0;
    new MutationObserver((records) => {
      for (const record of records)
        for (const node of record.removedNodes) {
          if ((node as Element).matches?.(".ib-msg, .ht")) w.messageRemovals++;
        }
    }).observe(document.getElementById("log")!, { childList: true, subtree: true });
    w.messageNode.addEventListener("mouseout", () => w.messageLeaves++);
    await w.codesense.subscribe("sessions", {
      onChange: (view: any) => {
        w.lastActivity = view.snapshot.find((s: any) => s.id === "builder")?.activity;
      },
    });
  });
  await grouped.hover();
  for (let i = 0; i < 3; i++) {
    await pane.upsert("sessions", { ...builder, activity: `snapshot ${i}` });
    await expect
      .poll(() => pane.frame.evaluate(() => (window as any).lastActivity))
      .toBe(`snapshot ${i}`);
    await expect
      .poll(() =>
        pane.frame.evaluate(() => {
          const w = window as any;
          return {
            same: w.messageNode === document.querySelector(".ib-msg.cont:not(.mine)"),
            removed: w.messageRemovals,
            leaves: w.messageLeaves,
          };
        }),
      )
      .toEqual({ same: true, removed: 0, leaves: 0 });
  }
});

test("Back immediately after opening unread messages clears their badge and marks them read", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, { ...messageViewFixtures(), width: 420 });
  const row = builderRow(pane);
  await expect(row.locator(".ib-count")).toHaveText("2");
  await row.click();
  await pane.frame.locator("#back").click();
  await expect(row.locator(".ib-count")).toHaveCount(0);
  await expect
    .poll(async () =>
      [
        ...new Set(
          (await pane.verbs())
            .filter((v) => v.verb === "markRead")
            .flatMap((v) => (v.args as { messageIds: number[] }).messageIds),
        ),
      ].sort((a, b) => a - b),
    )
    .toEqual([7, 8]);
});

test("message Markdown renders structured lists, tasks, quotes, tables and code", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, {
    ...messageViewFixtures(false),
    inbox: [
      inboxFixtureRow(500, MARKDOWN, {
        from: "builder",
        fromName: "Builder",
        contentType: "markdown",
        unread: false,
      }),
    ],
  });
  await builderRow(pane).click();
  const message = pane.frame.locator("#log .ib-msg").first();
  await expect(message.locator("h2")).toHaveText("Markdown message");
  await expect(message.locator("strong")).toHaveText("bold");
  await expect(message.locator("em")).toHaveText("italic");
  await expect(message.locator("a", { hasText: "link" })).toHaveAttribute(
    "href",
    "https://example.com",
  );
  await expect(message.locator("ul ul ol li")).toHaveText("deeper numbered");
  await expect(message.locator("input[type=checkbox]")).toHaveCount(2);
  await expect(message.locator("input[type=checkbox]").first()).not.toBeChecked();
  await expect(message.locator("input[type=checkbox]").last()).toBeChecked();
  await expect(message.locator("input[type=checkbox]").last()).toBeDisabled();
  await expect(message.locator("blockquote")).toHaveText("A quoted message.");
  await expect(message.locator("pre code")).toContainText(
    "const status = await codesense.sessions();",
  );
  await expect(message.locator("table tbody tr")).toHaveCount(2);
});

test("reactions draw from the message's reactions, toggle through react and support keyboard navigation", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, messageViewFixtures(false));
  await builderRow(pane).click();
  const message = pane.frame.locator(".ib-msg", { hasText: "Grouping is in" });
  await expect(message.locator(".rx-chip")).toHaveAttribute("aria-label", "Reacted 👍 by you");
  await expect(message.locator(".rx-chip")).toHaveAttribute("aria-pressed", "true");
  await expect(builderRow(pane)).not.toContainText("reacted");
  await message.locator(".bd").first().hover();
  const strip = message.locator(".ib-react");
  await expect(strip).toHaveCSS("opacity", "1");
  await expect(strip.getByRole("button", { name: "thumbs up" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(strip.getByRole("button", { name: "thumbs up" })).toHaveAttribute(
    "title",
    "Remove thumbs up",
  );
  await strip.getByRole("button", { name: "check mark button" }).click();
  await expect
    .poll(async () => (await pane.verbs()).find((v) => v.verb === "react")?.args)
    .toMatchObject({ messageId: 4, emoji: "✅", remove: false });
  await expect(message.locator(".rx-chip")).toHaveCount(2);
  expect((await pane.verbs()).some((v) => v.verb === "replyAsHuman")).toBe(false);
  await message.locator(".bd").first().hover();
  await strip.getByRole("button", { name: "thumbs up" }).focus();
  await pane.page.keyboard.press("ArrowRight");
  await expect(strip.getByRole("button", { name: "check mark button" })).toBeFocused();
  await pane.frame.evaluate(async () => {
    const w = window as any;
    await w.codesense.subscribe("sessions", {
      onChange: (view: any) => {
        w.reactionSnapshot = view.snapshot.find((s: any) => s.id === "builder")?.activity;
      },
    });
  });
  await pane.upsert("sessions", { ...builder, activity: "reaction keyboard update" });
  await expect
    .poll(() => pane.frame.evaluate(() => (window as any).reactionSnapshot))
    .toBe("reaction keyboard update");
  await expect(strip.getByRole("button", { name: "check mark button" })).toBeFocused();
  await pane.page.keyboard.press("End");
  await expect(strip.getByRole("button", { name: "All emoji" })).toBeFocused();
  await expect(message.locator('.ib-react button[tabindex="0"]')).toHaveCount(1);
  await pane.page.keyboard.press("Home");
  await expect(strip.getByRole("button", { name: "thumbs up" })).toBeFocused();
  await expect(message.locator('.ib-react button[tabindex="0"]')).toHaveCount(1);
});

test("the composer focuses without an outline and inserts host-dropped paths at the cursor", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, messageViewFixtures(false));
  await post(pane, { type: "codesense:file-drop", paths: ["/tmp/ignored.txt"] });
  await builderRow(pane).click();
  const draft = pane.frame.locator("#draft");
  await expect(draft).toHaveValue("");
  await draft.fill("see");
  await post(pane, { type: "codesense:file-drag", phase: "enter" });
  await expect(pane.frame.locator("#inbox")).toHaveAttribute("data-drop", "");
  await post(pane, { type: "codesense:file-drag", phase: "leave" });
  await expect(pane.frame.locator("#inbox")).not.toHaveAttribute("data-drop", "");
  await post(pane, { type: "codesense:file-drag", phase: "over" });
  await expect(pane.frame.locator("#inbox")).toHaveAttribute("data-drop", "");
  await post(pane, { type: "codesense:file-drop", paths: ["/Users/t/My File.md", "/tmp/b.ts"] });
  await expect(pane.frame.locator("#inbox")).not.toHaveAttribute("data-drop", "");
  await expect(draft).toHaveValue('see "/Users/t/My File.md" /tmp/b.ts ');
  await expect(draft).toBeFocused();
  await expect(draft).toHaveCSS("outline-style", "none");
});
