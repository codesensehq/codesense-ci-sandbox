// SPDX-License-Identifier: MIT
import { expect, test } from "@playwright/test";

test("the session Inbox rail respects reduced motion in its shared lenses and composer", async ({
  page,
}) => {
  await page.addInitScript(() => {
    let runtime: any;
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      get: () => runtime,
      set: (next) => {
        const invoke = next.invoke.bind(next);
        next.invoke = (command: string, args: any) =>
          command === "list_session_inbox"
            ? Promise.resolve({
                handle: args.handle,
                roles: [],
                messages: [
                  {
                    id: 1,
                    team: { org: "acme", team: "widgets" },
                    from: "sess-login-repro",
                    fromName: "Repro",
                    to: args.handle,
                    toName: "Lead",
                    toRole: null,
                    text: "Please review this change",
                    contentType: "plain",
                    createdAtMs: Date.now(),
                    inReplyTo: null,
                    via: null,
                    replyTo: null,
                    summary: null,
                    unread: false,
                    delivery: null,
                  },
                ],
              })
            : invoke(command, args);
        runtime = next;
      },
    });
  });
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.setViewportSize({ width: 1600, height: 950 });
  await page.goto("/?agents=team");
  await page.getByRole("button", { name: "Full screen" }).click();
  await page.getByTestId("terminal-grid-caption-inbox-sess-login-lead").click();
  const rail = page.getByTestId("inbox-rail").first();
  await expect(rail).toBeVisible();
  await rail.getByRole("button", { name: "Threads", exact: true }).click();
  await rail.locator('[data-thread="sess-login-repro"]').click();
  const composer = rail.locator(".ibx-box");
  await expect(composer).toBeVisible();
  expect(await composer.evaluate((el) => getComputedStyle(el).transitionDuration)).not.toBe("0s");
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect
    .poll(() => composer.evaluate((el) => getComputedStyle(el).transitionDuration))
    .toBe("0s");
  expect(await rail.evaluate((el) => el.getAnimations({ subtree: true }).length)).toBe(0);
  const message = rail.locator(".ibx-m").first();
  await expect(message).toBeVisible();
  expect(await message.evaluate((el) => getComputedStyle(el).animationName)).toBe("none");
});
