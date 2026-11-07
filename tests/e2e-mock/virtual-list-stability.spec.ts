// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { test, expect } from "@playwright/test";
import { watchRow, rowStability } from "../support/row-stability";

const kit = readFileSync("src-tauri/src/http/routes/dashboard_kit/kit.js", "utf8");
const virtualList = readFileSync("src-tauri/src/http/routes/dashboard_kit/virtual-list.js", "utf8");

for (const { mode, coalesced } of [
  { mode: "local", coalesced: false },
  { mode: "topic", coalesced: false },
  { mode: "topic", coalesced: true },
]) {
  const stream = coalesced ? "folded bursts" : "streaming deltas";
  test(`real LiveHub ${mode} list retains a pointed idle session through ${stream}`, async ({
    page,
  }) => {
    const paneId = `virtual-list-${mode}${coalesced ? "-burst" : ""}`;
    await page.request.put(`/__mock-panes/default/${paneId}`, {
      data: `<!doctype html><html><body data-mode="${mode}" style="margin:0"><div id="list" style="height:140px;overflow:auto"></div>
      <script>${kit}</script><script>${virtualList}</script><script>
        window.updates=0;
        window.resyncs=0;
        window.addEventListener('message',event=>{
          if(event.source===window.parent && event.data.type==='codesense:resync' && event.data.topic==='sessions') window.resyncs++;
        });
        function patch(el,row,i) { el.textContent=row.displayName; el.dataset.index=i; el.dataset.unread=row.unread; window.updates++; }
        const list=codesense.kit.virtualList({
          container:document.getElementById('list'), ${mode === "local" ? "rows:[]," : "topic:'sessions',"} rowHeight:24,
          layoutKey:row=>row.id,
          render(row,i) { const el=document.createElement('button'); patch(el,row,i); return el; }, update:patch
        });
        ${mode === "local" ? "codesense.subscribe('sessions',{onChange:s=>list.setRows(s.snapshot)});" : ""}
        codesense.ready();
      </script></body></html>`,
    });
    await page.goto("/?harness=live-hub");
    await page.waitForFunction(() => !!(window as any).__liveHubMount);
    await page.evaluate((id) => (window as any).__liveHubMount(1, true, true, id), paneId);
    await expect.poll(() => page.frames().filter((f) => f.url().includes("/pane/")).length).toBe(1);
    const frame = page.frames().find((f) => f.url().includes("/pane/"))!;
    await expect(frame.locator("body")).toHaveAttribute("data-mode", mode);
    expect(new URL(frame.url()).pathname).toContain(`${paneId}.html`);
    const row = frame.locator('[data-key="scale-1"]');
    await expect(row).toHaveText("Session 1");
    await row.focus();
    await watchRow(frame, row);
    if (coalesced) {
      await frame.evaluate(() => {
        const originals = [...document.querySelectorAll<HTMLElement>("[data-key]")];
        const keys = new Set(originals.map((el) => el.dataset.key));
        const focused = document.activeElement;
        let replacements = 0;
        let focusLosses = 0;
        const observer = new MutationObserver((mutations) => {
          // A retained node can move: count identity changes, not removal records alone.
          for (const mutation of mutations)
            for (const removed of mutation.removedNodes)
              if (
                removed instanceof HTMLElement &&
                keys.has(removed.dataset.key) &&
                document.querySelector(`[data-key="${removed.dataset.key}"]`) !== removed
              )
                replacements++;
          if (document.activeElement !== focused) focusLosses++;
        });
        observer.observe(document.querySelector("[data-cs-virtual-list]")!, {
          childList: true,
          subtree: true,
        });
        (window as any).resyncs = 0;
        (window as any).resyncStability = () => {
          observer.disconnect();
          return {
            resyncs: (window as any).resyncs as number,
            rows: originals.length,
            replacements,
            changedRows: originals.filter(
              (el) => document.querySelector(`[data-key="${el.dataset.key}"]`) !== el,
            ).length,
            focusLosses,
            sameFocus: document.activeElement === focused,
          };
        };
      });
    }
    const before = await frame.evaluate(() => (window as any).updates);
    const start = Date.now();
    let deltas = 0;
    for (; Date.now() - start < 5000; deltas++) {
      if (coalesced) {
        // Several same-topic deltas in one task fold into one delta; no resync is needed.
        const unread = await page.evaluate(() => {
          const fixture = (window as any).__liveHubFixture;
          for (let i = 0; i < 3; i++) {
            for (const session of fixture.sessions) session.unread++;
            fixture.fire(true);
          }
          return fixture.sessions[1].unread as number;
        });
        // Wait for the folded delta to reach the kit, not just the message event.
        await expect(row).toHaveAttribute("data-unread", String(unread));
      } else await page.evaluate(() => (window as any).__liveHubFixture.fire(true));
      await page.waitForTimeout(100);
    }
    await expect(frame.locator('[data-key="scale-0"]')).toHaveText(/^Session \d+/);
    expect(await frame.evaluate(() => (window as any).updates)).toBeGreaterThan(before + 10);
    const result = await rowStability(frame);
    console.log(`real LiveHub ${mode}: ${deltas} ${stream}`, result);
    if (coalesced) {
      const resync = await frame.evaluate(() => (window as any).resyncStability());
      console.log("folded delta stability", resync);
      expect(resync.resyncs).toBe(0);
      expect(resync.rows).toBeGreaterThan(2);
      expect(resync).toMatchObject({
        replacements: 0,
        changedRows: 0,
        focusLosses: 0,
        sameFocus: true,
      });
    }
    expect(result.frames).toBeGreaterThan(5);
    expect(result).toMatchObject({
      replacements: 0,
      pointerReplacements: 0,
      removals: 0,
      sameRow: true,
      samePointer: true,
    });
    await expect(row).toBeFocused();
  });
}
