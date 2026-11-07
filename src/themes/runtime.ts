// SPDX-License-Identifier: MIT
import { resolveTerminalTheme, ANSI_NAMES } from "./terminal";
import {
  mixHex,
  contrastRatio,
  CONTRAST_THRESHOLDS,
  relativeLuminance,
} from "@/shared/lib/color-contrast";
import type { TerminalTheme } from "@/shared/stores/viewer-store";
import { themeTokens, type AppTheme } from "./index";
import type { ThemeMode } from "./compiler";

/** Apply typed custom properties before publishing the one hub revision. */
export function applyTheme(
  id: AppTheme,
  mode: ThemeMode,
  root = document.documentElement,
  terminalChoice: TerminalTheme = "match-app",
) {
  const tokens = { ...themeTokens(id, mode) };
  if (terminalChoice !== "match-app") {
    const config = resolveTerminalTheme(terminalChoice, id, mode),
      t = config.theme;
    tokens["--theme-terminal-bg"] = tokens["--theme-surface-terminal-bg"] = t.background!;
    tokens["--theme-terminal-fg"] = tokens["--theme-text-terminal-primary"] = t.foreground!;
    tokens["--theme-terminal-cursor"] = t.cursor!;
    tokens["--theme-terminal-selection"] = tokens["--theme-surface-terminal-selected"] =
      t.selectionBackground!;
    tokens["--theme-surface-terminal-hover"] = config.chromeBg;
    const dark = themeTokens("codesense", "dark");
    const muted = dark["--theme-text-terminal-muted"];
    tokens["--theme-text-terminal-muted"] =
      contrastRatio(muted, t.background!) >= CONTRAST_THRESHOLDS.text
        ? muted
        : mixHex(t.background!, t.foreground!, 0.85);
    for (const state of ["needs", "failed", "busy", "done", "idle", "text"])
      tokens[`--theme-status-terminal-${state}`] = dark[`--theme-status-terminal-${state}`];
    for (const role of ["info", "success", "warning", "danger"])
      for (const part of ["fg", "bg", "border"])
        tokens[`--theme-severitySurface-terminal-${role}-${part}`] =
          dark[`--theme-severity-${role}-${part}`];
    for (const [i, name] of ANSI_NAMES.entries()) tokens[`--theme-terminal-ansi-${i}`] = t[name]!;
  }
  const darkTerminal = relativeLuminance(tokens["--theme-terminal-bg"]) < 0.4;
  tokens["--cs-terminal-face-l"] = darkTerminal ? "34%" : "90%";
  tokens["--cs-terminal-piece-l"] = darkTerminal ? "82%" : "50%";
  tokens["--cs-terminal-scheme"] = darkTerminal ? "dark" : "light";
  root.classList.toggle("dark", mode === "dark");
  root.dataset.appTheme = id;
  root.dataset.surface = "canvas";
  // Replace one rule atomically; setting hundreds of inline properties forces repeated serialization.
  const doc = root.ownerDocument;
  let style = doc.getElementById("app-theme-values") as HTMLStyleElement | null;
  if (!style) {
    style = doc.createElement("style");
    style.id = "app-theme-values";
    doc.head.append(style);
  }
  const css = `:root:root:root {${Object.entries(tokens)
    .map(([key, value]) => `${key}:${value}`)
    .join(";")}}`;
  if (style.textContent !== css) style.textContent = css;
  root.style.colorScheme = mode;
}
