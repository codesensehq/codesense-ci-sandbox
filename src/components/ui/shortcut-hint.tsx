// SPDX-License-Identifier: MIT
/**
 * Tooltip body: a label with an optional keyboard-shortcut chip. Shared by the
 * title-bar workspace identity, team switcher, and CLI bottom chrome so the
 * kbd-chip styling lives in one place (SW-EW-35 dedupe). Pass an already-
 * formatted keystroke (`formatKeystrokeDisplay(...)`) as `shortcut`.
 */
export function ShortcutHint({ label, shortcut }: { label: string; shortcut?: string }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span>{label}</span>
      {shortcut && (
        <kbd
          data-slot="kbd"
          className="rounded border border-background/30 bg-background/10 px-1 font-mono text-[10px]"
        >
          {shortcut}
        </kbd>
      )}
    </span>
  );
}
