// SPDX-License-Identifier: MIT
import { readdirSync, readFileSync } from "node:fs";
import { resolve, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { builtInPaneFiles } from "./lib/builtin-panes.mjs";
// Policy: these are the only colour keywords a built-in pane may use.
export const ALLOWED_PANE_COLORS = new Set(["transparent", "currentcolor", "inherit"]);
const NAMED_COLORS =
  "aliceblue antiquewhite aqua aquamarine azure beige bisque black blanchedalmond blue blueviolet brown burlywood cadetblue chartreuse chocolate coral cornflowerblue cornsilk crimson cyan darkblue darkcyan darkgoldenrod darkgray darkgreen darkgrey darkkhaki darkmagenta darkolivegreen darkorange darkorchid darkred darksalmon darkseagreen darkslateblue darkslategray darkslategrey darkturquoise darkviolet deeppink deepskyblue dimgray dimgrey dodgerblue firebrick floralwhite forestgreen fuchsia gainsboro ghostwhite gold goldenrod gray green greenyellow grey honeydew hotpink indianred indigo ivory khaki lavender lavenderblush lawngreen lemonchiffon lightblue lightcoral lightcyan lightgoldenrodyellow lightgray lightgreen lightgrey lightpink lightsalmon lightseagreen lightskyblue lightslategray lightslategrey lightsteelblue lightyellow lime limegreen linen magenta maroon mediumaquamarine mediumblue mediumorchid mediumpurple mediumseagreen mediumslateblue mediumspringgreen mediumturquoise mediumvioletred midnightblue mintcream mistyrose moccasin navajowhite navy oldlace olive olivedrab orange orangered orchid palegoldenrod palegreen paleturquoise palevioletred papayawhip peachpuff peru pink plum powderblue purple rebeccapurple red rosybrown royalblue saddlebrown salmon sandybrown seagreen seashell sienna silver skyblue slateblue slategray slategrey snow springgreen steelblue tan teal thistle tomato turquoise violet wheat white whitesmoke yellow yellowgreen accentcolor accentcolortext activetext buttonborder buttonface buttontext canvas canvastext field fieldtext graytext highlight highlighttext linktext mark marktext selecteditem selecteditemtext visitedtext transparent currentcolor inherit".split(
    " ",
  );
const PANE_LITERAL = new RegExp(
  "#[\\da-f]{3,8}\\b|\\b(?:rgba?|hsla?)\\(|(?<![\\w-])(?:" + NAMED_COLORS.join("|") + ")(?![\\w-])",
  "gi",
);

function cssValues(source) {
  // Font names, generated content and asset URLs are not colour values.
  const css = source.replace(
    /\burl\((?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^)])*\)|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'/gi,
    "",
  );
  return [...css.matchAll(/(?:^|[;{])\s*[\w-]+\s*:\s*([^;}]+)/gm)].map((m) => m[1]);
}

