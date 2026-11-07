// SPDX-License-Identifier: MIT
import { dirname, join } from "node:path";
import { test, expect } from "../pane-harness/harness";
import { agentRow } from "./checks";
import { writeTrailFixture } from "../../scripts/story-trail-conformance.mjs";
const path =
  process.env.STORY_TRAIL_TEST_FIXTURE ??
  writeTrailFixture("/tmp/cde19-conformance/browser-trail.html");
const artifact = (name: string) => join(dirname(path), name);
test("story hash, tree keyboard, grouping, focus return and theme captures", async ({
  mountPane,
}) => {
  const pane = await mountPane(path, {
    width: 1280,
    height: 800,
    sessions: [
      agentRow("lead", "riverside-lead"),
      agentRow("build", "dev-worker"),
      agentRow("review", "dev-reviewer"),
    ],
  });
  const frame = pane.frame;
  await expect(frame.locator("h1")).toHaveText("A story");
  await expect(frame.locator("[data-session]")).toHaveCount(3);
  await frame.locator("[role=treeitem]").first().focus();
  await frame.locator("[role=treeitem]").first().press("ArrowRight");
  await frame.locator('[data-group="task"]').click();
  await expect(frame.locator('[data-group="task"]')).toHaveAttribute("aria-pressed", "true");
  await frame.locator('[data-group="time"]').click();
  const opener = frame.locator("[data-open]").first();
  const id = await opener.getAttribute("data-open");
  await opener.click();
  await expect(frame.locator("#drill-panel")).toBeVisible();
  await frame.locator("[data-close]").press("Escape");
  await expect(frame.locator("#drill-panel")).toBeHidden();
  await expect(frame.locator(`[data-open="${id}"]`)).toBeFocused();
  for (const scheme of ["light", "dark"] as const) {
    await pane.setTheme({ id: "codesense", scheme });
    await expect
      .poll(() => frame.evaluate(() => getComputedStyle(document.documentElement).colorScheme))
      .toBe(scheme);
    await frame.locator("html").screenshot({ path: artifact(`trail-${scheme}.png`) });
  }
  await pane.page.setViewportSize({ width: 700, height: 800 });
  await frame.locator("html").screenshot({ path: artifact("trail-narrow.png") });
  await pane.close();
});

test("cross-origin file fragments keep the document and reopen the same story route", async ({
  mountPane,
}) => {
  const pane = await mountPane(path);
  const frame = pane.frame;
  await expect(frame.locator("h1")).toHaveText("A story");
  const boot = await frame.evaluate(() => (window as any).__harness.boot);
  const element = await frame.frameElement();
  const base = (await element.getAttribute("src"))!.split("#")[0];
  await element.evaluate((node: HTMLIFrameElement, url) => {
    node.contentWindow!.location.href = url;
    if (node.getAttribute("src") !== url) node.src = url;
  }, base + "#session/lead/turn/37?source=lines%203%E2%80%934%20of%20app.ts");
  await expect(frame.locator("#turn-37")).toHaveClass(/highlight/);
  await expect(frame.locator(".rd")).toHaveText("[redacted]");
  await expect(frame.locator(".edit-chip")).toContainText("src/app.ts");
  expect(await frame.evaluate(() => (window as any).__harness.boot)).toBe(boot);
  await frame.locator('[data-conv-action="earlier"]').click();
  await expect(frame.locator(".turn").first()).toHaveAttribute("id", "turn-0");
  expect(await frame.evaluate(() => location.hash)).toBe("#session/lead?from=0");
  await element.evaluate((node: HTMLIFrameElement, url) => {
    node.contentWindow!.location.href = url;
    if (node.getAttribute("src") !== url) node.src = url;
  }, base + "#session/lead/turn/37?source=lines%203%E2%80%934%20of%20app.ts");
  await expect(frame.locator("#turn-37")).toHaveClass(/highlight/);
  // Reopening a turn while its fragment is already current must reveal it again.
  await frame.locator(".thread").evaluate((node) => {
    node.scrollTop = 0;
  });
  const sameHash = await frame.evaluate(() => location.hash),
    token = await frame.evaluate(() => (window as any).__codesenseProxy.token);
  await element.evaluate(
    (node: HTMLIFrameElement, { hash, token }) =>
      node.contentWindow!.postMessage(
        { type: "codesense:navigate", hash, token, nonce: 1 },
        new URL(node.src).origin,
      ),
    { hash: sameHash, token },
  );
  await expect
    .poll(() =>
      frame.locator("#turn-37 .turn-head").evaluate((node) => {
        const rect = node.getBoundingClientRect(),
          thread = node.closest(".thread")!.getBoundingClientRect();
        return rect.top >= thread.top && rect.bottom <= thread.bottom;
      }),
    )
    .toBe(true);
  expect(await frame.evaluate(() => (window as any).__harness.boot)).toBe(boot);
  const historyLength = await frame.evaluate(() => history.length);
  await frame.locator('[data-jump="38"]').click();
  await expect(frame.locator("#turn-38")).toHaveClass(/highlight/);
  expect(await frame.evaluate(() => history.length)).toBe(historyLength + 1);
  expect(await frame.evaluate(() => location.hash)).toBe("#session/lead?from=20&turn=38");
  await element.evaluate((node: HTMLIFrameElement, url) => {
    node.contentWindow!.location.href = url;
    if (node.getAttribute("src") !== url) node.src = url;
  }, base + "#story/SV-X");
  await expect(frame.locator("h1")).toHaveText("A story");
  expect(await frame.evaluate(() => (window as any).__harness.boot)).toBe(boot);
  await pane.close();
});

