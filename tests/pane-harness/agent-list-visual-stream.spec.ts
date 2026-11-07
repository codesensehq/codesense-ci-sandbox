// SPDX-License-Identifier: MIT
// SV-AGP-9: hover a Ready row while other sessions stream deltas every 100ms for 5s.
// The hovered row's node must stay the same element, stay :hover, and its name must not repaint.
import { test, expect } from "./harness";
import { AGENT_LIST_PANE, agentListCaptureFixtures } from "./agent-list-fixtures";
test(`stream builtin: hovered Ready row stays put`, async ({ mountPane }) => {
  test.setTimeout(60_000);
  const fixtures = { ...agentListCaptureFixtures(), width: 236, height: 700 };
  const ready = fixtures.sessions!.find((s: any) => s.id === "s4")!;
  ready.effectiveStatus = "idle";
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const { frame } = pane;
  await expect(frame.locator('[data-agent-id="s4"]')).toBeVisible();
  await frame.locator('[data-agent-id="s4"] .al-main').hover();
  await frame.evaluate(() => {
    const w = window as any;
    const row = () => document.querySelector('[data-agent-id="s4"]');
    w.__s = { replaced: 0, underPointer: 0, frames: 0 };
    w.__row = row();
    const b = row()!.getBoundingClientRect();
    const px = b.left + 60,
      py = b.top + b.height / 2;
    w.__under = document.elementFromPoint(px, py);
    const tick = () => {
      const r = row();
      w.__s.frames++;
      if (r !== w.__row) {
        w.__s.replaced++;
        w.__row = r;
      }
      const u = document.elementFromPoint(px, py);
      if (u !== w.__under && !(w.__under as any)?.isConnected) w.__s.underPointer++;
      if (u !== w.__under) {
        (w.__s.log ||= []).push(
          `${w.__s.frames}:${(w.__under as any)?.className}->${(u as any)?.className}:${(w.__under as any)?.isConnected}`,
        );
        w.__under = u;
      }
      if (!w.__stop) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  const others = fixtures.sessions!.filter((s: any) => s.id !== "s4").slice(0, 5);
  const all = fixtures.sessions!;
  const statuses = ["tool-running", "thinking", "running"];
  const t0 = Date.now();
  for (let i = 0; Date.now() - t0 < 5000; i++) {
    for (const s of all)
      await pane.upsert("sessions", {
        ...s,
        ...(others.includes(s)
          ? {
              effectiveStatus: statuses[(i + others.indexOf(s)) % 3],
              progress: { note: `step ${i}` },
              activityAtMs: Date.now(),
            }
          : {}),
        heartbeatAtMs: Date.now(),
        deltaSeq: i,
      });
    await pane.page.waitForTimeout(100);
  }
  const r = await frame.evaluate(() => {
    (window as any).__stop = true;
    return (window as any).__s;
  });
  console.log(
    `STREAM builtin: frames=${r.frames} hovered-row replaced=${r.replaced} element-under-pointer changed=${r.underPointer} ${JSON.stringify(r.log || [])}`,
  );
  expect(r.replaced).toBe(0);
  expect(r.underPointer).toBe(0);
});