function paneValues(source) {
  source = source.replace(/\/\*[\s\S]*?\*\/|<!--[\s\S]*?-->/g, "");
  // Architecture: in an SVG mask or clip, white and black paint is luminance (white shows, black
  // cuts), not a displayed colour. Only those two values are exempt, and only inside a <mask> or
  // <clipPath> written in one string, or on a shape a page assembles into a mask at runtime and
  // marks `data-mask`. Any other colour there is still counted.
  const luminance = (markup) =>
    markup.replace(
      /\b(?:fill|stroke)\s*=\s*(["'])\s*(?:white|black|#fff|#ffffff|#000|#000000)\s*\1/gi,
      "",
    );
  source = source
    .replace(/<(mask|clipPath)\b[^>`]*>[^`]*?<\/\1>/gi, luminance)
    .replace(/<[a-z][\w-]*\b[^>`]*\sdata-mask(?=[\s/>=])[^>`]*>/gi, luminance);
  const values = [...source.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/gi)].flatMap((m) =>
    cssValues(m[1]),
  );
  for (const m of source.matchAll(/\bstyle\s*=\s*(["'])((?:\\.|(?!\1)[\s\S])*?)\1/gi))
    values.push(...cssValues(m[2]));
  for (const m of source.matchAll(
    /\b(?:fill|stroke|color|bgcolor)\s*=\s*(["'])((?:\\.|(?!\1)[\s\S])*?)\1/gi,
  ))
    values.push(m[2]);
  // Read the value of JavaScript strings, preserving quotes nested inside CSS.
  for (const m of source.matchAll(/(["'`])((?:\\[\s\S]|(?!\1)[^\\])*?)\1/g)) {
    const value = m[2]
      .replace(/\\(["'`\\])/g, "$1")
      .replace(/\\[nrt]/g, " ")
      .trim();
    const before = source.slice(0, m.index);
    if (
      /(?:\b(?:cssText|textContent|innerHTML)\s*\+?=\s*|\b(?:replaceSync|replace|insertRule)\(\s*)$/i.test(
        before,
      )
    ) {
      values.push(...cssValues(value));
    } else if (
      /(?:\bstyle\s*\.\s*\w+Color\s*=\s*|\b(?:color|background(?:Color)?|border(?:Color)?|fill|stroke|boxShadow|textShadow)["']?\s*[:=]\s*|\bsetProperty\([^,]*,\s*|\bsetAttribute\(\s*["'](?:fill|stroke|color|bgcolor|style)["']\s*,\s*)$/i.test(
        before,
      )
    ) {
      values.push(...cssValues(`value: ${value}`));
    } else if (/^(?:#[\da-f]{3,8}|(?:rgba?|hsla?)\([^)]*\))$/i.test(value)) {
      values.push(value);
    }
  }
  return values.join("\n");
}

export function colorCounts(source, { pane = false } = {}) {
  if (pane) {
    const counts = {};
    for (const m of paneValues(source).matchAll(PANE_LITERAL)) {
      const literal = m[0].toLowerCase();
      if (!ALLOWED_PANE_COLORS.has(literal)) counts[literal] = (counts[literal] ?? 0) + 1;
    }
    return counts;
  }
  const counts = {};
  for (const m of source.matchAll(
    /#[\da-fA-F]{3,8}\b|\b(?:text|bg|border|ring|outline|fill|stroke|from|via|to|decoration|shadow|accent|caret)-(?:slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose)-(?:\d{2,3}|\*)\b/g,
  ))
    counts[m[0]] = (counts[m[0]] ?? 0) + 1;
  return counts;
}
function files(dir) {
  return readdirSync(dir, { withFileTypes: true })
    .flatMap((e) => (e.isDirectory() ? files(`${dir}/${e.name}`) : [`${dir}/${e.name}`]))
    .filter(
      (p) => /\.(ts|tsx)$/.test(p) && !/\.(test|spec)\.|\/__tests__\/|\/__fixtures__\//.test(p),
    );
}
export function check(root = process.cwd(), { mode = "app", paths } = {}) {
  if (mode === "pane")
    return (paths ?? builtInPaneFiles(root)).flatMap((path) =>
      Object.keys(colorCounts(readFileSync(resolve(root, path), "utf8"), { pane: true })).map(
        (literal) => `${path}: ${literal}; use a semantic kit token.`,
      ),
    );
  const allow = JSON.parse(
    readFileSync(resolve(root, "scripts/hardcoded-colors.allow.json"), "utf8"),
  );
  const errors = [];
  for (const path of files(resolve(root, "src"))) {
    const name = relative(root, path);
    for (const [literal, count] of Object.entries(colorCounts(readFileSync(path, "utf8")))) {
      const baseline = allow[name]?.[literal] ?? 0;
      if (count > baseline)
        errors.push(
          `${name}: ${literal}: ${baseline} → ${count}; counts may only go down. Use a semantic token.`,
        );
    }
  }
  return errors;
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const pane = process.argv.includes("--panes");
  const paths = process.argv.slice(2).filter((arg) => arg !== "--panes");
  const errors = pane
    ? check(process.cwd(), { mode: "pane", paths: paths.length ? paths : undefined })
    : [...check(), ...check(process.cwd(), { mode: "pane" })];
  if (errors.length) {
    console.error(errors.join("\n"));
    process.exitCode = 1;
  } else console.log("Hard-coded colour ratchet passed (no counts increased).");
}
