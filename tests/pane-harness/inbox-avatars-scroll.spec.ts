// SPDX-License-Identifier: MIT
import { test, expect, type MountedPane } from "./harness";
import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import { builder, INBOX_PANE, messageViewFixtures } from "./inbox-message-view-fixtures";

const image =
  "data:image/svg+xml," +
  encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><circle cx="12" cy="12" r="12" fill="blue"/></svg>',
  );
const bindings = [
  ["Meta+ArrowUp", "top"],
  ["Meta+ArrowDown", "bottom"],
  ["Control+ArrowUp", "top"],
  ["Control+ArrowDown", "bottom"],
  ["Alt+ArrowUp", -600],
  ["Alt+ArrowDown", 600],
  ["PageUp", "page-up"],
  ["PageDown", "page-down"],
  ["Home", "top"],
  ["End", "bottom"],
  ["ArrowUp", -60],
  ["ArrowDown", 60],
] as const;

async function openThread(pane: MountedPane) {
  await pane.frame.locator(".ib-row").first().click();
  await expect(pane.frame.locator("#log")).toBeVisible();
}

async function scrollStart(pane: MountedPane) {
  const log = pane.frame.locator("#log");
  await log.evaluate((el) => (el as HTMLElement).focus({ preventScroll: true }));
  return log.evaluate((el) => {
    el.scrollTop = 1500;
    return {
      start: el.scrollTop,
      page: Math.max(el.clientHeight - 80, 200),
      max: el.scrollHeight - el.clientHeight,
    };
  });
}

const longFixtures = () => ({
  ...messageViewFixtures(false),
  sessions: [
    { ...builder, face: { kind: "image", src: "/kit/v1/avatars/builtin-chess/rook.svg" } },
  ],
  inbox: [
    inboxFixtureRow(
      50,
      Array.from(
        { length: 180 },
        (_, i) =>
          `Paragraph ${i}: enough conversation history to scroll with every native binding.`,
      ).join("\n\n"),
      {
        from: "builder",
        fromName: "Builder",
        conversation: "builder",
        unread: false,
      },
      { org: "fixture", team: "dev" },
    ),
  ],
});

test("Inbox faces follow session image, mark, avatar and initials fallbacks", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, {
    ...messageViewFixtures(false),
    sessions: [{ ...builder, face: { kind: "image", src: image } }],
  });
  await openThread(pane);
  const avatar = pane.frame.locator('[data-message-id="1"] > .av');
  await expect(avatar.locator("img")).toHaveAttribute("src", image);
  await expect(avatar.locator("img")).toHaveAttribute("alt", "Builder");
  await expect(avatar).toHaveAttribute("data-codesense-row", /"kind":"agent"/);
  await expect(avatar.locator(".st")).toHaveCount(1);
  expect(await avatar.evaluate((el) => el.getBoundingClientRect().width)).toBe(26);
  await expect(pane.frame.locator('[data-message-id="2"] > .av')).toBeHidden();

  await pane.upsert("sessions", {
    ...builder,
    face: { kind: "mark", text: "B!", color: "#64748b" },
  });
  await expect(avatar).toContainText("B!");
  await expect(avatar.locator("img")).toHaveCount(0);
  expect(
    await avatar.locator(".ib-face").evaluate((el) => getComputedStyle(el).backgroundColor),
  ).toBe("rgb(100, 116, 139)");

  await pane.upsert("sessions", { ...builder, avatar: "builtin-chess:rook" });
  await expect(avatar.locator("img")).toHaveCount(1);
  await pane.upsert("sessions", { ...builder, face: { kind: "image", src: "/missing-face.svg" } });
  await expect(avatar).toHaveText("BU");
  await expect(avatar.locator("img")).toHaveCount(0);
  await avatar.evaluate(async (el) => {
    const w = window as any;
    w.keptAvatar = el;
    await w.codesense.subscribe("sessions", {
      onChange: (snapshot: any) => {
        w.faceActivity = snapshot.snapshot.find((s: any) => s.id === "builder")?.activity;
      },
    });
  });
  for (const activity of ["working", "still working"]) {
    await pane.upsert("sessions", {
      ...builder,
      activity,
      face: { kind: "image", src: "/missing-face.svg" },
    });
    await expect.poll(() => pane.frame.evaluate(() => (window as any).faceActivity)).toBe(activity);
    expect(await avatar.evaluate((el) => el === (window as any).keptAvatar)).toBe(true);
    await expect(avatar.locator("img")).toHaveCount(0);
  }

  await pane.upsert("sessions", builder);
  await expect(avatar).toHaveText("BU");
});

