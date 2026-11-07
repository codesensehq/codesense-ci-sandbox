// SPDX-License-Identifier: MIT
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { cn } from "@/lib/utils";
import { isInHiddenTeam } from "@/shared/lib/warm-scopes";

/**
 * SW-SB-14: the shared in-app "maximize" shell, extracted from the SW-PP-5 pipeline
 * shell so the pipeline and the board share ONE overlay mechanism (AC-4: the shell
 * component is shared; each view keeps its own maximize state and passes it in via
 * `isFullscreen` / `onExit`).
 *
 * Wraps a view's body in ONE always-mounted container. When maximized it
 * CSS-promotes that container to a `position: fixed` overlay filling the app content
 * area — a pure class/style flip, so **nothing moves in React or the DOM** and any
 * live child (React Flow viewport, board scroll) is preserved entering AND exiting.
 * The right-pane layout is never touched, so exiting restores the prior pane state by
 * construction.
 *
 * Why `position: fixed` (not `absolute`): the body's nearest positioned ancestor is
 * the right pane, so an absolute overlay would be trapped at the right-pane width —
 * the exact constraint maximize removes. `fixed` escapes to the viewport. The top
 * offset is measured from the title bar so app chrome stays visible (no OS
 * full-screen — Trevor's decision).
 */
/** Where a maximized overlay spreads: the app content area (below the title bar),
 *  or only the right pane that hosts it. */
export type MaximizeScope = "app" | "pane";

/** The right pane's on-screen box, which a `pane`-scoped overlay covers. */
function paneRect(el: HTMLElement | null): DOMRect | null {
  const pane = el?.closest<HTMLElement>("[data-right-pane-root]");
  const rect = pane?.getBoundingClientRect();
  return rect && rect.width > 0 && rect.height > 0 ? rect : null;
}

export function FullscreenOverlayShell({
  isFullscreen,
  onExit,
  testId,
  header,
  scope = "app",
  bottomInset = 0,
  children,
}: {
  /** App scope only: px the overlay stops short of the window bottom, leaving that
   *  band (e.g. a terminal split) on screen and usable. */
  bottomInset?: number;
  /** `pane` keeps the overlay inside the right pane (SW-LN-B40): the rest of the
   *  app stays visible. Defaults to the app content area. */
  scope?: MaximizeScope;
  isFullscreen: boolean;
  /** Exit maximize — called by the document `Esc` listener. */
  onExit: () => void;
  /** `data-testid` for the overlay container (keeps per-view test addressing). */
  testId: string;
  /** Optional slim overlay header (e.g. the pipeline's drill + actions row). */
  header?: ReactNode;
  children: ReactNode;
}) {
  // Measure the title bar so the overlay starts exactly below app chrome (keeps the
  // title bar + its window controls visible). Runs once on enter; `2.5rem` (the
  // h-10 title bar) is the pre-measure default so tests / first paint are correct.
  const [titleBarHeight, setTitleBarHeight] = useState<number | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [pane, setPane] = useState<{
    top: number;
    left: number;
    width: number;
    height: number;
  } | null>(null);
  // A pane-scoped overlay tracks the right pane's box (it moves when the pane is
  // resized or the window changes size).
  useLayoutEffect(() => {
    if (!isFullscreen || scope !== "pane") return;
    const measure = () => {
      const r = paneRect(containerRef.current);
      setPane(r ? { top: r.top, left: r.left, width: r.width, height: r.height } : null);
    };
    measure();
    const root = containerRef.current?.closest<HTMLElement>("[data-right-pane-root]");
    const observer =
      root && typeof ResizeObserver !== "undefined" ? new ResizeObserver(measure) : null;
    if (root) observer?.observe(root);
    window.addEventListener("resize", measure);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [isFullscreen, scope]);
  useLayoutEffect(() => {
    if (!isFullscreen) return;
    const tb = document.querySelector<HTMLElement>(".titlebar-drag");
    const h = tb?.getBoundingClientRect().height;
    // A layout measurement, not derivable state — the title bar height is a
    // rendered DOM fact. Same measure-then-set idiom as the settings/org-switcher
    // popovers.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    if (h && h > 0) setTitleBarHeight(h);
  }, [isFullscreen]);

  // Restore focus to wherever it was when we entered, on exit.
  const prevFocusRef = useRef<HTMLElement | null>(null);
  const pointerOpener = useRef<HTMLElement | null>(null);
  useEffect(() => {
    if (isFullscreen) return;
    const remember = (event: PointerEvent) => {
      if (event.target instanceof HTMLElement)
        pointerOpener.current =
          event.target.closest<HTMLElement>("button, a[href], [tabindex]") ?? event.target;
    };
    document.addEventListener("pointerdown", remember, true);
    return () => document.removeEventListener("pointerdown", remember, true);
  }, [isFullscreen]);
  useEffect(() => {
    if (isFullscreen) {
      const active = document.activeElement as HTMLElement | null;
      // WebKit leaves pointer-activated buttons unfocused. Keep their opener too.
      prevFocusRef.current = active === document.body ? (pointerOpener.current ?? active) : active;
    } else if (prevFocusRef.current) {
      prevFocusRef.current.focus?.();
      prevFocusRef.current = null;
    }
  }, [isFullscreen]);

  // Esc exits. Mirrors source-diff-overlay.tsx / story-hud.tsx — a document listener
  // that ignores typing (input / textarea / contenteditable) and terminals
  // (`.xterm` keeps its own Esc). Mounted only while maximized.
  //
  // SW-SB-14: bail on `e.defaultPrevented`. Radix `DismissableLayer` handles Escape
  // in the CAPTURE phase and calls `preventDefault()` when it dismisses a layer, so
  // an open card context menu / dropdown closes on that earlier pass and marks the
  // event handled. This bubble-phase listener then no-ops — giving the documented
  // "topmost layer first, then fullscreen" order, one layer per keypress.
  useEffect(() => {
    if (!isFullscreen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (containerRef.current && isInHiddenTeam(containerRef.current)) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      if (t?.closest?.(".xterm")) return;
      onExit();
    };
    const container = containerRef.current;
    const onFrameEscape = () => onExit();
    container?.addEventListener("codesense:frame-escape", onFrameEscape);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      container?.removeEventListener("codesense:frame-escape", onFrameEscape);
    };
  }, [isFullscreen, onExit]);

  return (
    <div
      ref={containerRef}
      data-testid={testId}
      data-fullscreen={isFullscreen ? "true" : "false"}
      data-maximize-scope={scope}
      className={cn("flex flex-col flex-1 min-h-0", isFullscreen && "overflow-hidden")}
      style={
        !isFullscreen
          ? undefined
          : scope === "pane" && pane
            ? {
                position: "fixed",
                top: pane.top,
                left: pane.left,
                width: pane.width,
                height: pane.height,
                zIndex: 40,
                backgroundColor: "var(--cs-viewer-bg, var(--background))",
              }
            : {
                position: "fixed",
                top: titleBarHeight ?? "2.5rem",
                left: scope === "pane" ? undefined : 0,
                right: 0,
                bottom: scope === "pane" ? 0 : bottomInset,
                // Pane scope before the first measure: hug the right edge, never the whole app.
                width: scope === "pane" ? "min(480px, 40vw)" : undefined,
                zIndex: 40,
                backgroundColor: "var(--cs-viewer-bg, var(--background))",
              }
      }
    >
      {isFullscreen && header}
      {children}
    </div>
  );
}
