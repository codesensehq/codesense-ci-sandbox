// SPDX-License-Identifier: MIT
import { selectedIndexById } from "@/shared/lib/picker-selection";
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { CornerDownLeft, Hash, Search } from "lucide-react";

import { cn } from "@/lib/utils";
import { fuzzyScore } from "@/shared/lib/fuzzy-match";

import { FileGlyph } from "./file-glyph";

const MAX_RESULTS = 60;

/** A parsed palette query: plain text searches files; `:12` or `:12:4` goes to
 *  a line in the open file; `name:12` opens a file at a line. */
export function parseQuickOpenQuery(raw: string): {
  text: string;
  line: number | null;
  column: number | null;
} {
  const q = raw.trim();
  const m = /^(.*?):(\d+)(?::(\d+))?$/.exec(q);
  if (!m) return { text: q, line: null, column: null };
  return {
    text: (m[1] ?? "").trim(),
    line: Number(m[2]),
    column: m[3] ? Number(m[3]) : null,
  };
}

/** Indices of `query`'s characters matched greedily within `name` (case-
 *  insensitive), for highlighting; empty when it isn't a subsequence. */
export function matchIndices(query: string, name: string): number[] {
  const q = query.toLowerCase();
  const n = name.toLowerCase();
  if (!q) return [];
  const contiguous = n.indexOf(q);
  if (contiguous !== -1) return Array.from({ length: q.length }, (_, i) => contiguous + i);
  const out: number[] = [];
  let qi = 0;
  for (let i = 0; i < n.length && qi < q.length; i++) {
    if (n[i] === q[qi]) {
      out.push(i);
      qi++;
    }
  }
  return qi === q.length ? out : [];
}

/**
 * How well one query word matches a path, highest first: the file's exact
 * name (`session` → `session.ts`), a name prefix, a name substring, a folder
 * substring, then a subsequence of the name, then of the whole path. 0 = no
 * match. Folder hits never outrank file-name hits — people type file names.
 */
function scoreWord(word: string, path: string): number {
  const lower = path.toLowerCase();
  const name = lower.slice(lower.lastIndexOf("/") + 1);
  if (name === word || name.startsWith(`${word}.`)) return 200;
  if (name.startsWith(word)) return 150;
  if (name.includes(word)) return 120;
  if (lower.includes(word)) return 60;
  const inName = fuzzyScore(word, name);
  if (inName > 0) return 30 + Math.min(inName, 29);
  return Math.min(fuzzyScore(word, lower), 29);
}

/** Every space-separated word must match; scores add, shorter paths win ties. */
export function rankFiles(query: string, files: readonly string[]): string[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return files.slice(0, MAX_RESULTS);
  const scored: { path: string; score: number }[] = [];
  for (const path of files) {
    let score = 0;
    for (const word of words) {
      const s = scoreWord(word, path);
      if (s === 0) {
        score = 0;
        break;
      }
      score += s;
    }
    if (score > 0) scored.push({ path, score });
  }
  scored.sort((a, b) => b.score - a.score || a.path.length - b.path.length);
  return scored.slice(0, MAX_RESULTS).map((s) => s.path);
}

/**
 * The editor's quick open (⌘P) and go to line (⌃G) in one palette, scoped to
 * this worktree and drawn inside the editor rather than over the whole app.
 * With an empty query it lists the open tabs (most useful first); typing
 * fuzzy-matches every file Git knows about in the worktree, tracked or not.
 */
