// SPDX-License-Identifier: MIT
import { selectedIndexById } from "@/shared/lib/picker-selection";
import { useState, useEffect, useRef, useCallback, useId, type ComponentType } from "react";
import { Search } from "lucide-react";
import { fuzzyScore } from "@/shared/lib/fuzzy-match";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";

/**
 * A generic pickable row for {@link QuickOpenDialog}. The dialog reads exactly
 * these three roles: `id` (selection value + React key), `label` (primary
 * display, viewer-extension-stripped), `sublabel` (fuzzy-search target +
 * secondary line). Callers adapt their domain type to this shape — file-open
 * maps `ContextFile`, SW-SC-12 maps `StoryRef`.
 */
export interface QuickPickerItem {
  /**
   * Selection value + React key. MUST be unique within `items`: SW-FX-45 tracks
   * the keyboard selection by this identity (deriving the highlighted index via
   * `findIndex`), so a duplicate `id` would make the highlight collapse onto the
   * first occurrence and leave later duplicates unreachable by keyboard. All
   * callers key it on a stable, unique handle (file path, note id, encoded slug).
   */
  id: string;
  label: string;
  sublabel: string;
  /**
   * Optional ordering tier — the PRIMARY sort key when a query is active
   * (lower tier ranks first; fuzzy score breaks ties within a tier). Callers
   * that want a strict grouping the dialog's fuzzy score must not cross use
   * this; `undefined` (the default) means tier `0`, so existing single-tier
   * callers (⌘P, ⌘K) are unaffected. SW-SN-11 puts title matches in tier `0`
   * and body/content matches in tier `1` so titles always rank above bodies.
   */
  tier?: number;
  /** A leading 14 px icon. When any row has one, rows without keep the gutter. */
  icon?: ComponentType<{ size?: number; className?: string }>;
  /** The row's current chord, already formatted for display. */
  chord?: string;
}

interface QuickOpenDialogProps {
  open: boolean;
  onClose: () => void;
  onSelect: (id: string) => void;
  items: QuickPickerItem[];
  /** Search-input placeholder. Defaults to the file-open copy. */
  placeholder?: string;
  /** Empty-results message. Defaults to the file-open copy. */
  emptyLabel?: string;
  /** A failed index can coexist with usable cached navigation results. */
  error?: string | null;
  /**
   * Fired whenever the internal query changes (including the reset-to-empty on
   * open). Lets a caller drive an async item source off the live query — e.g.
   * SW-SN-11 debounces this to run the `search_notes` body scan and append
   * body-match items. Optional: with no handler the dialog behaves exactly as
   * before (⌘P / ⌘K unaffected). Keep the handler stable or cheap — it fires
   * once per query change.
   */
  onQueryChange?: (query: string) => void;
  /**
   * When true, an async item source is still resolving — the dialog shows a
   * "searching" affordance instead of the empty-results message so a caller
   * whose items arrive after a scan (SW-SN-11) doesn't flash "not found" then
   * contradict it. Optional; default false = today's behavior.
   */
  loading?: boolean;
  /** Copy shown in place of {@link emptyLabel} while `loading` and no results. */
  loadingLabel?: string;
  /**
   * SW-FX-45: when true (default), the primary display strips a recognised
   * viewer extension from `label` (`Release.md` → `Release`) — correct for
   * filename labels (⌘P file-open, relate-doc, attach-to-story). Callers whose
   * labels are FREEFORM titles, not filenames (⌘O note titles, ⌘⇧O headings),
   * pass `false` so the label renders verbatim and agrees with the rail. Match
   * logic is unaffected either way — this only governs display.
   */
  stripExtension?: boolean;
}

const MAX_VISIBLE = 15;

