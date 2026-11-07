// SPDX-License-Identifier: MIT
// SV-AGP-9 QA: the built-in agent list in the mock app with a real LiveHub, 50 realistic agents
// (pins, roles, crews, subagents, needs-you, blocked, offline, crashed, ended) while session
// deltas stream every 100ms. Every control must stay hittable: no flicker, no lost hit area.
import { test, expect, type Frame, type Page } from "@playwright/test";
import { AGENT_LIST_PANE } from "../pane-harness/agent-list-fixtures";
import { hitStability } from "../pane-harness/agent-list-hit-stability";
import { capturePaneErrors } from "../pane-harness/agent-list-console";
import { routeAgentListKit, installAgentListPane } from "./agent-list-support";

async function boot(page: Page, arrange: string) {
  const { errors, messages } = capturePaneErrors(page);
  await page.setViewportSize({ width: 1200, height: 900 });
  await routeAgentListKit(page);
  // Wrap the mock's Tauri internals: enrich agentList rows (crews, workspaces, pins), log
  // invokes, and remember delta listeners so the test can stream session deltas itself.
  await page.addInitScript(() => {
    const w = window as any;
    w.__qa = { invokes: [], listeners: [] };
    let internals: any;
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      get: () => internals,
      set: (value) => {
        const invoke = value.invoke;
        value.invoke = async (cmd: string, args: any) => {
          w.__qa.invokes.push([cmd, args?.topic ?? args?.verb ?? args?.event ?? null]);
          const out: any = await invoke(cmd, args);
          if (cmd === "plugin:event|listen" && args?.event === "codesense:delta")
            w.__qa.listeners.push(args.handler);
          if (cmd === "live_hub_subscribe" && args?.topic === "agentList" && w.__qaEnrich)
            out.snapshot = out.snapshot.map(w.__qaEnrich);
          return out;
        };
        w.__qaRun = value.runCallback;
        internals = value;
      },
    });
  });
  const paneId = `scale-${test.info().parallelIndex}`; // own pane id: parallel tests must not share one mock pane
  await installAgentListPane(page, AGENT_LIST_PANE, paneId);
  await page.goto("/?harness=live-hub", { timeout: 120_000 });
  await page.waitForFunction(() => !!(window as any).__liveHubMount);
  await page.evaluate(
    async ({ arrange, id }) => {
      const w = window as any;
      const fixture = w.__liveHubFixture;
      const pins = [8, 15, 1, 30, 12, 40, 22];
      fixture.startAgentPane({ pinned: pins });
      const statuses = ["running", "thinking", "tool-running", "idle", "running"];
      fixture.sessions.forEach((s: any, i: number) => {
        Object.assign(s, {
          effectiveStatus:
            i === 4
              ? "awaiting-answer"
              : i === 5
                ? "blocked"
                : i === 6
                  ? "paused"
                  : i === 7
                    ? "crashed"
                    : i === 9
                      ? "completed"
                      : i === 11
                        ? "stalled"
                        : statuses[i % 5],
          alive: i !== 9,
          needsYouKind: i === 4 ? "question" : i === 6 ? "permission" : null,
          pendingAsk: i === 4 ? "Merge now or wait?" : null,
          subagents: i % 8 === 3 || i === 11 ? 2 : 0,
          state: i === 26 || i === 27 ? "offline" : "running",
          machineId: i === 26 || i === 27 ? "emulator" : "local",
          launchedBy: i % 5 ? `scale-${i - (i % 5)}` : null,
          pinned: pins.includes(i),
          model: i % 2 ? "codex" : "opus",
          rowNames: { primary: `Agent ${i}`, secondary: "Developer", full: `Agent ${i}` },
          ...(i % 9 === 0
            ? { roleHoldings: [{ role: "builder", name: "Builder", label: "Builder" }] }
            : {}),
        });
      });
      w.__qaEnrich = (row: any) => {
        if (row.rowType === "state")
          return {
            ...row,
            pane: {
              ...row.pane,
              workspaceOrder: { "acme/widgets:default": ["ws0", "ws1", "ws2", "ws3", "ws4"] },
            },
          };
        const i = row.order;
        const ws = `ws${Math.floor(i / 10)}`;
        const lead = i - (i % 5);
        return {
          ...row,
          workspaceRef: ws,
          workspace: {
            id: ws,
            label: `Story ${Math.floor(i / 10)}`,
            kind: "story",
            storyStatus: "in-progress",
            stageRole: "active",
            stageLabel: "In progress",
          },
          workspaceRank: Math.floor(i / 10),
          parentId: i % 5 ? JSON.stringify(["acme/widgets:default", `scale-${lead}`]) : null,
          parentSource: i % 5 ? "launcher" : null,
          crewParentId: i % 5 ? JSON.stringify(["acme/widgets:default", `scale-${lead}`]) : null,
          crewLead: i % 5 === 0,
          crewOrder: i,
        };
      };
      const { liveHub } = await import("/src/features/dashboard/lib/live-hub.ts");
      const team = "acme/widgets:default";
      const canonical = {
        [team]: pins.map((i) => ({
          kind: "session",
          teamKey: team,
          handle: `scale-${i}`,
          sessionId: `scale-${i}`,
          name: `Agent ${i}`,
        })),
      };
      await liveHub.writeUi("ui:dock", "all", {
        arrange,
        folds: {},
        pins: canonical,
        search: { query: "", status: "all" },
        hiddenTeams: [],
      });
      w.__liveHubMount(1, true, false, id);
    },
    { arrange, id: paneId },
  );
  await page.addStyleTag({
    content: "main > div {height: 800px !important; width:268px !important}",
  });
  await expect.poll(() => page.frames().filter((f) => f.url().includes("/pane/")).length).toBe(1);
  const frame = page.frames().find((f) => f.url().includes("/pane/"))!;
  await expect(frame.locator("#count")).toHaveText("50");
  return { frame, errors, messages };
}

