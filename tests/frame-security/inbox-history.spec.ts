// SPDX-License-Identifier: MIT
import { expect, test } from "../support/quarantine-playwright";
import { readFileSync } from "node:fs";

const fixture = readFileSync("src/features/shell/right-pane/__fixtures__/inbox-pane.html", "utf8");
const tokens =
  readFileSync("src-tauri/src/http/routes/dashboard_kit/kit.css", "utf8") +
  `
.host-light, .host-dark { --cs-font-sans: -apple-system, BlinkMacSystemFont, system-ui, sans-serif; }
.host-light { color-scheme: light; --cs-bg: #fff; --cs-fg: #1f2328; --cs-muted-fg: #59636e; --cs-border: #d1d9e0; --cs-surface: #f6f8fa; --cs-accent: #0969da; --cs-accent-fg: #fff; --cs-link: #0969da; --cs-danger: #cf222e; --cs-success: #1a7f37; --cs-warning: #9a6700; }
.host-dark { color-scheme: dark; --cs-bg: #282a36; --cs-fg: #f8f8f2; --cs-muted-fg: #a9abb8; --cs-border: #44475a; --cs-surface: #313443; --cs-accent: #bd93f9; --cs-accent-fg: #282a36; --cs-link: #8be9fd; --cs-danger: #ff6e6e; --cs-success: #50fa7b; --cs-warning: #f1fa8c; }
`;

