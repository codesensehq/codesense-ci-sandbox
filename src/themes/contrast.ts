// SPDX-License-Identifier: MIT
import {
  compositeOver,
  contrastRatio,
  CONTRAST_THRESHOLDS as T,
} from "../shared/lib/color-contrast";
import { SURFACES, type ThemeMode } from "./compiler";
import { EDITOR_ACTIVE_LINE_ROLE } from "./css";
export interface ContrastPair {
  name: string;
  foreground: string;
  background: string;
  ratio: number;
  min: number;
}
/** Paths mirror the published design proof; colours come from the compiled cascade. */
export function contrastPairs(
  tokens: Record<string, string>,
  _mode: ThemeMode,
  terminalSeverity = false,
): ContrastPair[] {
  const out: ContrastPair[] = [];
  const v = (p: string) => tokens[`--theme-${p.split(".").join("-")}`];
  const add = (fg: string, bg: string, min: number, name = fg) => {
    const f = compositeOver(fg, bg);
    out.push({ name, foreground: f, background: bg, ratio: contrastRatio(f, bg), min });
  };
  const pair = (fg: string, bg: string, min: number) => add(v(fg), bg, min, fg);
  for (const s of SURFACES) {
    const states =
      s === "viewer" ? ["bg", "hover", "selected", "raised", "inset"] : ["bg", "hover", "selected"];
    const base = v(`surface.${s}.bg`);
    for (const st of states) {
      const bg = compositeOver(v(`surface.${s}.${st}`), base);
      const primary = st === "selected" && v(`text.${s}.selected`) ? "selected" : "primary";
      pair(`text.${s}.${primary}`, bg, T.text);
      pair(`text.${s}.muted`, bg, T.text);
      if (s !== "terminal") pair("focus.ring", bg, T.nonText);
    }
    pair(`text.${s}.muted`, v(`text.${s}.primary`), T.mutedDistinct);
    if (s !== "terminal") pair("border.control", base, T.nonText);
    // CodeSense's light terminal intentionally uses the dark severity set.
    if (s !== "terminal" || terminalSeverity) {
      for (const name of ["info", "success", "warning", "danger"]) {
        const fg = `severitySurface.${s}.${name}.fg`;
        pair(fg, base, T.text);
        pair(fg, compositeOver(v(`severitySurface.${s}.${name}.bg`), base), T.text);
      }
    }
  }
  pair("accent.fg", v("accent.default"), T.text);
  for (const s of ["canvas", "sidebar", "viewer", "overlay"])
    pair("accent.text", v(`surface.${s}.bg`), T.text);
  for (const s of ["sidebar", "viewer"])
    for (const st of ["bg", "hover", "selected"])
      for (const k of ["added", "modified", "deleted", "renamed"])
        pair(`git.${k}`, compositeOver(v(`surface.${s}.${st}`), v(`surface.${s}.bg`)), T.text);
  for (const background of ["surface.viewer.bg", EDITOR_ACTIVE_LINE_ROLE, "surface.viewer.inset"])
    for (const k of [
      "keyword",
      "string",
      "comment",
      "number",
      "constant",
      "function",
      "variable",
      "type",
      "operator",
      "punctuation",
      "meta",
      "regexp",
    ])
      pair(`code.${k}`, compositeOver(v(background), v("surface.viewer.bg")), T.text);
  out.push(
    ...terminalPairs(
      Object.fromEntries(
        Object.entries(tokens)
          .filter(([k]) => k.startsWith("--theme-terminal-"))
          .map(([k, v]) => [k.slice("--theme-terminal-".length), v]),
      ),
    ),
  );
  return out;
}
export function terminalPairs(t: Record<string, string>): ContrastPair[] {
  const out: ContrastPair[] = [];
  const add = (name: string, fg: string, bg: string, min: number) =>
    out.push({ name, foreground: fg, background: bg, ratio: contrastRatio(fg, bg), min });
  add("terminal.fg", t.fg, t.bg, T.text);
  add("terminal.selection", t.fg, compositeOver(t.selection, t.bg), T.text);
  add("terminal.cursor", t.cursor, t.bg, T.nonText);
  for (let i = 1; i < 16; i++) add(`terminal.ansi.${i}`, t[`ansi-${i}`], t.bg, T.ansi);
  return out;
}
