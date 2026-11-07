// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { ChevronDown, ChevronUp, X } from "lucide-react";
import type { ISearchOptions } from "@xterm/addon-search";

import type { TerminalFindState } from "@/shared/terminal/terminal-manager";
import { registerTerminalKeyHandle } from "../lib/terminal-tab-keys";

/** The slice of xterm's `SearchAddon` the find bar drives. */
export interface TerminalSearch {
  findNext: (term: string, options?: ISearchOptions) => boolean;
  findPrevious: (term: string, options?: ISearchOptions) => boolean;
  clearDecorations: () => void;
  onDidChangeResults: (listener: (e: { resultIndex: number; resultCount: number }) => void) => {
    dispose: () => void;
  };
}

/** Match highlights. The addon only reports result counts when decorations are on. */
const DECORATIONS: NonNullable<ISearchOptions["decorations"]> = {
  matchBackground: "#5a4a12",
  matchOverviewRuler: "#d29922",
  activeMatchBackground: "#d29922",
  activeMatchColorOverviewRuler: "#f2cc60",
};

interface TerminalFindBarProps {
  sessionId: string;
  /** The terminal's search addon, or null while the terminal isn't mounted. */
  getSearch: () => TerminalSearch | null;
  /** Clear the terminal's screen and scrollback; false when it didn't. */
  onClear: () => boolean;
  /** The bar closed — put the cursor back in the terminal. */
  onClose: () => void;
  /** The bar's state when it mounts: open (and where) or `null` (closed). The
   *  terminal's highlights are still on the display, so it is not searched
   *  again. */
  initialState?: TerminalFindState | null;
  /** Every change of the bar's state, `null` once closed — kept outside the
   *  bar so it outlives the pane. */
  onStateChange?: (state: TerminalFindState | null) => void;
  /** Changes when the terminal's display was rebuilt: its highlights are
   *  gone, so an open bar searches again. */
  displayGeneration?: number;
}

/**
 * Find in a terminal's scrollback. Opened by ⌘F from the terminal (through the
 * handle it registers for its session); Enter / ⇧Enter step to the next / previous
 * match, "Aa" makes the search case-sensitive, and Esc closes it and returns the
 * cursor to the terminal. It floats over the terminal's top-right corner.
 */