test("Retry pricing restores the row and Tab continues within it", async ({ mountPane }) => {
  const pane = await mountPane(path, { sessions: [agentRow("lead", "builder")] });
  const frame = pane.frame;
  await expect(frame.locator('[data-session="lead"]')).toBeVisible();
  await frame.evaluate(() => {
    const cs = (window as any).codesense,
      read = cs.storyChanges;
    let priced = false;
    cs.storyChanges = async () => {
      const data = await read();
      if (!priced) {
        data.sessions[0].cost = { state: "absent", totalUsd: null, reason: "unpriced" };
        data.sessions[0].models[0].costUsd = null;
      }
      return data;
    };
    cs.setModelRate = async () => 0;
    (window as any).priceNow = () => {
      priced = true;
    };
    window.dispatchEvent(
      new MessageEvent("message", {
        source: window.parent,
        data: { type: "codesense:agent-activity-changed" },
      }),
    );
  });
  await frame.locator('[data-session="lead"] [data-rate]').click();
  await frame.locator("[name=inputPerMtok]").fill("1");
  await frame.locator("[name=outputPerMtok]").fill("1");
  await frame.locator("[type=submit]").click();
  await frame.evaluate(() => (window as any).priceNow());
  await frame.locator("[data-retry]").focus();
  await frame.locator("[data-retry]").press("Enter");
  await expect(frame.locator('[data-open="lead"]')).toBeFocused();
  // macOS WebKit uses Option-Tab to include native buttons in keyboard navigation.
  await pane.page.keyboard.press("Alt+Tab");
  await expect(frame.locator('[data-task="story:SV-X"]')).not.toBeFocused();
  await pane.close();
});

