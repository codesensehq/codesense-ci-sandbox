// SPDX-License-Identifier: MIT
import { mkdirSync } from "node:fs";
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type MountedPane } from "./harness";
import {
  AGENT_LIST_PANE,
  agentListFixtures,
  agentListCaptureFixtures,
} from "./agent-list-fixtures";

for (const count of [0, 1, 50, 500]) {
  test(`${count} agents in the dock: host order, nested depth and windowed DOM`, async ({
    mountPane,
  }) => {
    const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(count));
    const { frame } = pane;
    await expect(frame.locator("#count")).toHaveText(String(count));
    if (!count) {
      await expect(frame.getByText("No agents running", { exact: true })).toBeVisible();
      await expect(frame.getByRole("link")).toHaveCount(0);
      return;
    }
    await expect(frame.locator('[data-agent-id="s0"]')).toBeVisible();
    await expect(frame.locator(".al-team-head")).toContainText("Fixture org›dev");
    expect(await frame.locator(".al-row").count()).toBeLessThan(24);
    if (count > 2) {
      await expect(frame.locator('[data-agent-id="s2"]')).toHaveAttribute("data-depth", "2");
      await expect(frame.locator('[data-agent-id="s1"] .al-subs')).toHaveText("2");
      await expect(frame.locator('[data-agent-id="s2"] .al-ask')).toHaveText(
        "Waiting on you · Which branch should I use?",
      );
      await frame.getByRole("button", { name: "Collapse Agent 0's crew" }).click();
      const folded = frame.getByRole("img", { name: "2 agents folded, 1 needs you", exact: true });
      await expect(folded).toContainText("+2");
      await expect(folded).toHaveAttribute("title", "2 agents folded, 1 needs you");
      await expect(frame.locator('[data-agent-id="s2"]')).toHaveCount(0);
    }
    if (count === 500) {
      await frame.locator("#list").evaluate((el) => (el.scrollTop = 15000));
      await expect(frame.locator('[data-agent-id="s0"]')).toHaveCount(0);
      expect(await frame.locator(".al-row").count()).toBeLessThan(32);
    }
  });
}

test("one session delta keeps other nodes, focus, selection scroll and order", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(50);
  fixtures.sessions!.forEach((s) => {
    s.startedAtMs = Date.now() - 1000;
  });
  const pane = await mountPane(AGENT_LIST_PANE, fixtures),
    { frame } = pane;
  await expect(frame.locator('[data-agent-id="s3"]')).toBeVisible();
  await frame.locator('[data-agent-id="s3"] .al-main').focus();
  await expect(frame.locator('[data-agent-id="s3"] .al-main')).toBeFocused();
  await frame.evaluate(() => {
    (window as any).before = new Map(
      [...document.querySelectorAll<HTMLElement>(".al-row")].map((el) => [el.dataset.agentId, el]),
    );
  });
  await expect(frame.locator(".al-clock")).toHaveCount(0);
  expect(
    await frame.evaluate(
      () => (window as any).before.get("s0") === document.querySelector('[data-agent-id="s0"]'),
    ),
  ).toBe(true);
  await pane.upsert("sessions", {
    ...fixtures.sessions![3],
    progress: { note: "Working on layout", phase: "dev" },
    rowNames: { primary: "Renamed worker", secondary: "Developer", full: "Renamed worker" },
  });
  await expect(frame.locator('[data-agent-id="s3"] .al-name')).toHaveText("Renamed worker");
  await pane.upsert("agentList", {
    ...fixtures.topics!.agentList.find((r) => r.rowType === "state")!,
    teamNames: { "fixture:dev": { org: "Updated org", team: "dev" } },
    workspaceRollups: {
      "fixture:dev": {
        ws0: { mark: "tool", count: 1, total: 10, summary: "1 running a tool, 9 ready" },
      },
    },
  });
  await expect(frame.locator(".al-team-head")).toContainText("Updated org");
  expect(
    await frame.evaluate(() => {
      const before: Map<string, Element> = (window as any).before;
      return [...document.querySelectorAll<HTMLElement>(".al-row")]
        .filter((el) => el.dataset.agentId !== "s3")
        .every((el) => before.get(el.dataset.agentId!) === el);
    }),
  ).toBe(true);
  await expect(frame.locator('[data-agent-id="s3"] .al-main')).toBeFocused();
  const spacing = await frame.evaluate(() => {
    const current = document
      .querySelector('[data-agent-id="s3"]')!
      .closest("[data-row-id]")!
      .getBoundingClientRect();
    const next = document
      .querySelector('[data-agent-id="s4"]')!
      .closest("[data-row-id]")!
      .getBoundingClientRect();
    return next.top - current.bottom;
  });
  expect(spacing).toBe(0);
  await frame.locator("#list").evaluate((el) => (el.scrollTop = 300));
  const scroll = await frame.locator("#list").evaluate((el) => el.scrollTop);
  await frame.locator('[data-agent-id="s8"] .al-main').click();
  await expect
    .poll(() => pane.verbs())
    .toContainEqual({ verb: "gotoAgent", args: { agentId: "s8" } });
  expect(await frame.locator("#list").evaluate((el) => el.scrollTop)).toBe(scroll);
});

