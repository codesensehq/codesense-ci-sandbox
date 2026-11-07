// SPDX-License-Identifier: MIT
import { Bold, Italic, Code2, Link2, Send } from "lucide-react";

import {
  NtPopover,
  NtPopoverAnchor,
  NtPopoverContent,
} from "@/features/notes/components/nt-popover";
import type { ActiveMarks } from "../lib/scratch-selection";

export interface SelectionAnchorRect {
  top: number;
  left: number;
  width: number;
  height: number;
}

interface ScratchSelectionMenuProps {
  open: boolean;
  /** Viewport rect of the current selection, used to anchor the popover. */
  rect: SelectionAnchorRect | null;
  marks: ActiveMarks;
  onToggleBold: () => void;
  onToggleItalic: () => void;
  onToggleCode: () => void;
  /** Links the selection to a note or story (opens the `[[` picker on its words). */
  onLink?: () => void;
  /** Opens "Send to…" for the selected text. */
  onSend: () => void;
  /** Called when Radix requests close (Escape / outside click). */
  onDismiss: () => void;
}

/**
 * The one selection menu: Bold / Italic / Code / Link, then "Send to…" (an agent, a new
 * agent, or the focused terminal). Anchored by an invisible fixed box over the
 * selection; focus stays in the editor (auto-focus prevented, mousedown suppressed
 * on every button) so the ProseMirror selection survives the click.
 */
export function ScratchSelectionMenu({
  open,
  rect,
  marks,
  onToggleBold,
  onToggleItalic,
  onToggleCode,
  onLink,
  onSend,
  onDismiss,
}: ScratchSelectionMenuProps) {
  return (
    <NtPopover
      open={open && rect !== null}
      onOpenChange={(next) => {
        if (!next) onDismiss();
      }}
    >
      <NtPopoverAnchor asChild>
        <div
          aria-hidden
          style={{
            position: "fixed",
            top: rect?.top ?? 0,
            left: rect?.left ?? 0,
            width: rect?.width ?? 0,
            height: rect?.height ?? 0,
            pointerEvents: "none",
          }}
        />
      </NtPopoverAnchor>
      <NtPopoverContent
        side="top"
        align="center"
        onOpenAutoFocus={(e) => e.preventDefault()}
        onCloseAutoFocus={(e) => e.preventDefault()}
        className="nt-selmenu"
      >
        <SelectionMenuButton active={marks.bold} label="Bold" onClick={onToggleBold}>
          <Bold size={14} />
        </SelectionMenuButton>
        <SelectionMenuButton active={marks.italic} label="Italic" onClick={onToggleItalic}>
          <Italic size={14} />
        </SelectionMenuButton>
        <SelectionMenuButton active={marks.code} label="Code" onClick={onToggleCode}>
          <Code2 size={14} />
        </SelectionMenuButton>
        {onLink && (
          <SelectionMenuButton active={false} label="Link to a note or story" onClick={onLink}>
            <Link2 size={14} />
          </SelectionMenuButton>
        )}
        <span className="nt-selmenu-sep" aria-hidden />
        <button
          type="button"
          aria-label="Send to…"
          className="nt-selmenu-btn nt-selmenu-btn-wide"
          onMouseDown={(e) => e.preventDefault()}
          onClick={onSend}
        >
          <Send size={13} />
          Send to…
        </button>
      </NtPopoverContent>
    </NtPopover>
  );
}

interface SelectionMenuButtonProps {
  active: boolean;
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}

function SelectionMenuButton({ active, label, onClick, children }: SelectionMenuButtonProps) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={active}
      // Preserve the editor's selection: a plain click would blur the editor
      // and collapse the ProseMirror selection before the command runs.
      onMouseDown={(e) => e.preventDefault()}
      onClick={onClick}
      className="nt-selmenu-btn"
    >
      {children}
    </button>
  );
}