export function EditorQuickOpen({
  initialQuery,
  files,
  filesError,
  truncated,
  recent,
  lineCount,
  onOpenFile,
  onGoToLine,
  onClose,
}: {
  initialQuery: string;
  /** Root-relative paths; `null` while loading. */
  files: readonly string[] | null;
  filesError: string | null;
  truncated: boolean;
  /** Root-relative paths of the open tabs, shown before typing. */
  recent: readonly string[];
  /** Lines in the active file (for go-to-line's hint); `null` = no file open. */
  lineCount: number | null;
  onOpenFile: (relPath: string, line: number | null, column: number | null) => void;
  onGoToLine: (line: number, column: number | null) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState(initialQuery);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const listId = useId();

  useEffect(() => {
    const input = inputRef.current;
    if (!input) return;
    input.focus();
    // Go to line opens with ":" typed; put the caret after it.
    input.setSelectionRange(input.value.length, input.value.length);
  }, []);

  const parsed = parseQuickOpenQuery(query);
  // A leading ":" means go to line, even before any digits are typed.
  const lineMode = query.trim().startsWith(":");
  const results = useMemo(() => {
    if (lineMode) return [];
    if (!parsed.text) {
      const seen = new Set(recent);
      return [...recent, ...(files ?? []).filter((f) => !seen.has(f))].slice(0, MAX_RESULTS);
    }
    // Open tabs are searchable even when the list leaves them out (an ignored
    // file opened from the tree, or one past a truncated list).
    const listed = new Set(files ?? []);
    return rankFiles(parsed.text, [...(files ?? []), ...recent.filter((r) => !listed.has(r))]);
  }, [lineMode, parsed.text, files, recent]);
  const clamped = selectedIndexById(results, selectedPath, (path) => path);
  // The line a go-to-line lands on: within the open file, as the hint says.
  const targetLine =
    parsed.line == null || lineCount == null
      ? null
      : Math.min(Math.max(1, parsed.line), Math.max(1, lineCount));

  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-index="${clamped}"]`);
    el?.scrollIntoView?.({ block: "nearest" });
  }, [clamped]);

  const choose = (index: number) => {
    if (lineMode) {
      if (targetLine != null) onGoToLine(targetLine, parsed.column);
      onClose();
      return;
    }
    const path = results[index];
    if (!path) return;
    onOpenFile(
      path,
      parsed.line != null && parsed.line >= 1 ? parsed.line : null,
      parsed.column != null && parsed.column >= 1 ? parsed.column : null,
    );
    onClose();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    // The palette owns these keys; nothing underneath (the editor, the overlay's
    // Escape-to-close) should also act on them.
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelectedPath(results[Math.max(0, Math.min(results.length - 1, clamped + 1))] ?? null);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedPath(results[Math.max(0, clamped - 1)] ?? null);
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(clamped);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    } else {
      return;
    }
    e.stopPropagation();
  };

  return (
    <div className="absolute inset-0 z-20" onMouseDown={onClose} data-testid="worktree-quick-open">
      <div
        role="dialog"
        aria-label={lineMode ? "Go to line" : "Quick open"}
        onMouseDown={(e) => e.stopPropagation()}
        className="cs-editor-palette absolute left-1/2 top-2 flex max-h-[min(26rem,70%)] w-[min(36rem,calc(100%-1.5rem))] -translate-x-1/2 flex-col overflow-hidden rounded-lg border border-[var(--border)] bg-[var(--popover)] text-[var(--popover-foreground)] shadow-[0_12px_32px_-8px_rgb(0_0_0/0.35),0_2px_6px_-2px_rgb(0_0_0/0.2)]"
      >
        <div className="flex items-center gap-2 border-b border-[var(--border)] px-3">
          {lineMode ? (
            <Hash aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
          ) : (
            <Search aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
          )}
          <input
            ref={inputRef}
            role="combobox"
            aria-expanded={!lineMode}
            aria-controls={listId}
            aria-activedescendant={!lineMode && results.length ? `${listId}-${clamped}` : undefined}
            aria-autocomplete="list"
            spellCheck={false}
            autoComplete="off"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelectedPath(null);
            }}
            onKeyDown={onKeyDown}
            placeholder="Search files by name — or : to go to a line"
            data-testid="worktree-quick-open-input"
            className="h-10 min-w-0 flex-1 bg-transparent text-[0.8rem] text-foreground outline-none placeholder:text-muted-foreground"
          />
        </div>
        {lineMode ? (
          <div className="flex items-center gap-2 px-3 py-3 text-xs text-muted-foreground">
            <CornerDownLeft aria-hidden className="size-3.5" />
            {lineCount == null
              ? "Open a file first to go to a line."
              : targetLine != null
                ? `Go to line ${targetLine}${parsed.column ? `, column ${parsed.column}` : ""}`
                : `Type a line number between 1 and ${lineCount}.`}
          </div>
        ) : (
          <div
            ref={listRef}
            id={listId}
            role="listbox"
            aria-label="Files"
            className="min-h-0 flex-1 overflow-y-auto py-1"
          >
            {results.length === 0 ? (
              <p className="px-3 py-4 text-center text-xs text-muted-foreground">
                {filesError
                  ? `Couldn’t list the worktree’s files. ${filesError}`
                  : files == null
                    ? "Listing files…"
                    : parsed.text
                      ? `No files match “${parsed.text}”.`
                      : "This worktree has no files yet."}
              </p>
            ) : (
              results.map((path, i) => (
                <ResultRow
                  key={path}
                  id={`${listId}-${i}`}
                  index={i}
                  path={path}
                  query={parsed.text}
                  selected={i === clamped}
                  isOpen={recent.includes(path)}
                  onHover={() => setSelectedPath(path)}
                  onChoose={() => choose(i)}
                />
              ))
            )}
          </div>
        )}
        {!lineMode && truncated && (
          <p className="border-t border-[var(--border)] px-3 py-1.5 text-[0.68rem] text-muted-foreground">
            Showing the first files only — type more of the name to narrow it down.
          </p>
        )}
      </div>
    </div>
  );
}

function ResultRow({
  id,
  index,
  path,
  query,
  selected,
  isOpen,
  onHover,
  onChoose,
}: {
  id: string;
  index: number;
  path: string;
  query: string;
  selected: boolean;
  isOpen: boolean;
  onHover: () => void;
  onChoose: () => void;
}) {
  const slash = path.lastIndexOf("/");
  const name = path.slice(slash + 1);
  const dir = slash === -1 ? "" : path.slice(0, slash);
  // Highlight the word that matched the name (the last one, by convention:
  // folders first, then the file — "auth sess").
  const lastWord = query.trim().split(/\s+/).pop() ?? "";
  const hits = new Set(matchIndices(lastWord, name));
  return (
    <div
      id={id}
      role="option"
      aria-selected={selected}
      // The per-character highlight spans would otherwise be read letter by letter.
      aria-label={isOpen ? `${path} (open)` : path}
      data-index={index}
      onMouseMove={onHover}
      onClick={onChoose}
      className={cn(
        "mx-1 flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-xs",
        selected ? "bg-[var(--accent)]/12 text-foreground" : "text-foreground/90",
      )}
    >
      <FileGlyph path={path} />
      <span className="shrink-0 truncate">
        {Array.from(name).map((ch, i) =>
          hits.has(i) ? (
            <mark key={i} className="bg-transparent font-semibold text-[var(--accent)]">
              {ch}
            </mark>
          ) : (
            <span key={i}>{ch}</span>
          ),
        )}
      </span>
      <span className="min-w-0 flex-1 truncate text-[0.7rem] text-muted-foreground" dir="rtl">
        {/* rtl truncation keeps the nearest folders visible, not the root. */}
        <bdi>{dir}</bdi>
      </span>
      {isOpen && <span className="shrink-0 text-[0.65rem] text-muted-foreground">open</span>}
    </div>
  );
}
