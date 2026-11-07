// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import { AppButton } from "@/shared/components/app-button";
import { formatKeystrokeDisplay, keystrokeFromEvent } from "@/shared/lib/shortcuts/keystroke";
import {
  findConflict,
  shortcutLabelById,
  type ResolvedBinding,
  type ShortcutAction,
} from "@/shared/lib/shortcuts/registry";
import { useResolvedBinding, useShortcutsStore } from "@/shared/stores/shortcuts-store";

import { Switch } from "@/components/ui/switch";

interface ShortcutRowProps {
  action: ShortcutAction;
  /** A dashboard command's "Humans only" switch, at the end of the row. */
  humanOnly?: {
    on: boolean;
    locked: boolean;
    /** The switch's accessible name, with why it's locked. */
    label: string;
    onChange: (on: boolean) => void;
    /** The person changed it; Reset returns it to the manifest's value. */
    overridden: boolean;
    onReset: () => void;
    note: string | null;
  };
}

/**
 * Give `keystroke` to `action` at once, unbinding whoever held it, with an Undo
 * toast. The one take-over path: the Conflicts group and a conflicting Edit both
 * use it.
 */
export function takeChord(action: ShortcutAction, keystroke: string): void {
  const store = useShortcutsStore.getState();
  const result = store.takeShortcut(action.id, keystroke);
  if (!result.ok) {
    toast.error(result.detail);
    return;
  }
  const chord = formatKeystrokeDisplay(result.keystroke);
  const lost = result.displaced ? ` ${shortcutLabelById(result.displaced)} has no shortcut.` : "";
  toast(`${chord} now runs ${action.label}.${lost}`, {
    action: { label: "Undo", onClick: () => store.restoreShortcuts(result.previous) },
  });
}

/** The muted line under a row that has no chord, saying why. */
function unboundNote(action: ShortcutAction, binding: ResolvedBinding): string | null {
  const reason = binding.unbound;
  if (!reason) return null;
  if (reason.kind === "reserved") {
    return `${action.command?.dashboardTitle ?? action.label} asked for ${formatKeystrokeDisplay(reason.wanted)}. ${reason.message}, so pick another with Edit.`;
  }
  if (reason.kind === "conflict")
    return `Wants ${formatKeystrokeDisplay(reason.wanted)}. See Conflicts.`;
  if (reason.kind === "cleared" && !action.command && action.defaultKeystroke) {
    const shortcuts = useShortcutsStore.getState().shortcuts;
    const holder = findConflict(action.defaultKeystroke, shortcuts, action.id);
    if (holder) {
      const chord = formatKeystrokeDisplay(action.defaultKeystroke);
      return `${chord} now runs ${shortcutLabelById(holder)}. Reset gives ${chord} back to ${action.label}.`;
    }
  }
  return null;
}

/**
 * One editable row in Settings → Shortcuts. Displays the action label
 * + current keystroke + Edit / Reset controls. In edit mode the row
 * becomes a capture field: the next non-modifier keystroke the user
 * presses is submitted to `setShortcut`. Capture blocks propagation so
 * the app's own shortcut handler doesn't re-trigger mid-capture.
 *
 * Errors from `setShortcut` (reserved / conflict / invalid) render
 * inline beneath the row until the user retries or Escapes.
 */