test("Inbox sender without a live session uses initials", async ({ mountPane }) => {
  const pane = await mountPane(INBOX_PANE, { ...messageViewFixtures(false), sessions: [] });
  await openThread(pane);
  await expect(pane.frame.locator('[data-message-id="1"] > .av')).toHaveText("BU");
  await expect(pane.frame.locator('[data-message-id="1"] > .av img')).toHaveCount(0);
});

test("Inbox scrollbars have a visible themed track and thumb in light and dark", async ({
  mountPane,
}, testInfo) => {
  const pane = await mountPane(INBOX_PANE, longFixtures());
  await openThread(pane);
  for (const id of ["codesense", "github"] as const) {
    for (const scheme of ["light", "dark"] as const) {
      const vars = await pane.page.evaluate(
        async ([id, scheme]) => {
          const modulePath = "/src/themes/index.ts";
          const { themeTokens } = (await import(modulePath)) as typeof import("../../src/themes");
          return themeTokens(id, scheme);
        },
        [id, scheme] as const,
      );
      await pane.setTheme({ id, scheme, vars });
      const styles = await pane.frame.locator("#log").evaluate((el) => ({
        width: getComputedStyle(el, "::-webkit-scrollbar").width,
        thumb: getComputedStyle(el, "::-webkit-scrollbar-thumb").backgroundColor,
      }));
      expect(parseFloat(styles.width)).toBeGreaterThan(0);
      expect(styles.thumb).not.toBe("rgba(0, 0, 0, 0)");
      // Final pass M3: the resting thumb is a non-text UI part, so it needs 3:1
      // against the track it sits on.
      const ratio = await pane.frame.locator("#log").evaluate((el) => {
        const probe = document.createElement("canvas").getContext("2d")!;
        const rgb = (css: string) => {
          probe.fillStyle = "#000";
          probe.fillStyle = css;
          probe.fillRect(0, 0, 1, 1);
          const [r, g, b] = probe.getImageData(0, 0, 1, 1).data;
          return [r, g, b].map((c) => {
            const v = c / 255;
            return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
          });
        };
        const lum = (css: string) => {
          const [r, g, b] = rgb(css);
          return 0.2126 * r + 0.7152 * g + 0.0722 * b;
        };
        const a = lum(getComputedStyle(el, "::-webkit-scrollbar-thumb").backgroundColor);
        const b = lum(getComputedStyle(el, "::-webkit-scrollbar-track").backgroundColor);
        return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
      });
      expect(ratio, `${id} ${scheme} thumb contrast`).toBeGreaterThanOrEqual(3);
      await pane.page.screenshot({ path: testInfo.outputPath(`${id}-${scheme}.png`) });
    }
  }
});

test("Inbox thread scrolls smoothly with every native viewer binding", async ({ mountPane }) => {
  test.setTimeout(60_000);
  const pane = await mountPane(INBOX_PANE, longFixtures());
  await openThread(pane);
  const log = pane.frame.locator("#log");
  // Observe options while retaining the browser's actual scrolling implementation.
  await log.evaluate((el) => {
    const scrollTo = el.scrollTo.bind(el);
    el.scrollTo = ((options: ScrollToOptions) => {
      (el as any).lastScroll = options;
      scrollTo(options);
    }) as HTMLElement["scrollTo"];
  });
  const composerBounds = () =>
    pane.frame.locator("#composer").evaluate((el) => el.getBoundingClientRect().toJSON());
  const composer = await composerBounds();
  for (const [key, action] of bindings) {
    const { start, page, max } = await scrollStart(pane);
    const expected =
      action === "top"
        ? 0
        : action === "bottom"
          ? max
          : start + (action === "page-up" ? -page : action === "page-down" ? page : action);
    await pane.page.keyboard.press(key);
    await expect
      .poll(() => log.evaluate((el) => Math.round(el.scrollTop)))
      .toBe(Math.round(expected));
    expect(await log.evaluate((el) => (el as any).lastScroll?.behavior)).toBe("smooth");
  }
  expect(await composerBounds()).toEqual(composer);
});

