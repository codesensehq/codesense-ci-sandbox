// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, type CSSProperties } from "react";
import { isInHiddenTeam } from "@/shared/lib/warm-scopes";
import { useTheme } from "next-themes";
import { AlertTriangle, Check } from "lucide-react";
import { toast } from "sonner";
import { persistent } from "@/components/ui/persistent-toast";
import { Button } from "@/components/ui/button";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { BUNDLED_THEMES, DEFAULT_APP_THEME, themeTokens, type AppTheme } from "@/themes";
import { resolveTerminalTheme } from "@/themes/terminal";
import { TERMINAL_THEME_OPTIONS } from "@/shared/constants/settings-options";
import { rowMatches, type SettingRowSpec } from "../lib/row-matches";
import { SectionHeader } from "./section-header";
import { SettingRow } from "./setting-row";
import "./appearance-section.css";

export const APPEARANCE_ROW_SPECS: SettingRowSpec[] = [
  { key: "mode", label: "Mode", keywords: ["light", "dark", "system", "appearance"] },
  {
    key: "appTheme",
    label: "Theme",
    keywords: [
      "theme",
      "appearance",
      "colours",
      "colors",
      "codesense",
      "dracula",
      "github",
      "nord",
      "catppuccin",
      "confluence",
    ],
  },
  {
    key: "terminalTheme",
    label: "Terminal colours",
    keywords: ["terminal", "palette", "colours", "colors", "theme"],
  },
];
const SUBTITLES: Record<AppTheme, string> = {
  codesense: "The original",
  dracula: "Alucard · Dracula",
  github: "Primer light · dark",
  nord: "Snow Storm · Polar Night",
  catppuccin: "Latte · Mocha",
  confluence: "Atlassian light · dark",
};
const TOAST_ID = "appearance-theme";

function ThemePreview({ id, mode }: { id: AppTheme; mode: "light" | "dark" }) {
  const t = themeTokens(id, mode),
    v = (role: string) => t[`--theme-${role}`];
  const style = {
    "--p-canvas": v("surface-canvas-bg"),
    "--p-sidebar": v("surface-sidebar-bg"),
    "--p-viewer": v("surface-viewer-bg"),
    "--p-fg": v("text-viewer-primary"),
    "--p-muted": v("text-viewer-muted"),
    "--p-side-fg": v("text-sidebar-primary"),
    "--p-selected": v("surface-sidebar-selected"),
    "--p-selected-fg": v("text-sidebar-selected") ?? v("text-sidebar-primary"),
    "--p-border": v("border-default"),
    "--p-link": v("accent-text"),
    "--p-accent": v("accent-default"),
    "--p-accent-fg": v("accent-fg"),
    "--p-warn": v("severity-warning-fg"),
    "--p-warn-bg": v("severity-warning-bg"),
    "--p-added": v("git-added"),
    "--p-modified": v("git-modified"),
    "--p-terminal": v("terminal-bg"),
    "--p-terminal-fg": v("terminal-fg"),
    "--p-green": v("terminal-ansi-2"),
  } as CSSProperties;
  return (
    <div className="theme-preview" style={style} aria-hidden="true">
      <div className="preview-titlebar">
        CodeSense <span>workspace</span>
      </div>
      <div className="preview-sidebar">
        <div>project</div>
        <div className="preview-row selected">
          release.md <b style={{ color: "var(--p-modified)" }}>M</b>
        </div>
        <div className="preview-row">
          app.ts <b style={{ color: "var(--p-added)" }}>A</b>
        </div>
        <div className="preview-row">README.md</div>
      </div>
      <div className="preview-document">
        <strong>Release notes</strong>
        <div className="preview-muted">A calmer place to build.</div>
        <span className="preview-link">Getting started</span>
        <div className="preview-alert">
          <AlertTriangle /> Check before publishing
        </div>
        <span className="preview-button">Publish</span>
      </div>
      <div className="preview-terminal">
        $ pnpm test
        <br />
        <span style={{ color: "var(--p-green)" }}>✓</span> All checks passed
      </div>
    </div>
  );
}