export function QuickOpenDialog({
  open,
  onClose,
  onSelect,
  items,
  placeholder = "Search files...",
  emptyLabel = "No files found",
  error,
  onQueryChange,
  loading = false,
  loadingLabel = "Searching…",
  stripExtension = true,
}: QuickOpenDialogProps) {
  const [query, setQuery] = useState("");
  // SW-FX-45: track the selection by item IDENTITY, not index — the highlighted
  // index is DERIVED from this below, so it follows its item across a live/async
  // `items` reorder (⌘O note list refresh, SW-SN-11 body-scan append, ⌘P fs-change)
  // instead of snapping to whatever now sits at the old index. `null` = no explicit
  // selection yet → the derivation falls back to the first row.
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const listId = useId();

  // Notify the caller through a ref so firing doesn't depend on the handler's
  // identity (an inline parent callback won't matter).
  const onQueryChangeRef = useRef(onQueryChange);
  useEffect(() => {
    onQueryChangeRef.current = onQueryChange;
  });

  // Update the query AND notify the caller in the SAME synchronous update, so a
  // caller that mirrors the query (SW-SN-11) re-renders in the same React batch
  // as this dialog — no one-frame skew where the two transiently disagree on the
  // current query. Every query mutation (typing + the reset-on-open) goes through
  // here so the notification never falls out of sync with the state.
  const changeQuery = useCallback((next: string) => {
    setQuery(next);
    setSelectedId(null);
    onQueryChangeRef.current?.(next);
  }, []);

  // Match on the trimmed query so surrounding whitespace never changes results
  // (a trailing space while mid-type would otherwise drop matches — and, for an
  // async caller like SW-SN-11, disagree with a backend that trims its query).
  // The input still displays the raw text; only matching uses the trimmed form.
  const q = query.trim();

  // Filter and rank by the BEST match across both the visible label and the
  // sublabel — so an item is findable by the text on either line. (Epic picker
  // rows, for one, display their title as the label but carry a dir slug as the
  // sublabel; searching sublabel alone made them unfindable by name.)
  const filtered = q
    ? items
        .map((item) => ({
          item,
          score: Math.max(
            fuzzyScore(q, item.label, { filenameFromPath: true }),
            fuzzyScore(q, item.sublabel, { filenameFromPath: true }),
          ),
        }))
        .filter((r) => r.score > 0)
        // Tier is the PRIMARY key (lower first); fuzzy score orders within a
        // tier. Default tier 0 → single-tier callers sort purely by score as
        // before.
        .sort((a, b) => (a.item.tier ?? 0) - (b.item.tier ?? 0) || b.score - a.score)
        .map((r) => r.item)
    : items;

  const visible = filtered.slice(0, MAX_VISIBLE);
  const iconGutter = visible.some((item) => item.icon);

  // SW-FX-45: DERIVE the highlighted index from the selected identity every
  // render. `findIndex` → -1 when the selected item is gone (deleted / filtered
  // out) or when nothing is explicitly selected (`selectedId === null`), and
  // `Math.max(0, …)` clamps that to the first row — no stale index, no crash.
  // Derived (not stored), so it never disagrees with the current visible rows.
  const selectedIndex = selectedIndexById(visible, selectedId, (item) => item.id);

  // Focus input when dialog opens
  useEffect(() => {
    if (open) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- intentional reset + notify when the dialog opens
      changeQuery(""); // clears any prior async items in the caller
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open, changeQuery]);

  // Scroll selected item into view
  useEffect(() => {
    if (!listRef.current) return;
    const item = listRef.current.children[selectedIndex] as HTMLElement | undefined;
    item?.scrollIntoView({ block: "nearest" });
  }, [selectedIndex]);

  // Move the highlight by `delta` rows, clamped to the list bounds, and store the
  // landed item's IDENTITY (SW-FX-45). Both arrow keys share this one clamp so the
  // two directions can't drift; an empty list resolves to `null` (no crash).
  const moveSelection = useCallback(
    (delta: number) => {
      const next = Math.min(Math.max(selectedIndex + delta, 0), visible.length - 1);
      setSelectedId(visible[next]?.id ?? null);
    },
    [visible, selectedIndex],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        moveSelection(1);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        moveSelection(-1);
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (visible[selectedIndex]) {
          onSelect(visible[selectedIndex].id);
          onClose();
        }
      } else if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    },
    [visible, selectedIndex, moveSelection, onSelect, onClose],
  );

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[15vh]" onClick={onClose}>
      {/* Backdrop */}
      <div className="fixed inset-0 bg-black/50" />

      {/* Dialog */}
      <div
        className="relative z-50 w-[500px] max-h-[60vh] bg-popover border rounded-lg shadow-xl flex flex-col overflow-hidden"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={handleKeyDown}
      >
        {/* Search input */}
        <div className="flex items-center gap-2 px-3 py-2 border-b">
          <Search size={14} className="text-muted-foreground flex-shrink-0" />
          <input
            ref={inputRef}
            role="combobox"
            aria-label={placeholder}
            aria-expanded={open}
            aria-autocomplete="list"
            aria-controls={listId}
            aria-activedescendant={
              visible[selectedIndex] ? `${listId}-${selectedIndex}` : undefined
            }
            type="text"
            className="flex-1 bg-transparent text-sm text-foreground placeholder:text-muted-foreground outline-none"
            placeholder={placeholder}
            value={query}
            onChange={(e) => changeQuery(e.target.value)}
          />
        </div>

        {error && (
          <div role="alert" className="px-3 py-2 text-sm text-destructive">
            {error}
          </div>
        )}

        {/* Results list */}
        <div
          ref={listRef}
          id={listId}
          role="listbox"
          aria-label="Results"
          className="overflow-y-auto flex-1"
        >
          {filtered.length === 0 ? (
            <div className="px-3 py-6 text-center text-sm text-muted-foreground">
              {loading ? loadingLabel : emptyLabel}
            </div>
          ) : (
            visible.map((item, i) => {
              const selected = i === selectedIndex;
              const Icon = item.icon;
              return (
                <button
                  key={item.id}
                  id={`${listId}-${i}`}
                  role="option"
                  aria-selected={selected}
                  className={`w-full text-left px-3 py-2 flex items-center gap-2 transition-colors ${
                    selected ? "bg-accent text-accent-foreground" : "hover:bg-muted/50"
                  }`}
                  onClick={() => {
                    onSelect(item.id);
                    onClose();
                  }}
                  onMouseMove={() => setSelectedId(item.id)}
                >
                  {iconGutter &&
                    (Icon ? (
                      <Icon
                        size={14}
                        className={`flex-shrink-0 ${selected ? "text-accent-foreground" : "text-muted-foreground"}`}
                      />
                    ) : (
                      <span aria-hidden className="w-3.5 flex-shrink-0" />
                    ))}
                  <span className="flex min-w-0 flex-1 flex-col">
                    <span className="text-sm font-medium truncate">
                      {stripExtension ? stripViewerExtension(item.label) : item.label}
                    </span>
                    <span
                      className={`text-xs truncate ${selected ? "text-accent-foreground" : "text-muted-foreground"}`}
                    >
                      {item.sublabel}
                    </span>
                  </span>
                  {item.chord && (
                    <kbd
                      className={`flex-shrink-0 rounded border px-1.5 py-0.5 font-mono text-[11px] ${
                        selected
                          ? "border-accent-foreground/35 bg-accent-foreground/12 text-accent-foreground"
                          : "border-border bg-muted/60 text-muted-foreground"
                      }`}
                    >
                      {item.chord}
                    </kbd>
                  )}
                </button>
              );
            })
          )}
          {filtered.length > MAX_VISIBLE && (
            <div className="px-3 py-1.5 text-xs text-muted-foreground text-center border-t">
              {filtered.length - MAX_VISIBLE} more results...
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
