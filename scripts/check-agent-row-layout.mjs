// SPDX-License-Identifier: MIT
// Real-geometry check that a Limited agent row's "Release not supported" note is fully visible
// at the normal 268px dock width, in By status and By manager, light and dark.
//
// It lays out the row's real markup classes with the real stylesheets in a browser engine and
// asks, for each line of the note, whether it lies inside every ancestor that clips. Not a
// textContent check: a note squeezed to "Releas…" or wrapped out of a fixed-height line fails.
//
//   node scripts/check-agent-row-layout.mjs [--shots <dir>]
//   node scripts/check-agent-row-layout.mjs --control   # the old placement, in the clipped via line: must FAIL
import { readFileSync, mkdirSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium, webkit } = require("@playwright/test");
const shots = process.argv.includes("--shots")
  ? process.argv[process.argv.indexOf("--shots") + 1]
  : null;
if (shots) mkdirSync(shots, { recursive: true });

const css = ["src/styles/theme-tokens.css", "src/styles/agents.css"]
  .map((f) => readFileSync(f, "utf8"))
  .join("\n");
const font = `@font-face{font-family:Geist;src:url(data:font/woff2;base64,${readFileSync("node_modules/@fontsource-variable/geist/files/geist-latin-wght-normal.woff2").toString("base64")})}`;
const NOTE = "Release not supported";
const LAUNCHER = "A very long launcher agent name";

const control = process.argv.includes("--control");
// Proof hook: extra CSS appended after the real stylesheets (a check that cannot fail is no check).
const extraCss = process.env.CHECK_EXTRA_CSS ?? "";
const allowSkip = process.argv.includes("--allow-skipped-engines");
const EXPECTED_CASES = 8;
let ran = 0;
const limitedNote = `<span class="ag-limited" data-testid="agent-limited-note">${NOTE}</span>`;
const row = (manager) => `
<div class="cs-agents" style="width:268px">
  <div class="ag-row" ${manager ? 'data-manager-view="true"' : ""} style="padding-left:${manager ? 36 : 2}px">
    <button class="ag-main flex min-w-0 flex-1 items-start">
      <span style="width:26px;height:26px;flex:none"></span>
      <span class="ml-1 flex min-w-0 flex-1 flex-col">
        <span class="ag-title"><span class="ag-name">oc1</span><span class="ag-task"> · Limited</span></span>
        <span class="ag-sub flex min-w-0" data-testid="agent-detail">
          <span class="ag-via-line"><span class="ag-via-lead">
            <span class="ag-via-head">${control ? `<span data-testid="agent-limited-note">${NOTE}</span> · ` : ""}${manager ? "" : "Riverside · Workspace · "}</span>
            <span class="ag-via-pill"><svg class="ag-via-glyph"></svg><span class="truncate">${LAUNCHER}</span></span>
          </span><span class="ag-via-rest"> · OpenCode · 12m</span></span>
          ${manager ? '<span class="ag-workspace-badge">SV-ENG-87 long workspace name</span>' : ""}
        </span>
        ${control ? "" : limitedNote}
      </span>
    </button>
  </div>
</div>`;

const probe = (note) => {
  const el = document.querySelector('[data-testid="agent-limited-note"]');
  const range = document.createRange();
  range.selectNodeContents(el);
  const lines = [...range.getClientRects()].filter((r) => r.width > 0);
  const clips = [];
  for (let a = el.parentElement; a && a !== document.body; a = a.parentElement) {
    const o = getComputedStyle(a);
    if (o.overflowX !== "visible" || o.overflowY !== "visible")
      clips.push(a.getBoundingClientRect());
  }
  const root = document.querySelector(".cs-agents").getBoundingClientRect();
  clips.push(root);
  const inside =
    lines.length > 0 &&
    lines.every((l) =>
      clips.every(
        (c) =>
          l.left >= c.left - 0.5 &&
          l.right <= c.right + 0.5 &&
          l.top >= c.top - 0.5 &&
          l.bottom <= c.bottom + 0.5,
      ),
    );
  // Every character is drawn: no ellipsis, and the box is not narrower than its text.
  return {
    inside,
    lines: lines.length,
    fits: el.scrollWidth <= el.clientWidth + 1,
    truncated: getComputedStyle(el).textOverflow === "ellipsis",
    text: el.textContent === note,
  };
};

let failed = false;
for (const [name, engine] of Object.entries({ chromium, webkit })) {
  let browser;
  try {
    browser = await engine.launch({ headless: true });
  } catch (e) {
    console.log(`${name}: skipped (${String(e.message).split("\n")[0]})`);
    continue;
  }
  const page = await browser.newPage({ viewport: { width: 400, height: 200 } });
  for (const manager of [false, true]) {
    for (const mode of ["light", "dark"]) {
      await page.setContent(
        `<style>${font}${css}${extraCss}*{box-sizing:border-box}button{font:inherit}body{margin:0;font-family:Geist,sans-serif}.flex{display:flex}.min-w-0{min-width:0}.flex-1{flex:1}.items-start{align-items:flex-start}.flex-col{flex-direction:column}.truncate{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.ag-main{gap:6px;padding:6px 8px 6px 4px;border:0;background:transparent;text-align:left}.ml-1{margin-left:4px}</style><body class="${mode}" style="background:${mode === "dark" ? "#0d1117" : "#fff"}">${row(manager)}</body>`,
      );
      await page.evaluate(() => {
        document.documentElement.className = document.body.className;
      });
      await page.evaluate(() => document.fonts.ready);
      const r = await page.evaluate(probe, NOTE);
      ran += 1;
      const label = `${name} ${manager ? "by-manager" : "by-status"} ${mode}`;
      const ok = r.inside && r.fits && !r.truncated && r.text;
      console.log(`${ok ? "ok  " : "FAIL"} ${label} ${JSON.stringify(r)}`);
      if (!ok) failed = true;
      if (shots)
        await page.screenshot({
          path: `${shots}/${name}-${manager ? "by-manager" : "by-status"}-${mode}.png`,
          clip: { x: 0, y: 0, width: 268, height: 90 },
        });
    }
  }
  await browser.close();
}
// A check that ran nothing, or fewer cases than expected, has proved nothing.
if (ran === 0 || (ran < EXPECTED_CASES && !allowSkip)) {
  console.log(`FAIL ran ${ran} of ${EXPECTED_CASES} cases (a browser engine did not launch)`);
  process.exit(1);
}
process.exit(control ? (failed ? 0 : 1) : failed ? 1 : 0);