test("focus intents survive every trail control refresh and continue keyboard navigation", async ({
  mountPane,
}) => {
  for (const kind of ["rate", "save", "cancel", "retry", "tree", "sort", "group", "details"]) {
    const pane = await mountPane(path, { sessions: [agentRow("lead", "builder")] });
    const frame = pane.frame;
    await expect(frame.locator('[data-session="lead"]')).toBeVisible();
    await frame.evaluate(() => {
      const cs = (window as any).codesense,
        read = cs.storyChanges;
      let priced = false,
        empty = false;
      cs.storyChanges = async () => {
        const d = await read();
        if (empty) d.sessions = [];
        else if (!priced) {
          d.sessions[0].cost = { state: "absent", totalUsd: null, reason: "unpriced" };
          d.sessions[0].models[0].costUsd = null;
        }
        return d;
      };
      cs.setModelRate = async () => 0;
      (window as any).focusRefresh = (remove = false) => {
        priced = true;
        empty = remove;
        window.dispatchEvent(
          new MessageEvent("message", {
            source: window.parent,
            data: { type: "codesense:agent-activity-changed" },
          }),
        );
      };
      window.dispatchEvent(
        new MessageEvent("message", {
          source: window.parent,
          data: { type: "codesense:agent-activity-changed" },
        }),
      );
    });
    await expect(frame.locator('[data-session="lead"] [data-rate]')).toBeVisible();
    if (["save", "cancel", "retry"].includes(kind)) {
      await frame.locator('[data-session="lead"] [data-rate]').click();
      await frame.locator("[name=inputPerMtok]").fill("1");
      await frame.locator("[name=outputPerMtok]").fill("1");
      if (kind === "retry") await frame.locator("[type=submit]").click();
    }
    const selector = (
      {
        rate: '[data-session="lead"] [data-rate]',
        save: "[type=submit]",
        cancel: "[data-cancel-rate]",
        retry: "[data-retry]",
        tree: '[data-task="story:SV-X"]',
        sort: '[data-sort="cost"]',
        group: '[data-group="task"]',
        details: '[data-open="lead"]',
      } as Record<string, string>
    )[kind];
    await frame.locator(selector).focus();
    await frame.evaluate((remove) => (window as any).focusRefresh(remove), kind === "details");
    await expect(frame.locator("[data-rate]")).toHaveCount(0);
    if (["save", "cancel"].includes(kind)) await frame.locator(selector).press("Enter");
    if (kind === "tree") await frame.locator(selector).press("Enter");
    await expect.poll(() => frame.evaluate(() => document.activeElement?.tagName)).not.toBe("BODY");
    await pane.page.keyboard.press("Alt+Tab");
    const hostInFrame = await pane.page.evaluate(
      () => document.activeElement?.tagName === "IFRAME",
    );
    if (hostInFrame)
      expect(await frame.evaluate(() => document.activeElement?.tagName), kind).not.toBe("BODY");
    await expect(frame.locator('[data-task="story:SV-X"]')).not.toBeFocused();
    await pane.close();
  }
});

test("session panel and conversation light and dark captures", async ({ mountPane }) => {
  for (const view of ["panel", "conversation"] as const) {
    const route =
      view === "panel" ? "#story/SV-X" : "#session/lead/turn/37?source=lines%203-4%20of%20app.ts";
    const fixture = writeTrailFixture(artifact(`capture-${view}.html`), {
      route,
      panel: view === "panel",
    });
    const pane = await mountPane(fixture, {
      width: 1280,
      height: 800,
      sessions: [agentRow("lead", "riverside-lead")],
    });
    const frame = pane.frame;
    await expect(frame.locator(view === "panel" ? "#panel-title" : "#turn-37")).toBeVisible();
    if (view === "panel")
      await expect(frame.locator("#drill-panel")).toContainText("Open conversation");
    for (const scheme of ["light", "dark"] as const) {
      await pane.setTheme({ id: "codesense", scheme });
      await expect
        .poll(() => frame.evaluate(() => getComputedStyle(document.documentElement).colorScheme))
        .toBe(scheme);
      await frame.locator("html").screenshot({ path: artifact(`${view}-${scheme}.png`) });
    }
    await pane.close();
  }
});

