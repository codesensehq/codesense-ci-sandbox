// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test, expect } from "../pane-harness/harness";
import { agentRow } from "./checks";
import { writeTrailFixture } from "../../scripts/story-trail-conformance.mjs";
const base = dirname(
  process.env.STORY_TRAIL_TEST_FIXTURE ?? "/tmp/cde19-conformance/navigation.html",
);
test("history return keeps cached conversation thread and rail reading positions", async ({
  mountPane,
}) => {
  const fixture = writeTrailFixture(join(base, "history.html"), { route: "#session/lead" });
  const pane = await mountPane(fixture, { height: 400, sessions: [agentRow("lead", "Builder")] });
  try {
    const frame = pane.frame;
    await expect(frame.locator("#turn-0")).toBeVisible();
    const saved = await frame.locator(".thread").evaluate((node) => {
      node.scrollTop = 500;
      return node.scrollTop;
    });
    expect(saved).toBeGreaterThan(100);
    const rail = await frame.locator(".turn-rail").evaluate((node) => {
      node.scrollTop = 120;
      return node.scrollTop;
    });
    expect(rail).toBeGreaterThan(0);
    await frame.evaluate(() => {
      location.hash = "#story/SV-X";
    });
    await expect(frame.locator("[data-open=lead]")).toBeVisible();
    await frame.evaluate(() => history.back());
    await expect(frame.locator(".conversation-body")).toBeVisible();
    expect(await frame.locator(".thread").evaluate((node) => node.scrollTop)).toBe(saved);
    expect(await frame.locator(".turn-rail").evaluate((node) => node.scrollTop)).toBe(rail);
  } finally {
    await pane.close();
  }
});
test("Earlier across a retained-tail gap stops before the current page and Later follows the reader cursor", async ({
  mountPane,
}) => {
  const pane = await mountPane(writeTrailFixture(join(base, "gap.html")), {
    height: 400,
    sessions: [agentRow("lead", "Builder")],
  });
  try {
    const frame = pane.frame;
    await expect(frame.locator("[data-open=lead]")).toBeVisible();
    await frame.evaluate(() => {
      const cs = (window as any).codesense,
        old = cs.sessionRead;
      cs.sessionRead = async (args) => {
        const data = await old({ sessionId: args.sessionId });
        const retained = [0, ...Array.from({ length: 100 }, (_, i) => 100 + i)],
          rest = retained.filter((i) => i >= (args.turn ?? args.cursor ?? 0)),
          page = rest.slice(0, args.limit ?? 20);
        data.conversation = {
          sessionId: args.sessionId,
          state: "truncated",
          reason: null,
          turns: page.map((index) => ({
            index,
            role: index === 0 ? "user" : "assistant",
            text: "Recorded turn " + index,
            tool_calls: [],
          })),
          turnCount: retained.length,
          toolCallCount: 0,
          nextCursor: rest.length > page.length ? page.at(-1)! + 1 : null,
          truncated: {
            droppedTurns: [1, 99],
            droppedTurnCount: 99,
            droppedBytes: 10000,
            trimmedFields: 0,
          },
          unavailable: {},
          lookup: null,
        };
        return data;
      };
      location.hash = "#session/lead/turn/100";
    });
    await expect(frame.locator(".turn").first()).toHaveAttribute("id", "turn-100");
    await frame.locator("[data-conv-action=earlier]").click();
    await expect(frame.locator(".turn").first()).toHaveAttribute("id", "turn-0");
    expect(
      await frame
        .locator(".turn")
        .evaluateAll((rows) => rows.map((row) => Number(row.id.slice(5)))),
    ).toEqual([0]);
    await frame.locator("[data-conv-action=later]").click();
    await expect(frame.locator(".turn").first()).toHaveAttribute("id", "turn-100");
  } finally {
    await pane.close();
  }
});
for (const mode of ["anchor", "anchor-control", "reload"])
  test(`ordinary HTML load positioning: ${mode}`, async ({ page }) => {
    const shim = readFileSync("src-tauri/src/http/routes/viewer.rs", "utf8")
      .split('const LINK_INTERCEPTOR_JS: &str = r#"')[1]
      .split('"#;')[0];
    // Execute the actual host load callback against the actual injected viewer shim.
    const callback = readFileSync("src/features/dashboard/components/html-viewer.tsx", "utf8")
      .split("const handleIframeLoad = useCallback(() => {")[1]
      .split("}, [rejectCaptures")[0];
    await page.route("http://trail-navigation.test/**", async (route) => {
      const host = new URL(route.request().url()).pathname === "/host",
        hash = mode === "reload" ? "" : "#section";
      const body = host
        ? `<script>window.lastY=${mode === "reload" ? 650 : 0};addEventListener('message',e=>{if(e.data.type==='codesense:scroll')window.lastY=e.data.y;});function handleLoad(){const viewerUrl='/doc',onPageLoadRef={},hashLoaded={},hashRef={current:'${hash}'},routeRef={current:{hash:'${hash}'}},scrollYRef={current:window.lastY},live=false;const applyPendingHash=()=>{},rejectCaptures=()=>{},endpoint=()=>document.querySelector('iframe');${mode === "anchor-control" ? "" : callback}}</script><iframe style="height:400px;width:700px" src="/doc${hash}" onload="handleLoad()"></iframe>`
        : `<!doctype html><script>${shim}</script><div style="height:2400px">Earlier content</div><h2 id="section">Requested section</h2><div style="height:500px"></div>`;
      await route.fulfill({ contentType: "text/html", body });
    });
    await page.goto("http://trail-navigation.test/host");
    const frame = page.frameLocator("iframe");
    await expect(frame.locator("#section")).toBeAttached();
    await page.evaluate(
      () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
    );
    const y = await frame.locator("body").evaluate(() => window.scrollY);
    if (mode === "reload") expect(y).toBe(650);
    else expect(y).toBeGreaterThan(1000);
  });
