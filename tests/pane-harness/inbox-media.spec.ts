// SPDX-License-Identifier: MIT
import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import type { InboxMediaAttachment } from "../../src/tauri/commands";
import { expect, test } from "./harness";
import { builder, INBOX_PANE } from "./inbox-message-view-fixtures";

const sha = "a".repeat(64);
const team = { org: "fixture", team: "dev" };
const media = (state: InboxMediaAttachment["state"], extra = {}): InboxMediaAttachment => ({
  kind: "image",
  state,
  sha256: sha,
  mime: "image/gif",
  width: 480,
  height: 270,
  alt: "A cat types very fast",
  sourceUrl: "https://i.giphy.com/cat.gif",
  sourceHost: "i.giphy.com",
  ...extra,
});
const fixtures = (attachment: InboxMediaAttachment) => ({
  width: 420,
  height: 800,
  sessions: [builder],
  inbox: [
    inboxFixtureRow(
      1,
      "",
      {
        from: "builder",
        fromName: "Builder",
        contentType: "markdown",
        unread: false,
        attachments: [attachment],
      },
      team,
    ),
  ],
});

const other = {
  id: "other",
  sessionId: "other",
  handle: "other",
  displayName: "Other",
  rowNames: { primary: "Other", secondary: null, full: "Other" },
  state: "running",
  alive: true,
  team,
};

const multiConversationFixtures = (attachment: InboxMediaAttachment) => ({
  width: 420,
  height: 800,
  sessions: [builder, other],
  inbox: [
    inboxFixtureRow(
      1,
      "Image conversation",
      {
        from: "builder",
        fromName: "Builder",
        contentType: "markdown",
        unread: false,
        attachments: [attachment],
        createdAtMs: Date.now() + 1000,
      },
      team,
    ),
    inboxFixtureRow(
      2,
      "Adjacent unread conversation",
      {
        from: "other",
        fromName: "Other",
        contentType: "markdown",
        unread: true,
        createdAtMs: Date.now(),
      },
      team,
    ),
  ],
});

const overflowingMultiConversationFixtures = (attachment: InboxMediaAttachment) => ({
  width: 420,
  height: 800,
  sessions: [builder, other],
  inbox: [
    inboxFixtureRow(
      1,
      "Image conversation\n\n" +
        Array.from(
          { length: 180 },
          (_, i) =>
            `Paragraph ${i}: enough conversation history to scroll while a lightbox is open.`,
        ).join("\n\n"),
      {
        from: "builder",
        fromName: "Builder",
        contentType: "markdown",
        unread: false,
        attachments: [attachment],
        createdAtMs: Date.now() + 1000,
      },
      team,
    ),
    inboxFixtureRow(
      2,
      "Adjacent unread conversation",
      {
        from: "other",
        fromName: "Other",
        contentType: "markdown",
        unread: true,
        createdAtMs: Date.now(),
      },
      team,
    ),
  ],
});

test("cached image uses only local bytes, previews alt, and opens an accessible lightbox", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, fixtures(media("cached")));
  const external: string[] = [];
  pane.page.on("request", (request) => {
    if (/giphy\.com/.test(request.url())) external.push(request.url());
  });
  await servePixel(pane.page);
  await expect(pane.frame.locator(".ib-row .prev")).toContainText("GIF · A cat types very fast");
  await pane.frame.locator(".ib-row").first().click();
  const open = pane.frame.getByRole("button", { name: "Open GIF: A cat types very fast" });
  await expect(open.locator("img")).toHaveAttribute("src", `/media/v1/${sha}`);
  await expect(pane.frame.locator(".ib-media")).toHaveAttribute("aria-busy", "false");
  await open.click();
  const dialog = pane.frame.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("link")).toHaveText("Open source page · i.giphy.com ↗");
  await expect(dialog.getByRole("button", { name: "Close image" })).toBeFocused();
  await dialog.screenshot({ path: "/tmp/sv-inp-14-lightbox-" + test.info().project.name + ".png" });
  await pane.page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(open).toBeFocused();
  const imageNode = await open.locator("img").elementHandle();
  await pane.upsert("sessions", { ...builder, state: "idle" });
  await expect(pane.frame.locator(".ib-msg .av .st.idle")).toBeVisible();
  expect(
    await imageNode!.evaluate((node) => node === document.querySelector(".ib-media-img")),
  ).toBe(true);
  expect(external).toEqual([]);
  await pane.frame
    .locator(".ib")
    .screenshot({ path: "/tmp/sv-inp-14-media-" + test.info().project.name + "-light.png" });
  await pane.setTheme({ id: "codesense", scheme: "dark" });
  await pane.frame
    .locator(".ib")
    .screenshot({ path: "/tmp/sv-inp-14-media-" + test.info().project.name + "-dark.png" });
});