test("conversation paging, highlight and tool refresh use the shared focus fallback", async ({
  mountPane,
}) => {
  for (const kind of ["earlier", "later", "unhighlight", "summary"]) {
    const fixture = writeTrailFixture(artifact(`focus-${kind}.html`), {
      route: kind === "later" ? "#session/lead" : "#session/lead/turn/37",
    });
    const pane = await mountPane(fixture, { sessions: [agentRow("lead", "builder")] });
    const frame = pane.frame;
    const selector = kind === "summary" ? "#turn-37 summary" : `[data-conv-action="${kind}"]`;
    await expect(frame.locator(selector)).toBeVisible();
    await frame.locator(selector).focus();
    if (kind === "summary")
      await frame.evaluate(() => {
        const cs = (window as any).codesense,
          read = cs.sessionRead;
        cs.sessionRead = async (args: unknown) => {
          const data = await read(args);
          data.conversation.turns.forEach((turn: any) => {
            turn.tool_calls = [];
          });
          return data;
        };
        window.dispatchEvent(
          new MessageEvent("message", {
            source: window.parent,
            data: { type: "codesense:agent-activity-changed" },
          }),
        );
      });
    else {
      for (let i = 0; i < 3 && (await frame.locator(selector).count()); i++) {
        const first = await frame.locator(".turn").first().getAttribute("id");
        await frame.locator(selector).press("Enter");
        if (kind === "unhighlight") {
          await expect(frame.locator(selector)).toHaveCount(0);
          break;
        }
        await expect.poll(() => frame.locator(".turn").first().getAttribute("id")).not.toBe(first);
        await expect(frame.locator("[data-conv-action][aria-disabled=true]")).toHaveCount(0);
      }
    }
    await expect(frame.locator(kind === "summary" ? "#turn-37" : "h1")).toBeFocused();
    await pane.page.keyboard.press("Alt+Tab");
    if (await pane.page.evaluate(() => document.activeElement?.tagName === "IFRAME"))
      expect(await frame.evaluate(() => document.activeElement?.tagName)).not.toBe("BODY");
    await pane.close();
  }
});

test("the last of twenty sessions opens its rate editor inside the viewport", async ({
  mountPane,
}) => {
  const pane = await mountPane(path, {
    width: 1000,
    height: 600,
    sessions: Array.from({ length: 20 }, (_, i) => agentRow(`session-${i}`, `Builder ${i}`)),
  });
  const frame = pane.frame;
  await expect(frame.locator("[data-session]")).toHaveCount(20);
  await frame.evaluate(() => {
    const cs = (window as any).codesense,
      read = cs.storyChanges;
    cs.storyChanges = async () => {
      const data = await read();
      data.sessions.forEach((s: any) => {
        s.cost = { state: "absent", totalUsd: null, reason: "unpriced" };
        s.models[0].costUsd = null;
      });
      return data;
    };
    window.dispatchEvent(
      new MessageEvent("message", {
        source: window.parent,
        data: { type: "codesense:agent-activity-changed" },
      }),
    );
  });
  const opener = frame.locator("[data-session]").last().locator("[data-rate]");
  await opener.click();
  const editor = frame.locator("#rate-form [name=modelPrefix]");
  await expect(editor).toBeFocused();
  await expect(editor).toBeInViewport();
  await pane.close();
});

test("a live refresh preserves the independently scrolled turn rail", async ({ mountPane }) => {
  const fixture = writeTrailFixture(artifact("rail-scroll.html"), { route: "#session/lead" });
  const pane = await mountPane(fixture, { height: 350, sessions: [agentRow("lead", "Builder")] });
  const frame = pane.frame;
  await expect(frame.locator("#turn-0")).toBeVisible();
  const saved = await frame.evaluate(() => {
    const rail = document.querySelector<HTMLElement>(".turn-rail")!;
    rail.scrollTop = 70;
    (window as any).oldRail = rail;
    return rail.scrollTop;
  });
  expect(saved).toBeGreaterThan(0);
  await pane.upsert("sessions", agentRow("lead", "Builder changed"));
  await expect(frame.locator(".conversation-head h1")).toHaveText("Builder changed");
  expect(
    await frame
      .locator(".turn-rail")
      .evaluate((el) => el === (window as any).oldRail && el.isConnected),
  ).toBe(true);
  expect(await frame.locator(".turn-rail").evaluate((el) => el.scrollTop)).toBe(saved);
  await pane.close();
});

