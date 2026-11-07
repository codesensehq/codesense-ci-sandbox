// SPDX-License-Identifier: MIT
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
  type WheelEvent,
} from "react";
import { ChevronDown, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { InfoTip } from "@/components/ui/tooltip";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

import { tabLabels, tabsOutOfView } from "../../lib/editor-tabs";
import { FileGlyph } from "./file-glyph";

export interface TabState {
  dirty: boolean;
  /** Disk diverged under unsaved edits — tinted so a background tab's
   *  conflict is visible before switching to it. */
  stale: boolean;
}

/**
 * The open-file tabs. Follows the convention people already know from their
 * editors: the active tab joins the editor surface (same background, an accent
 * rule on top), a dirty tab shows a dot that turns into the close button on
 * hover, middle-click closes, and the strip scrolls sideways rather than
 * squeezing names to nothing. Arrow keys move between tabs (roving tabindex).
 * When tabs run off either end, the edges fade and a count opens a list of
 * every open tab; a mouse wheel scrolls the strip sideways.
 */
export function EditorTabStrip({
  tabs,
  active,
  states,
  rootPath,
  onActivate,
  onClose,
  leading,
  trailing,
}: {
  tabs: readonly string[];
  active: string | null;
  states: ReadonlyMap<string, TabState>;
  rootPath: string;
  onActivate: (path: string) => void;
  onClose: (path: string) => void;
  /** A control pinned to the strip's left end (show files). */
  leading?: ReactNode;
  /** Controls pinned to the strip's right end (quick open, save, close…). */
  trailing?: ReactNode;
}) {
  const labels = useMemo(() => tabLabels(tabs), [tabs]);
  const listRef = useRef<HTMLDivElement>(null);

  // Keep the active tab scrolled into view when it changes (quick open can
  // activate a tab that sits off the end of a long strip).
  useEffect(() => {
    if (!active) return;
    const el = listRef.current?.querySelector<HTMLElement>(
      `[data-tab-path="${CSS.escape(active)}"]`,
    );
    el?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }, [active]);

  // Overflow: which tabs are off either end of the strip.
  const [overflow, setOverflow] = useState({ before: 0, after: 0 });
  const measure = useCallback(() => {
    const list = listRef.current;
    if (!list) return;
    const spans = Array.from(list.querySelectorAll<HTMLElement>("[data-tab-path]"), (el) => ({
      left: el.offsetLeft - list.offsetLeft,
      right: el.offsetLeft - list.offsetLeft + el.offsetWidth,
    }));
    const next = tabsOutOfView(spans, list.scrollLeft, list.clientWidth);
    setOverflow((prev) => (prev.before === next.before && prev.after === next.after ? prev : next));
  }, []);
  useEffect(() => {
    const list = listRef.current;
    if (!list || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => measure());
    ro.observe(list);
    return () => ro.disconnect();
  }, [measure]);
  useEffect(() => {
    measure();
  }, [tabs, labels, measure]);
  const hidden = overflow.before + overflow.after;

  // A plain mouse wheel scrolls the strip sideways (the strip never scrolls
  // vertically, so nothing else wants the wheel here).
  const onWheel = (e: WheelEvent<HTMLDivElement>) => {
    const list = listRef.current;
    if (!list || Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return;
    list.scrollLeft += e.deltaY;
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft" && e.key !== "Home" && e.key !== "End") {
      return;
    }
    const index = active ? tabs.indexOf(active) : -1;
    let next = index;
    if (e.key === "ArrowRight") next = Math.min(tabs.length - 1, index + 1);
    if (e.key === "ArrowLeft") next = Math.max(0, index - 1);
    if (e.key === "Home") next = 0;
    if (e.key === "End") next = tabs.length - 1;
    const target = tabs[next];
    if (!target) return;
    e.preventDefault();
    onActivate(target);
    listRef.current?.querySelector<HTMLElement>(`[data-tab-path="${CSS.escape(target)}"]`)?.focus();
  };

  return (
    <div className="flex h-9 min-w-0 shrink-0 items-stretch border-b border-[var(--border)] bg-[var(--card)]">
      {leading && (
        <div className="flex shrink-0 items-center border-r border-[var(--border)] px-1.5">
          {leading}
        </div>
      )}
      <div
        ref={listRef}
        role="tablist"
        aria-label="Open files"
        onKeyDown={onKeyDown}
        onScroll={measure}
        onWheel={onWheel}
        style={{ maskImage: edgeMask(overflow.before > 0, overflow.after > 0) }}
        className="flex min-w-0 flex-1 items-stretch overflow-x-auto overflow-y-hidden [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
      >
        {tabs.map((path) => {
          const isActive = path === active;
          const label = labels.get(path) ?? { name: path, hint: null };
          const state = states.get(path);
          const dirty = state?.dirty ?? false;
          const rel = path.startsWith(`${rootPath}/`) ? path.slice(rootPath.length + 1) : path;
          return (
            <div
              key={path}
              role="tab"
              aria-selected={isActive}
              aria-label={`${label.name}${dirty ? ", unsaved changes" : ""}`}
              tabIndex={isActive ? 0 : -1}
              data-tab-path={path}
              data-testid={`worktree-tab-${rel}`}
              onClick={() => onActivate(path)}
              onAuxClick={(e) => {
                if (e.button === 1) {
                  e.preventDefault();
                  onClose(path);
                }
              }}
              title={rel}
              className={cn(
                "group relative flex max-w-[15rem] shrink-0 cursor-pointer select-none items-center gap-1.5 border-r border-[var(--border)] pl-3 pr-1.5 text-xs transition-colors duration-150",
                "focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:[outline-color:var(--ring)]",
                isActive
                  ? "bg-[var(--background)] text-foreground shadow-[inset_0_2px_0_var(--accent)]"
                  : "text-muted-foreground hover:bg-[var(--background)]/60 hover:text-foreground",
                state?.stale && "text-[var(--cs-status-modified)]",
              )}
            >
              {/* The active tab erases the strip's bottom rule so it reads as
                  one surface with the editor beneath it. */}
              {isActive && (
                <span
                  aria-hidden
                  className="absolute inset-x-0 -bottom-px h-px bg-[var(--background)]"
                />
              )}
              <FileGlyph path={path} />
              <span className="min-w-0 truncate">{label.name}</span>
              {label.hint && (
                <span className="min-w-0 truncate text-[0.68rem] text-muted-foreground/80">
                  {label.hint}
                </span>
              )}
              <TabCloseButton
                name={label.name}
                dirty={dirty}
                onClose={() => onClose(path)}
                visible={isActive}
              />
            </div>
          );
        })}
      </div>
      {hidden > 0 && (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              data-testid="worktree-tab-overflow"
              aria-label={`${hidden} more open ${hidden === 1 ? "file" : "files"} — show all tabs`}
              className="flex shrink-0 items-center gap-0.5 border-l border-[var(--border)] px-2 text-[0.7rem] tabular-nums text-muted-foreground hover:bg-[var(--background)]/60 hover:text-foreground focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:[outline-color:var(--ring)]"
            >
              +{hidden}
              <ChevronDown aria-hidden className="size-3" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="max-h-[min(24rem,70vh)] w-64 overflow-y-auto">
            {tabs.map((path) => {
              const label = labels.get(path) ?? { name: path, hint: null };
              const dirty = states.get(path)?.dirty ?? false;
              return (
                <DropdownMenuItem
                  key={path}
                  onSelect={() => onActivate(path)}
                  className={cn("gap-2 text-xs", path === active && "font-medium")}
                >
                  <FileGlyph path={path} />
                  <span className="min-w-0 flex-1 truncate">
                    {label.name}
                    {label.hint && (
                      <span className="ml-1.5 text-muted-foreground">{label.hint}</span>
                    )}
                  </span>
                  {dirty && (
                    <span
                      aria-label="unsaved changes"
                      className="size-1.5 shrink-0 rounded-full bg-foreground/70"
                    />
                  )}
                </DropdownMenuItem>
              );
            })}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      {trailing && (
        <div className="flex shrink-0 items-center gap-0.5 border-l border-[var(--border)] px-1.5">
          {trailing}
        </div>
      )}
    </div>
  );
}

function TabCloseButton({
  name,
  dirty,
  visible,
  onClose,
}: {
  name: string;
  dirty: boolean;
  visible: boolean;
  onClose: () => void;
}) {
  const button = (
    <button
      type="button"
      aria-label={dirty ? `Close ${name} (unsaved changes)` : `Close ${name}`}
      onClick={(e) => {
        e.stopPropagation();
        onClose();
      }}
      tabIndex={-1}
      className={cn(
        "relative ml-0.5 inline-flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground",
        !dirty && !visible && "opacity-0 group-hover:opacity-100",
      )}
    >
      {dirty ? (
        <>
          {/* The unsaved dot; hover swaps it for the close cross. */}
          <span
            data-testid="worktree-dirty-dot"
            aria-hidden
            className="size-2 rounded-full bg-foreground/70 group-hover:hidden"
          />
          <X size={13} aria-hidden className="hidden group-hover:block" />
        </>
      ) : (
        <X size={13} aria-hidden />
      )}
    </button>
  );
  if (!dirty) return button;
  return (
    <InfoTip label="Unsaved changes" side="bottom">
      {button}
    </InfoTip>
  );
}

/** Fade whichever edge has tabs scrolled past it. */
function edgeMask(start: boolean, end: boolean): string | undefined {
  if (!start && !end) return undefined;
  const fade = "1.5rem";
  return `linear-gradient(to right, ${start ? "transparent" : "#000"} 0, #000 ${start ? fade : "0"}, #000 calc(100% - ${end ? fade : "0px"}), ${end ? "transparent" : "#000"} 100%)`;
}