for (const state of ["pending", "blocked", "failed"] as const) {
  test(`${state} keeps alt visible and offers the appropriate action without fetching`, async ({
    mountPane,
  }) => {
    const pane = await mountPane(INBOX_PANE, fixtures(media(state, { reason: "size" })));
    const requests: string[] = [];
    pane.page.on("request", (r) => {
      if (/\/media\/v1\/|giphy\.com/.test(r.url())) requests.push(r.url());
    });
    await pane.frame.locator(".ib-row").first().click();
    const card = pane.frame.locator(".ib-media-card");
    await expect(card).toContainText("A cat types very fast");
    if (state === "pending")
      await expect(card.getByRole("button", { name: /Load GIF from/ })).toBeVisible();
    if (state === "blocked")
      await expect(card.getByRole("link")).toHaveAttribute("href", "https://i.giphy.com/cat.gif");
    if (state === "failed") {
      await expect(card).toContainText("over 8 MB");
      await expect(card.getByRole("button")).toHaveCount(0);
    }
    expect(requests).toEqual([]);
    await card.screenshot({
      path: "/tmp/sv-inp-14-" + state + "-" + test.info().project.name + ".png",
    });
  });
}

test("OS Reduce Motion shows a canvas and explicit play/pause; hover does not play", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, fixtures(media("cached")));
  await pane.page.emulateMedia({ reducedMotion: "reduce" });
  await servePixel(pane.page);
  await pane.frame.locator(".ib-row").first().click();
  await expect(pane.frame.locator(".ib-media-still")).toBeVisible();
  const play = pane.frame.getByRole("button", { name: "Play GIF", exact: true });
  await play.hover();
  await expect(play).toHaveAttribute("aria-pressed", "false");
  await play.click();
  const pause = pane.frame.getByRole("button", { name: "Pause GIF", exact: true });
  await expect(pause).toHaveAttribute("aria-pressed", "true");
  await pause.click();
  await expect(play).toHaveAttribute("aria-pressed", "false");
});

test("tap and retry enter loadMedia through the inbox action gate", async ({ mountPane }) => {
  const pane = await mountPane(
    INBOX_PANE,
    fixtures(media("failed", { reason: "network", retryable: true })),
  );
  await pane.frame.locator(".ib-row").first().click();
  await pane.frame.getByRole("button", { name: "Retry", exact: true }).click();
  await expect
    .poll(async () =>
      (await pane.verbs()).some((v) => v.verb === "loadMedia" && v.args.messageId === 1),
    )
    .toBe(true);
});

test("media cards retain contrast across bundled themes", async ({ mountPane }) => {
  const { themeCheck } = await import("../conformance/checks");
  test.setTimeout(90_000);
  const pane = await mountPane(INBOX_PANE, fixtures(media("pending")));
  await pane.frame.locator(".ib-row").first().click();
  await expect(pane.frame.locator(".ib-media-card")).toBeVisible();
  await themeCheck.run(pane);
});