test("Inbox scrolling leaves composer, selection, hidden threads and list navigation alone", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, longFixtures());
  await openThread(pane);
  const log = pane.frame.locator("#log");
  const { start } = await scrollStart(pane);
  await pane.frame.locator("#draft").fill("A draft to keep");
  for (const [key] of bindings) await pane.page.keyboard.press(key);
  await expect(log).toHaveJSProperty("scrollTop", start);
  await expect(pane.frame.locator("#draft")).toHaveValue("A draft to keep");
  await log.evaluate((el) => {
    const scrollTo = el.scrollTo.bind(el);
    (el as any).scrollCalls = [];
    el.scrollTo = ((options: ScrollToOptions) => {
      (el as any).scrollCalls.push(options);
      scrollTo(options);
    }) as HTMLElement["scrollTo"];
  });
  await log.focus();
  for (const [key] of bindings) await pane.page.keyboard.press(`Shift+${key}`);
  expect(await log.evaluate((el) => (el as any).scrollCalls)).toEqual([]);
  await pane.frame.locator("#conversations").focus();
  await pane.page.keyboard.press("ArrowDown");
  expect(await log.evaluate((el) => (el as any).scrollCalls)).toEqual([]);
  await pane.frame.frameElement().then((el) =>
    el.evaluate((el) => {
      (el as HTMLIFrameElement).style.width = "420px";
    }),
  );
  await expect(pane.frame.locator("#inbox")).toHaveAttribute("data-layout", "stack");
  await pane.frame.locator("#back").click();
  await expect(log).toBeHidden();
  await pane.page.keyboard.press("End");
  expect(await log.evaluate((el) => (el as any).scrollCalls)).toEqual([]);
});

test("failed faces make no new requests across chat, header and list session redraws", async ({
  page,
  mountPane,
}) => {
  let available = false;
  await page.route("**/failed-review-face.svg", (route) =>
    route.fulfill({
      status: available ? 200 : 404,
      contentType: "image/svg+xml",
      headers: { "cache-control": "no-store" },
      body: available
        ? '<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><rect width="20" height="20" fill="red"/></svg>'
        : "missing",
    }),
  );
  let requests = 0;
  page.on("request", (request) => {
    if (request.url().endsWith("/failed-review-face.svg")) requests++;
  });
  const session = { ...builder, face: { kind: "image", src: "/failed-review-face.svg" } };
  const pane = await mountPane(INBOX_PANE, { ...messageViewFixtures(false), sessions: [session] });
  await openThread(pane);
  await expect(pane.frame.locator(".av img")).toHaveCount(0);
  await expect.poll(() => requests).toBeGreaterThan(0);
  await expect(pane.frame.locator(".av img")).toHaveCount(0);
  const before = requests;
  await pane.frame.evaluate(async () => {
    await (window as any).codesense.subscribe("sessions", {
      onChange: (s: any) => {
        (window as any).faceReviewActivity = s.snapshot.find(
          (r: any) => r.id === "builder",
        )?.activity;
      },
    });
  });
  for (const activity of ["delta one", "delta two", "delta three"]) {
    await pane.upsert("sessions", { ...session, activity });
    await expect
      .poll(() => pane.frame.evaluate(() => (window as any).faceReviewActivity))
      .toBe(activity);
    await expect(pane.frame.locator(".av img")).toHaveCount(0);
    expect(requests).toBe(before);
    for (const selector of [".ib-row .av", "#thread-avatar .av", '[data-message-id="1"] > .av']) {
      await expect(pane.frame.locator(selector).first()).toHaveText("BU");
    }
  }
  await pane.upsert("sessions", { ...session, face: { kind: "mark", text: "X" } });
  await expect(pane.frame.locator("#thread-avatar .av")).toHaveText("X");
  available = true;
  await pane.upsert("sessions", { ...session, activity: "reselected" });
  await expect.poll(() => requests).toBeGreaterThan(before);
  await expect
    .poll(() =>
      pane.frame
        .locator("#thread-avatar .av img")
        .evaluateAll((images) =>
          images.some((image) => (image as HTMLImageElement).naturalWidth > 0),
        ),
    )
    .toBe(true);
});