/** Stream session deltas for 7 sessions every 100ms through the real LiveHub listeners. */
async function stream(page: Page, on: boolean, separate = false) {
  if (on)
    await page.evaluate(async () => {
      const w = window as any;
      if (w.__qaRev !== undefined) return;
      const snap = await w.__TAURI_INTERNALS__.invoke("live_hub_subscribe", {
        topic: "sessions",
        viewKey: "all",
      });
      w.__qaRev = snap.rev;
    });
  await page.evaluate(
    ([on, separate]) => {
      const w = window as any;
      clearInterval(w.__qaTimer);
      if (!on) return;
      // Continue the hub's own sessions rev, so a delta is never a gap (a gap makes the hub resync).
      let rev = w.__qaRev ?? 0,
        i = 0;
      w.__qaTicks = 0;
      const ids = [0, 2, 3, 13, 17, 21, 33];
      w.__qaTimer = setInterval(() => {
        i++;
        const sessions = w.__liveHubFixture.sessions;
        const upserts = ids.map((n, k) => {
          Object.assign(sessions[n], {
            effectiveStatus: ["tool-running", "thinking", "running"][(i + k) % 3],
            progress: { note: `Live step ${i}` },
            heartbeatAtMs: Date.now(),
            activityAtMs: Date.now(),
          });
          return { id: sessions[n].handle, ...sessions[n] };
        });
        // One delta per tick with every change, or (separate) one delta per session change.
        const payloads = separate
          ? upserts.map((row) => ({
              topic: "sessions",
              viewKey: "all",
              rev: ++rev,
              upserts: [row],
              removes: [],
            }))
          : [{ topic: "sessions", viewKey: "all", rev: ++rev, upserts, removes: [] }];
        for (const payload of payloads)
          for (const handler of w.__qa.listeners)
            w.__qaRun(handler, { event: "codesense:delta", id: payload.rev, payload });
        w.__qaRev = rev;
        w.__qaTicks++;
      }, 100);
    },
    [on, separate] as const,
  );
}

const hit = (frame: Frame, selector: string) => hitStability(frame, selector, 2000);