const pixel = Buffer.from("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7", "base64");
async function servePixel(page: import("@playwright/test").Page) {
  await page.route("**/media/v1/*", (route) =>
    route.fulfill({ contentType: "image/gif", body: pixel }),
  );
}
test("review 03 reduced-motion canvas allocation is bounded", async ({ mountPane }) => {
  const pane = await mountPane(INBOX_PANE, fixtures(media("cached", { width: 4097, height: 1 })));
  await pane.page.emulateMedia({ reducedMotion: "reduce" });
  await servePixel(pane.page);
  await pane.frame.locator(".ib-row").first().click();
  await expect(pane.frame.locator(".ib-media-still")).toBeVisible();
  expect(
    await pane.frame.locator("canvas").evaluate((n: HTMLCanvasElement) => n.width),
  ).toBeLessThanOrEqual(2048);
});
test("review 05 rendered media icons resolve to local symbols", async ({ mountPane }) => {
  const pane = await mountPane(INBOX_PANE, fixtures(media("cached")));
  await servePixel(pane.page);
  await pane.frame.locator(".ib-row").first().click();
  await pane.frame.getByRole("button", { name: "Open GIF: A cat types very fast" }).click();
  const targets = await pane.frame
    .locator(".ib-lightbox use")
    .evaluateAll((nodes) => nodes.every((n) => !!document.querySelector(n.getAttribute("href")!)));
  expect(targets).toBe(true);
  for (const name of ["x", "play", "pause", "image", "image-off", "circle-alert"])
    await expect(pane.frame.locator("#i-" + name)).toHaveCount(1);
});
test("review 06 html media updates reconcile its body", async ({ mountPane }) => {
  const f = fixtures(media("cached"));
  const row = inboxFixtureRow(1, "", { ...f.inbox[0].message, contentType: "html" }, team);
  const pane = await mountPane(INBOX_PANE, { ...f, inbox: [row] });
  await servePixel(pane.page);
  await pane.frame.locator(".ib-row").first().click();
  await expect(pane.frame.locator(".ib-media")).toBeVisible();
  await pane.upsert(
    "inbox",
    inboxFixtureRow(
      row.messageId,
      row.message.text,
      { ...row.message, attachments: [media("blocked")] },
      team,
    ),
  );
  await expect(pane.frame.locator(".ib-media-card.blocked")).toBeVisible();
  await expect(pane.frame.locator(".ib-media-img")).toHaveCount(0);
});
test("review 08 live off closes an open lightbox", async ({ mountPane }) => {
  const f = fixtures(media("cached"));
  const row = f.inbox[0];
  const pane = await mountPane(INBOX_PANE, f);
  await servePixel(pane.page);
  await pane.frame.locator(".ib-row").first().click();
  await pane.frame.getByRole("button", { name: "Open GIF: A cat types very fast" }).click();
  await expect(pane.frame.getByRole("dialog")).toBeVisible();
  await pane.upsert(
    "inbox",
    inboxFixtureRow(
      row.messageId,
      row.message.text,
      { ...row.message, attachments: [media("blocked")] },
      team,
    ),
  );
  await expect(pane.frame.getByRole("dialog")).toHaveCount(0);
});
test("review 07 loaded older media follows attachment-only changes", async ({ mountPane }) => {
  test.setTimeout(90_000);
  const older = inboxFixtureRow(
    1,
    "",
    { from: "builder", fromName: "Builder", unread: false, attachments: [media("pending")] },
    team,
  );
  const rows = [
    older,
    ...Array.from({ length: 500 }, (_, i) =>
      inboxFixtureRow(
        i + 2,
        "Older message " + i,
        { from: "builder", fromName: "Builder", unread: false },
        team,
      ),
    ),
  ];
  const pane = await mountPane(INBOX_PANE, { ...fixtures(media("pending")), inbox: rows });
  await servePixel(pane.page);
  await pane.frame.locator(".ib-row").first().click();
  for (let i = 0; i < 12 && !(await pane.frame.locator(".ib-media-card").count()); i++) {
    await pane.frame.getByRole("button", { name: "Load older messages", exact: true }).click();
  }
  await expect(pane.frame.locator(".ib-media-card")).toBeVisible();
  // The host rollup retains only its newest 500; history reads can still reach row 1.
  await pane.page.evaluate(() => {
    const api = (window as any).__TAURI_INTERNALS__;
    const invoke = api.invoke;
    api.invoke = async (command: string, args: any) => {
      if (command === "list_inbox_rollup") {
        const result = await invoke(command, args);
        return { ...result, messages: result.messages.filter((m: any) => m.id !== 1) };
      }
      if (command === "agent_read_inbox" && args.query?.before === 2) {
        const result = await invoke(command, { ...args, query: { ...args.query, limit: 1000 } });
        return { ...result, messages: result.messages.filter((m: any) => m.messageId === 1) };
      }
      return invoke(command, args);
    };
  });
  await pane.upsert(
    "inbox",
    inboxFixtureRow(
      older.messageId,
      older.message.text,
      { ...older.message, attachments: [media("cached")] },
      team,
    ),
  );
  await expect(pane.frame.locator(".ib-media-img")).toHaveCount(1);
});

