// SPDX-License-Identifier: MIT
import { expect, test, type Locator, type Page } from "@playwright/test";

/**
 * Real-browser guards for a long inbox (the inbox keeps the newest 500 messages):
 *
 *  - The Mailspool opens at its newest line, draws only a window of the 500, and follows new
 *    mail while the reader is at the bottom.
 *  - A reader who scrolled up stays put — the line they are reading keeps its place even as the
 *    oldest line is trimmed at the cap — and is offered "Jump to latest".
 *  - The Threads conversation keeps a scrolled-up reader in place the same way. The desktop app
 *    is WebKit, which has no CSS scroll anchoring, so this is worth running there too.
 *
 * The mock backend has no session inbox, so `list_session_inbox` is answered here with a
 * generated conversation, and the inbox-changed event is fired by hand.
 */

const AGENT_TAB = "sess-login-lead";
const PEER = "sess-login-repro";

declare global {
  interface Window {
    __longLog: { first: number; last: number };
    __emitInbox: () => void;
  }
}

async function boot(page: Page) {
  await page.addInitScript(
    ({ peer }) => {
      window.__longLog = { first: 1, last: 500 };
      const now = Date.now();
      const make = (handle: string) => {
        const { first, last } = window.__longLog;
        const messages = [];
        for (let id = first; id <= last; id++) {
          const mine = id % 3 === 0;
          messages.push({
            id,
            team: { org: "acme", team: "widgets" },
            from: mine ? handle : peer,
            fromName: mine ? "lead" : "repro",
            to: mine ? peer : handle,
            toName: mine ? "repro" : "lead",
            toRole: null,
            // Mixed lengths, so rows and bubbles differ in height.
            text: `line ${id}: ${"lorem ipsum dolor sit amet ".repeat((id % 4) * 3 + 1)}`,
            contentType: "markdown",
            createdAtMs: now - (600 - id) * 20 * 60_000,
            inReplyTo: null,
            via: null,
            replyTo: null,
            summary: null,
            unread: false,
            delivery: null,
          });
        }
        return { handle, roles: [], messages };
      };
      type Internals = {
        invoke: (c: string, a?: Record<string, unknown>) => Promise<unknown>;
        runCallback: (id: number, p: unknown) => void;
      };
      let inner: Internals | undefined;
      Object.defineProperty(window, "__TAURI_INTERNALS__", {
        configurable: true,
        get: () => inner,
        set: (v: Internals) => {
          const orig = v.invoke.bind(v);
          const handlers: number[] = [];
          window.__emitInbox = () =>
            handlers.forEach((h) =>
              v.runCallback(h, { event: "agent-inbox-changed", id: 1, payload: {} }),
            );
          v.invoke = (cmd, args) => {
            if (cmd === "plugin:event|listen" && args?.event === "agent-inbox-changed")
              handlers.push(Number(args.handler));
            return cmd === "list_session_inbox"
              ? Promise.resolve(make(String(args?.handle ?? "x")))
              : orig(cmd, args);
          };
          inner = v;
        },
      });
    },
    { peer: PEER },
  );
  await page.setViewportSize({ width: 1600, height: 950 });
  await page.goto("/?agents=team");
  await page.getByRole("button", { name: "Full screen" }).click();
  await page.getByTestId(`terminal-grid-caption-inbox-${AGENT_TAB}`).click();
  const rail = page.locator('[data-testid="inbox-rail"]').first();
  await expect(rail).toBeVisible();
  return rail;
}

/** New mail lands: `add` newer messages, the oldest `trim` dropped (the 500 cap). */
async function land(page: Page, add: number, trim: number) {
  await page.evaluate(
    ({ add, trim }) => {
      window.__longLog.last += add;
      window.__longLog.first += trim;
      window.__emitInbox();
    },
    { add, trim },
  );
}

const distanceFromEnd = (scroller: Locator) =>
  scroller.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight);

/** Same item at the top, within a pixel (WebKit keeps scrollTop whole, layout is fractional). */
function expectSameTop(
  after: { key: string | null; at: number },
  before: { key: string | null; at: number },
) {
  expect(after.key).toBe(before.key);
  expect(Math.abs(after.at - before.at)).toBeLessThanOrEqual(1);
}

