// SPDX-License-Identifier: MIT
import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import { PANE_STARTUP_TIMEOUT_MS } from "../../src/features/dashboard/lib/frame-lifecycle";
import { expect, test } from "../pane-harness/harness";
import {
  CHECKS,
  ConformanceFailure,
  DEFAULT_FIXTURES,
  DISPATCHER_BUDGET_WINDOW_MS,
  runCheck,
} from "./checks";

const fixture = (name: string) => `tests/conformance/fixtures/${name}.html`;
/** Each check's bad fixture (named for the check) must fail for this reason. */
const REASONS: Record<string, RegExp> = {
  "focus-in": /focus-in left focus on <body>/,
  escape: /focus stayed in the frame after Escape/,
  flush: /not acknowledged within 300 ms/,
  budget: /retries instead of stopping/,
  "team-switch": /never resynced its "sessions" subscription/,
  maximize: /reloaded on maximize/,
  reparent: /view changed across the move/,
  theme: /body.*did not change/,
  axe: /button-name \(critical\)/,
};

for (const check of CHECKS) {
  test(`good fixture passes: ${check.title}`, ({ mountPane }) => {
    if (check.id === "theme") test.setTimeout(90_000);
    return runCheck(mountPane, fixture("good"), check);
  });
  test(`bad fixture fails: ${check.title}`, async ({ mountPane }) => {
    if (check.id === "theme") test.setTimeout(90_000);
    const error = await runCheck(mountPane, fixture(check.id), check).then(
      () => null,
      (e: unknown) => e,
    );
    expect(error, `${check.id} passed its bad fixture`).toBeInstanceOf(ConformanceFailure);
    expect((error as ConformanceFailure).check).toBe(check.id);
    expect((error as ConformanceFailure).reason).toMatch(REASONS[check.id]);
  });
}

test("a budget denial must show a visible error, even when the page stops calling", async ({
  mountPane,
}) => {
  const check = CHECKS.find((check) => check.id === "budget")!;
  await expect(runCheck(mountPane, fixture("budget-silent"), check)).rejects.toThrow(
    /budget: .*left no error on the page/,
  );
});

test("a one-shot retry 120 ms after a budget rejection fails", async ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "budget")!;
  await expect(runCheck(mountPane, fixture("budget-once"), check)).rejects.toThrow(
    /budget: .*retries instead of stopping/,
  );
});

test("resubscribing without rendering the new inbox snapshot fails", async ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  await expect(runCheck(mountPane, fixture("team-switch-discard"), check)).rejects.toThrow(
    /team-switch: .*row added while hidden never appeared/,
  );
});

test("a pane rendering inbox rows passes team switch", ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  return runCheck(mountPane, fixture("inbox-good"), check);
});

test("budget calls already outstanding at rejection are exempt", ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "budget")!;
  return runCheck(mountPane, fixture("budget-in-flight"), check);
});

test("an auxiliary subscription need not render its own rows", ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  return runCheck(mountPane, fixture("auxiliary-topic"), check);
});

test("a pane with subscriptions but no rendered data fails", async ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  await expect(runCheck(mountPane, fixture("team-switch-unrendered"), check)).rejects.toThrow(
    /team-switch: no subscribed data rendered/,
  );
});

test("a mixed pane cannot discard inbox updates while sessions keep rendering", async ({
  mountPane,
}) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  await expect(runCheck(mountPane, fixture("team-switch-mixed-stale"), check)).rejects.toThrow(
    /team-switch: a "inbox" row added while hidden never appeared/,
  );
});

test("the fake rollup returns conversation summaries consistent with its messages", async ({
  mountPane,
}) => {
  const pane = await mountPane(fixture("good"), {
    ...DEFAULT_FIXTURES,
    inbox: [
      { id: "counts:team:fixture/dev", rowType: "counts", needsYouBySender: {} },
      inboxFixtureRow(11, "First message", { from: "ada", conversation: "ada", createdAtMs: 1000 }),
      inboxFixtureRow(12, "Latest message", {
        from: "ada",
        conversation: "ada",
        createdAtMs: 2000,
        unread: false,
      }),
      inboxFixtureRow(13, "Other conversation", {
        from: "grace",
        conversation: "grace",
        createdAtMs: 3000,
      }),
    ],
  });
  await pane.frame.waitForLoadState("load");
  const rollup = await pane.frame.evaluate(() => (window as any).codesense.inboxRollup());
  expect(rollup.conversations).toEqual([
    {
      with: "ada",
      team: { org: "fixture", team: "dev" },
      unread: 1,
      messageCount: 2,
      lastAt: 2000,
      lastMessageId: 12,
      lastPreview: "Latest message",
    },
    {
      with: "grace",
      team: { org: "fixture", team: "dev" },
      unread: 1,
      messageCount: 1,
      lastAt: 3000,
      lastMessageId: 13,
      lastPreview: "Other conversation",
    },
  ]);
  expect(rollup.messages.map((message: { id: number }) => message.id)).toEqual([11, 12, 13]);
  expect(rollup.humanUnread).toBe(2);
});

test("a sessions pane rendering handles passes team switch", ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  return runCheck(mountPane, fixture("handle-good"), check);
});

test("sessions plus shared ui state pass team switch", ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  return runCheck(mountPane, fixture("ui-state-good"), check);
});