test("re-review R2: cancelling the host confirmation restores the pending Load action", async ({
  mountPane,
}) => {
  const pending = media("pending", { sha256: undefined, mime: undefined });
  const pane = await mountPane(INBOX_PANE, { ...fixtures(pending), confirm: ["loadMedia"] });
  await pane.frame.locator(".ib-row").first().click();
  await pane.frame.getByRole("button", { name: /Load GIF from/ }).click();
  await expect(pane.page.getByTestId("verb-confirmation")).toBeVisible();
  await pane.page.getByTestId("verb-confirmation-cancel").click();
  await expect(pane.page.getByTestId("verb-confirmation")).toHaveCount(0);
  await expect(pane.frame.getByRole("button", { name: /Load GIF from/ })).toBeEnabled();
  await expect(pane.frame.locator(".ib-media-card.loading")).toHaveCount(0);
  await expect(pane.frame.getByText("Loading GIF…")).toHaveCount(0);
});

test("lightbox lets the terminal shortcut bubble while Escape still closes it", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, fixtures(media("cached")));
  await servePixel(pane.page);
  await pane.frame.locator(".ib-row").first().click();
  const open = pane.frame.getByRole("button", { name: "Open GIF: A cat types very fast" });
  await open.click();
  const dialog = pane.frame.getByRole("dialog");
  await expect(dialog).toBeVisible();

  // The lightbox must only consume keys it actually handles. Non-handled chords such
  // as the configured terminal shortcut must bubble to the injected window key bridge.
  const chordBubbled = await pane.frame.evaluate(() => {
    const dialog = document.querySelector("dialog[open]");
    if (!dialog) return false;
    let saw = false;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "\\" && e.metaKey) saw = true;
    };
    window.addEventListener("keydown", handler, { once: true });
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "\\", metaKey: true, bubbles: true }));
    window.removeEventListener("keydown", handler);
    return saw;
  });
  expect(chordBubbled).toBe(true);
  // The shortcut should hand focus to the host terminal textarea.
  await expect(pane.page.getByRole("textbox", { name: "terminal", exact: true })).toBeFocused();
  await expect(dialog).toBeVisible();

  // Escape only closes the dialog when focus is on a control that owns it.
  const closeImage = dialog.getByRole("button", { name: "Close image", exact: true });
  await closeImage.focus();
  await expect(closeImage).toBeFocused();
  await pane.page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(open).toBeFocused();
});

test("SV-PBF-184: ordinary inbox keys do not navigate behind an open lightbox", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, multiConversationFixtures(media("cached")));
  await servePixel(pane.page);

  // Select the image conversation and enter an unsent draft.
  await pane.frame.locator(".ib-row").first().click();
  const draft = pane.frame.locator("#draft");
  await draft.fill("unsent draft");

  // Open the image lightbox; focus lands on the Close image button inside the dialog.
  const open = pane.frame.getByRole("button", { name: "Open GIF: A cat types very fast" });
  await open.click();
  const dialog = pane.frame.getByRole("dialog");
  await expect(dialog).toBeVisible();
  const closeImage = dialog.getByRole("button", { name: "Close image", exact: true });
  await expect(closeImage).toBeFocused();

  const imageConversation = pane.frame.locator(".ib-row").filter({ hasText: "Builder" });
  const otherConversation = pane.frame.locator(".ib-row").filter({ hasText: "Other" });
  await expect(imageConversation).toHaveAttribute("aria-selected", "true");
  await expect(otherConversation).toHaveClass(/unread/);

  // Plain j/k/c from the modal must be ignored by Inbox letter navigation.
  for (const key of ["j", "k", "c"]) {
    await pane.page.keyboard.press(key);
  }
  await expect(imageConversation).toHaveAttribute("aria-selected", "true");
  await expect(otherConversation).toHaveClass(/unread/);
  await expect(draft).toHaveValue("unsent draft");

  // Once the modal is closed, ordinary navigation resumes from the restored opener focus.
  await pane.page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(open).toBeFocused();
  await pane.page.keyboard.press("j");
  await expect(otherConversation).toHaveAttribute("aria-selected", "true");
  await expect(otherConversation).not.toHaveClass(/unread/);
});