test("shared folds and keyboard browse keep focus; Escape leaves", async ({ mountPane }) => {
  const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(5)),
    { frame } = pane;
  const lead = frame.locator('[data-agent-id="s0"] .al-main');
  await lead.focus();
  await pane.page.keyboard.press("ArrowLeft");
  await expect(
    frame.getByRole("img", { name: "2 agents folded, 1 needs you", exact: true }),
  ).toBeVisible();
  await expect(lead).toBeFocused();
  await pane.page.keyboard.press("ArrowRight");
  await expect(frame.locator('[data-agent-id="s2"]')).toBeVisible();
  await pane.page.keyboard.press("ArrowDown");
  const worker = frame.locator('[data-agent-id="s1"] .al-main');
  await expect(worker).toBeFocused();
  await pane.writeUi({
    arrange: "manager",
    pins: {},
    selected: "s1",
    folds: { "#manager-fold:fixture:dev|agent-1": true },
  });
  await expect(
    frame.getByRole("img", { name: "1 agent folded, 1 needs you", exact: true }),
  ).toBeVisible();
  await expect(frame.locator('[data-agent-id="s1"] .al-main')).toHaveAttribute(
    "aria-current",
    "true",
  );
  await pane.page.keyboard.press("Escape");
  await expect(pane.page.locator("#terminal")).toBeFocused();
});

test("workspace reveals unfold temporarily, scroll only offscreen and never save folds", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(50, "teams");
  const pane = await mountPane(AGENT_LIST_PANE, fixtures),
    { frame } = pane;
  await expect(frame.locator(".al-ws-name").first()).toHaveText("Work 0");
  await pane.writeUi({
    arrange: "teams",
    folds: {
      "#team-fold:fixture:dev": true,
      "#ws-fold:fixture:dev|ws4": true,
      "#ws-fold:fixture:dev|ws0": true,
    },
    pins: {},
    selected: null,
  });
  await expect(frame.locator(".al-row")).toHaveCount(0);
  const writesBefore = await pane.page.evaluate(
    () => (window as any).calls.filter((c: any) => c[0] === "save_app_state").length,
  );
  await pane.sendIntent({
    kind: "reveal-workspace",
    teamKey: "fixture:dev",
    workspaceId: "ws4",
    nonce: 1,
  });
  await expect(frame.locator(".al-ws-name", { hasText: "Work 4" })).toBeVisible();
  const top = await frame.locator("#list").evaluate((el) => el.scrollTop);
  expect(top).toBeGreaterThan(0);
  await pane.sendIntent({
    kind: "reveal-workspace",
    teamKey: "fixture:dev",
    workspaceId: "ws4",
    nonce: 2,
  });
  expect(await frame.locator("#list").evaluate((el) => el.scrollTop)).toBe(top);
  await pane.sendIntent({
    kind: "reveal-workspace",
    teamKey: "fixture:dev",
    workspaceId: "ws0",
    nonce: 3,
  });
  await expect(frame.locator('[data-agent-id="s0"]')).toBeVisible();
  await frame.locator("#list").evaluate((el) => {
    el.scrollTop = 100000;
  });
  await expect(frame.locator(".al-ws-head", { hasText: "Work 4" })).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  const writes = await pane.page.evaluate(() =>
    (window as any).calls.filter((c: any) => c[0] === "save_app_state"),
  );
  expect(writes).toHaveLength(writesBefore);
});

