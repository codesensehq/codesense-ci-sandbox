// SPDX-License-Identifier: MIT
import { useMenuOwnership } from "@/shared/hooks/use-menu-ownership";
import { textMenuWithin } from "@/shared/lib/text-menu-focus";
import { useOwnsScope } from "@/shared/lib/warm-scopes";
import {
  forwardRef,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";

import { ChevronRight } from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

export type ContextMenuPosition = { x: number; y: number } | null;

/**
 * SW-FX-55: clamp a cursor-anchored menu so it stays fully inside the viewport.
 * Pure arithmetic — no DOM, no CSS — so it's exhaustively unit-testable and
 * happy-dom-safe (the component's `getBoundingClientRect` is zero there).
 *
 * Prefer the raw cursor position; pull inward only enough to fit, and never
 * cross the `padding` floor. Ordering matters: the inward bound itself is
 * floored at `padding` (`Math.max(padding, …)`) so a menu LARGER than the
 * viewport pins to `padding` instead of producing a negative coordinate.
 */
const VIEWPORT_PADDING = 8;

export function clampMenuToViewport({
  x,
  y,
  menuWidth,
  menuHeight,
  viewportWidth,
  viewportHeight,
  padding = VIEWPORT_PADDING,
}: {
  x: number;
  y: number;
  menuWidth: number;
  menuHeight: number;
  viewportWidth: number;
  viewportHeight: number;
  padding?: number;
}): { left: number; top: number } {
  const maxLeft = Math.max(padding, viewportWidth - menuWidth - padding);
  const maxTop = Math.max(padding, viewportHeight - menuHeight - padding);
  return {
    left: Math.min(Math.max(x, padding), maxLeft),
    top: Math.min(Math.max(y, padding), maxTop),
  };
}

/** The items the menu's keyboard navigation moves between: enabled menu items,
 *  plus unavailable ones marked `data-focusable-disabled` (an item whose reason
 *  must stay reachable, per the ARIA menu pattern — focusable, never activated). */
const MENU_ITEM_SELECTOR =
  '[role="menuitem"]:not([disabled]):not([aria-disabled="true"]),' +
  '[role="menuitemcheckbox"]:not([disabled]):not([aria-disabled="true"]),' +
  '[role="menuitemradio"]:not([disabled]):not([aria-disabled="true"]),' +
  '[role^="menuitem"][aria-disabled="true"][data-focusable-disabled]';

function menuItems(menu: HTMLElement): HTMLElement[] {
  return Array.from(menu.querySelectorAll<HTMLElement>(MENU_ITEM_SELECTOR));
}

// Fixed descendants inherit CSS zoom in some consumers. DOM rects and cursor
// coordinates are viewport pixels; sizing and scrolling use local CSS pixels.
function menuScale(menu: HTMLElement): number {
  const zoom = (menu as HTMLElement & { currentCSSZoom?: number }).currentCSSZoom;
  if (zoom && Number.isFinite(zoom) && zoom > 0) return zoom;
  const width = Number.parseFloat(getComputedStyle(menu).width);
  const scale = menu.getBoundingClientRect().width / width;
  return Number.isFinite(scale) && scale > 0 ? scale : 1;
}

function revealMenuItem(menu: HTMLElement, item: HTMLElement): void {
  const scale = menuScale(menu);
  const top = menu.getBoundingClientRect().top + menu.clientTop * scale;
  const bottom = top + menu.clientHeight * scale;
  const rect = item.getBoundingClientRect();
  // Scroll this menu only; focusing must never move the underlying workspace.
  if (rect.top < top) menu.scrollTop += (rect.top - top) / scale;
  else if (rect.bottom > bottom) menu.scrollTop += (rect.bottom - bottom) / scale;
}

/**
 * Reusable manually-positioned context menu. Renders a backdrop overlay
 * that closes the menu on click, plus a positioned container for children.
 * Shared by pipeline-status-nav
 * and tab-context-menu.
 *
 * It is a keyboard menu (WAI-ARIA menu pattern): opening moves focus to the
 * first item, ↑/↓ move with wrap, Home/End jump, Enter/Space activate, Esc and
 * Tab close, and closing puts focus back where it was when the menu opened.
 * Items are the `role="menuitem"` descendants — render them with
 * `PositionedMenuItem`.
 */
export function PositionedContextMenu({
  position,
  owner,
  onClose,
  minWidth = "160px",
  label,
  role = "menu",
  children,
}: {
  position: ContextMenuPosition;
  owner?: object;
  onClose: () => void;
  minWidth?: string;
  /** What the menu is for, read out as the menu's name ("Tab actions"). */
  label?: string;
  role?: "menu" | "dialog";
  children: ReactNode;
}) {
  // SW-FX-55: measure the menu's own rect after it mounts and clamp left/top so
  // it never spills off the right/bottom edge (raw cursor coords have no clamp).
  const [ownOwner] = useState(() => ({}));
  const menuRef = useRef<HTMLDivElement>(null);
  // A warm team keeps its menu's owner mounted while hidden, and this menu's document-capture Escape
  // would then swallow the shown team's. A menu is a transient surface: leaving the team closes it.
  const owns = useOwnsScope();
  useMenuOwnership(owner ?? ownOwner, position !== null && owns, onClose, menuRef);
  useEffect(() => {
    if (!owns && position) onClose();
  }, [owns, position, onClose]);
  const [clamped, setClamped] = useState<{ left: number; top: number } | null>(null);

  // Bound content before measuring/clamping. One shared scroll owner keeps
  // every item reachable, including menus that grow while open or resize.
  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (!position || !menu) {
      setClamped(null); // eslint-disable-line react-hooks/set-state-in-effect
      return;
    }
    const measure = () => {
      const scale = menuScale(menu);
      menu.style.maxHeight = `${Math.max(0, window.innerHeight - 2 * VIEWPORT_PADDING) / scale}px`;
      const rect = menu.getBoundingClientRect();
      // Preserve the raw seed for DOM environments without layout.
      if (rect.width === 0 && rect.height === 0) {
        setClamped(null);
        return;
      }
      const viewportPosition = clampMenuToViewport({
        x: position.x,
        y: position.y,
        menuWidth: rect.width,
        menuHeight: rect.height,
        viewportWidth: window.innerWidth,
        viewportHeight: window.innerHeight,
      });
      const next = { left: viewportPosition.left / scale, top: viewportPosition.top / scale };
      setClamped((previous) =>
        previous?.left === next.left && previous?.top === next.top ? previous : next,
      );
      const focused = document.activeElement;
      if (focused instanceof HTMLElement && menu.contains(focused)) revealMenuItem(menu, focused);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(menu);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [position]);

  // SW-EW-30 D4: Escape dismisses the menu — keyboard parity with the
  // backdrop's click/right-click dismissal, shared by every consumer (tab menu,
  // manager card, the foreground Exit menu). Additive: Escape was a no-op for
  // these menus before. Hook runs before the early return (rules of hooks);
  // it self-guards on `position` so a closed menu has no listener attached.
  //
  // CAPTURE phase + stopPropagation so an OPEN menu *consumes* the Escape: it's
  // the topmost layer, so the keypress belongs to it alone. Without this, the
  // same Escape also reaches the still-registered document/bubble Esc handlers
  // underneath (SW-EW-4 zoom-restore, the management panel's focus-to-terminal),
  // so dismissing a menu would also un-zoom a tile or yank terminal focus.
  useEffect(() => {
    if (!position || !owns) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (textMenuWithin(menuRef.current)) return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [position, owns, onClose]);

  // Focus in, and back out. Keyed on open/closed rather than on `position`, so
  // re-anchoring an open menu neither re-captures the invoker (which would then
  // be one of the menu's own items) nor yanks focus back to the first item.
  const open = position !== null;
  useEffect(() => {
    if (!open) return;
    const invoker = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const menu = menuRef.current;
    if (menu) (menuItems(menu)[0] ?? menu).focus({ preventScroll: true });
    return () => {
      // Return focus only when nothing else claimed it: focus is still in the
      // (now removed) menu, i.e. on <body>. An item that moved focus on purpose
      // (a dialog it opened) keeps it; an invoker that went away (an ended tab)
      // is skipped.
      const now = document.activeElement;
      const focusLost = !now || now === document.body || (menu?.contains(now) ?? false);
      if (focusLost && invoker?.isConnected) invoker.focus({ preventScroll: true });
    };
  }, [open]);

  if (!position) return null;

  const onMenuKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const menu = menuRef.current;
    if (!menu || role === "dialog") return;
    const items = menuItems(menu);
    const current = items.findIndex((el) => el === document.activeElement);
    const focusAt = (i: number) => {
      const item = items[i];
      if (!item) return;
      item.focus({ preventScroll: true });
      revealMenuItem(menu, item);
    };
    switch (e.key) {
      case "ArrowDown":
        if (items.length) focusAt(current < 0 ? 0 : (current + 1) % items.length);
        break;
      case "ArrowUp":
        if (items.length)
          focusAt(current < 0 ? items.length - 1 : (current - 1 + items.length) % items.length);
        break;
      case "Home":
        focusAt(0);
        break;
      case "End":
        focusAt(items.length - 1);
        break;
      case "Enter":
      case " ":
        // An unavailable item takes focus (its reason is read) but never acts.
        if (current >= 0 && items[current]?.getAttribute("aria-disabled") !== "true") {
          items[current]?.click();
        }
        break;
      case "Tab":
        onClose();
        break;
      default:
        return;
    }
    // The menu is the topmost layer: a key it handles is its alone (no arrow
    // scrolling a list underneath, no global shortcut, no double activation from
    // the button's own Enter/Space).
    e.preventDefault();
    e.stopPropagation();
  };

  return (
    <div
      className="fixed inset-0 z-40"
      onClick={onClose}
      onContextMenu={(e) => {
        e.preventDefault();
        onClose();
      }}
    >
      <div
        ref={menuRef}
        role={role}
        aria-label={label}
        aria-orientation={role === "menu" ? "vertical" : undefined}
        tabIndex={-1}
        className="fixed z-50 overflow-y-auto rounded-lg bg-popover p-1 shadow-md ring-1 ring-foreground/10 outline-none"
        // Seed at the raw cursor (x, y) so the first measurement sizes real
        // content; the layout-effect clamp then applies before paint (AC-5).
        style={{ left: clamped?.left ?? position.x, top: clamped?.top ?? position.y, minWidth }}
        // Clicks inside the menu must not reach the backdrop's onClose — each
        // item owns its own close, so a non-closing item (e.g. an inline
        // confirm toggle) survives. The backdrop still closes on outside click.
        onClick={(e) => e.stopPropagation()}
        onKeyDown={onMenuKeyDown}
        // Pointer and keyboard share one highlight: hovering an item focuses it,
        // so the next arrow key moves on from where the pointer is.
        onPointerMove={(e) => {
          const item = (e.target as HTMLElement).closest<HTMLElement>(MENU_ITEM_SELECTOR);
          // An open submenu's trigger keeps focus in its submenu: focusing the
          // trigger would close the submenu under the pointer on its way there.
          if (
            item &&
            item !== document.activeElement &&
            item.getAttribute("aria-expanded") !== "true" &&
            menuRef.current?.contains(item)
          ) {
            item.focus({ preventScroll: true });
          }
        }}
        // Same guard for right-click: a right-click on an item must not bubble
        // to the backdrop's onContextMenu→onClose (which would dismiss the menu
        // and undo a confirm toggle). preventDefault suppresses the native menu
        // inside our menu, since the click no longer reaches the backdrop's.
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
        }}
      >
        {children}
      </div>
    </div>
  );
}

