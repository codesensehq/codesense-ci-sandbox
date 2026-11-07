// SPDX-License-Identifier: MIT
import { Maximize2, Minimize2, X, type LucideIcon } from "lucide-react";
import { useCallback, type ReactNode } from "react";

import { ChromeIconButton } from "@/shared/components/chrome-icon-button";
import {
  FullscreenOverlayShell,
  type MaximizeScope,
} from "@/features/viewer/components/fullscreen-overlay-shell";
import { useMaximized } from "@/features/viewer/components/use-maximized";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { usePanelsFor, type PanelDefinition } from "./panel-registry";
import { useOwningScope } from "@/shared/lib/warm-scopes";
import { isPanelVisible, type RightPaneLayout } from "./right-pane-layout";

/** A maximize request lifts content only when the registered renderer owns an overlay. */
export function isPanelOverlayShown(
  layout: RightPaneLayout,
  panel: PanelDefinition | undefined,
  isFullscreen: boolean,
): boolean {
  return isFullscreen && panel?.maximizable === true && isPanelVisible(layout, panel.id);
}

/**
 * SW-WG-9: the single body wrapper that gives ANY right-pane panel the maximize
 * overlay — the generic replacement for the four bespoke per-panel fullscreen shells
 * (pipeline / board / pipeline-config).
 *
 * Takes the view's `repoPath` explicitly (the caller already has it — the panel body
 * from `useRightPaneHost`, the pipeline shell from its prop), so the wrapper stays
 * decoupled from the host provider and renders anywhere. Resolves "is this panel
 * maximized?" from the generic `maximizedPanel` slice (`useMaximized`) and wraps
 * `children` in the shared {@link FullscreenOverlayShell}. While maximized it renders
 * ONE generic overlay header — a slim bar with the panel's optional icon/title,
 * optional panel-supplied `headerActions` (e.g. the pipeline drill + toolbar), and a ✕
 * exit — because the host section-header maximize button is covered by the
 * `position:fixed` overlay (AC-3b). A panel that already renders its OWN in-body header
 * omits `title`/`icon` so the overlay bar is just the ✕ (no doubled title). Exit is a synchronous `setMaximized(teamKey, null)`; Esc still exits via the shell.
 *
 * Because it is a real component, its hooks are legal inside a panel `render`/closure
 * body (per the `panel-registry.ts` contract) — so a custom pane inherits maximize
 * with no hooks in its closure and no pane-specific fullscreen code.
 */
/**
 * SW-WG-12: whether THIS panel's maximize overlay is actually on screen — the store's
 * maximize slice targets it AND it is the active, expanded panel of its section (the
 * SAME gate the overlay renders under, below). Docked chrome (a pane's badge / header
 * actions) keys its dedup on this so the docked copy and the overlay can never diverge:
 * `isFullscreen` alone would hide a docked badge on a maximized-but-collapsed or
 * inactive-tab pane while the overlay (which needs active+expanded) isn't showing it
 * either — the count would vanish exactly where it's most useful (glance-while-collapsed).
 */
export function usePanelOverlayShown(
  panelId: string,
  repoPath: string,
): { ownTeamKey: string | null; showOverlay: boolean } {
  const { ownTeamKey, isFullscreen } = useMaximized(panelId, repoPath);
  // Only hold the overlay (+ its document Esc listener) while this panel is the ACTIVE,
  // expanded panel of its section. A programmatic same-section tab-switch or collapse
  // while maximized would otherwise strand a hidden `position:fixed` overlay with a live
  // Esc listener. The maximize STATE is kept (in the store), so returning to the panel
  // re-shows it maximized. Read-side gate → covers every layout-change path uniformly.
  const layout = useViewerStore((s) => s.rightPaneLayout);
  const panel = usePanelsFor(useOwningScope())[panelId];
  return { ownTeamKey, showOverlay: isPanelOverlayShown(layout, panel, isFullscreen) };
}

