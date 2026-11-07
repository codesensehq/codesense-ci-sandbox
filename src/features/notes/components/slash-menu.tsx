// SPDX-License-Identifier: MIT
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  CheckSquare,
  Code2,
  Heading1,
  Heading2,
  Heading3,
  List,
  ListOrdered,
  Minus,
  Quote,
} from "lucide-react";

import type { SlashMenuState } from "../lib/slash-commands";

const ICONS: Record<string, typeof List> = {
  task: CheckSquare,
  h1: Heading1,
  h2: Heading2,
  h3: Heading3,
  bullets: List,
  numbers: ListOrdered,
  code: Code2,
  quote: Quote,
  divider: Minus,
};

/** The `/` menu at the caret: blocks to insert, with the markdown that types them. */
export function SlashMenu({
  state,
  registerKeyHandler,
}: {
  state: SlashMenuState;
  registerKeyHandler: (fn: ((event: KeyboardEvent) => boolean) | null) => void;
}): React.JSX.Element | null {
  const { items } = state;
  const [index, setIndex] = useState(0);
  const indexRef = useRef(0);
  const listRef = useRef<HTMLDivElement>(null);

  const [prevState, setPrevState] = useState(state);
  if (state !== prevState) {
    setPrevState(state);
    setIndex(0);
  }

  useEffect(() => {
    indexRef.current = index;
  }, [index]);

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
        default:
          return false;
      }
    });
    return () => registerKeyHandler(null);
  }, [state, items, registerKeyHandler]);

  useLayoutEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>('[data-active="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [index]);

  const rect = state.clientRect();
  if (!rect || items.length === 0) return null;

  return (
    <div
      ref={listRef}
      role="listbox"
      aria-label="Insert a block"
      aria-activedescendant={`nt-slash-${index}`}
      className="nt-pop"
      style={{
        position: "fixed",
        left: rect.left,
        top: rect.bottom + 6,
        width: 240,
        maxHeight: 300,
        overflowY: "auto",
      }}
    >
      <div className="nt-pop-label">Insert</div>
      {items.map((item, i) => {
        const Icon = ICONS[item.id] ?? List;
        const active = i === index;
        return (
          <button
            key={item.id}
            id={`nt-slash-${i}`}
            type="button"
            role="option"
            aria-selected={active}
            data-active={active}
            className="nt-row"
            onMouseDown={(e) => e.preventDefault()}
            onMouseEnter={() => setIndex(i)}
            onClick={() => state.select(item)}
          >
            <span className="nt-row-icon">
              <Icon size={14} />
            </span>
            <span className="nt-row-label">{item.label}</span>
            <span className="nt-row-sub">
              <kbd className="nt-kbd" style={{ fontFamily: "var(--nt-font-mono)" }}>
                {item.hint}
              </kbd>
            </span>
          </button>
        );
      })}
    </div>
  );
}