for (const count of [35, 50])
  test(`${count} expanded tasks reveal their own row after End then Home`, async ({
    mountPane,
  }) => {
    const pane = await mountPane(path, {
      width: 1280,
      height: 800,
      sessions: [agentRow("lead", "builder")],
    });
    const frame = pane.frame;
    await expect(frame.locator("[data-session]")).toHaveCount(1);
    await frame.evaluate((count) => {
      const cs = (window as any).codesense,
        read = cs.storyChanges;
      cs.storyChanges = async () => {
        const data = await read();
        data.tasks = [
          data.tasks[0],
          ...Array.from({ length: count }, (_, i) => ({
            ...data.tasks[1],
            taskId: `child-${i}`,
            parentId: data.tasks[0].taskId,
            goal: `Build task ${i}`,
          })),
        ];
        return data;
      };
      window.dispatchEvent(
        new MessageEvent("message", {
          source: window.parent,
          data: { type: "codesense:agent-activity-changed" },
        }),
      );
    }, count);
    await expect(frame.locator("[role=treeitem]")).toHaveCount(count + 1);
    const root = frame.locator('[data-task="story:SV-X"]'),
      last = frame.locator(`[data-task="child-${count - 1}"]`);
    await root.locator(":scope > .tr .tg").click();
    await root.press("ArrowRight");
    await root.press("End");
    await expect(last).toBeFocused();
    await last.press("Home");
    await expect(root).toBeFocused();
    // WebKit rounds scroll offsets to device pixels; allow one CSS pixel at the edge.
    await expect
      .poll(() =>
        root.locator(":scope > .tr .tg").evaluate((el) => {
          const r = el.getBoundingClientRect();
          return r.top >= -1 && r.bottom <= innerHeight + 1;
        }),
      )
      .toBe(true);
    await pane.close();
  });