for (const scheme of ["light", "dark"] as const)
  for (const width of [236, 268, 400]) {
    test(`theme and accessible tree at ${width}px ${scheme}`, async ({ mountPane }, testInfo) => {
      const pane = await mountPane(AGENT_LIST_PANE, {
          ...agentListFixtures(8),
          width,
          theme: { scheme },
        }),
        { frame } = pane;
      await expect(frame.locator('[data-agent-id="s0"]')).toBeVisible();
      const overflow = await frame.evaluate(
        () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
      );
      expect(overflow).toBe(false);
      await pane.page.screenshot({ path: testInfo.outputPath(`${scheme}-${width}.png`) });
      const results = await new AxeBuilder({ page: pane.page })
        .include("iframe")
        .withTags(["wcag2a", "wcag2aa"])
        .analyze();
      expect(results.violations).toEqual([]);
      await pane.flush();
    });
  }

test("agent context menu belongs to the host and returns focus", async ({ mountPane }) => {
  const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(5));
  const row = pane.frame.locator('[data-agent-id="s0"] .al-main');
  await expect(pane.page.locator("iframe")).toHaveAttribute("title", "Agents");
  await expect(pane.frame.locator('[data-agent-id="s0"]')).toHaveAttribute("id", /agent-row:/);
  await pane.frame.evaluate(() => {
    window.codesense.onFocusReturn((event) => {
      (window as any).returned = event;
    });
  });
  await row.click({ button: "right" });
  await expect(pane.page.getByRole("menu")).toBeVisible();
  expect(await pane.frame.getByRole("menu").count()).toBe(0);
  await pane.page.keyboard.press("Escape");
  await expect.poll(() => pane.frame.evaluate(() => (window as any).returned)).toBeTruthy();

  await expect(row).toBeFocused();
});

test("via pill reveals the launcher and uses row-click terminal focus", async ({ mountPane }) => {
  const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(50));
  await expect(pane.frame.locator('[data-agent-id="s1"] .al-via')).toBeVisible();
  await pane.frame.locator("#list").evaluate((el) => (el.scrollTop = 100));
  await pane.frame.locator('[data-agent-id="s1"] .al-via').click();
  await expect
    .poll(() => pane.verbs())
    .toContainEqual({ verb: "gotoAgent", args: { agentId: "s0" } });
  await expect(pane.frame.locator('[data-agent-id="s0"]')).toBeVisible();
});

test("focus follows the same control when an agent gains or loses its only worker", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(7),
    pane = await mountPane(AGENT_LIST_PANE, fixtures),
    { frame } = pane;
  const main = frame.locator('[data-agent-id="s3"] .al-main');
  await main.focus();
  const worker = fixtures.topics!.agentList.find((r) => r.id === "r4")!;
  await pane.upsert("agentList", { ...worker, parentId: "r3", parentSource: "chosen" });
  await expect(frame.getByRole("button", { name: "Collapse Agent 3's crew" })).toBeVisible();
  await expect(main).toBeFocused();
  await frame.getByRole("button", { name: "Collapse Agent 3's crew" }).focus();
  await pane.upsert("agentList", worker);
  await expect(frame.getByRole("button", { name: "Collapse Agent 3's crew" })).toHaveCount(0);
  await expect(main).toBeFocused();
});