test("scrolling to older history keeps the reader's place and waits until opening finishes", async ({
  page,
}) => {
  await page.setViewportSize({ width: 860, height: 740 });
  await page.goto("about:blank");
  await page.evaluate(() => {
    const w = window as any;
    const team = { org: "acme", team: "default" };
    const rows = Array.from({ length: 570 }, (_, n) => ({
      messageId: n + 1,
      conversation: "Mia",
      from: "Mia",
      fromName: "Mia",
      to: "human",
      toName: "You",
      toHuman: true,
      text: `Message ${n + 1}: ${"A little history. ".repeat((n % 4) + 2)}`,
      createdAtMs: Date.UTC(2026, 9, 3) + n * 1000,
      unread: n < 70,
      archivedAtMs: 0,
      contentType: "markdown",
      inReplyTo: null,
    }));
    w.historyCalls = [];
    w.historyReads = [];
    w.historyRelease = null;
    w.codesense = {
      context: async () => ({ ...team, activeTeam: team, inboxTeams: [team] }),
      prefs: async () => ({}),
      setPrefs: async () => {},
      setBadge: () => {},
      onInboxChanged: (fn: () => void) => {
        w.inboxChanged = fn;
      },
      onIntent: () => {},
      onFocusIn: () => {},
      onVisibility: (fn: any) => fn(true),
      onTeardown: () => {},
      subscribe: async () => ({ snapshot: [] }),
      unsubscribe: () => {},
      roles: async () => ({ roles: [] }),
      humanStatus: async () => ({ status: "online" }),
      act: async (verb: string, args: { messageIds: number[] }) => {
        if (verb === "markRead") {
          w.historyReads.push(...args.messageIds);
          rows.forEach((m) => {
            if (args.messageIds.includes(m.messageId)) m.unread = false;
          });
        }
        return {};
      },
      inbox: async (args: any) => {
        if (args.conversation && args.scope === "all") {
          w.historyCalls.push(args);
          await new Promise((resolve) => {
            w.historyRelease = resolve;
          });
        }
        const filtered =
          args.scope === "to-agents"
            ? []
            : rows.filter((m) => !args.before || m.messageId < args.before).reverse();
        return {
          messages: filtered.slice(0, args.limit),
          hasMore: filtered.length > args.limit,
          humanUnread: rows.filter((m) => m.unread).length,
          conversations: [
            {
              with: "Mia",
              team,
              unread: rows.filter((m) => m.unread).length,
              messageCount: rows.filter((m) => !m.archivedAtMs).length,
              lastAt: rows.at(-1)!.createdAtMs,
              lastMessageId: rows.at(-1)!.messageId,
              lastPreview: "A little history.",
            },
          ],
        };
      },
    };
    w.archiveOlder = () => {
      rows.slice(0, 20).forEach((m) => (m.archivedAtMs = Date.now()));
      w.inboxChanged();
    };
    w.receiveMessage = () => {
      rows.push({
        ...rows.at(-1)!,
        messageId: 571,
        text: "Message 571: A new message while reading history.",
        createdAtMs: rows.at(-1)!.createdAtMs + 1000,
        unread: false,
      });
      w.inboxChanged();
    };
  });
  await page.setContent(
    fixture
      .replace("</head>", `<style>${tokens}</style></head>`)
      .replace("<body>", '<body class="host-light">'),
  );
  await page.locator(".ib-row").click();
  await expect(page.locator("#log .ib-msg")).toHaveCount(500);
  await expect
    .poll(() =>
      page.locator("#log").evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight),
    )
    .toBeLessThanOrEqual(1);
  await expect.poll(() => page.evaluate(() => new Set((window as any).historyReads).size)).toBe(70);
  expect(await page.evaluate(() => (window as any).historyCalls.length)).toBe(0);
  await page.locator("#log").evaluate((el) => {
    el.scrollTop = 80;
  });
  await expect.poll(() => page.evaluate(() => (window as any).historyCalls.length)).toBe(1);
  await page.locator("#log").evaluate((el) => {
    el.scrollTop = 140;
  });
  const before = await page.locator("#log").evaluate((el) => {
    const top = el.getBoundingClientRect().top;
    const anchor = [...el.querySelectorAll<HTMLElement>(".ib-msg .bd")].find(
      (m) => m.getBoundingClientRect().bottom > top + 1,
    )!;
    return {
      id: anchor.closest(".ib-msg")!.id,
      y: anchor.getBoundingClientRect().top,
      fromBottom: el.scrollHeight - el.scrollTop,
    };
  });
  await page.evaluate(() => (window as any).historyRelease());
  await expect(page.locator("#older-status")).toHaveText("Loaded 50 older messages.");
  await expect(page.locator("#log .ib-msg")).toHaveCount(550);
  await expect.poll(() => page.evaluate(() => new Set((window as any).historyReads).size)).toBe(70);
  await expect(page.locator("#log .ib-new")).toHaveCount(0);
  const after = await page.locator("#log").evaluate(
    (el, id) => ({
      y: document.getElementById(id)!.querySelector(".bd")!.getBoundingClientRect().top,
      fromBottom: el.scrollHeight - el.scrollTop,
    }),
    before.id,
  );
  expect(Math.abs(after.y - before.y)).toBeLessThanOrEqual(1);
  expect(Math.abs(after.fromBottom - before.fromBottom)).toBeLessThanOrEqual(1);
  expect(await page.evaluate(() => (window as any).historyCalls[0])).toMatchObject({
    conversation: "Mia",
    before: 71,
    limit: 50,
  });
  await expect(page.locator("#log")).toHaveAttribute("aria-busy", "false");
  await expect(page.locator("#log")).toHaveAttribute("aria-live", "polite");
  await page.screenshot({ path: "target/inp11-history-light.png" });
  await page.locator("#log").evaluate((el) => {
    el.scrollTop = 0;
  });
  await expect.poll(() => page.evaluate(() => (window as any).historyCalls.length)).toBe(2);
  await expect(page.locator(".ib-older button")).toHaveText("Loading older messages…");
  await page.screenshot({ path: "target/inp11-history-loading-light.png" });
  await page.evaluate(() => {
    document.body.className = "host-dark";
  });
  await page.screenshot({ path: "target/inp11-history-loading-dark.png" });
  await page.evaluate(() => (window as any).historyRelease());
  await expect(page.locator(".ib-older")).toContainText("Start of conversation");
  await expect(page.locator("#older-status")).toHaveText(
    "Loaded 20 older messages. Start of conversation.",
  );
  await expect.poll(() => page.evaluate(() => new Set((window as any).historyReads).size)).toBe(70);
  expect(
    await page.evaluate(() =>
      [...new Set((window as any).historyReads)].sort((a: any, b: any) => a - b),
    ),
  ).toEqual(Array.from({ length: 70 }, (_, n) => n + 1));
  await expect(page.locator("#log .ib-new")).toHaveCount(0);
  await page.setViewportSize({ width: 420, height: 740 });
  await page.locator("#log").evaluate((el) => {
    el.scrollTop = 0;
  });
  await page.screenshot({ path: "target/inp11-history-end-dark.png" });
  await page.locator("#message-300").evaluate((el) => el.scrollIntoView({ block: "center" }));
  await page.evaluate(() => (window as any).receiveMessage());
  await expect.poll(() => page.evaluate(() => (window as any).historyCalls.length)).toBe(3);
  const olderAnchorTop = await page
    .locator("#message-300 .bd")
    .evaluate((el) => el.getBoundingClientRect().top);
  await page.evaluate(() => (window as any).historyRelease());
  await expect(page.locator("#log .ib-msg")).toHaveCount(571);
  const retainedAnchorTop = await page
    .locator("#message-300 .bd")
    .evaluate((el) => el.getBoundingClientRect().top);
  expect(Math.abs(retainedAnchorTop - olderAnchorTop)).toBeLessThanOrEqual(1);
  await expect(page.locator(".ib-older")).toContainText("Start of conversation");
  expect(await page.evaluate(() => (window as any).historyCalls[2].before)).toBeUndefined();
  await page.locator("#log").evaluate((el) => {
    el.scrollTop = el.scrollHeight - el.clientHeight - 160;
  });
  await page.evaluate(() => (window as any).archiveOlder());
  await expect.poll(() => page.evaluate(() => (window as any).historyCalls.length)).toBe(4);
  const refreshAnchor = await page.locator("#log").evaluate((el) => {
    const top = el.getBoundingClientRect().top;
    const anchor = [...el.querySelectorAll<HTMLElement>(".ib-msg .bd")].find(
      (m) => m.getBoundingClientRect().bottom > top + 1,
    )!;
    return {
      id: anchor.closest(".ib-msg")!.id,
      y: anchor.getBoundingClientRect().top,
      fromBottom: el.scrollHeight - el.scrollTop,
    };
  });
  await page.evaluate(() => (window as any).historyRelease());
  await expect(page.locator("#log .ib-msg")).toHaveCount(50);
  await expect(page.locator("#thread-meta")).toHaveText("551 messages");
  const refreshed = await page.locator("#log").evaluate(
    (el, id) => ({
      y: document.getElementById(id)!.querySelector(".bd")!.getBoundingClientRect().top,
      fromBottom: el.scrollHeight - el.scrollTop,
    }),
    refreshAnchor.id,
  );
  expect(Math.abs(refreshed.y - refreshAnchor.y)).toBeLessThanOrEqual(1);
  expect(Math.abs(refreshed.fromBottom - refreshAnchor.fromBottom)).toBeLessThanOrEqual(1);
  expect(await page.evaluate(() => (window as any).historyCalls[3].before)).toBeUndefined();
});
