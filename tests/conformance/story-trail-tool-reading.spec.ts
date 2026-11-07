// SPDX-License-Identifier: MIT
import { test, expect } from "../pane-harness/harness";
import { agentRow } from "./checks";
import { writeTrailFixture } from "../../scripts/story-trail-conformance.mjs";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
test("paging away and back retains tool disclosure and output reading position", async ({
  mountPane,
}) => {
  const path = writeTrailFixture(
    join(
      dirname(process.env.STORY_TRAIL_TEST_FIXTURE ?? "/tmp/cde19-pr2-r7/trail.html"),
      "tool-scroll.html",
    ),
    { route: "#session/lead" },
  );
  let html = readFileSync(path, "utf8");
  const boot = html.lastIndexOf("<script>(()=>{");
  html =
    html.slice(0, boot) +
    `<script>(()=>{const old=codesense.sessionRead;codesense.sessionRead=async args=>{const result=await old(args);for(const turn of result.conversation.turns){if(turn.index===0)turn.tool_calls=[{name:'Bash',native_id:'long-result',input:'cargo test',is_error:false,result:Array.from({length:500},(_,i)=>'Recorded output line '+i).join('\\n')}];}return result;};})();</script>` +
    html.slice(boot);
  writeFileSync(path, html);
  const pane = await mountPane(path, { height: 400, sessions: [agentRow("lead", "Builder")] });
  try {
    const frame = pane.frame,
      detail = frame.locator("#turn-0 details");
    await expect(detail).toBeAttached();
    await detail.locator("summary").click();
    const pre = detail.locator("pre").last();
    const saved = await pre.evaluate((node) => {
      node.scrollTop = 500;
      return node.scrollTop;
    });
    expect(saved).toBeGreaterThan(100);
    await frame.evaluate(
      () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
    );
    await frame.locator("[data-conv-action=later]").click();
    await expect(frame.locator("#turn-20")).toBeAttached();
    await frame.locator("[data-conv-action=earlier]").click();
    await expect(detail).toBeAttached();
    await expect(detail).toHaveAttribute("open", "");
    expect(await pre.evaluate((node) => node.scrollTop)).toBe(saved);
  } finally {
    await pane.close();
  }
});