test("status marks use native shapes, busy tone and offline end-state rules", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(1),
    pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const mark = pane.frame.locator('[data-agent-id="s0"] .al-dot .mark');
  for (const [effectiveStatus, state, shape, tone] of [
    ["running", "running", "thinking", "busy"],
    ["tool-running", "running", "thinking", "busy"],
    ["dropped", "running", "question", "needs"],
    ["failed", "offline", "offline", "idle"],
    ["crashed", "offline", "crashed", "failed"],
    ["done", "offline", "ended", "idle"],
  ]) {
    await pane.upsert("sessions", { ...fixtures.sessions![0], effectiveStatus, state });
    await expect(mark).toHaveAttribute("data-mark", shape);
    await expect(mark).toHaveAttribute("data-tone", tone);
  }
});

test("workspace cards show live state tallies and host stage roles", async ({ mountPane }) => {
  const fixtures = agentListFixtures(10, "teams");
  const refs = fixtures.topics!.agentList.filter((r) => r.sessionRef);
  refs.forEach((r) => {
    r.crewLead = r.id === "r7";
    r.workspace = { ...(r.workspace as object), stageRole: "review", stageLabel: "Checking" };
  });
  const metadata = fixtures.topics!.agentList.find((r) => r.rowType === "state")!;
  metadata.workspaceRollups = {
    "fixture:dev": { ws0: { mark: "offline", count: 2, total: 10, summary: "2 offline, 8 ready" } },
  };
  const pane = await mountPane(AGENT_LIST_PANE, fixtures),
    { frame } = pane;
  const card = frame.locator(".al-ws-card").first();
  await expect(card.locator(".al-ws-tail")).toHaveAttribute(
    "aria-label",
    "10 agents: 1 waiting on you, 9 working",
  );
  await expect(card.locator(".al-ws-tail .al-hot")).toHaveText("?1");
  await expect(card.locator(".al-ws-tail .mark")).toHaveAttribute("data-mark", "question");
  const ref = refs[0];
  for (const [stageRole, shape] of [
    ["not_started", "empty"],
    ["active", "fill"],
    ["review", "fill"],
    ["done", "fill"],
    ["blocked", "wall"],
    [undefined, "off"],
  ]) {
    await pane.upsert("agentList", {
      ...ref,
      workspace: { ...(ref.workspace as object), stageRole },
    });
    const rail = card.locator(".al-stage-rail");
    await expect(rail).toHaveAttribute("data-rail-role", stageRole || "off");
    if (shape === "wall" || shape === "fill")
      await expect(rail.locator(`[data-rail="${shape}"]`)).toHaveCount(1);
    else if (shape === "off")
      await expect(rail.locator("rect")).toHaveAttribute("stroke-dasharray", "1.5 1.5");
    else await expect(rail.locator("[data-rail]")).toHaveCount(0);
  }
});

test("a stopped row separates its name from its starting prompt", async ({ mountPane }) => {
  const fixtures = agentListFixtures(1);
  fixtures.sessions![0] = {
    ...fixtures.sessions![0],
    effectiveStatus: "done",
    startedWith: "Fix login",
  };
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  await expect(pane.frame.locator(".al-line-title")).toHaveText("Agent 0");
  await expect(pane.frame.locator(".al-main")).toHaveAttribute("title", /Started with: Fix login/);
});

test("empty dock list has a visible keyboard focus ring", async ({ mountPane }) => {
  const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(0));
  await pane.focus();
  const list = pane.frame.locator("#list");
  await expect(list).toBeFocused();
  expect(
    await list.evaluate((el) => ({
      width: getComputedStyle(el).outlineWidth,
      style: getComputedStyle(el).outlineStyle,
    })),
  ).toEqual({ width: "2px", style: "solid" });
});