for (const arrange of ["teams", "manager"]) {
  test(`QA live hub ${arrange}: controls stay hittable and rows stay put under 100ms deltas`, async ({
    page,
    browserName,
  }, info) => {
    test.setTimeout(120_000);
    const { frame, errors, messages } = await boot(page, arrange);
    const subscribesBefore = await page.evaluate(
      () => (window as any).__qa.invokes.filter((c: any) => c[0] === "live_hub_subscribe").length,
    );
    await stream(page, true);
    const out: Record<string, any> = {};
    // Node identity across the stream.
    await frame.evaluate(() => {
      const w = window as any;
      w.__ids = new Map();
      w.__rep = 0;
      w.__fr = 0;
      const tick = () => {
        w.__fr++;
        for (const el of document.querySelectorAll<HTMLElement>(".al-item[data-row-id]")) {
          const id = el.dataset.rowId!;
          if (
            w.__ids.has(id) &&
            w.__ids.get(id) !== el &&
            w.__ids.get(id).isConnected === false &&
            document.contains(el)
          )
            w.__rep++;
          w.__ids.set(id, el);
        }
        if (!w.__stopIds) requestAnimationFrame(tick);
      };
      tick();
    });
    const streamed = frame.locator(
      '.al-item:not([data-row-id^="pinned:"]) .al-row[data-agent-id="scale-0"] .al-main',
    );
    await streamed.hover();
    out.quick = await hit(
      frame,
      '.al-item:not([data-row-id^="pinned:"]) .al-row[data-agent-id="scale-0"] .al-quick',
    );
    out.messages = await hit(
      frame,
      '.al-item:not([data-row-id^="pinned:"]) .al-row[data-agent-id="scale-0"] .al-messages',
    );
    out.main = await hit(
      frame,
      '.al-item:not([data-row-id^="pinned:"]) .al-row[data-agent-id="scale-0"] .al-main',
    );
    out.pinnedMain = await hit(frame, '.al-item[data-row-id^="pinned:"] .al-main');
    out.tallyWaiting = await hit(frame, '#al-tally [data-status="needs"]');
    out.tallyWorking = await hit(frame, '#al-tally [data-status="working"]');
    out.filter = await hit(frame, "#al-filter");
    out.arrange = await hit(frame, "#al-arrange");
    out.wsHead = await hit(frame, ".al-ws-head");
    out.more = await hit(frame, '[data-row-id="pinned-more"] button, [data-row-id="pinned-more"]');
    // Row-node replacements during pure streaming (before any filter or click repaints the list).
    out.replacedRowsStreamOnly = await frame.evaluate(() => (window as any).__rep);
    // Clicks land while streaming.
    const before = (await page.evaluate(() => (window as any).__qa.invokes.length)) as number;
    await frame.locator('#al-tally [data-status="needs"]').click();
    await page.waitForTimeout(800);
    out.waitingRows = await frame
      .locator(".al-item:not([data-row-id^='pinned:']) .al-row")
      .evaluateAll((els) =>
        els.map(
          (e: any) =>
            e.hasAttribute("data-needs") || e.querySelector(".al-main")?.dataset.context === "true",
        ),
      );
    out.waitingRowIds = await frame
      .locator(".al-item:not([data-row-id^='pinned:']) .al-row")
      .evaluateAll((els) =>
        els.map(
          (e: any) =>
            `${e.id}${e.hasAttribute("data-needs") ? " needs" : ""}${e.querySelector(".al-main")?.dataset.context ? " context" : ""}`,
        ),
      );
    // Mouse-click focus follows the browser's platform policy; keyboard focus is covered separately.
    if (browserName === "chromium")
      await expect.soft(frame.locator('#al-tally [data-status="needs"]')).toBeFocused();
    await frame.locator('#al-tally [data-status="needs"]').click();
    await frame.locator("#al-filter").click();
    await page.waitForTimeout(1000);
    out.menuOpenAfterStream = await frame.locator("#al-menu").isVisible();
    await page.keyboard.press("Escape");
    await streamed.click();
    await page.waitForTimeout(300);
    out.newInvokes = (
      await page.evaluate((b) => (window as any).__qa.invokes.slice(b), before)
    ).filter(
      (c: any) => !String(c[0]).startsWith("live_hub") && !String(c[0]).startsWith("plugin:event"),
    );
    await page.waitForTimeout(1500);
    out.ticks = await page.evaluate(() => (window as any).__qaTicks);
    out.subscribesDuringStream =
      (await page.evaluate(
        () => (window as any).__qa.invokes.filter((c: any) => c[0] === "live_hub_subscribe").length,
      )) - subscribesBefore;
    out.replacedRows = await frame.evaluate(
      () => (((window as any).__stopIds = true), (window as any).__rep),
    );
    out.frames = await frame.evaluate(() => (window as any).__fr);
    await stream(page, false);
    // Verify no real-hub re-subscribe when each session change is its own delta (7 per tick).
    const subs0 = await page.evaluate(
      () => (window as any).__qa.invokes.filter((c: any) => c[0] === "live_hub_subscribe").length,
    );
    await stream(page, true, true);
    await page.waitForTimeout(2000);
    await stream(page, false);
    out.separateDeltaTicks = await page.evaluate(() => (window as any).__qaTicks);
    out.subscribesDuringSeparateDeltas =
      (await page.evaluate(
        () => (window as any).__qa.invokes.filter((c: any) => c[0] === "live_hub_subscribe").length,
      )) - subs0;
    await page.screenshot({ path: info.outputPath(`live-${arrange}.png`) });
    out.errors = errors;
    out.console = messages;
    console.log(`QA-LIVE[${info.project.name}] ${arrange}: ${JSON.stringify(out, null, 1)}`);
    await info.attach("live", {
      body: JSON.stringify(out, null, 1),
      contentType: "application/json",
    });
    expect.soft(out.ticks).toBeGreaterThan(30);
    for (const k of [
      "quick",
      "messages",
      "main",
      "pinnedMain",
      "tallyWaiting",
      "tallyWorking",
      "filter",
      "arrange",
      "more",
    ])
      expect
        .soft(
          out[k].found && out[k].misses === 0 && out[k].replaced === 0,
          `${k}: ${JSON.stringify(out[k])}`,
        )
        .toBe(true);
    expect
      .soft(out.wsHead.found, "workspace heads exist only in Workspace view")
      .toBe(arrange === "teams");
    if (arrange === "teams")
      expect
        .soft(
          out.wsHead.misses === 0 && out.wsHead.replaced === 0,
          `wsHead: ${JSON.stringify(out.wsHead)}`,
        )
        .toBe(true);
    // Filter toggles intentionally change the list; stability is measured before those clicks.
    expect.soft(out.replacedRowsStreamOnly, "row nodes replaced during pure stream").toBe(0);
    expect
      .soft(out.subscribesDuringStream, "only the test's initial rev probe subscribes")
      .toBeLessThanOrEqual(1);
    expect.soft(out.subscribesDuringSeparateDeltas, "separate deltas never re-subscribe").toBe(0);
    expect
      .soft(
        out.waitingRows.every((n: boolean) => n),
        "waiting filter",
      )
      .toBe(true);
    expect.soft(out.menuOpenAfterStream, "filter menu survives deltas").toBe(true);
    expect.soft(errors).toEqual([]);
  });
}
