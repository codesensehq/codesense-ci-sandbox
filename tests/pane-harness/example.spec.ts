// SPDX-License-Identifier: MIT
import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import { expect, test } from "./harness";

const PANE = "tests/pane-harness/fixtures/sample-pane.html";
const agent = (id: string, name: string) => ({
  id,
  sessionId: id,
  handle: id,
  kind: "cli",
  alive: true,
  team: { org: "fixture", team: "dev" },
  rowNames: { primary: name, secondary: null, full: name },
});

test("a pane renders its fixtures, follows the host's lifecycle and calls verbs", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, {
    sessions: [agent("ada", "Ada"), agent("grace", "Grace")],
    inbox: [
      inboxFixtureRow(1, "Hello", { unread: true, createdAtMs: 2 }),
      inboxFixtureRow(2, "Done", { unread: false, createdAtMs: 1 }),
    ],
    intent: { note: "Opened from a test" },
    theme: { scheme: "light" },
  });
  const { frame } = pane;
  await expect(frame.locator("#agents li")).toHaveText(["AdaStop", "GraceStop"]);
  await expect(frame.getByRole("button", { name: "Stop Ada" })).toBeEnabled();
  await expect(frame.locator("#intent")).toHaveText("Opened from a test");
  await expect(frame.locator("#greeting")).toHaveText("Loaded lib/greeting.js");
  await expect(frame.locator("#count")).toHaveText("2 sessions");
  await expect(frame.locator("#mail")).toHaveText("2 messages, 1 unread");
  const font = await frame.evaluate(async () => {
    const response = await fetch("/kit/v1/fonts/geist.woff2");
    return [response.status, response.headers.get("content-type")];
  });
  expect(font).toEqual([200, "font/woff2"]);
  const face = await frame.evaluate(async () => {
    const response = await fetch("/kit/v1/avatars/builtin-chess/king");
    return [response.status, response.headers.get("content-type")];
  });
  expect(face).toEqual([200, "image/svg+xml"]);
  const scheme = () =>
    frame.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue("--cs-color-scheme").trim(),
    );
  await expect.poll(scheme).toBe("light");
  await pane.setTheme({ scheme: "dark" });
  await expect.poll(scheme).toBe("dark");

  await pane.upsert("sessions", agent("grace", "Grace Hopper"));
  await expect(frame.locator("#agents li")).toHaveText(["AdaStop", "Grace HopperStop"]);
  await pane.upsert("sessions", agent("linus", "Linus"));
  await expect(frame.locator("#count")).toHaveText("3 sessions");

  await frame.getByRole("button", { name: "Grace Hopper", exact: true }).click();
  await expect
    .poll(() => pane.verbs())
    .toContainEqual({ verb: "gotoAgent", args: { agentId: "grace" } });

  await pane.show(false);
  await expect(frame.locator("body")).toHaveAttribute("data-visible", "false");
  await pane.show(true);
  await expect(frame.locator("body")).toHaveAttribute("data-visible", "true");

  await pane.flush();
  await expect(frame.locator("#saved")).toBeVisible();
});

test("a narrowed grant reports its verbs unavailable", async ({ mountPane }) => {
  const pane = await mountPane(PANE, {
    sessions: [agent("ada", "Ada")],
    effectiveGrant: ["sessions:read"],
  });
  await expect(pane.frame.getByRole("button", { name: "Stop Ada" })).toBeDisabled();
  await expect(pane.frame.getByRole("button", { name: "Ada", exact: true })).toBeEnabled();
});