test("scroll-mounted rows have elapsed and stalled detail in their tooltip", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(100);
  fixtures.sessions![80] = {
    ...fixtures.sessions![80],
    effectiveStatus: "stalled",
    startedAtMs: Date.now() - 5 * 60000 - 1000,
    activityAtMs: Date.now() - 2 * 60000 - 1000,
  };
  const pane = await mountPane(AGENT_LIST_PANE, fixtures),
    { frame } = pane;
  await expect(frame.locator('[data-agent-id="s0"]')).toBeVisible();
  await expect(frame.locator('[data-agent-id="s80"]')).toHaveCount(0);
  const firstText = await frame.evaluate(
    () =>
      new Promise<string>((resolve) => {
        const list = document.getElementById("list")!;
        const observer = new MutationObserver(() => {
          const row = list.querySelector('[data-agent-id="s80"]');
          if (!row) return;
          observer.disconnect();
          resolve(row.querySelector(".al-main")!.getAttribute("title") || "");
        });
        observer.observe(list, { childList: true, subtree: true });
        list.scrollTop = 4000;
      }),
  );
  expect(firstText).toContain("5m");
  expect(firstText).toContain("Last activity 2m ago");
});

async function postCommand(pane: MountedPane, id: string) {
  await pane.page.locator("iframe").evaluate((el: HTMLIFrameElement, command: string) => {
    el.contentWindow!.postMessage({ type: "codesense:command", id: command }, "*");
  }, id);
}

test("commands queue before ready, browse folded needs-you rows and open the host row menu", async ({
  mountPane,
  page,
}) => {
  await page.addInitScript(() => {
    let posted = false;
    window.addEventListener("message", (event) => {
      if (
        !posted &&
        event.data?.type === "codesense:invoke" &&
        event.data.command === "subscribe"
      ) {
        posted = true;
        (window as any).earlyCommandSent = true;
        (event.source as Window).postMessage(
          { type: "codesense:command", id: "collapse-all" },
          "*",
        );
      }
    });
  });
  const fixtures = agentListFixtures(5);
  fixtures.sessions![4].effectiveStatus = "blocked";
  fixtures.topics!["ui:dock"][0].folds = { "#manager-fold:fixture:dev|agent-0": true };
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const row = (id: string) => pane.frame.locator(`[data-agent-id="${id}"] .al-main`);
  await expect.poll(() => page.evaluate(() => (window as any).earlyCommandSent)).toBe(true);
  await expect(pane.frame.locator(".al-row")).toHaveCount(0);
  await postCommand(pane, "next-needs-you");
  await expect(row("s2")).toBeFocused();
  expect(await pane.verbs()).not.toContainEqual(expect.objectContaining({ verb: "gotoAgent" }));
  await postCommand(pane, "next-needs-you");
  await expect(row("s4")).toBeFocused();
  await postCommand(pane, "next-needs-you");
  await expect(row("s2")).toBeFocused();
  await postCommand(pane, "previous-needs-you");
  await expect(row("s4")).toBeFocused();
  await postCommand(pane, "previous-needs-you");
  await expect(row("s2")).toBeFocused();
  await page.evaluate(() => {
    (window as any).rowMenus = [];
    window.addEventListener("message", (event) => {
      if (event.data?.type === "codesense:contextmenu") (window as any).rowMenus.push(event.data);
    });
  });
  await postCommand(pane, "open-row-menu");
  await expect
    .poll(() => page.evaluate(() => (window as any).rowMenus))
    .toEqual([
      expect.objectContaining({
        type: "codesense:contextmenu",
        target: "row",
        row: expect.objectContaining({ kind: "agent", sessionId: "s2" }),
      }),
    ]);
});