export function ShortcutRow({ action, humanOnly }: ShortcutRowProps) {
  const binding = useResolvedBinding(action.id) ?? { keystroke: "" };
  const current = binding.keystroke;
  const hasOverride = useShortcutsStore((s) => action.id in s.shortcuts);
  const setShortcut = useShortcutsStore((s) => s.setShortcut);
  const clearShortcut = useShortcutsStore((s) => s.clearShortcut);
  const resetShortcut = useShortcutsStore((s) => s.resetShortcut);

  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<{ detail: string; takeable?: string } | null>(null);
  const closeEditor = () => {
    setEditing(false);
    setError(null);
  };
  const [pendingModifiers, setPendingModifiers] = useState<string>("");
  const captureRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (editing) {
      captureRef.current?.focus();
      return;
    }
    // Resetting the pending hint on exit is part of "reset per
    // session" — an effect is the right shape; the setter is targeting
    // local state, not a render-dependent derivation.
    /* eslint-disable react-hooks/set-state-in-effect */
    setPendingModifiers("");
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [editing]);

  const handleCapture = (e: React.KeyboardEvent<HTMLButtonElement>) => {
    // Bare modifier presses: update the "waiting for key" hint so the
    // user gets feedback that their modifier is registered.
    if (e.key === "Meta" || e.key === "Control" || e.key === "Shift" || e.key === "Alt") {
      const parts: string[] = [];
      if (e.metaKey || e.ctrlKey) parts.push("⌘");
      if (e.shiftKey) parts.push("⇧");
      if (e.altKey) parts.push("⌥");
      setPendingModifiers(parts.join(""));
      return;
    }
    // Block propagation so the global shortcut handler doesn't also
    // fire on the keystroke the user is currently binding.
    e.preventDefault();
    e.stopPropagation();

    if (e.key === "Escape") {
      closeEditor();
      return;
    }

    // ⌫ or Delete alone removes the chord: "explicitly no shortcut".
    if (
      (e.key === "Backspace" || e.key === "Delete") &&
      !e.metaKey &&
      !e.ctrlKey &&
      !e.altKey &&
      !e.shiftKey
    ) {
      clearShortcut(action.id);
      closeEditor();
      return;
    }

    // Bare Tab (no modifier) traps keyboard users — a shortcut bound
    // to Tab would break tab navigation app-wide. Require a modifier
    // to bind Tab.
    if (e.key === "Tab" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      setError({ detail: "Tab alone would break keyboard navigation — add a modifier" });
      return;
    }

    const keystroke = keystrokeFromEvent(e.nativeEvent);
    if (!keystroke) return;
    const result = setShortcut(action.id, keystroke);
    if (result.ok) {
      closeEditor();
    } else {
      setError({
        detail: result.detail,
        takeable: result.reason === "conflict" ? keystroke : undefined,
      });
    }
  };

  const handleReset = () => {
    if (humanOnly?.overridden) humanOnly.onReset();
    const reclaimed = resetShortcut(action.id);
    closeEditor();
    if (reclaimed)
      toast(`${formatKeystrokeDisplay(action.defaultKeystroke)} is back on ${action.label}.`);
  };

  const note = editing ? null : unboundNote(action, binding);

  return (
    <div data-testid={`shortcut-row-${action.id}`} className="py-2">
      <div className="flex items-center justify-between gap-3">
        <span className="text-sm">{action.label}</span>
        <div className="flex items-center gap-2">
          {editing ? (
            <button
              ref={captureRef}
              type="button"
              data-testid={`shortcut-row-${action.id}-capture`}
              onKeyDown={handleCapture}
              className="rounded border border-accent bg-accent/20 px-2 py-1 text-xs font-mono text-foreground"
            >
              {pendingModifiers ? `${pendingModifiers} …` : "Press keys, or ⌫ to remove"}
            </button>
          ) : (
            <kbd
              data-testid={`shortcut-row-${action.id}-current`}
              className="rounded border border-border bg-muted/60 px-2 py-0.5 text-xs font-mono"
            >
              {current ? (
                formatKeystrokeDisplay(current)
              ) : (
                <span className="font-sans text-muted-foreground">No shortcut</span>
              )}
            </kbd>
          )}
          <AppButton
            size="sm"
            variant="ghost"
            data-testid={`shortcut-row-${action.id}-edit`}
            onClick={() => setEditing((e) => !e)}
          >
            {editing ? "Cancel" : "Edit"}
          </AppButton>
          <AppButton
            size="sm"
            variant="ghost"
            data-testid={`shortcut-row-${action.id}-reset`}
            aria-label={
              hasOverride || humanOnly?.overridden ? "Reset to default" : "Already at default"
            }
            disabled={!hasOverride && !humanOnly?.overridden}
            onClick={handleReset}
          >
            Reset
          </AppButton>
          {humanOnly && (
            <Switch
              size="sm"
              checked={humanOnly.on}
              disabled={humanOnly.locked}
              aria-label={humanOnly.label}
              data-testid={`shortcut-row-${action.id}-human-only`}
              onCheckedChange={humanOnly.onChange}
            />
          )}
        </div>
      </div>
      {error && (
        <div className="mt-1 flex items-center gap-2">
          <p
            data-testid={`shortcut-row-${action.id}-error`}
            className="text-xs text-red-600 dark:text-red-400"
          >
            {error.detail}
          </p>
          {error.takeable && (
            <AppButton
              size="sm"
              variant="outline"
              data-testid={`shortcut-row-${action.id}-take`}
              onClick={() => {
                takeChord(action, error.takeable!);
                closeEditor();
              }}
            >
              Use {formatKeystrokeDisplay(error.takeable)} for this
            </AppButton>
          )}
        </div>
      )}
      {humanOnly?.note && (
        <p
          data-testid={`shortcut-row-${action.id}-human-only-note`}
          className="mt-1 text-xs text-muted-foreground"
        >
          {humanOnly.note}
        </p>
      )}
      {note && (
        <p
          data-testid={`shortcut-row-${action.id}-note`}
          className="mt-1 text-xs text-muted-foreground"
        >
          {note}
        </p>
      )}
    </div>
  );
}

/** Export a helper used by `ShortcutsSection` for the "Reset all" toast flow. */
export function showResetAllConfirm(): void {
  toast("Reset all shortcuts to defaults?", {
    action: {
      label: "Reset",
      onClick: () => useShortcutsStore.getState().resetAll(),
    },
  });
}
