// SPDX-License-Identifier: MIT
import {
  useCallback,
  useMemo,
  useState,
  type MouseEvent as ReactMouseEvent,
  type RefObject,
} from "react";
import { useCopyToClipboard } from "@/shared/hooks/use-copy-to-clipboard";
import { logger } from "@/shared/lib/logger";
import {
  PositionedContextMenu,
  PositionedMenuItem,
  type ContextMenuPosition,
} from "@/shared/components/positioned-context-menu";

/**
 * The minimal seam the menu items need from the live xterm terminal. Kept
 * narrow so the menu is unit-testable with a plain stub, and so the terminal
 * keeps ownership of bracketed-paste framing and input gating behind `paste()`.
 */
export interface TerminalMenuTarget {
  hasSelection(): boolean;
  getSelection(): string;
  paste(data: string): void;
}

/**
 * Everything the body-menu hook reads off the terminal: the copy/paste seam
 * plus the mouse-tracking mode (for the open rule) and focus (for dismissal).
 * A structural supertype of xterm's `Terminal`, so the panel passes `termRef`
 * directly and tests pass a small fake.
 */
export interface TerminalMenuTerminal extends TerminalMenuTarget {
  readonly modes: { readonly mouseTrackingMode: string };
  focus(): void;
}

/**
 * When the menu may open on a right-click. A plain right-click opens it only
 * when the app is NOT capturing the mouse; while a TUI holds mouse tracking on,
 * the click is left to the app (xterm already reported it on mousedown) and
 * Shift — xterm's own bypass modifier — is the discoverable way in.
 */
export function shouldOpenTerminalMenu(shiftKey: boolean, isMouseTracking: boolean): boolean {
  return shiftKey || !isMouseTracking;
}

/**
 * Where to anchor the menu for a `contextmenu` event. A mouse right-click uses
 * the cursor. A keyboard-triggered menu (Menu key / Shift+F10) reports no
 * pointer position (`detail === 0`, coords `0,0`) — anchor just inside the
 * terminal body (`currentTarget`) instead of the viewport corner.
 */
export function contextMenuAnchor(e: {
  detail: number;
  clientX: number;
  clientY: number;
  currentTarget: { getBoundingClientRect(): { left: number; top: number } };
}): { x: number; y: number } {
  if (e.detail === 0 && e.clientX === 0 && e.clientY === 0) {
    const rect = e.currentTarget.getBoundingClientRect();
    return { x: rect.left + 8, y: rect.top + 8 };
  }
  return { x: e.clientX, y: e.clientY };
}

/**
 * Read the clipboard and paste into the terminal. `term.paste()` frames
 * bracketed paste per the terminal's own mode and routes through the gated
 * `onData` input seam — so an input-locked terminal (agent driving) drops the
 * paste like any other input, and we never write to the PTY directly. We pass
 * the raw text. Async read only (never the synchronous Electron clipboard,
 * which would block the renderer); `?.` tolerates an environment without the
 * Clipboard API.
 */
export async function pasteFromClipboard(target: TerminalMenuTarget): Promise<void> {
  try {
    const text = await navigator.clipboard?.readText();
    if (text) target.paste(text);
  } catch (e) {
    // readText rejects when the document isn't focused or permission is denied;
    // a failed paste is a no-op, not a crash.
    logger.warn("terminal", `paste failed: ${e}`);
  }
}

/**
 * Owns the terminal body menu: right-click position, the open rule, and
 * focus-restore on dismiss. Extracted from the panel so this wiring is
 * unit-testable against a fake terminal.
 */
export function useTerminalBodyContextMenu<T extends TerminalMenuTerminal>(
  termRef: RefObject<T | null>,
  visible = true,
) {
  const [position, setPosition] = useState<ContextMenuPosition>(null);
  if (!visible && position) setPosition(null);

  const onContextMenu = useCallback(
    (e: ReactMouseEvent<HTMLElement>) => {
      // The terminal never shows the native browser menu — suppress it on every
      // right-click, even before the terminal instance is ready and whether or
      // not we open ours.
      e.preventDefault();
      const term = termRef.current;
      if (!term) return;
      if (!shouldOpenTerminalMenu(e.shiftKey, term.modes.mouseTrackingMode !== "none")) return;
      setPosition(contextMenuAnchor(e));
    },
    [termRef],
  );

  // Every dismiss path (item pick, outside-click, Escape) routes through here,
  // so focus always returns to the terminal — typing continues and no stray
  // input reaches the PTY.
  const onClose = useCallback(() => {
    setPosition(null);
    termRef.current?.focus();
  }, [termRef]);

  const target = useMemo<TerminalMenuTarget>(
    () => ({
      hasSelection: () => termRef.current?.hasSelection() ?? false,
      getSelection: () => termRef.current?.getSelection() ?? "",
      paste: (data) => termRef.current?.paste(data),
    }),
    [termRef],
  );

  return { position, onContextMenu, onClose, target };
}

/**
 * Right-click Copy/Paste menu for the terminal body. Dumb child: position and
 * dismissal (which restores terminal focus) are owned by the hook above.
 */
export function TerminalBodyContextMenu({
  position,
  onClose,
  target,
}: {
  position: ContextMenuPosition;
  onClose: () => void;
  target: TerminalMenuTarget;
}) {
  const { handleCopy } = useCopyToClipboard();
  if (!position) return null;

  const canCopy = target.hasSelection();
  return (
    <PositionedContextMenu
      position={position}
      onClose={onClose}
      minWidth="140px"
      label="Terminal actions"
    >
      <PositionedMenuItem
        className="disabled:pointer-events-none disabled:opacity-40"
        disabled={!canCopy}
        onClick={() => {
          // Re-read at click time: the terminal can drop its selection while the
          // menu is open (e.g. a repaint), and a stale-enabled Copy must not
          // clobber the clipboard with an empty string.
          const selection = target.getSelection();
          if (selection) handleCopy(selection);
          onClose();
        }}
      >
        Copy
      </PositionedMenuItem>
      {/* Always enabled: the clipboard can't be cheaply pre-checked synchronously,
          so an empty-clipboard Paste is a harmless no-op, not a disabled item. */}
      <PositionedMenuItem
        onClick={() => {
          void pasteFromClipboard(target);
          onClose();
        }}
      >
        Paste
      </PositionedMenuItem>
    </PositionedContextMenu>
  );
}