const propertySelectors: Record<string, string> = {
  sort: '[data-sort="cost"]',
  group: '[data-group="task"]',
  flat: "[data-flat]",
  tree: '[data-task="lead"]',
  details: '[data-open="lead"]',
  rate: '[data-session="lead"] [data-rate]',
  taskRate: '[data-task="lead"] [data-rate]',
  text: "[name=modelPrefix]",
  date: "[name=effectiveFrom]",
  number: "[name=inputPerMtok]",
  save: "[type=submit]",
  cancel: "[data-cancel-rate]",
  retry: "[data-retry]",
  close: "[data-close]",
  link: "#drill-panel [data-panel-key=conversation]",
};
for (const [kind, selector] of Object.entries(propertySelectors))
  test(`guardian property ${kind}: every lifecycle transition`, async ({ mountPane }) => {
    test.setTimeout(60_000);
    const failures: string[] = [];
    for (const change of ["remove", "disabled", "busy", "hidden", "render", "pending"]) {
      const pane = await mountPane(path, {
        width: 1280,
        height: 800,
        sessions: [agentRow("lead", "Builder")],
      });
      const frame = pane.frame;
      await expect(frame.locator('[data-open="lead"]')).toBeVisible();
      await frame.evaluate(() => {
        const cs = (window as any).codesense,
          read = cs.storyChanges;
        cs.storyChanges = async () => {
          const data = await read();
          for (const s of data.sessions) {
            s.tasks = [data.tasks[1]];
            s.cost = { state: "absent", totalUsd: null, reason: "unpriced" };
            s.models[0].costUsd = null;
          }
          return data;
        };
        cs.setModelRate = async () => 0;
        window.dispatchEvent(
          new MessageEvent("message", {
            source: window.parent,
            data: { type: "codesense:agent-activity-changed" },
          }),
        );
      });
      await expect(frame.locator('[data-session="lead"] [data-rate]')).toBeVisible();
      if (["text", "date", "number", "save", "cancel", "retry"].includes(kind)) {
        await frame.locator('[data-session="lead"] [data-rate]').click();
        await frame.locator("[name=inputPerMtok]").fill("1");
        await frame.locator("[name=outputPerMtok]").fill("1");
        if (kind === "retry") await frame.locator("[type=submit]").click();
      }
      if (["close", "link"].includes(kind)) await frame.locator('[data-open="lead"]').click();
      await expect(frame.locator(selector)).toHaveCount(1);
      await frame.locator(selector).focus();
      if (change === "pending" && kind === "save") {
        await frame.evaluate(() => {
          const w = window as any;
          w.saveCount = 0;
          w.codesense.setModelRate = () => {
            w.saveCount++;
            return new Promise((_, reject) => {
              w.rejectSave = reject;
            });
          };
        });
        await frame.locator(selector).press("Enter");
      } else if (change === "pending") {
        await frame.evaluate(() => {
          const w = window as any,
            cs = w.codesense,
            read = cs.storyChanges;
          cs.storyChanges = () =>
            new Promise((resolve) => {
              w.releaseRead = async () => resolve(await read());
            });
          window.dispatchEvent(
            new MessageEvent("message", {
              source: window.parent,
              data: { type: "codesense:agent-activity-changed" },
            }),
          );
        });
      } else if (change === "render")
        await frame.evaluate(() =>
          window.dispatchEvent(
            new MessageEvent("message", {
              source: window.parent,
              data: { type: "codesense:agent-activity-changed" },
            }),
          ),
        );
      else
        await frame.locator(selector).evaluate((el, change) => {
          if (change === "remove") el.remove();
          if (change === "hidden") (el as HTMLElement).hidden = true;
          if (change === "disabled") {
            if ("disabled" in el) (el as HTMLButtonElement).disabled = true;
            else el.setAttribute("aria-disabled", "true");
          }
          if (change === "busy") el.setAttribute("aria-disabled", "true");
        }, change);
      await frame.evaluate(
        () =>
          new Promise<void>((resolve) =>
            requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
          ),
      );
      if (await frame.evaluate(() => document.activeElement === document.body))
        failures.push(`${kind}/${change}: BODY`);
      if (change === "pending") {
        if (kind === "save") {
          await frame.locator(selector).press("Enter");
          if ((await frame.evaluate(() => (window as any).saveCount)) !== 1)
            failures.push("Save accepted a duplicate busy submission");
          await frame.evaluate(() => (window as any).rejectSave(new Error("Rate save failed")));
        } else await frame.evaluate(() => (window as any).releaseRead?.());
        await frame.evaluate(
          () =>
            new Promise<void>((resolve) =>
              requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
            ),
        );
        if (await frame.evaluate(() => document.activeElement === document.body))
          failures.push(`${kind}/${change}: BODY after settlement`);
      }
      await pane.close();
    }
    expect(failures).toEqual([]);
  });

test("a burst of fifty child mutations causes only one guardian restore", async ({ mountPane }) => {
  const pane = await mountPane(path, { sessions: [agentRow("lead", "Builder")] });
  const frame = pane.frame;
  await expect(frame.locator('[data-sort="cost"]')).toBeVisible();
  await pane.focus();
  await expect(frame.locator("[data-flat]")).toBeFocused();
  await frame.locator('[data-sort="cost"]').focus();
  await frame.evaluate(() => {
    const w = window as any,
      original = HTMLElement.prototype.focus;
    w.guardianRestores = 0;
    HTMLElement.prototype.focus = function (options) {
      if (this.id === "sessions-title") w.guardianRestores++;
      original.call(this, options);
    };
    document.querySelector('[data-sort="cost"]')!.remove();
    for (let i = 0; i < 50; i++)
      document.querySelector("main")!.append(document.createElement("span"));
  });
  await frame.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  await expect(frame.locator("#sessions-title")).toBeFocused();
  expect(await frame.evaluate(() => (window as any).guardianRestores)).toBe(1);
  await pane.close();
});

