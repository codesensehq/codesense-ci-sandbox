// SPDX-License-Identifier: MIT
import schema from "./theme.schema.json";

export type ThemeMode = "light" | "dark";
export type Surface = "canvas" | "sidebar" | "viewer" | "terminal" | "overlay";
export const SURFACES: Surface[] = ["canvas", "sidebar", "viewer", "terminal", "overlay"];
export const THEME_LIMITS = { bytes: 256 * 1024, tokens: 2048, aliasDepth: 1 } as const;
export const THEME_FONTS = ["Geist Variable", "ui-monospace", "sans-serif", "monospace"] as const;
export interface ColorValue {
  colorSpace: "srgb";
  components: number[];
  alpha?: number;
}
export interface Token {
  $type: "color" | "dimension" | "fontFamily" | "number";
  $value: ColorValue | { value: number; unit: string } | string | number;
}
export interface Group {
  [key: string]: Group | Token;
}
export interface Theme {
  id: string;
  name: string;
  light: Group;
  dark: Group;
}
const plainName = (s: string) =>
  // eslint-disable-next-line no-control-regex -- Display names must strip control and bidi characters.
  s.replace(/[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, "");
function fail(path: string, reason: string): never {
  throw new Error(`${path}: ${reason}`);
}
const object = (v: unknown): v is Record<string, unknown> =>
  !!v && typeof v === "object" && !Array.isArray(v);
const finite = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
const alias = /^\{((?:light|dark)(?:\.[a-zA-Z0-9_-]+)+)\}$/;

/** Validate the bounded DTCG subset before any value can reach a stylesheet. */
export function validateTheme(input: unknown): Theme {
  if (new TextEncoder().encode(JSON.stringify(input)).length > THEME_LIMITS.bytes)
    fail("theme", "exceeds 256 KB");
  if (!object(input)) fail("theme", "expected object");
  for (const key of Object.keys(input)) if (!(key in schema.properties)) fail(key, "unknown field");
  if (typeof input.id !== "string" || !/^[a-z][a-z0-9-]*$/.test(input.id))
    fail("id", "invalid identifier");
  if (typeof input.name !== "string" || !plainName(input.name).trim())
    fail("name", "expected plain text");
  const tokens = new Map<string, Token>();
  const walk = (value: unknown, path: string) => {
    if (!object(value)) fail(path, "expected group or token");
    if ("$type" in value || "$value" in value) {
      if (Object.keys(value).some((k) => !["$type", "$value", "$description"].includes(k)))
        fail(path, "unknown token field");
      if (!["color", "dimension", "fontFamily", "number"].includes(String(value.$type)))
        fail(path, "unknown $type");
      if (!("$value" in value)) fail(path, "missing $value");
      tokens.set(path, value as unknown as Token);
      if (tokens.size > THEME_LIMITS.tokens) fail(path, "token cap exceeded");
      return;
    }
    for (const [k, v] of Object.entries(value)) {
      if (k === "$description" && typeof v === "string") continue;
      if (!/^[a-zA-Z0-9_-]+$/.test(k)) fail(`${path}.${k}`, "invalid group/token key");
      walk(v, `${path}.${k}`);
    }
  };
  for (const mode of ["light", "dark"] as const) {
    if (!object(input[mode])) fail(mode, "required mode missing");
    walk(input[mode], mode);
  }
  for (const [path, token] of tokens) {
    let value: unknown = token.$value;
    if (typeof value === "string" && alias.test(value)) {
      const target = tokens.get(value.slice(1, -1));
      if (!target || target.$type !== token.$type) fail(path, "missing alias or type mismatch");
      if (typeof target.$value === "string" && alias.test(target.$value))
        fail(path, "alias depth exceeds 1");
      value = target.$value;
    }
    switch (token.$type) {
      case "color":
        if (
          !object(value) ||
          value.colorSpace !== "srgb" ||
          !Array.isArray(value.components) ||
          value.components.length !== 3 ||
          !value.components.every((c) => finite(c) && c >= 0 && c <= 1) ||
          (value.alpha !== undefined &&
            (!finite(value.alpha) || value.alpha < 0 || value.alpha > 1)) ||
          Object.keys(value).some((k) => !["colorSpace", "components", "alpha"].includes(k))
        )
          fail(path, "expected typed sRGB color");
        break;
      case "dimension":
        if (
          !object(value) ||
          !finite(value.value) ||
          !["px", "rem", "em", "%", "ms"].includes(String(value.unit)) ||
          Object.keys(value).some((k) => !["value", "unit"].includes(k))
        )
          fail(path, "expected number and allowed unit");
        break;
      case "fontFamily":
        if (!THEME_FONTS.includes(value as (typeof THEME_FONTS)[number]))
          fail(path, "font is not allow-listed");
        break;
      case "number":
        if (!finite(value)) fail(path, "expected finite number");
    }
  }
  return { ...input, name: plainName(input.name as string) } as unknown as Theme;
}

export function colorCss(value: ColorValue): string {
  const byte = (n: number) =>
    Math.round(n * 255)
      .toString(16)
      .padStart(2, "0");
  return `#${value.components.map(byte).join("")}${value.alpha !== undefined && value.alpha < 1 ? byte(value.alpha) : ""}`;
}

/** Only validated typed data is serialized, never arbitrary CSS text. */
export function compileTheme(input: unknown, mode: ThemeMode): Record<string, string> {
  const theme = validateTheme(input);
  if (mode !== "light" && mode !== "dark") fail("mode", "expected light or dark");
  const output: Record<string, string> = {};
  const lookup = (path: string): Token =>
    path.split(".").reduce<unknown>((v, k) => (v as Record<string, unknown>)[k], theme) as Token;
  const walk = (group: Group, parts: string[]) => {
    for (const [key, child] of Object.entries(group)) {
      if (key.startsWith("$")) continue;
      const path = [...parts, key];
      if (!("$type" in child)) {
        walk(child as Group, path);
        continue;
      }
      const token = child as Token;
      const v =
        typeof token.$value === "string" && alias.test(token.$value)
          ? lookup(token.$value.slice(1, -1)).$value
          : token.$value;
      output[`--theme-${path.join("-")}`] =
        token.$type === "color"
          ? colorCss(v as ColorValue)
          : token.$type === "dimension"
            ? `${(v as { value: number }).value}${(v as { unit: string }).unit}`
            : token.$type === "fontFamily" && v === "Geist Variable"
              ? '"Geist Variable"'
              : String(v);
    }
  };
  walk(theme[mode], []);
  return output;
}
