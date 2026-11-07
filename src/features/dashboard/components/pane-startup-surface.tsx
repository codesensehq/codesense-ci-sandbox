// SPDX-License-Identifier: MIT
import { useState, type CSSProperties, type ReactNode } from "react";
import { MoreHorizontal } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import { DEFAULT_APP_THEME, isAppTheme, themeTokens } from "@/themes";
import { kitRoles } from "@/themes/kit";
import { readDocTheme, useDocTheme, type DocTheme } from "./doc-theme";

export const paneSurfaceStyle: CSSProperties = {
  background: "var(--cs-bg, var(--theme-surface-sidebar-bg, var(--background)))",
  color: "var(--cs-fg, var(--foreground))",
};

/** Resolve before navigation; later theme pushes must not change the document's URL. */
export function usePaneFirstPaint(
  documentKey: string,
  enabled = true,
  visible = true,
  inherited?: DocTheme | null,
) {
  const [element, ref] = useState<HTMLDivElement | null>(null);
  const resolved = useDocTheme(element, enabled && inherited === undefined, visible);
  const [saved, setSaved] = useState<{ key: string; theme: string } | null>(null);
  let theme =
    inherited === undefined ? (resolved ?? (element ? readDocTheme(element) : null)) : inherited;
  if (element && !theme?.tokens["--cs-bg"]) {
    const id = document.documentElement.dataset.appTheme;
    const scheme = document.documentElement.classList.contains("dark") ? "dark" : "light";
    const vars = themeTokens(isAppTheme(id) ? id : DEFAULT_APP_THEME, scheme);
    const surface = element.closest("[data-surface]")?.getAttribute("data-surface");
    const roles = kitRoles(
      surface === "sidebar" ||
        surface === "terminal" ||
        surface === "canvas" ||
        surface === "overlay"
        ? surface
        : "viewer",
      (role) => `--theme-${role}` in vars,
    );
    theme = {
      scheme,
      tokens: Object.fromEntries(
        Object.entries(roles).map(([name, role]) => [`--cs-${name}`, vars[`--theme-${role}`]]),
      ),
    };
  }
  let initialTheme = saved?.key === documentKey ? saved.theme : undefined;
  if (!initialTheme && theme) {
    initialTheme = JSON.stringify(theme);
    setSaved({ key: documentKey, theme: initialTheme });
  }
  const style: CSSProperties = theme
    ? {
        ...theme.tokens,
        background: theme.tokens["--cs-bg"],
        color: theme.tokens["--cs-fg"],
        colorScheme: theme.scheme,
      }
    : paneSurfaceStyle;
  return { ref, theme: theme as DocTheme | null, initialTheme, style };
}

/** Presentation shared by visible hosts and off-screen replacement candidates. */
export function PaneStartupSurface({
  title,
  state,
  onReload,
  onEdit,
  children,
}: {
  title: string;
  state: "starting" | "ready" | "recovering";
  onReload?: () => void;
  onEdit?: () => void;
  children?: ReactNode;
}) {
  const options = onReload && (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={`${title} pane options`}
          className="flex size-7 items-center justify-center rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <MoreHorizontal className="size-4" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem onSelect={onReload}>Reload pane</DropdownMenuItem>
        {onEdit && <DropdownMenuItem onSelect={onEdit}>Open pane source</DropdownMenuItem>}
      </DropdownMenuContent>
    </DropdownMenu>
  );
  if (state === "ready") return null;
  if (state === "recovering")
    return (
      <div className="absolute right-2 top-1 z-10 flex items-center gap-2 text-xs text-muted-foreground">
        <span className="pointer-events-none">Updating…</span>
        {options}
      </div>
    );
  return (
    <div
      data-testid="pane-startup-placeholder"
      className="absolute inset-0 z-10 flex flex-col"
      style={paneSurfaceStyle}
    >
      <div className="flex h-10 shrink-0 items-center gap-2 px-3 text-xs">
        <span className="min-w-0 flex-1 truncate font-medium">{title}</span>
        {options}
      </div>
      {children ?? (
        <div aria-hidden="true" className="flex flex-col gap-4 px-3 pt-3">
          {[0, 1].map((row) => (
            <div key={row} className="flex items-center gap-2">
              <span
                className="size-5 shrink-0 rounded-sm"
                style={{
                  background:
                    "color-mix(in srgb, var(--cs-fg, var(--foreground)) 10%, transparent)",
                }}
              />
              <span className="flex flex-1 flex-col gap-2">
                <span
                  className="h-1.5 w-3/4 rounded-sm"
                  style={{
                    background:
                      "color-mix(in srgb, var(--cs-fg, var(--foreground)) 10%, transparent)",
                  }}
                />
                <span
                  className="h-1.5 w-1/2 rounded-sm"
                  style={{
                    background:
                      "color-mix(in srgb, var(--cs-fg, var(--foreground)) 10%, transparent)",
                  }}
                />
              </span>
            </div>
          ))}
        </div>
      )}
      <span className="px-3 pt-4 text-xs text-muted-foreground">Loading {title}…</span>
    </div>
  );
}
