// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";
import { X } from "lucide-react";
import { EpicRepoCard, type EpicRepoCardProps } from "./epic-repo-card";

export interface EpicRepoOverlayProps extends EpicRepoCardProps {
  /** Dismiss the overlay → return to the live terminal. */
  onClose: () => void;
}

/**
 * SW-EW-15 (Work-stream B) — full-pane takeover that hosts the SW-EW-9
 * `EpicRepoCard` over an epic workspace's terminal pane. It is layered ON TOP
 * of the terminal area; the PTY stays mounted underneath (the terminal panels
 * use a display-toggle, not a conditional render), so open→close→open never
 * tears down the session.
 *
 * Owns only chrome (backdrop, header, close, Esc + focus capture) — all repo
 * read/write is the reused card, untouched. The container grabs focus on mount
 * and captures Escape (stopPropagation) so the underlying xterm never also sees
 * the keystroke while the overlay is up.
 */
export function EpicRepoOverlay({ onClose, ...cardProps }: EpicRepoOverlayProps) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    // Pull focus off the terminal so the overlay owns the keyboard while open.
    ref.current?.focus();
  }, []);

  return (
    <div
      ref={ref}
      tabIndex={-1}
      role="dialog"
      aria-modal="true"
      aria-label="Epic repositories"
      data-testid="epic-repo-overlay"
      // SLW-FX-15: dialog-surface token (`bg-popover`) so the config takeover
      // reads as a focused task surface, distinct from the app background.
      className="absolute inset-0 z-20 flex flex-col bg-popover text-popover-foreground outline-none"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          // Don't let the terminal underneath also act on Escape.
          e.stopPropagation();
          onClose();
        }
      }}
    >
      <div className="flex items-center justify-between border-b border-border px-3 py-2">
        <div className="flex flex-col">
          <span className="text-sm font-medium">Configure repos</span>
          <span className="text-xs text-muted-foreground">
            Add the source repos this epic assembles into its workspace.
          </span>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close repo config"
          data-testid="epic-repo-overlay-close"
          className="rounded p-1 text-muted-foreground hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-accent"
        >
          <X style={{ width: "1em", height: "1em" }} />
        </button>
      </div>
      <div className="flex-1 overflow-auto p-3">
        <EpicRepoCard {...cardProps} />
      </div>
    </div>
  );
}