/** Standard style for context menu items: the same highlight for hover and
 *  keyboard focus. */
export const contextMenuItemClass =
  "w-full text-left rounded-sm px-2 py-1 text-sm outline-none hover:bg-accent hover:text-accent-foreground focus-visible:bg-accent focus-visible:text-accent-foreground transition-colors";

/** One item of a `PositionedContextMenu`: a `menuitem` button that the menu's
 *  arrow keys reach (it is not in the Tab order — the menu moves focus itself). */
export const PositionedMenuItem = forwardRef<
  HTMLButtonElement,
  ButtonHTMLAttributes<HTMLButtonElement>
>(function PositionedMenuItem({ className, type = "button", ...props }, ref) {
  return (
    <button
      ref={ref}
      type={type}
      role="menuitem"
      tabIndex={-1}
      className={className ? `${contextMenuItemClass} ${className}` : contextMenuItemClass}
      {...props}
    />
  );
});

/**
 * An item of a `PositionedContextMenu` that opens a submenu beside it (`Role ▸`). The
 * submenu is a Radix menu, so its own items may nest further `DropdownMenuSub`s.
 *
 * Keyboard: →, Enter or Space opens it and moves focus into it; ↑/↓ in the submenu move
 * within it; ← closes it and puts focus back on its item; ↑/↓ on the item move through the
 * parent menu as for any item. Esc closes the whole menu. A click toggles it.
 */
export function PositionedSubmenu({
  label,
  contentClassName,
  children,
}: {
  label: ReactNode;
  contentClassName?: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <DropdownMenu open={open} onOpenChange={setOpen} modal={false}>
      <DropdownMenuTrigger asChild>
        <PositionedMenuItem
          className="flex items-center gap-2"
          onKeyDown={(e) => {
            if (e.key === "ArrowRight" || e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              e.stopPropagation();
              setOpen(true);
            } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              // The parent menu moves on; the trigger's own arrow-opens does not apply.
              e.preventDefault();
            }
          }}
        >
          {label}
          <ChevronRight aria-hidden className="ml-auto size-3.5 text-muted-foreground" />
        </PositionedMenuItem>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        side="right"
        align="start"
        sideOffset={2}
        className={contentClassName}
        onKeyDown={(e) => {
          // The submenu is portaled, but React still bubbles its keys to the parent
          // menu, whose arrow keys would move focus there too.
          e.stopPropagation();
          const own =
            (e.target as HTMLElement).closest("[data-radix-menu-content]") === e.currentTarget;
          if (e.key === "ArrowLeft" && own) {
            e.preventDefault();
            setOpen(false);
          }
        }}
      >
        {children}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