test("a one-shot retry 3.5 seconds after a budget rejection fails", async ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "budget")!;
  await expect(runCheck(mountPane, fixture("budget-delayed"), check)).rejects.toThrow(
    /budget: .*retries instead of stopping/,
  );
});

test("budget observation fails by name if the page falls back during the window", async ({
  mountPane,
  page,
}) => {
  const check = CHECKS.find((check) => check.id === "budget")!;
  // Since SV-PLG-28 a never-ready page stays neutral until PANE_STARTUP_TIMEOUT_MS (30 s), longer
  // than the check's real-time window, so the page clock must jump past it mid-window.
  await page.clock.install();
  const pane = await mountPane(fixture("never-ready"), { ...DEFAULT_FIXTURES, ...check.fixtures });
  const started = Date.now();
  const outcome = expect(check.run(pane)).rejects.toThrow(/budget: the page is gone/);
  await page.waitForTimeout(5000); // well past the settle, well before the window ends
  await page.clock.fastForward(PANE_STARTUP_TIMEOUT_MS + 500);
  await outcome;
  // A rejection from settle's own liveFrame would arrive early and not guard the post-window one.
  expect(Date.now() - started).toBeGreaterThanOrEqual(DISPATCHER_BUDGET_WINDOW_MS);
});

test("startup budget denial leaves reads available and the good pane live", async ({
  mountPane,
}) => {
  const check = CHECKS.find((check) => check.id === "budget")!;
  const pane = await mountPane(fixture("good"), { ...DEFAULT_FIXTURES, ...check.fixtures });
  await check.run(pane);
  const dispatches = await pane.dispatches();
  expect(dispatches.filter((call) => call.class === "read" && call.status === "deny")).toEqual([]);
  expect(dispatches.some((call) => call.class === "prefs" && call.status === "deny")).toBe(true);
  expect(pane.currentFrame()).toBeDefined();
});

test("a retry after the ten-second budget window passes", async ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "budget")!;
  const pane = await mountPane(fixture("budget-after-window"), {
    ...DEFAULT_FIXTURES,
    ...check.fixtures,
  });
  await check.run(pane);
  const calls = (await pane.probe()).invocations.filter((event) => event.kind === "call");
  expect(calls).toHaveLength(2);
});

test("inbox sender handles cannot hide a discarded session update", async ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  await expect(runCheck(mountPane, fixture("team-switch-session-stale"), check)).rejects.toThrow(
    /team-switch: a "sessions" row added while hidden never appeared/,
  );
});

test("inbox rollup plus onInboxChanged renders the hidden arrival", ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  return runCheck(mountPane, fixture("inbox-rollup-good"), check);
});

test("an inbox rollup renderer that never re-reads fails by inbox topic", async ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  await expect(runCheck(mountPane, fixture("inbox-rollup-stale"), check)).rejects.toThrow(
    /team-switch: a "inbox" row added while hidden never appeared/,
  );
});

test("a once-read inbox without subscriptions cannot skip team switch", async ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  const pane = await mountPane(fixture("inbox-rollup-once"), DEFAULT_FIXTURES);
  await expect(check.run(pane)).rejects.toThrow(
    /team-switch: a "inbox" row added while hidden never appeared/,
  );
  expect((await pane.dispatches()).filter((call) => call.verb === "subscribe")).toEqual([]);
});

test("a records:notes pane renders its hidden arrival", ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  return runCheck(mountPane, fixture("records-good"), check);
});

test("a pane discarding its records:notes snapshot fails by collection", async ({ mountPane }) => {
  const check = CHECKS.find((check) => check.id === "team-switch")!;
  await expect(runCheck(mountPane, fixture("records-discard"), check)).rejects.toThrow(
    /team-switch: a "records:notes" row added while hidden never appeared/,
  );
});

test("theme checks alpha-composited text on the pane's own surface", async ({ mountPane }) => {
  test.setTimeout(90_000);
  const pane = await mountPane(fixture("good"), DEFAULT_FIXTURES);
  await pane.frame.evaluate(() => {
    const sample = document.createElement("p");
    sample.id = "low-contrast-text";
    sample.textContent = "Low contrast";
    sample.style.cssText =
      "background: var(--cs-fg); color: color-mix(in srgb, var(--cs-fg) 20%, transparent)";
    document.body.append(sample);
  });
  await expect(CHECKS.find((c) => c.id === "theme")!.run(pane)).rejects.toThrow(
    /codesense\/light p#low-contrast-text text: 1\.00:1 < 4\.5:1/,
  );
});

test("theme checks the pane's control borders and icons", async ({ mountPane }) => {
  test.setTimeout(90_000);
  const pane = await mountPane(fixture("good"), DEFAULT_FIXTURES);
  await pane.frame.evaluate(() => {
    const button = document.createElement("button");
    button.id = "low-contrast-control";
    button.style.cssText = "background: var(--cs-bg); border: 1px solid var(--cs-bg)";
    button.innerHTML =
      '<svg width="20" height="20"><path id="low-contrast-icon" d="M 0 0 L 20 20" style="stroke: var(--cs-bg); fill: none"/></svg>Low contrast';
    document.body.append(button);
  });
  const error = await CHECKS.find((c) => c.id === "theme")!
    .run(pane)
    .catch((error) => error);
  expect(error.message).toMatch(
    /codesense\/light button#low-contrast-control nonText: 1\.00:1 < 3:1/,
  );
  expect(error.message).toMatch(/codesense\/light path#low-contrast-icon nonText: 1\.00:1 < 3:1/);
});