test("SV-PBF-184: lightbox scroll keys do not scroll the overflowing message log", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, overflowingMultiConversationFixtures(media("cached")));
  await servePixel(pane.page);

  // Select the image conversation and enter a draft.
  await pane.frame.locator(".ib-row").first().click();
  const draft = pane.frame.locator("#draft");
  await draft.fill("unsent draft");
  const log = pane.frame.locator("#log");

  // Observe real scroll requests while preserving the browser's actual scrolling.
  await log.evaluate((el) => {
    const scrollTo = el.scrollTo.bind(el);
    (el as any).scrollCalls = [];
    el.scrollTo = ((options: ScrollToOptions) => {
      (el as any).scrollCalls.push(options);
      scrollTo(options);
    }) as HTMLElement["scrollTo"];
  });

  // Open the image lightbox; the opener scrolls the image into view normally.
  const open = pane.frame.getByRole("button", { name: "Open GIF: A cat types very fast" });
  await open.click();
  const dialog = pane.frame.getByRole("dialog");
  await expect(dialog).toBeVisible();
  const closeImage = dialog.getByRole("button", { name: "Close image", exact: true });
  await expect(closeImage).toBeFocused();

  // Capture the stable nonzero position after the opener has done its normal work.
  const { scrollBefore, isOverflowing } = await log.evaluate((el) => ({
    scrollBefore: el.scrollTop,
    isOverflowing: el.scrollHeight > el.clientHeight,
  }));
  expect(isOverflowing).toBe(true);
  expect(scrollBefore).toBeGreaterThan(0);

  // Reset the observer before sending keys that must not scroll the log.
  await log.evaluate((el) => {
    (el as any).scrollCalls = [];
  });

  const imageConversation = pane.frame.locator(".ib-row").filter({ hasText: "Builder" });
  const otherConversation = pane.frame.locator(".ib-row").filter({ hasText: "Other" });
  await expect(imageConversation).toHaveAttribute("aria-selected", "true");
  await expect(otherConversation).toHaveClass(/unread/);

  // Scroll-style keys from the modal must not request background log scroll.
  for (const key of ["ArrowUp", "PageUp", "Home"]) {
    await pane.page.keyboard.press(key);
  }
  await expect.poll(() => log.evaluate((el) => el.scrollTop)).toBe(scrollBefore);
  expect(await log.evaluate((el) => (el as any).scrollCalls)).toEqual([]);
  await expect(imageConversation).toHaveAttribute("aria-selected", "true");
  await expect(otherConversation).toHaveClass(/unread/);
  await expect(draft).toHaveValue("unsent draft");

  // After closing, normal log scrolling resumes from a non-modal focus target.
  await pane.page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(pane.frame.locator(".ib-lightbox")).toHaveCount(0);
  await expect(open).toBeFocused();
  await log.evaluate((el) => {
    (el as any).scrollCalls = [];
  });
  await log.evaluate((el) => (el as HTMLElement).focus({ preventScroll: true }));
  await expect(log).toBeFocused();
  await pane.page.keyboard.press("Home");
  await expect.poll(() => log.evaluate((el) => Math.round(el.scrollTop))).toBe(0);
  expect(await log.evaluate((el) => (el as any).scrollCalls.length)).toBeGreaterThan(0);
});

test("the lightbox image is painted inside its figure", async ({ mountPane }) => {
  const pane = await mountPane(INBOX_PANE, { ...fixtures(media("cached")), height: 800 });
  await servePixel(pane.page);
  await pane.frame.locator(".ib-row").first().click();
  await pane.frame.getByRole("button", { name: "Open GIF: A cat types very fast" }).click();
  const dialog = pane.frame.getByRole("dialog");
  await expect(dialog.locator(".ib-media")).toHaveAttribute("aria-busy", "false");
  const [figure, image] = await Promise.all([
    dialog.locator(".ib-media").evaluate((el) => el.getBoundingClientRect().toJSON()),
    dialog.locator(".ib-media-img").evaluate((el) => el.getBoundingClientRect().toJSON()),
  ]);
  // A letterboxed image taller than its clipping figure paints only the figure background.
  expect(image.top).toBeGreaterThanOrEqual(figure.top - 1);
  expect(image.bottom).toBeLessThanOrEqual(figure.bottom + 1);
  expect(image.height).toBeGreaterThan(0);
});