for (const selectionAfterClick of [false, true])
  test(`deliberate prose blur preserves user intent; selection=${selectionAfterClick}`, async ({
    mountPane,
  }) => {
    const pane = await mountPane(path, { sessions: [agentRow("lead", "builder")] });
    const frame = pane.frame;
    await expect(frame.locator("[data-session]")).toHaveCount(1);
    await frame.evaluate(() => {
      const cs = (window as any).codesense,
        read = cs.storyChanges;
      cs.storyChanges = async () => {
        const data = await read();
        data.sessions[0].cost = { state: "absent", totalUsd: null, reason: "unpriced" };
        data.sessions[0].models[0].costUsd = null;
        return data;
      };
      window.dispatchEvent(
        new MessageEvent("message", {
          source: window.parent,
          data: { type: "codesense:agent-activity-changed" },
        }),
      );
    });
    await frame.locator("[data-session] [data-rate]").click();
    const input = frame.locator("[name=inputPerMtok]");
    await input.fill("2");
    await input.focus();
    await frame.locator(".intro").click();
    await frame.evaluate(
      () =>
        new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        ),
    );
    if (selectionAfterClick) {
      await frame.locator(".intro").selectText();
      const selected = await frame.evaluate(() => {
        (window as any).selectedIntro = document.querySelector(".intro");
        return getSelection()?.toString();
      });
      expect(selected).toBeTruthy();
      await frame.evaluate(() => {
        const w = window as any,
          read = w.codesense.storyChanges;
        w.selectionReads = 0;
        w.codesense.storyChanges = async () => {
          const data = await read();
          w.selectionReads++;
          return data;
        };
        window.dispatchEvent(
          new MessageEvent("message", {
            source: window.parent,
            data: { type: "codesense:agent-activity-changed" },
          }),
        );
      });
      await expect
        .poll(() => frame.evaluate(() => (window as any).selectionReads))
        .toBeGreaterThan(0);
      await frame.evaluate(
        () =>
          new Promise<void>((resolve) =>
            requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
          ),
      );
      expect(await frame.evaluate(() => (window as any).selectedIntro.isConnected)).toBe(true);
      expect(await frame.evaluate(() => getSelection()?.toString())).toBe(selected);
    } else await expect(input).not.toBeFocused();
    await pane.close();
  });

async function unpricedRate(frame: any, model = "gpt-model") {
  await frame.evaluate((model) => {
    const cs = (window as any).codesense,
      read = cs.storyChanges;
    cs.storyChanges = async () => {
      const data = await read();
      data.sessions[0].cost = { state: "absent", totalUsd: null, reason: "unpriced" };
      data.sessions[0].models[0].model = model;
      data.sessions[0].models[0].costUsd = null;
      return data;
    };
    window.dispatchEvent(
      new MessageEvent("message", {
        source: window.parent,
        data: { type: "codesense:agent-activity-changed" },
      }),
    );
  }, model);
  await frame.locator("[data-session=lead] [data-rate]").click();
  await frame.locator("[name=inputPerMtok]").fill("1");
  await frame.locator("[name=outputPerMtok]").fill("2");
}
for (const destination of ["host", "other", "opener"])
  test(`delayed save completion respects focus at ${destination}`, async ({ mountPane }) => {
    const pane = await mountPane(path, { sessions: [agentRow("lead", "Builder")] });
    const frame = pane.frame;
    await expect(frame.locator("[data-open=lead]")).toBeVisible();
    await unpricedRate(frame);
    await frame.evaluate(() => {
      (window as any).codesense.setModelRate = () =>
        new Promise((resolve) => {
          (window as any).completeSave = resolve;
        });
    });
    const save = frame.locator("[type=submit]");
    await save.focus();
    await save.press("Enter");
    await expect(save).toHaveAttribute("aria-disabled", "true");
    if (destination === "host") {
      await frame.locator("[data-flat]").focus();
      await frame.locator("[data-flat]").press("Alt+Shift+Tab");
      await expect(pane.page.locator("#terminal")).toBeFocused();
    }
    if (destination === "other") await frame.locator("[data-flat]").focus();
    await frame.evaluate(() => (window as any).completeSave(1));
    await expect(frame.locator("#rate-form")).toHaveCount(0);
    if (destination === "host") await expect(pane.page.locator("#terminal")).toBeFocused();
    else
      await expect(
        frame.locator(destination === "other" ? "[data-flat]" : "[data-session=lead] [data-rate]"),
      ).toBeFocused();
    await pane.close();
  });
