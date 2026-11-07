// SPDX-License-Identifier: MIT
import { test, expect } from "@playwright/test";
import { installAgentListPane, routeAgentListKit } from "./agent-list-support";
import { AGENT_LIST_PANE } from "../pane-harness/agent-list-fixtures";
import { capturePaneErrors } from "../pane-harness/agent-list-console";
import { watchRow, rowStability } from "../support/row-stability";

for (const burst of [false, true]) {
  test(`built-in agent list retains pointed idle row and focus through ${burst ? "folded bursts" : "100ms session deltas"}`, async ({
    page,
  }) => {
    const { errors } = capturePaneErrors(page);
    const paneId = `agent-list-stability-${burst ? "burst" : "stream"}`;
    await page.setViewportSize({ width: 1200, height: 900 });
    await routeAgentListKit(page);
    await installAgentListPane(page, AGENT_LIST_PANE, paneId);
    await page.goto("/?harness=live-hub");
    await page.waitForFunction(() => !!(window as any).__liveHubMount);
    await page.evaluate(async (id) => {
      const fixture = (window as any).__liveHubFixture;
      fixture.startAgentPane();
      const { liveHub } = await import("/src/features/dashboard/lib/live-hub.ts");
      await liveHub.writeUi("ui:dock", "all", {
        arrange: "teams",
        folds: {},
        pins: {},
        search: { query: "", status: "all" },
        hiddenTeams: [],
      });
      (window as any).__liveHubMount(1, true, true, id);
    }, paneId);
    await page.addStyleTag({
      content: "main > div {height: 800px !important; width:268px !important}",
    });
    await expect.poll(() => page.frames().filter((f) => f.url().includes("/pane/")).length).toBe(1);
    const frame = page.frames().find((f) => f.url().includes("/pane/"))!;
    expect(new URL(frame.url()).pathname).toContain(`${paneId}.html`);
    await expect(frame.locator("#count")).toHaveText("50");
    const row = frame
      .locator(".al-item")
      .filter({ has: frame.locator('[data-agent-id="scale-1"]') });
    const main = row.locator(".al-main");
    await expect(main).toContainText("Session 1");
    await main.focus();
    await watchRow(frame, row);
    await frame.evaluate(() => {
      const rows = [...document.querySelectorAll<HTMLElement>(".al-item[data-key]")];
      const focused = document.activeElement;
      const state = { resyncs: 0, replacements: 0, focusLosses: 0, livePaints: 0 };
      addEventListener("message", (event) => {
        if (
          event.source === parent &&
          event.data?.type === "codesense:resync" &&
          event.data.topic === "sessions"
        )
          state.resyncs++;
      });
      const observer = new MutationObserver((mutations) => {
        for (const mutation of mutations) {
          for (const removed of mutation.removedNodes) {
            if (
              rows.includes(removed as HTMLElement) &&
              document.querySelector(
                `[data-key="${CSS.escape((removed as HTMLElement).dataset.key!)}"]`,
              ) !== removed
            )
              state.replacements++;
          }
          if (mutation.type === "attributes" && mutation.attributeName === "title")
            state.livePaints++;
        }
        if (document.activeElement !== focused) state.focusLosses++;
      });
      observer.observe(document.querySelector("#list")!, {
        childList: true,
        subtree: true,
        attributes: true,
        attributeFilter: ["title"],
      });
      (window as any).__agentStability = () => {
        observer.disconnect();
        return {
          ...state,
          rows: rows.length,
          changedRows: rows.filter(
            (el) => document.querySelector(`[data-key="${CSS.escape(el.dataset.key!)}"]`) !== el,
          ).length,
          sameFocus: document.activeElement === focused,
        };
      };
    });
    const start = Date.now();
    let ticks = 0;
    for (; Date.now() - start < 5000; ticks++) {
      await page.evaluate(
        ({ ticks, burst }) => {
          const fixture = (window as any).__liveHubFixture;
          for (let i = 0; i < (burst ? 3 : 1); i++) {
            Object.assign(fixture.sessions[0], {
              effectiveStatus: i % 2 ? "thinking" : "tool-running",
              progress: { note: `Stability step ${ticks}.${i}` },
              activityAtMs: Date.now(),
            });
            fixture.fire(true);
          }
        },
        { ticks, burst },
      );
      await expect(frame.locator('[data-agent-id="scale-0"] .al-main')).toHaveAttribute(
        "title",
        new RegExp(`Stability step ${ticks}\\.${burst ? 2 : 0}`),
      );
      await page.waitForTimeout(100);
    }
    const stability = await rowStability(frame);
    const allRows = await frame.evaluate(() => (window as any).__agentStability());
    console.log(`built-in agent list: ${ticks} ${burst ? "bursts" : "deltas"}`, {
      stability,
      allRows,
    });
    expect(ticks).toBeGreaterThan(10);
    expect(stability.frames).toBeGreaterThan(5);
    expect(stability).toMatchObject({
      replacements: 0,
      pointerReplacements: 0,
      removals: 0,
      sameRow: true,
      samePointer: true,
    });
    expect(allRows.rows).toBeGreaterThan(5);
    expect(allRows.livePaints).toBeGreaterThan(10);
    expect(allRows).toMatchObject({
      replacements: 0,
      changedRows: 0,
      focusLosses: 0,
      sameFocus: true,
    });
    // SV-PLG-33 (#2590): several same-topic deltas in one task fold into one delta, so a burst
    // never forces a resync. The per-tick title wait above proves the folded delta carried the
    // last step of each burst.
    if (burst) expect(allRows.resyncs).toBe(0);
    await expect(main).toBeFocused();
    expect(errors).toEqual([]);
  });
}