for (const arrange of ["manager", "teams"]) {
  test(`commands focus the current row and persist collapse/expand all in ${arrange} view`, async ({
    mountPane,
  }) => {
    const fixtures = agentListFixtures(50, arrange);
    fixtures.topics!["ui:dock"][0].selected = "s42";
    const pane = await mountPane(AGENT_LIST_PANE, fixtures);
    const current = pane.frame.locator('[data-agent-id="s42"] .al-main');
    await postCommand(pane, "focus-list");
    await expect(current).toBeFocused();
    await postCommand(pane, "collapse-all");
    await expect(pane.frame.locator(".al-row")).toHaveCount(0);
    const folds = () =>
      pane.page.evaluate(() => {
        const saves = (window as any).calls.filter((c: any) => c[0] === "save_app_state");
        const state = JSON.parse(saves.at(-1)[1].state);
        return Object.values(state.liveHubUi)
          .map((v: any) => v.data)
          .find((v: any) => v.folds)?.folds;
      });
    await expect.poll(folds).toMatchObject({ "#team-fold:fixture:dev": true });
    await postCommand(pane, "expand-all");
    await expect
      .poll(async () => Object.values(await folds()).every((v) => v === false))
      .toBe(true);
    await postCommand(pane, "reveal-current");
    await expect(current).toBeFocused();
    await pane.writeUi({ selected: null });
    await postCommand(pane, "focus-list");
    await expect(pane.frame.locator('[data-agent-id="s0"] .al-main')).toBeFocused();
    await pane.page.keyboard.press("Escape");
    await expect(pane.page.locator("#terminal")).toBeFocused();
    await pane.page.keyboard.type("ajkmq");
    await expect(pane.page.locator("#terminal")).toHaveValue("ajkmq");
    expect(await pane.verbs()).not.toContainEqual(expect.objectContaining({ verb: "gotoAgent" }));
  });
}

test("bulk fold commands persist 35 workspaces with one write each", async ({
  mountPane,
  page,
}) => {
  await page.addInitScript(() => {
    (window as any).bulkWrites = [];
    window.addEventListener("message", (event) => {
      if (event.data?.type === "codesense:invoke" && event.data.command === "setUiState")
        (window as any).bulkWrites.push(event.data);
    });
  });
  const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(350, "teams"));
  await expect(pane.frame.locator('[data-agent-id="s0"]')).toBeVisible();
  await postCommand(pane, "collapse-all");
  await expect(pane.frame.locator(".al-row")).toHaveCount(0);
  const writes = () => page.evaluate(() => (window as any).bulkWrites.length);
  const folds = () =>
    page.evaluate(() => {
      const saves = (window as any).calls.filter((c: any) => c[0] === "save_app_state");
      return Object.values(JSON.parse(saves.at(-1)[1].state).liveHubUi)
        .map((v: any) => v.data)
        .find((v: any) => v.folds)?.folds;
    });
  await expect.poll(writes).toBe(1);
  await expect.poll(async () => Object.keys(await folds()).length).toBe(36);
  expect(Object.values(await folds()).every((v) => v === true)).toBe(true);
  await postCommand(pane, "expand-all");
  await expect(pane.frame.locator('[data-agent-id="s0"]')).toBeVisible();
  await expect.poll(writes).toBe(2);
  await expect.poll(async () => Object.values(await folds()).every((v) => v === false)).toBe(true);
});

test("command navigation uses workspace and crew display order, including folded rows", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(5, "teams");
  for (const i of [3, 4]) fixtures.sessions![i].effectiveStatus = "blocked";
  for (const ref of fixtures.topics!.agentList.filter((r) => r.sessionRef)) {
    const i = Number(String(ref.sessionRef).slice(1));
    ref.workspaceRef = i < 3 ? "ws0" : "ws1";
    ref.workspaceRank = i < 3 ? 1 : 0;
    ref.workspace = { id: ref.workspaceRef, label: ref.workspaceRef };
    ref.crewOrder = 4 - i;
  }
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const row = (id: string) => pane.frame.locator(`[data-agent-id="${id}"] .al-main`);
  await expect(pane.frame.locator(".al-row").first()).toHaveAttribute("data-agent-id", "s4");
  await pane.writeUi({
    folds: {
      "#team-fold:fixture:dev": true,
      "#ws-fold:fixture:dev|ws0": true,
      "#ws-fold:fixture:dev|ws1": true,
    },
  });
  await expect(pane.frame.locator(".al-row")).toHaveCount(0);
  await postCommand(pane, "focus-list");
  await expect(row("s4")).toBeFocused();
  for (const [command, id] of [
    ["next-needs-you", "s3"],
    ["next-needs-you", "s2"],
    ["next-needs-you", "s4"],
    ["previous-needs-you", "s2"],
    ["previous-needs-you", "s3"],
    ["previous-needs-you", "s4"],
  ]) {
    await postCommand(pane, command);
    await expect(row(id)).toBeFocused();
  }
});

