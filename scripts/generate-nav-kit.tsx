// SPDX-License-Identifier: MIT
import React from "react";
import { readFileSync, writeFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { StageRailShape } from "../src/shared/components/stage-rail";
import { StatusMark } from "../src/shared/components/status-mark";
import {
  AGENT_DEFINITION_PATTERNS,
  DASHBOARD_DIR_PATTERN,
  FileKindIcon,
  FILE_KIND_EXTENSIONS,
  FILE_KINDS,
  fileKindOf,
} from "../src/shared/lib/icons/file-kind";
import { STATUS_MARKS } from "../src/shared/lib/status-mark";
import {
  ChangesIcon,
  GroupedIcon,
  HistoryIcon,
} from "../src/features/dashboard/components/status-nav/nav-filter-field";
import { VIEWER_EXTENSIONS, stripViewerExtension } from "../src/shared/lib/viewer-file-type";
import { stripPipelineExtension } from "../src/shared/lib/pipeline-graph/pipeline-utils";
import { PIPELINE_EXTENSIONS } from "../src/shared/lib/pipeline-graph/pipeline-file-filter";
import { NAV_LENSES, inLens, stageGroupOf } from "../src/shared/lib/nav-lens";
import {
  compareNavNames,
  NAV_NAME_COLLATOR_OPTIONS,
} from "../src/features/dashboard/lib/nav-ordering";

Object.assign(globalThis, { React });

const path = "src-tauri/src/http/routes/dashboard_kit/kit.js";
const cssPath = "src-tauri/src/http/routes/dashboard_kit/kit.css";
const rails = Object.fromEntries(
  ([undefined, "not_started", "active", "review", "blocked", "done"] as const).flatMap((role) =>
    [false, true].map((ready) => [
      `${role ?? "off"}:${ready}`,
      renderToStaticMarkup(<StageRailShape role={role} ready={ready} />),
    ]),
  ),
);
const marks = Object.fromEntries(
  STATUS_MARKS.flatMap((mark) =>
    [6, 7, 8].map((size) => [
      `${mark}:${size}`,
      renderToStaticMarkup(<StatusMark mark={mark} size={size} />)
        .replaceAll(/id="[^"]+"/g, 'id="CS_MASK"')
        .replaceAll(/url\(#[^)]+\)/g, "url(#CS_MASK)"),
    ]),
  ),
);
const files = Object.fromEntries(
  FILE_KINDS.map((kind) => [kind, renderToStaticMarkup(<FileKindIcon kind={kind} />)]),
);
const navGlyphs = {
  changes: renderToStaticMarkup(<ChangesIcon />),
  history: renderToStaticMarkup(<HistoryIcon />),
  grouped: renderToStaticMarkup(<GroupedIcon />),
};
const extensions = [...FILE_KIND_EXTENSIONS];
const fileKinds = Object.fromEntries(
  extensions.flatMap((ext) => {
    const value = fileKindOf(`file.${ext}`);
    return value ? [[ext, value]] : [];
  }),
);
// What a dashboards/ folder changes: only the extensions whose kind differs there.
const dashboardFileKinds = Object.fromEntries(
  extensions.flatMap((ext) => {
    const value = fileKindOf(`dashboards/file.${ext}`);
    return value === fileKinds[ext] ? [] : [[ext, value]];
  }),
);
const roleValues = [undefined, "not_started", "active", "review", "blocked", "done"] as const;
const markValues = [undefined, ...STATUS_MARKS] as const;
const lensStories = roleValues.flatMap((role) =>
  markValues.flatMap((mark) => [false, true].map((ready) => ({ role, mark, ready }))),
);
const lensMasks = Object.fromEntries(
  lensStories.map((story) => [
    `${story.role ?? ""}|${story.mark ?? ""}|${String(story.ready)}`,
    NAV_LENSES.map(({ lens }) => Number(inLens(lens, story))).join(""),
  ]),
);
const stageTable = Object.fromEntries(
  roleValues.flatMap((role) =>
    markValues.map((mark) => {
      const story = { role, mark };
      return [`${role ?? ""}|${mark ?? ""}`, stageGroupOf(story)];
    }),
  ),
);
const ruleData = {
  fileKinds,
  dashboardFileKinds,
  agentDefinitionPatterns: AGENT_DEFINITION_PATTERNS.map(({ source, flags }) => [source, flags]),
  dashboardDirPattern: [DASHBOARD_DIR_PATTERN.source, DASHBOARD_DIR_PATTERN.flags],
  defaultFileKind: fileKindOf("file.__codesense_unknown_extension__"),
  stripViewerExtensions: Object.keys(VIEWER_EXTENSIONS).filter(
    (ext) => stripViewerExtension(`file.${ext}`) === "file",
  ),
  pipelineExtensions: [...PIPELINE_EXTENSIONS].filter(
    (ext) => stripPipelineExtension(`file.${ext}`) === "file",
  ),
  lenses: NAV_LENSES.map(({ lens }) => lens),
  lensMasks,
  stageTable,
  collatorOptions: NAV_NAME_COLLATOR_OPTIONS,
};
const rules = `var NATIVE_RULE_DATA = ${JSON.stringify(ruleData)};
  var NATIVE_FILE_KINDS = NATIVE_RULE_DATA.fileKinds;
  var NATIVE_STRIP_VIEWER = new Set(NATIVE_RULE_DATA.stripViewerExtensions);
  var NATIVE_STRIP_PIPELINE = new Set(NATIVE_RULE_DATA.pipelineExtensions);
  var NATIVE_LENS_MASKS = NATIVE_RULE_DATA.lensMasks;
  var NATIVE_STAGE_TABLE = NATIVE_RULE_DATA.stageTable;
  var NATIVE_NAV_LENSES = NATIVE_RULE_DATA.lenses;
  var NATIVE_NAV_COLLATOR = new Intl.Collator(undefined, NATIVE_RULE_DATA.collatorOptions);
  var NATIVE_AGENT_PATTERNS = NATIVE_RULE_DATA.agentDefinitionPatterns.map(function(p) { return new RegExp(p[0], p[1]); });
  var NATIVE_DASHBOARD_DIR = new RegExp(NATIVE_RULE_DATA.dashboardDirPattern[0], NATIVE_RULE_DATA.dashboardDirPattern[1]);
  function fileKindOf(path) {
    if (NATIVE_AGENT_PATTERNS.some(function(pattern) { return pattern.test(path); })) return "agentDef";
    var base = path.slice(path.lastIndexOf("/") + 1), dot = base.lastIndexOf("."), ext = dot < 0 ? null : base.slice(dot + 1).toLowerCase();
    if (ext === null) return "text";
    if (NATIVE_DASHBOARD_DIR.test(path) && NATIVE_RULE_DATA.dashboardFileKinds[ext]) return NATIVE_RULE_DATA.dashboardFileKinds[ext];
    return NATIVE_FILE_KINDS[ext] || NATIVE_RULE_DATA.defaultFileKind;
  }
  function stripKnownExtension(name, extensions) { var dot = name.lastIndexOf("."), ext = dot < 0 ? "" : name.slice(dot + 1).toLowerCase(); return dot >= 0 && extensions.has(ext) ? name.slice(0, dot) : name; }
  function stripViewerExtension(name) { return stripKnownExtension(name, NATIVE_STRIP_VIEWER); }
  function stripPipelineExtension(name) { return stripKnownExtension(name, NATIVE_STRIP_PIPELINE); }
  function inLens(lens, story) { var index = NATIVE_NAV_LENSES.indexOf(lens), mask = NATIVE_LENS_MASKS[(story.role || "") + "|" + (story.mark || "") + "|" + String(story.ready === true)]; return index >= 0 && !!mask && mask.charAt(index) === "1"; }
  function countLenses(stories) { var counts = Object.fromEntries(NATIVE_NAV_LENSES.map(function(lens) { return [lens, 0]; })); for (var story of stories) for (var lens of NATIVE_NAV_LENSES) if (inLens(lens, story)) counts[lens]++; return counts; }
  function stageGroupOf(story) { return NATIVE_STAGE_TABLE[(story.role || "") + "|" + (story.mark || "")] || story.role || "off"; }
  function compareNavNames(a, b) { return NATIVE_NAV_COLLATOR.compare(a, b); }`;
const block = `/*@native-nav-start*/
  // Generated from the native components by scripts/generate-nav-kit.tsx.
  var NATIVE_RAILS = ${JSON.stringify(rails)};
  var NATIVE_MARKS = ${JSON.stringify(marks)};
  var NATIVE_FILES = ${JSON.stringify(files)};
  var NATIVE_NAV_GLYPHS = ${JSON.stringify(navGlyphs)};
  ${rules}
  var navMask = 0;
  function nativeGlyph(markup) {
    var template = document.createElement("template");
    template.innerHTML = markup.replaceAll("CS_MASK", "cs-kit-mask-" + (++navMask));
    return template.content.firstElementChild;
  }
  function stageRail(role, ready) { return nativeGlyph(NATIVE_RAILS[(role || "off") + ":" + !!ready]); }
  function statusMark(mark, options) {
    options = options || {};
    var size = options.size === 6 ? 6 : options.size === 8 ? 8 : 7;
    var markup = NATIVE_MARKS[mark + ":" + size];
    return nativeGlyph(options.still ? '<span class="cs-mark-still inline-flex">' + markup + "</span>" : markup);
  }
  function fileKindIcon(kind) { return nativeGlyph(NATIVE_FILES[kind]); }
  function navGlyph(name) { return nativeGlyph(NATIVE_NAV_GLYPHS[name]); }
  /*@native-nav-end*/`;
function replaceOrAppend(source: string, start: string, end: string, value: string) {
  const from = source.indexOf(start),
    to = source.indexOf(end);
  return from < 0
    ? `${source}\n${value}\n`
    : source.slice(0, from) + value + source.slice(to + end.length);
}
function output(dest: string, next: string) {
  if (process.argv.includes("--check")) {
    if (readFileSync(dest, "utf8") !== next)
      throw new Error(`Stale generated file: ${dest}; run pnpm gen:nav-kit`);
  } else writeFileSync(dest, next);
}
let js = readFileSync(path, "utf8");
if (!js.includes("/*@native-nav-start*/"))
  js = js.replace("  cs.kit = {", `${block}\n\n  cs.kit = {`);
else js = replaceOrAppend(js, "/*@native-nav-start*/", "/*@native-nav-end*/", block);
if (!js.includes("stageRail: stageRail"))
  js = js.replace(
    '    version: "1",',
    '    version: "1",\n    stageRail: stageRail,\n    statusMark: statusMark,\n    fileKindIcon: fileKindIcon,\n    navGlyph: navGlyph,',
  );
if (!js.includes("navGlyph: navGlyph"))
  js = js.replace(
    "    fileKindIcon: fileKindIcon,",
    "    fileKindIcon: fileKindIcon,\n    navGlyph: navGlyph,\n    fileKindOf: fileKindOf,\n    stripViewerExtension: stripViewerExtension,\n    stripPipelineExtension: stripPipelineExtension,\n    inLens: inLens,\n    countLenses: countLenses,\n    stageGroupOf: stageGroupOf,\n    compareNavNames: compareNavNames,",
  );
output(path, js);
const css = readFileSync("src/styles/status-mark.css", "utf8").replaceAll(
  "var(--foreground)",
  "var(--cs-fg)",
);
const cssBlock = `/*@native-nav-start*/\n/* Generated from src/styles/status-mark.css. */\n${css}\n.cs-file-kind-icon { flex: none; }\n/*@native-nav-end*/`;
output(
  cssPath,
  replaceOrAppend(
    readFileSync(cssPath, "utf8"),
    "/*@native-nav-start*/",
    "/*@native-nav-end*/",
    cssBlock,
  ),
);
console.log("native nav kit: " + (process.argv.includes("--check") ? "ok" : "generated"));