export function PanelMaximizeBoundary({
  panelId,
  repoPath,
  title,
  icon: Icon,
  badge,
  scopeLabel,
  scopeIcon: ScopeIcon,
  headerActions,
  testId,
  scope = "app",
  bottomInset,
  children,
}: {
  /** `pane` keeps the maximized panel inside the right pane (panes); the board and
   *  pipeline keep the app-wide overlay. */
  scope?: MaximizeScope;
  bottomInset?: number;
  panelId: string;
  /** The view's source-repo path — its own team is derived from this. */
  repoPath: string;
  /** Overlay-header title/icon. Omit both when the panel renders its own in-body header
   *  — the overlay bar then carries just the ✕ exit. */
  title?: string;
  icon?: LucideIcon;
  /** SW-WG-12 (AC-6): optional badge rendered next to the title (a pane's warning/error
   *  severity count). Symmetric with `headerActions`; benefits any boundary consumer. */
  badge?: ReactNode;
  scopeLabel?: string;
  scopeIcon?: LucideIcon;
  /** Optional overlay-header content (right-aligned, before the ✕) — e.g. pipeline drill + toolbar. */
  headerActions?: ReactNode;
  /** `data-testid` for the overlay container; defaults to `<panelId>-fullscreen-shell`. */
  testId?: string;
  children: ReactNode;
}) {
  const { ownTeamKey, showOverlay } = usePanelOverlayShown(panelId, repoPath);
  const setMaximized = useViewerStore((s) => s.setMaximized);
  const exit = useCallback(
    () => ownTeamKey && setMaximized(ownTeamKey, null),
    [ownTeamKey, setMaximized],
  );

  return (
    <FullscreenOverlayShell
      isFullscreen={showOverlay}
      onExit={exit}
      testId={testId ?? `${panelId}-fullscreen-shell`}
      scope={scope}
      bottomInset={bottomInset}
      header={
        // SW-WG-20 (AC-2): the maximized header is the stable row its actions measure — a
        // roomy bar shows more inline; marker matches the docked row in right-pane-stack.
        <div
          data-panel-header-row
          tabIndex={-1}
          className="flex items-center gap-2 px-3 py-1.5 border-b flex-shrink-0 select-none"
          style={{
            backgroundColor: "color-mix(in srgb, var(--card) 50%, transparent)",
            borderColor: "var(--border)",
          }}
        >
          {Icon && (
            <Icon size={14} className="text-muted-foreground flex-shrink-0" aria-hidden="true" />
          )}
          {title && (
            // SW-WG-12 (AC-7): the title `truncate`s (panes carry arbitrary titles), so
            // carry the full string as a native tooltip AND an explicit accessible name so
            // the visual clip never hides it from a screen reader or a mouse hover.
            <span
              className="text-xs font-medium text-foreground truncate min-w-0"
              title={title}
              aria-label={title}
            >
              {title}
            </span>
          )}
          {/* SW-WG-12 (AC-6): severity badge sits next to the title, not in the action group. */}
          {badge}
          {scopeLabel && (
            <span
              className="inline-flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-full border bg-muted px-[7px] text-[11px] text-muted-foreground"
              data-testid="slot-scope-chip"
            >
              {ScopeIcon && <ScopeIcon size={12} aria-hidden="true" />}
              {scopeLabel}
            </span>
          )}
          <div className="flex items-center gap-1 ml-auto min-w-0">
            {headerActions}
            {scopeLabel && (
              <span className="text-[10px] text-muted-foreground" aria-hidden="true">
                Esc
              </span>
            )}
            <ChromeIconButton
              icon={X}
              title={
                scopeLabel
                  ? "Exit full screen (Esc)"
                  : scope === "pane"
                    ? "Restore panel"
                    : "Exit full screen"
              }
              onClick={exit}
            />
          </div>
        </div>
      }
    >
      {children}
    </FullscreenOverlayShell>
  );
}

/**
 * SW-WG-9: the single generic maximize/restore control rendered in the host section
 * header for the active, expanded panel that declares `PanelDefinition.maximizable`
 * (AC-3) — the one replacement for the four per-panel maximize buttons. Toggles the
 * generic slice for THIS panel + the view's own team; renders nothing when the view
 * has no team key (legacy real-path repos). While maximized the header is covered by
 * the overlay, so the boundary's ✕ / Esc is the visible restore — this button just
 * reflects state.
 */
export function PanelMaximizeButton({ panelId, repoPath }: { panelId: string; repoPath: string }) {
  const { ownTeamKey, isFullscreen } = useMaximized(panelId, repoPath);
  const toggleMaximized = useViewerStore((s) => s.toggleMaximized);
  if (!ownTeamKey) return null;
  // Reuse the shared header chrome button (its onClick stop-propagates, so the click
  // never toggles the section collapse) — same control the sibling toolbar/✕ use.
  return (
    <ChromeIconButton
      icon={isFullscreen ? Minimize2 : Maximize2}
      title={isFullscreen ? "Restore panel" : "Maximize panel"}
      onClick={() => toggleMaximized(ownTeamKey, panelId)}
    />
  );
}
