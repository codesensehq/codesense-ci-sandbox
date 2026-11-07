// SPDX-License-Identifier: MIT
import type { Extension } from "@codemirror/state";
import type { CmModule } from "../cm-loader";

export interface SaveBindingDeps {
  /** Returns the current `onSave` callback (host may swap it across renders). */
  getOnSave: () => ((value: string) => Promise<void>) | undefined;
  /** Returns the current `readOnly` flag. When true, Cmd+S is a swallowed no-op. */
  getReadOnly: () => boolean;
  /** Returns the in-flight save promise (for dedup) or null. */
  getInFlight: () => Promise<void> | null;
  /** Caches the in-flight promise so additional Cmd+S keystrokes can dedup. */
  setInFlight: (p: Promise<void> | null) => void;
}

/**
 * CM6 keymap extension binding Cmd+S (and Ctrl+S on non-mac) to the host's
 * `onSave` callback. Returns `true` from the binding handler so the keystroke
 * is consumed by CM6 before codesense's global `useShortcutHandler` sees it.
 *
 * Dedup (D-23): while a prior `onSave` promise is unresolved, additional
 * Cmd+S keystrokes are swallowed (do NOT trigger a second `onSave` call).
 *
 * Read-only (AC-9): when `readOnly` is true, the keystroke is still consumed
 * (so the browser's "Save Page" dialog never fires) but `onSave` is not
 * called.
 */
export function buildSaveKeybinding(cm: CmModule, deps: SaveBindingDeps): Extension {
  return cm.keymap.of([
    {
      key: "Mod-s",
      preventDefault: true,
      run: (view) => {
        if (deps.getReadOnly()) return true;
        if (deps.getInFlight()) return true; // dedup
        const onSave = deps.getOnSave();
        if (!onSave) return true; // no callback → still consume the key
        const value = view.state.sliceDoc();
        // E2-H2 patch: defer the onSave call into a microtask via
        // `Promise.resolve().then(...)` so a *synchronous* throw from the
        // host's callback becomes a rejection that .catch handles. The
        // iter-1 `Promise.resolve(onSave(value))` form evaluated onSave
        // immediately and let sync throws escape from the CM6 keymap
        // handler. Host owns error UX per D-9; editor must not leak.
        const p = Promise.resolve()
          .then(() => onSave(value))
          .catch(() => {})
          .finally(() => {
            if (deps.getInFlight() === p) deps.setInFlight(null);
          });
        deps.setInFlight(p);
        return true;
      },
    },
  ]);
}
