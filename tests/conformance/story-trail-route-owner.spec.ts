// SPDX-License-Identifier: MIT
import { test, expect } from "../pane-harness/harness";
import { agentRow } from "./checks";
import { writeTrailFixture } from "../../scripts/story-trail-conformance.mjs";
import { dirname, join } from "node:path";
import type { Frame } from "@playwright/test";
async function nativeNavigate(frame: Frame, hash: string) {
  const element = await frame.frameElement(),
    base = (await element.getAttribute("src"))!.split("#")[0];
  const token = await frame.evaluate(() => (window as any).__codesenseProxy.token);
  await element.evaluate(
    (node: HTMLIFrameElement, { url, hash, token }) => {
      node.contentWindow!.location.href = url;
      if (node.getAttribute("src") !== url) node.src = url;
      node.contentWindow!.postMessage(
        { type: "codesense:navigate", hash, token },
        new URL(url).origin,
      );
    },
    { url: base + hash, hash, token },
  );
}
for (const origin of ["conversation", "date", "close"] as const)
  test(`a late passive conversation read after ${origin} cannot replace native story navigation`, async ({
    mountPane,
  }) => {
    const path = writeTrailFixture(
      join(
        dirname(process.env.STORY_TRAIL_TEST_FIXTURE ?? "/tmp/cde19-route-owner/trail.html"),
        `route-owner-${origin}.html`,
      ),
    );
    const pane = await mountPane(path, { sessions: [agentRow("lead", "Builder")] });
    try {
      const frame = pane.frame;
      await expect(frame.locator("h1")).toHaveText("A story");
      if (origin === "date") {
        await frame.evaluate(() => {
          const cs = (window as any).codesense,
            read = cs.storyChanges;
          cs.storyChanges = async (args) => {
            const data = await read(args);
            for (const row of data.sessions) {
              row.cost = { state: "absent", totalUsd: null, reason: "unpriced" };
              for (const model of row.models) model.costUsd = null;
            }
            return data;
          };
        });
        await pane.upsert("sessions", agentRow("lead", "Builder"));
        await frame.locator("[data-session=lead] [data-rate]").click();
        await frame.locator("[name=effectiveFrom]").focus();
      } else if (origin === "close") {
        await frame.locator("[data-open=lead]").click();
        await expect(frame.locator("#drill-panel [data-loading]")).toBeHidden();
        await frame.locator("[data-close]").focus();
      }
      await nativeNavigate(frame, "#session/lead");
      await expect(frame.locator("#turn-0")).toBeAttached();
      await expect.poll(() => frame.evaluate(() => location.hash)).toBe("#session/lead?from=0");
      await frame.evaluate(() => {
        const w = window as any,
          read = w.codesense.sessionRead;
        w.codesense.sessionRead = async (args) => {
          const result = await read(args);
          return new Promise((resolve) => {
            w.releaseRouteRead = () => resolve(result);
          });
        };
      });
      await pane.upsert("sessions", agentRow("lead", "Builder updated"));
      await frame.waitForFunction(() => typeof (window as any).releaseRouteRead === "function");
      // The native fragment traversal releases the already-read reply before hashchange.
      await frame.evaluate(() =>
        addEventListener(
          "popstate",
          () => {
            (window as any).releaseRouteRead();
          },
          { once: true },
        ),
      );
      await nativeNavigate(frame, "#story/SV-X");
      await expect(frame.locator("h1")).toHaveText("A story");
      await expect.poll(() => frame.evaluate(() => location.hash)).toBe("#story/SV-X");
    } finally {
      await pane.close();
    }
  });

test("a late story A read leaves the newer native story B route and focus intact", async ({
  mountPane,
}) => {
  const path = writeTrailFixture(
    join(
      dirname(process.env.STORY_TRAIL_TEST_FIXTURE ?? "/tmp/cde19-route-owner/trail.html"),
      "story-owner.html",
    ),
  );
  const pane = await mountPane(path, { sessions: [agentRow("lead", "Builder")] });
  try {
    const frame = pane.frame;
    await expect(frame.locator("h1")).toHaveText("A story");
    await frame.evaluate(() => {
      const w = window as any,
        read = w.codesense.storyChanges;
      w.codesense.storyChanges = async (args) => {
        const data = await read(args);
        for (const row of data.sessions)
          row.story = { ...row.story, id: args.storyId, title: "Story " + args.storyId };
        if (args.storyId === "SV-A")
          return new Promise((resolve) => {
            w.finishStoryA = () => resolve(data);
          });
        return data;
      };
    });
    await nativeNavigate(frame, "#story/SV-A");
    await frame.waitForFunction(() => typeof (window as any).finishStoryA === "function");
    await nativeNavigate(frame, "#story/SV-B");
    await expect(frame.locator("h1")).toHaveText("Story SV-B");
    await frame.locator("[data-flat]").focus();
    await frame.evaluate(() => {
      (window as any).finishStoryA();
    });
    await frame.evaluate(
      () =>
        new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        ),
    );
    await expect(frame.locator("h1")).toHaveText("Story SV-B");
    await expect.poll(() => frame.evaluate(() => location.hash)).toBe("#story/SV-B");
    await expect(frame.locator("[data-flat]")).toBeFocused();
  } finally {
    await pane.close();
  }
});

for (const action of ["unhighlight", "rail"] as const)
  test(`a retained ${action} during a passive read keeps fresh facts`, async ({ mountPane }) => {
    const path = writeTrailFixture(
      join(
        dirname(process.env.STORY_TRAIL_TEST_FIXTURE ?? "/tmp/cde19-route-owner/trail.html"),
        `retained-${action}.html`,
      ),
    );
    const pane = await mountPane(path, { sessions: [agentRow("lead", "Builder")] });
    try {
      const frame = pane.frame;
      await nativeNavigate(frame, "#session/lead/turn/7");
      await expect(frame.locator("#turn-7")).toHaveClass(/highlight/);
      await expect(frame.locator(".conversation-head h1")).toHaveText("Builder");
      await frame.evaluate(() => {
        const w = window as any,
          read = w.codesense.sessionRead;
        let held = false;
        w.retainedReads = 0;
        w.codesense.sessionRead = async (args) => {
          w.retainedReads++;
          const result = await read(args);
          if (held) return result;
          held = true;
          return new Promise((resolve) => {
            w.finishRetainedRead = () => resolve(result);
          });
        };
      });
      await pane.upsert("sessions", agentRow("lead", "Builder updated"));
      await frame.waitForFunction(() => typeof (window as any).finishRetainedRead === "function");
      await frame
        .locator(action === "unhighlight" ? "[data-conv-action=unhighlight]" : '[data-jump="8"]')
        .click();
      await frame.evaluate(() => {
        (window as any).finishRetainedRead();
      });
      await expect(frame.locator(".conversation-head h1")).toHaveText("Builder updated");
      await expect.poll(() => frame.evaluate(() => (window as any).retainedReads)).toBe(2);
      if (action === "unhighlight") await expect(frame.locator(".turn.highlight")).toHaveCount(0);
      else await expect(frame.locator("#turn-8")).toHaveClass(/highlight/);
      expect(
        await frame
          .locator(".turn")
          .evaluateAll((nodes) => nodes.map((node) => Number(node.id.slice(5)))),
      ).toEqual(Array.from({ length: 20 }, (_, i) => i));
    } finally {
      await pane.close();
    }
  });
