// SPDX-License-Identifier: MIT
import { kitRoles } from "@/themes/kit";
/**
 * The theme a live HTML doc is given: the app's tokens as resolved where the
 * doc's frame actually sits, so a pane on the right pane's surface (which follows
 * the viewer theme and can be dark while the app is light) gets that surface's
 * colours, not the app chrome's.
 */
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { parseHexColor, relativeLuminance, toHexColor } from "@/shared/lib/color-contrast";
import { MANAGED_TREE_ACCENTS } from "@/shared/lib/pipeline-graph/epic-colors";
import { liveHub } from "../lib/live-hub";
import type { Surface } from "@/themes/compiler";

export interface DocTheme {
  scheme: "dark" | "light";
  /** CSS custom properties the doc's root gets, `--cs-*` → a resolved value. */
  tokens: Record<string, string>;
}

/** Each doc token and the app properties it reads from, first non-empty wins.
 *  `color` and `font-family` are the frame's own computed values. A viewer theme
 *  (which sets `--cs-viewer-bg`) owns the surface, so its own palette comes first;
 *  the app chrome's accent would not read on it. */
const TOKEN_SOURCES: Array<[string, string[]]> = [
  ["--cs-bg", ["--cs-viewer-bg", "--background"]],
  ["--cs-fg", ["color", "--foreground"]],
  ["--cs-muted-fg", ["--cs-sidebar-text-muted", "--muted-foreground"]],
  ["--cs-border", ["--cs-sidebar-border", "--border"]],
  ["--cs-surface", ["--cs-workspace-card-bg", "--card"]],
  ["--cs-accent", ["--cs-viewer-accent", "--primary"]],
  ["--cs-accent-fg", ["--cs-viewer-bg", "--primary-foreground"]],
  ["--cs-highlight", ["--accent"]],
  ["--cs-code-comment", ["--cs-code-comment"]],
  ["--cs-code-keyword", ["--cs-code-keyword"]],
  ["--cs-code-string", ["--cs-code-string"]],
  ["--cs-link", ["--tw-prose-links", "--primary"]],
  ["--cs-danger", ["--destructive"]],
  ["--cs-deleted", ["--cs-status-deleted"]],
  ["--cs-success", ["--cs-status-added"]],
  ["--cs-warning", ["--cs-status-modified"]],
  ["--cs-font-sans", ["font-family"]],
  ["--cs-sidebar-bg", ["--cs-sidebar-bg"]],
  ["--cs-sidebar-fg", ["--cs-sidebar-text"]],
];

/** Relative luminance (0..1) of a hex or `rgb()/rgba()` colour; null for anything else. */
export function luminance(color: string): number | null {
  const c = color.trim().toLowerCase();
  const fn = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/.exec(c);
  if (fn) {
    return relativeLuminance(toHexColor([Number(fn[1]), Number(fn[2]), Number(fn[3])]));
  }
  return parseHexColor(c) ? relativeLuminance(c) : null;
}

/** The theme for a doc framed in `el`, read from its computed style. */
export function readDocTheme(el: Element): DocTheme {
  const style = getComputedStyle(el);
  const raw = (prop: string) => style.getPropertyValue(prop).trim();
  // A viewer theme's accent is its heading colour.
  const read = (prop: string) =>
    prop === "--cs-viewer-accent"
      ? raw("--cs-viewer-bg")
        ? raw("--tw-prose-headings")
        : ""
      : raw(prop);
  const tokens: Record<string, string> = {};
  for (const [token, sources] of TOKEN_SOURCES) {
    for (const prop of sources) {
      const value = read(prop);
      if (value) {
        tokens[token] = value;
        break;
      }
    }
  }
  const surface = el.closest("[data-surface]")?.getAttribute("data-surface") as Surface | null;
  if (surface)
    for (const [name, role] of Object.entries(
      kitRoles(surface, (role) => !!raw(`--theme-${role}`)),
    )) {
      const value = raw(`--theme-${role}`);
      if (value) tokens[`--cs-${name}`] = value;
    }
  const bg = tokens["--cs-bg"];
  const lum = bg ? luminance(bg) : null;
  const dark = lum === null ? document.documentElement.classList.contains("dark") : lum < 0.4;
  // Policy: in a light app the native note sheet uses neutral ink on light viewer themes.
  if (
    !surface &&
    !dark &&
    !document.documentElement.classList.contains("dark") &&
    raw("--foreground")
  ) {
    tokens["--cs-fg"] = raw("--foreground");
  }
  // A management tree's guide colour is `--cs-tree-accent-${hash(teamKey|treeHandle) % n + 1}`,
  // the same rule as the built-in list's `getManagedTreeAccent`.
  MANAGED_TREE_ACCENTS[dark ? "dark" : "light"].forEach((color, i) => {
    tokens[`--cs-tree-accent-${i + 1}`] = color;
  });
  return { scheme: dark ? "dark" : "light", tokens };
}

function sameTheme(a: DocTheme | null, b: DocTheme): boolean {
  if (!a || a.scheme !== b.scheme) return false;
  const ak = Object.keys(a.tokens);
  return ak.length === Object.keys(b.tokens).length && ak.every((k) => a.tokens[k] === b.tokens[k]);
}

let nextThemeSubscriber = 0;
/** A frame is a consumer of the one hub topic; hidden frames resolve once on show. */
export function useDocTheme(
  frame: Element | null,
  enabled: boolean,
  visible = true,
): DocTheme | null {
  const [theme, setTheme] = useState<DocTheme | null>(null);
  const subscription = useRef<ReturnType<typeof liveHub.attach> | null>(null);
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  useLayoutEffect(() => {
    if (!enabled || !frame) return;
    const refresh = () => {
      const next = readDocTheme(frame);
      setTheme((prev) => (sameTheme(prev, next) ? prev : next));
    };
    const sub = liveHub.attach(
      `theme-consumer-${++nextThemeSubscriber}`,
      "all",
      () => {},
      visibleRef.current,
      true,
      refresh,
    );
    subscription.current = sub;
    if (visibleRef.current) refresh();
    return () => {
      subscription.current = null;
      sub.dispose();
    };
  }, [enabled, frame]);
  useEffect(() => subscription.current?.visible(visible), [visible]);
  return enabled && frame ? theme : null;
}