export function AppearanceSection({ query }: { query: string }) {
  const { theme: preference, resolvedTheme, setTheme } = useTheme();
  const mode = resolvedTheme === "dark" ? "dark" : "light";
  const id = useViewerStore((s) => s.settings.appTheme) ?? DEFAULT_APP_THEME;
  const terminalChoice = useViewerStore((s) => s.settings.terminalTheme);
  const update = useViewerStore((s) => s.updateSettings);
  const root = useRef<HTMLDivElement>(null),
    origin = useRef<AppTheme | null>(null),
    settle = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const undo = useCallback(() => {
    const previous = origin.current;
    if (!previous) return;
    clearTimeout(settle.current);
    origin.current = null;
    toast.dismiss(TOAST_ID);
    update({ appTheme: previous });
    // Let the toast finish its own blur restoration before returning to the selected radio.
    // A retained toast can outlive the pane's team being shown: its radio cannot take focus then.
    if (root.current && isInHiddenTeam(root.current)) return;
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
    root.current
      ?.querySelector<HTMLInputElement>(`input[name="app-theme"][value="${previous}"]`)
      ?.focus();
  }, [update]);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (
        !(e.metaKey || e.ctrlKey) ||
        e.shiftKey ||
        e.altKey ||
        e.key.toLowerCase() !== "z" ||
        !origin.current ||
        (root.current && isInHiddenTeam(root.current))
      )
        return;
      const target = e.target as HTMLElement | null;
      if (
        target?.isContentEditable ||
        target?.tagName === "TEXTAREA" ||
        (target?.tagName === "INPUT" && (target as HTMLInputElement).type !== "radio")
      )
        return;
      e.preventDefault();
      e.stopPropagation();
      undo();
    };
    document.addEventListener("keydown", key, true);
    return () => {
      document.removeEventListener("keydown", key, true);
      clearTimeout(settle.current);
      toast.dismiss(TOAST_ID);
    };
  }, [undo]);
  const choose = (next: AppTheme) => {
    if (next === id) return;
    origin.current ??= id;
    clearTimeout(settle.current);
    update({ appTheme: next });
    settle.current = setTimeout(
      () =>
        toast(
          `Theme changed to ${BUNDLED_THEMES[next].name}`,
          persistent({
            id: TOAST_ID,
            action: { label: "Undo", onClick: undo },
            onDismiss: () => {
              origin.current = null;
            },
          }),
        ),
      700,
    );
  };
  const visibleRows = APPEARANCE_ROW_SPECS.filter((s) => rowMatches(s, query));
  const show = (key: string) => visibleRows.some((s) => s.key === key);
  return (
    <div
      ref={root}
      className="appearance-content"
      data-surface="canvas"
      data-testid="settings-appearance"
    >
      <SectionHeader id="appearance">Appearance</SectionHeader>
      <div className="sr-only" aria-live="polite" data-testid="settings-search-count">
        {query.trim() ? `${visibleRows.length} settings shown` : ""}
      </div>
      {!visibleRows.length && (
        <p className="py-4 text-sm text-muted-foreground">No matching settings.</p>
      )}
      {show("mode") && (
        <div className="appearance-mode-row">
          <SettingRow label="Mode">
            <fieldset className="appearance-modes">
              <legend className="sr-only">Mode</legend>
              {(["light", "dark", "system"] as const).map((value) => (
                <label key={value}>
                  <input
                    type="radio"
                    name="app-mode"
                    value={value}
                    checked={(preference ?? "system") === value}
                    onChange={() => setTheme(value)}
                  />
                  <span>{value[0].toUpperCase() + value.slice(1)}</span>
                </label>
              ))}
            </fieldset>
          </SettingRow>
        </div>
      )}
      {show("appTheme") && (
        <section className="appearance-theme-section" aria-labelledby="app-theme-label">
          <div className="appearance-theme-heading">
            <div>
              <h3 id="app-theme-label">Theme</h3>
              <p>
                One theme for the app, documents and panes. Every theme includes light and dark.
              </p>
            </div>
            {id !== DEFAULT_APP_THEME && (
              <div className="theme-reset">
                <span>Changed</span>
                <Button variant="ghost" size="sm" onClick={() => choose(DEFAULT_APP_THEME)}>
                  Reset to CodeSense
                </Button>
              </div>
            )}
          </div>
          <fieldset className="appearance-themes">
            <legend className="sr-only">Theme</legend>
            {(Object.keys(BUNDLED_THEMES) as AppTheme[]).map((value) => (
              <label className="appearance-theme-card" key={value}>
                <input
                  type="radio"
                  name="app-theme"
                  value={value}
                  aria-label={BUNDLED_THEMES[value].name}
                  aria-describedby={`theme-desc-${value}`}
                  checked={id === value}
                  onChange={() => choose(value)}
                />
                <ThemePreview id={value} mode={mode} />
                <span className="theme-card-caption">
                  <strong>{BUNDLED_THEMES[value].name}</strong>
                  {id === value && (
                    <span className="theme-selected">
                      <Check size={12} />
                      Selected
                    </span>
                  )}
                </span>
                <span className="theme-description" id={`theme-desc-${value}`}>
                  {SUBTITLES[value]}
                </span>
              </label>
            ))}
          </fieldset>
        </section>
      )}
      {show("terminalTheme") && (
        <section className="appearance-terminal-section" aria-labelledby="terminal-colours-label">
          <h3 id="terminal-colours-label">Terminal colours</h3>
          <p>
            Your choice applies everywhere. A chosen palette stays the same when you switch themes.
          </p>
          <fieldset className="appearance-terminal-options">
            <legend className="sr-only">Terminal colours</legend>
            {TERMINAL_THEME_OPTIONS.map((option) => {
              const config = resolveTerminalTheme(option.value, id, mode);
              return (
                <label key={option.value} className="appearance-terminal-option">
                  <input
                    type="radio"
                    name="terminal-colours"
                    value={option.value}
                    checked={terminalChoice === option.value}
                    onChange={() => update({ terminalTheme: option.value })}
                  />
                  <span>
                    {option.label}
                    {option.value === "match-app" && (
                      <small>
                        {BUNDLED_THEMES[id].name} · {mode}
                      </small>
                    )}
                  </span>
                  <code
                    aria-hidden="true"
                    style={{ background: config.theme.background, color: config.theme.foreground }}
                  >
                    $ pnpm test
                    <br />
                    <span style={{ color: config.theme.green }}>✓</span> All checks passed
                  </code>
                </label>
              );
            })}
          </fieldset>
        </section>
      )}
    </div>
  );
}