export function TerminalFindBar({
  sessionId,
  getSearch,
  onClear,
  onClose,
  initialState = null,
  onStateChange,
  displayGeneration = 0,
}: TerminalFindBarProps) {
  const [open, setOpen] = useState(initialState !== null);
  const [query, setQuery] = useState(initialState?.query ?? "");
  const [caseSensitive, setCaseSensitive] = useState(initialState?.caseSensitive ?? false);
  const regex = initialState?.regex ?? false;
  const wholeWord = initialState?.wholeWord ?? false;
  const [results, setResults] = useState<{ index: number; count: number } | null>(
    initialState && initialState.resultCount > 0
      ? { index: initialState.resultIndex, count: initialState.resultCount }
      : null,
  );
  const [focusNonce, setFocusNonce] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const onStateChangeRef = useRef(onStateChange);
  onStateChangeRef.current = onStateChange;
  useEffect(() => {
    onStateChangeRef.current?.(
      open
        ? {
            query,
            caseSensitive,
            regex,
            wholeWord,
            resultIndex: results?.index ?? -1,
            resultCount: results?.count ?? 0,
          }
        : null,
    );
  }, [open, query, caseSensitive, regex, wholeWord, results]);

  useEffect(
    () =>
      registerTerminalKeyHandle(sessionId, {
        openFind: () => {
          setOpen(true);
          setFocusNonce((n) => n + 1);
        },
        clear: onClear,
      }),
    [sessionId, onClear],
  );

  // ⌘F focuses and selects the query (again while open). A bar that mounts
  // already open, with its pane, leaves focus where it is.
  useEffect(() => {
    if (!open || focusNonce === 0) return;
    inputRef.current?.focus();
    inputRef.current?.select();
  }, [open, focusNonce]);

  useEffect(() => {
    if (!open) return;
    const sub = getSearch()?.onDidChangeResults((e) =>
      setResults({ index: e.resultIndex, count: e.resultCount }),
    );
    return () => sub?.dispose();
  }, [open, getSearch, displayGeneration]);

  const search = useCallback(
    (direction: "next" | "prev", term: string, incremental = false) => {
      const addon = getSearch();
      if (!addon) return;
      if (!term) {
        addon.clearDecorations();
        setResults(null);
        return;
      }
      const options: ISearchOptions = {
        caseSensitive,
        regex,
        wholeWord,
        incremental,
        decorations: DECORATIONS,
      };
      if (direction === "next") addon.findNext(term, options);
      else addon.findPrevious(term, options);
    },
    [getSearch, caseSensitive, regex, wholeWord],
  );

  // Re-run when the case toggle flips (not when the bar mounts already open:
  // its highlights are still on the terminal).
  const caseRef = useRef(caseSensitive);
  useEffect(() => {
    if (caseRef.current === caseSensitive) return;
    caseRef.current = caseSensitive;
    if (open) search("next", query, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only the case toggle re-runs; typing searches in onChange
  }, [caseSensitive]);

  const generationRef = useRef(displayGeneration);
  useEffect(() => {
    if (generationRef.current === displayGeneration) return;
    generationRef.current = displayGeneration;
    if (open && query) search("next", query, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only a rebuilt display re-runs
  }, [displayGeneration]);

  const close = () => {
    getSearch()?.clearDecorations();
    setOpen(false);
    setResults(null);
    onClose();
  };

  if (!open) return null;

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      search(e.shiftKey ? "prev" : "next", query);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  };

  const count = results?.count ?? 0;
  const status = !query
    ? ""
    : count === 0
      ? "No results"
      : results && results.index >= 0
        ? `${results.index + 1} of ${count}`
        : `${count} found`;

  const iconButton =
    "flex h-5 w-5 items-center justify-center rounded text-[#8b949e] hover:bg-[#ffffff14] hover:text-[#e6edf3] focus-visible:outline focus-visible:outline-1 focus-visible:outline-[#388bfd]";

  return (
    <div
      role="search"
      aria-label="Terminal search"
      data-testid="terminal-find-bar"
      className="absolute right-3 top-1 z-20 flex items-center gap-1 rounded border border-[#30363d] bg-[#161b22] px-1.5 py-1 text-xs shadow-lg"
    >
      <input
        ref={inputRef}
        type="text"
        aria-label="Find in terminal"
        placeholder="Find"
        spellCheck={false}
        className="w-40 bg-transparent px-1 text-[#e6edf3] outline-none placeholder:text-[#6e7681]"
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          search("next", e.target.value, true);
        }}
        onKeyDown={onKeyDown}
      />
      <span
        data-testid="terminal-find-count"
        aria-live="polite"
        className="min-w-[4.5rem] whitespace-nowrap text-right text-[#8b949e]"
      >
        {status}
      </span>
      <button
        type="button"
        aria-label="Match case"
        aria-pressed={caseSensitive}
        title="Match case"
        onClick={() => setCaseSensitive((c) => !c)}
        className={`${iconButton} font-medium ${caseSensitive ? "bg-[#ffffff1f] text-[#e6edf3]" : ""}`}
      >
        Aa
      </button>
      <button
        type="button"
        aria-label="Previous match"
        title="Previous match · ⇧⏎"
        onClick={() => search("prev", query)}
        className={iconButton}
      >
        <ChevronUp size={14} />
      </button>
      <button
        type="button"
        aria-label="Next match"
        title="Next match · ⏎"
        onClick={() => search("next", query)}
        className={iconButton}
      >
        <ChevronDown size={14} />
      </button>
      <button
        type="button"
        aria-label="Close find"
        title="Close · Esc"
        onClick={close}
        className={iconButton}
      >
        <X size={14} />
      </button>
    </div>
  );
}