test("search and arrangement commands use the chrome and shared dock setting without default chords", async ({
  mountPane,
}) => {
  const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(5, "teams")),
    frame = pane.frame;
  const declared = await frame.locator('meta[name="codesense:commands"]').getAttribute("content");
  const commands = JSON.parse(declared!);
  expect(commands.find((command: { id: string }) => command.id === "focus-search")).toEqual({
    id: "focus-search",
    label: "Search agents",
    icon: "search",
    scope: "global",
  });
  expect(commands.find((command: { id: string }) => command.id === "switch-arrangement")).toEqual({
    id: "switch-arrangement",
    label: "Switch arrangement",
    icon: "layers",
    scope: "slot-visible",
  });
  await postCommand(pane, "focus-search");
  await expect(frame.locator("#al-search")).toBeFocused();
  await frame.locator("#al-search").fill("Agent 1");
  await frame.locator("#al-search").press("ArrowDown");
  await expect(frame.locator("#al-search")).toBeFocused();
  await postCommand(pane, "switch-arrangement");
  await expect(frame.locator("#al-arrange")).toHaveValue("manager");
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
    .toMatchObject({ patch: { arrange: "manager" } });
  await postCommand(pane, "switch-arrangement");
  await expect(frame.locator("#al-arrange")).toHaveValue("teams");
  await pane.writeUi({ arrange: "status" });
  await postCommand(pane, "switch-arrangement");
  await expect(frame.locator("#al-arrange")).toHaveValue("teams");
  await frame.locator("#al-history").click();
  await expect(frame.locator("#al-search")).toBeHidden();
  await postCommand(pane, "focus-search");
  await expect(frame.locator("#al-search")).toBeFocused();
  await expect(frame.locator("#al-title")).toHaveText("Agents");
  expect(await pane.verbs()).not.toContainEqual(expect.objectContaining({ verb: "gotoAgent" }));
});

test.describe("parity captures", () => {
  test.skip(({ browserName }) => browserName !== "chromium", "Pair with native Chromium captures");
  test.use({ deviceScaleFactor: 2, viewport: { width: 1440, height: 900 } });
  for (const scheme of ["light", "dark"] as const) {
    test(`parity pane 236px ${scheme}`, async ({ mountPane }) => {
      mkdirSync("design/SV-AGP-5", { recursive: true });
      const pane = await mountPane(AGENT_LIST_PANE, {
        ...agentListCaptureFixtures(),
        width: 236,
        height: 700,
        theme: { id: "codesense", scheme },
      });
      await expect(pane.page.frameLocator("iframe").locator('[data-agent-id="s0"]')).toBeVisible();
      const capture = async (state: string) => {
        await pane.page
          .frameLocator("iframe")
          .locator("body")
          .evaluate(() => {
            if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
          });
        const bounds = (await pane.page.locator("iframe").boundingBox())!;
        expect(bounds.width).toBe(236);
        await pane.page.screenshot({
          animations: "disabled",
          path: `design/SV-AGP-5/pane-${scheme}-${state}.png`,
          clip: bounds,
        });
      };
      await pane.page.mouse.move(1000, 700);
      await capture("default");
      await pane.page.frameLocator("iframe").locator('[data-agent-id="s0"]').hover();
      await capture("hover");
      await pane.page.frameLocator("iframe").locator(".al-ws-head").first().click();
      await expect(pane.page.frameLocator("iframe").locator(".al-ws-head").first()).toHaveAttribute(
        "aria-expanded",
        "false",
      );
      await pane.page.mouse.move(1000, 700);
      await capture("folded");
    });
  }
});
