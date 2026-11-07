// SPDX-License-Identifier: MIT
import { test, expect } from "@playwright/test";

test("LiveHub folds a multi-session visible burst without a resync or row replacement", async ({
  page,
}) => {
  const paneId = `scale-${test.info().parallelIndex}`; // own pane id: parallel tests must not share one mock pane
  await page.request.put(`/__mock-panes/default/${paneId}`, {
    data: `<!doctype html><html><body><ul id="rows"></ul><script>
      window.deltas=[]; window.replacements=0; var nodes=new Map();
      codesense.subscribe('sessions',{onChange:function(value,delta){
        if(delta) window.deltas.push(delta);
        var list=document.querySelector('#rows');
        value.snapshot.forEach(function(row){
          var node=nodes.get(row.id);
          if(!node){node=document.createElement('li');node.dataset.rowId=row.id;nodes.set(row.id,node);}
          else if(!node.isConnected) window.replacements++;
          node.textContent=row.displayName;
          list.appendChild(node);
        });
        list.dataset.count=String(value.snapshot.length);
      }});
    </script></body></html>`,
  });
  await page.goto("/?harness=live-hub");
  await page.waitForFunction(() => !!(window as any).__liveHubMount);
  await page.evaluate((id) => (window as any).__liveHubMount(1, true, false, id), paneId);
  await expect
    .poll(() => page.frames().filter((candidate) => candidate.url().includes("/pane/")).length)
    .toBe(1);
  const frame = page.frames().find((candidate) => candidate.url().includes("/pane/"))!;
  await expect(frame.locator("#rows li")).toHaveCount(50);
  await frame.evaluate(() => ((window as any).deltas = []));
  await page.evaluate(() => (window as any).__liveHubFixture.reset());
  await page.evaluate(() => (window as any).__liveHubFixture.fireBurst(4));
  await expect(frame.locator("#rows li").first()).toContainText("burst");
  await expect.poll(() => frame.evaluate(() => (window as any).deltas.length)).toBe(1);
  const result = await frame.evaluate(() => ({
    delta: (window as any).deltas[0],
    replacements: (window as any).replacements,
  }));
  const reads = await page.evaluate(() => (window as any).__liveHubFixture.counters().reads);
  expect(result.delta).toMatchObject({ baseRev: expect.any(Number), rev: expect.any(Number) });
  expect(result.replacements).toBe(0);
  expect(reads).toBe(0);
});
