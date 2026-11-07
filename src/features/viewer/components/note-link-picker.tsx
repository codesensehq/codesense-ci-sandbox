// SPDX-License-Identifier: MIT
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { StickyNote, Workflow, FileText } from "lucide-react";
import type { PickerItemKind } from "../lib/note-link-picker-items";
import type { NoteLinkPickerState } from "../lib/note-link-suggestion";

const KIND_ICON: Record<PickerItemKind, typeof StickyNote> = {
  note: StickyNote,
  story: Workflow,
  file: FileText,
};

/** SW-SN-9 Task 2: the `[[` autocomplete popup. Presentational + keyboard —
 *  it renders the current candidates at the caret, owns the highlighted index,
 *  and registers a keydown handler that `@tiptap/suggestion` forwards editor
 *  keystrokes to (arrow/enter select; escape dismisses; anything else falls
 *  through to the editor). a11y: a `listbox` with `option` rows + `aria-selected`
 *  (AC-7). */
export function NoteLinkPicker({
  state,
  registerKeyHandler,
}: {
  state: NoteLinkPickerState;
  registerKeyHandler: (fn: ((event: KeyboardEvent) => boolean) | null) => void;
}) {
  const { items } = state;
  const [index, setIndex] = useState(0);
  const indexRef = useRef(0);
  const listRef = useRef<HTMLDivElement>(null);

  // A new query (fresh `state` object each keystroke) resets the selection.
  // Render-phase reset on prop change (React's recommended pattern — mirrors the
  // panel's `prevHeaderActive`), not an effect. Escape/dismissal is owned by the
  // suggestion plugin (it calls onKeyDown then dispatchExit → onChange(null) →
  // this unmounts), so there's no local dismissed state to track.
  const [prevState, setPrevState] = useState(state);
  if (state !== prevState) {
    setPrevState(state);
    setIndex(0);
  }

  // Mirror the highlighted index into a ref (post-commit) so the forwarded
  // keydown handler can read the live value without re-registering each move.
  useEffect(() => {
    indexRef.current = index;
  }, [index]);

  // Register the keyboard handler suggestion forwards to. Reads the live index
  // via a ref so it doesn't need to re-register on every arrow press.
  useEffect(() => {
    registerKeyHandler((event) => {
      if (!items.length) return false;
      switch (event.key) {
        case "ArrowDown":
          setIndex((i) => (i + 1) % items.length);
          return true;
        case "ArrowUp":
          setIndex((i) => (i - 1 + items.length) % items.length);
          return true;
        case "Enter":
        case "Tab": {
          const item = items[indexRef.current];
          if (item) state.select(item);
          return true;
        }
        // Escape is intentionally NOT handled here — the suggestion plugin exits
        // on Escape itself; returning false lets that path run.
        default:
          return false;
      }
    });
    return () => registerKeyHandler(null);
  }, [state, items, registerKeyHandler]);

  // Keep the active row scrolled into view as the selection moves.
  useLayoutEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>('[data-active="true"]');
    el?.scrollIntoView({ block: "nearest" });
  }, [index]);

  if (items.length === 0) return null;
  const rect = state.clientRect();
  if (!rect) return null;

  return (
    <div
      ref={listRef}
      role="listbox"
      aria-label="Insert link to note or doc"
      // Options never take DOM focus (mousedown is prevented so the editor keeps
      // its selection), so point AT at the active row explicitly.
      aria-activedescendant={items[index] ? `note-link-opt-${index}` : undefined}
      className="nt-pop"
      style={{
        position: "fixed",
        left: rect.left,
        top: rect.bottom + 6,
        width: 288,
        maxHeight: 256,
        overflowY: "auto",
      }}
    >
      {items.map((item, i) => {
        const Icon = KIND_ICON[item.kind];
        const active = i === index;
        return (
          <button
            key={item.key}
            id={`note-link-opt-${i}`}
            type="button"
            role="option"
            aria-selected={active}
            data-active={active}
            className="nt-row"
            // Prevent the editor losing selection before the click commits.
            onMouseDown={(e) => e.preventDefault()}
            onMouseEnter={() => setIndex(i)}
            onClick={() => state.select(item)}
          >
            <span className="nt-row-icon">
              <Icon size={14} />
            </span>
            <span className="nt-row-label">{item.label}</span>
            {item.sublabel && <span className="nt-row-sub">{item.sublabel}</span>}
          </button>
        );
      })}
    </div>
  );
}