for (const [model, prefix] of [
  ["openrouter/qwen3.7-plus", "qwen3-7-plus"],
  ["gpt-model", "gpt-model"],
  ["provider/BAD", "BAD"],
])
  test(`rate prefix follows engine model key: ${model}`, async ({ mountPane }) => {
    const pane = await mountPane(path, { sessions: [agentRow("lead", "Builder")] });
    const frame = pane.frame;
    await expect(frame.locator("[data-open=lead]")).toBeVisible();
    await unpricedRate(frame, model);
    await expect(frame.locator("[name=modelPrefix]")).toHaveValue(prefix);
    await frame.evaluate(() => {
      (window as any).codesense.setModelRate = async (args) => {
        (window as any).savedPrefix = args.modelPrefix;
        if (args.modelPrefix === "BAD")
          throw new Error(
            "invalid: model prefix must be at least 6 lowercase letters, digits, dots and dashes, and not start or end in a separator",
          );
        return 0;
      };
    });
    await frame.locator("[type=submit]").click();
    await expect.poll(() => frame.evaluate(() => (window as any).savedPrefix)).toBe(prefix);
    if (prefix === "BAD")
      await expect(frame.locator("#rate-error")).toContainText("invalid: model prefix");
    await pane.close();
  });

for (const view of ["panel", "conversation"])
  for (const destination of ["host", "other"])
    test(`async ${view} read preserves newer ${destination} focus`, async ({ mountPane }) => {
      const fixture = writeTrailFixture(artifact(`${view}-${destination}.html`), {
        route: view === "panel" ? "#story/SV-X" : "#session/lead/turn/37",
      });
      const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] });
      const frame = pane.frame;
      await expect(frame.locator(view === "panel" ? "[data-open=lead]" : "#turn-37")).toBeVisible();
      await frame.evaluate(() => {
        const cs = (window as any).codesense,
          read = cs.sessionRead;
        cs.sessionRead = (args) =>
          new Promise((resolve) => {
            (window as any).completeRead = async () => {
              const result = await read(args);
              result.conversation.nextCursor = null;
              resolve(result);
            };
          });
      });
      const start = frame.locator(
        view === "panel" ? "[data-open=lead]" : "[data-conv-action=earlier]",
      );
      await start.focus();
      await start.press("Enter");
      await expect
        .poll(() => frame.evaluate(() => typeof (window as any).completeRead))
        .toBe("function");
      const other = frame.locator(view === "panel" ? "[data-flat]" : "[data-conv-action=back]");
      if (destination === "host") await pane.page.locator("#terminal").click();
      else await other.focus();
      await frame.evaluate(() => (window as any).completeRead());
      await expect(
        frame.locator(view === "panel" ? "#drill-panel [data-panel-key=conversation]" : "#turn-0"),
      ).toHaveCount(1);
      if (destination === "host") await expect(pane.page.locator("#terminal")).toBeFocused();
      else await expect(other).toBeFocused();
      await pane.close();
    });

test("panel Escape closes to its opener and the next unhandled Escape reaches the host", async ({
  mountPane,
}) => {
  const pane = await mountPane(path, { sessions: [agentRow("lead", "Builder")] }),
    frame = pane.frame;
  await frame.locator("[data-open=lead]").click();
  await expect(frame.locator("#panel-title")).toBeVisible();
  await frame.locator("[data-close]").focus();
  await pane.page.keyboard.press("Escape");
  await expect(frame.locator("#drill-panel")).toBeHidden();
  await expect(frame.locator("[data-open=lead]")).toBeFocused();
  await pane.page.keyboard.press("Escape");
  await expect
    .poll(() => pane.page.evaluate(() => document.activeElement?.tagName))
    .not.toBe("IFRAME");
  await pane.close();
});