/** The item at the top of the view and how far below the top it sits. */
async function topItem(scroller: Locator, selector: string, attr: string) {
  return scroller.evaluate(
    (el, { selector, attr }) => {
      const top = el.getBoundingClientRect().top;
      const items = [...el.querySelectorAll<HTMLElement>(selector)]
        .map((n) => ({ n, r: n.getBoundingClientRect() }))
        .filter(({ r }) => r.bottom > top + 1)
        .sort((a, b) => a.r.top - b.r.top);
      const first = items[0]!;
      return { key: first.n.getAttribute(attr), at: Math.round(first.r.top - top) };
    },
    { selector, attr },
  );
}

test.describe("inbox on a long log", () => {
  test("Mailspool: opens at the newest of 500 lines, windowed, and follows new mail", async ({
    page,
  }) => {
    const rail = await boot(page);
    await rail.getByRole("button", { name: "Mailspool" }).click();
    const log = rail.getByTestId("ibx-log");
    await expect(log.locator('[data-log="acme/widgets#500"]')).toBeVisible();
    await expect.poll(() => distanceFromEnd(log)).toBeLessThanOrEqual(1);
    expect(await log.locator(".ibx-lrow").count()).toBeLessThan(80);
    await land(page, 1, 1);
    await expect(log.locator('[data-log="acme/widgets#501"]')).toBeVisible();
    await expect.poll(() => distanceFromEnd(log)).toBeLessThanOrEqual(1);
    await expect(rail.getByRole("button", { name: /Jump to latest/ })).toHaveCount(0);
  });

  test("Mailspool: a scrolled-up reader stays put as the oldest line is trimmed", async ({
    page,
  }) => {
    const rail = await boot(page);
    await rail.getByRole("button", { name: "Mailspool" }).click();
    const log = rail.getByTestId("ibx-log");
    await expect(log.locator('[data-log="acme/widgets#500"]')).toBeVisible();
    await log.evaluate((el) => (el.scrollTop = el.scrollHeight / 2));
    await page.waitForTimeout(300);
    const before = await topItem(log, "[data-log]", "data-log");
    await land(page, 2, 2);
    const jump = rail.getByRole("button", { name: /Jump to latest/ });
    await expect(jump).toBeVisible();
    await page.waitForTimeout(300);
    expectSameTop(await topItem(log, "[data-log]", "data-log"), before);
    await jump.click();
    await expect(log.locator('[data-log="acme/widgets#502"]')).toBeVisible();
    await expect.poll(() => distanceFromEnd(log)).toBeLessThanOrEqual(1);
    await expect(jump).toHaveCount(0);
  });

  test("Threads: keeps a scrolled-up reader in place and animates Jump to latest with reduced motion respected", async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "no-preference" });
    const rail = await boot(page);
    // A narrow rail lists conversations first: open the one with repro.
    await rail.locator(`[data-thread="${PEER}"]`).click();
    const msgs = rail.getByTestId("ibx-msgs");
    await expect(msgs.locator('[data-mid="acme/widgets#500"]')).toBeVisible();
    await expect.poll(() => distanceFromEnd(msgs)).toBeLessThanOrEqual(1);
    await msgs.evaluate((el) => (el.scrollTop = el.scrollHeight / 2));
    await page.waitForTimeout(300);
    const before = await topItem(msgs, "[data-mid]", "data-mid");
    // #501 is the reader's own, #502 is repro's; #1 and #2 are trimmed.
    await land(page, 2, 2);
    const jump = rail.getByRole("button", { name: /Jump to latest/ });
    await expect(jump).toBeVisible();
    await expect(msgs.locator('[data-mid="acme/widgets#2"]')).toHaveCount(0);
    expectSameTop(await topItem(msgs, "[data-mid]", "data-mid"), before);
    // Restart and pause the real control's CSS animation so slow test machines
    // can inspect its authored keyframes after the short entrance has finished.
    const keyframes = await jump.evaluate((el) => {
      el.style.animationPlayState = "paused";
      el.style.animationName = "none";
      void el.offsetWidth;
      el.style.animationName = "";
      return el
        .getAnimations()
        .flatMap((animation) => (animation.effect as KeyframeEffect).getKeyframes());
    });
    expect(keyframes.some((frame) => frame.opacity === "0")).toBe(true);
    expect(keyframes.some((frame) => frame.transform === "scale(0.985)")).toBe(true);
    await page.emulateMedia({ reducedMotion: "reduce" });
    await expect.poll(() => jump.evaluate((el) => el.getAnimations().length)).toBe(0);
  });
});
