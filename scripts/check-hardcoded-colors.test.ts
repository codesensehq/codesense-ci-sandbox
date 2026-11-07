// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
// @ts-expect-error The lint is an executable JS module.
import { check, colorCounts } from "./check-hardcoded-colors.mjs";
it("counts literal colours and palette utilities, and allows counts only to decrease", () => {
  expect(colorCounts("text-amber-600 bg-slate-950 #aabbcc text-(--foreground)")).toEqual({
    "text-amber-600": 1,
    "bg-slate-950": 1,
    "#aabbcc": 1,
  });
  const root = mkdtempSync(join(tmpdir(), "theme-lint-"));
  try {
    mkdirSync(join(root, "src"));
    mkdirSync(join(root, "scripts"));
    writeFileSync(
      join(root, "scripts/hardcoded-colors.allow.json"),
      JSON.stringify({ "src/a.ts": { "#fff": 1 } }),
    );
    writeFileSync(join(root, "src/a.ts"), 'const a="#fff",b="#fff";');
    expect(check(root)).toEqual([expect.stringContaining("counts may only go down")]);
    writeFileSync(join(root, "src/a.ts"), 'const a="var(--foreground)";');
    expect(check(root)).toEqual([]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

it("pane mode rejects colour syntax and named colours while allowing semantic tokens", () => {
  const source = `<style>
    body { color: red; background: rgb(1 2 3); border: 1px solid hsl(0 0% 0%); }
    button { box-shadow: 0 0 1px #123; fill: rebeccapurple; stroke: rgba(0,0,0,.5); }
    :root { --custom: hsla(0,0%,0%,.5); }
    a { color: currentColor; background: transparent; border-color: inherit; }
    .red { color: var(--cs-fg); } /* blue */
  </style><p style="color: navy">Words like red are not CSS.</p>
  <script>el.style.color = "teal"; el.setAttribute("fill", "orange"); el.setAttribute("class", "mark");</script>`;
  expect(Object.keys(colorCounts(source, { pane: true }))).toEqual(
    expect.arrayContaining([
      "red",
      "rgb(",
      "hsl(",
      "#123",
      "rebeccapurple",
      "rgba(",
      "hsla(",
      "navy",
      "teal",
      "orange",
    ]),
  );
  expect(
    colorCounts('<script>el.setAttribute("class", "mark"); const label = "red";</script>', {
      pane: true,
    }),
  ).toEqual({});
  expect(
    colorCounts(
      "<style>a {color: var(--cs-fg); fill: currentColor; background: transparent; border-color: inherit;}</style>",
      { pane: true },
    ),
  ).toEqual({});
});

it("the hard-coded pane fixture is rejected by pane lint", () => {
  expect(
    check(process.cwd(), { mode: "pane", paths: ["tests/conformance/fixtures/theme.html"] }),
  ).toEqual(
    expect.arrayContaining([expect.stringContaining("#fff"), expect.stringContaining("#111")]),
  );
});

it("all built-in pane sources pass the shared inventory's pane lint", () => {
  expect(check(process.cwd(), { mode: "pane" })).toEqual([]);
});

it("pane lint reads cssText and stylesheet-text assignments", () => {
  const source = `
    element.style.cssText = "color: red; background: rgb(1 2 3)";
    style.textContent = 'body { border: 1px solid navy; }';
    style.innerHTML = \`button { color: #123456; }\`;
  `;
  expect(Object.keys(colorCounts(source, { pane: true }))).toEqual(
    expect.arrayContaining(["red", "rgb(", "navy", "#123456"]),
  );
});

it("pane lint ignores quoted font names, CSS strings and URL payloads", () => {
  const source = `<style>
    body { font-family: "Red Hat Display"; background-image: url(/icons/white.svg); }
    p { content: "red"; mask-image: url('/icons/blue.svg'); }
  </style><script>
    element.style.cssText = "font-family: 'Red Hat Display'; background: url(/icons/white.svg)";
    style.textContent = \`body { font-family: "Red Hat Display"; background-image: url('/icons/white.svg'); }\`;
  </script>`;
  expect(colorCounts(source, { pane: true })).toEqual({});
});

it("pane lint reads escaped quotes and whitespace in JavaScript CSS strings", () => {
  const source = String.raw`element.style.cssText = "font-family: \"Red Hat Display\";\ncolor: rebeccapurple; background-image: url(\"/icons/white.svg\")";`;
  expect(colorCounts(source, { pane: true })).toEqual({ rebeccapurple: 1 });
});

it("pane lint reads colour longhand style assignments", () => {
  const source = `
    el.style.borderTopColor = "red";
    el.style.outlineColor = "navy";
    el.style.textDecorationColor = "teal";
    el.style.caretColor = "orange";
    el.style.accentColor = "purple";
  `;
  expect(colorCounts(source, { pane: true })).toEqual({
    red: 1,
    navy: 1,
    teal: 1,
    orange: 1,
    purple: 1,
  });
});

it("pane lint treats mask and clip paint as luminance, not colour", () => {
  expect(
    colorCounts(
      '<svg><mask id="m"><rect fill="white"/><text fill="black">?</text></mask><clipPath id="c"><rect fill="black"/></clipPath></svg>',
      { pane: true },
    ),
  ).toEqual({});
  expect(
    colorCounts(
      '<script>const cut = `<mask id="${id}"><rect fill="white"/>${shape}</mask>`; const shape = `<text data-mask fill="black">?</text>`;</script>',
      { pane: true },
    ),
  ).toEqual({});
  expect(
    Object.keys(
      colorCounts('<svg><rect fill="white"/><text fill="black">!</text></svg>', { pane: true }),
    ).sort(),
  ).toEqual(["black", "white"]);
  expect(
    Object.keys(colorCounts('<svg><rect data-masked fill="white"/></svg>', { pane: true })),
  ).toEqual(["white"]);
});

it("the mask exemption covers only white and black luminance, within one string", () => {
  const keys = (source: string) => Object.keys(colorCounts(source, { pane: true })).sort();
  expect(keys('<svg><mask id="m"><rect fill="red"/><rect fill="white"/></mask></svg>')).toEqual([
    "red",
  ]);
  expect(keys('<svg><rect data-mask fill="red" style="color: blue"/></svg>')).toEqual([
    "blue",
    "red",
  ]);
  expect(keys('<svg><rect data-mask-foo fill="white"/></svg>')).toEqual(["white"]);
  expect(
    keys(
      '<script>const a = `<mask id="a">`; el.setAttribute("fill", "red"); const b = `</mask>`;</script>',
    ),
  ).toEqual(["red"]);
});
