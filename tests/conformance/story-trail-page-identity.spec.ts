// SPDX-License-Identifier: MIT
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test, expect } from "../pane-harness/harness";
import { agentRow } from "./checks";
import { writeTrailFixture } from "../../scripts/story-trail-conformance.mjs";
import {
  navigationCases,
  navigationConversation,
  runNavigationMatrix,
} from "../../src-tauri/assets/dashboards/story-trail/navigation-matrix.mjs";
const base = dirname(process.env.STORY_TRAIL_TEST_FIXTURE ?? "/tmp/cde19-page-identity/trail.html");
for (const spec of navigationCases)
  test(`page identity transition matrix: ${spec.name}`, async ({ mountPane }) => {
    let pane: any,
      serial = 0;
    const reading = () =>
      pane.frame.evaluate(() => ({
        thread: document.querySelector(".thread")!.scrollTop,
        rail: document.querySelector(".turn-rail")!.scrollTop,
      }));
    const paint = () =>
      pane.frame.evaluate(
        () =>
          new Promise<void>((resolve) =>
            requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
          ),
      );
    try {
      await runNavigationMatrix(
        spec,
        {
          async open(route: string) {
            await pane?.close();
            const path = writeTrailFixture(
              join(base, `identity-${spec.name.replaceAll(" ", "-")}-${serial++}.html`),
              { route },
            );
            let html = readFileSync(path, "utf8");
            const boot = html.lastIndexOf("<script>(()=>{");
            html =
              html.slice(0, boot) +
              `<script>(()=>{const spec=${JSON.stringify(spec)},page=${navigationConversation.toString()},read=codesense.sessionRead;codesense.sessionRead=async args=>{const data=await read({sessionId:args.sessionId});data.conversation=page(spec,args);return data;};})();</script>` +
              html.slice(boot);
            writeFileSync(path, html);
            pane = await mountPane(path, { height: 400, sessions: [agentRow("lead", "Builder")] });
            await expect(pane.frame.locator(".turn").first()).toBeVisible();
            await paint();
          },
          async snapshot() {
            return {
              indices: await pane.frame
                .locator("article.turn")
                .evaluateAll((nodes: HTMLElement[]) =>
                  nodes.map((node) => Number(node.id.slice(5))),
                ),
              highlight: await pane.frame
                .locator("article.turn.highlight")
                .evaluateAll((nodes: HTMLElement[]) =>
                  nodes.map((node) => Number(node.id.slice(5))),
                ),
              route: await pane.frame.evaluate(() => location.hash),
              reading: await reading(),
            };
          },
          async refresh() {
            const name = "Builder " + serial++;
            await pane.upsert("sessions", agentRow("lead", name));
            await expect(pane.frame.locator(".conversation-head h1")).toContainText(name);
            await paint();
          },
          async scroll(thread: number, rail: number) {
            await pane.frame.evaluate(
              ({ thread, rail }) => {
                document.querySelector(".thread")!.scrollTop = thread;
                document.querySelector(".turn-rail")!.scrollTop = rail;
              },
              { thread, rail },
            );
            return reading();
          },
          async click(action: string, index?: number) {
            const control = pane.frame.locator(
              action === "jump" ? `[data-jump="${index}"]` : `[data-conv-action="${action}"]`,
            );
            await control.scrollIntoViewIfNeeded();
            const before = await reading();
            await control.click();
            await pane.frame.waitForFunction(
              () => !document.querySelector('[data-conv-action][aria-disabled="true"]'),
            );
            await paint();
            return before;
          },
          async revealed(index: number) {
            await expect(
              pane.frame.locator(`#turn-${index} [data-row-label]`).first(),
            ).toBeInViewport();
          },
          async story() {
            await pane.frame.evaluate(() => {
              location.hash = "#story/SV-X";
            });
            await expect(pane.frame.locator("[data-open=lead]")).toBeVisible();
          },
          async back() {
            await pane.frame.evaluate(() => history.back());
            await expect(pane.frame.locator(".conversation-body")).toBeVisible();
            await paint();
          },
        },
        expect,
      );
    } finally {
      await pane?.close();
    }
  });
