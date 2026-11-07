// SPDX-License-Identifier: MIT
import { forwardRef, useImperativeHandle, useRef, type CSSProperties, type ReactNode } from "react";
import { useVirtualizer, type ScrollToOptions, type VirtualItem } from "@tanstack/react-virtual";

export interface VirtualListHandle {
  scrollToIndex: (index: number, options?: ScrollToOptions) => void;
  /** The scroll container DOM node (SW-VE-7): drop hit-testing needs the nav's
   *  on-screen bounds + the row rects inside it. `null` before mount. */
  getScrollElement: () => HTMLDivElement | null;
}

export interface VirtualListProps<T> {
  items: readonly T[];
  estimateSize: number | ((index: number) => number);
  getKey: (item: T, index: number) => string | number;
  overscan?: number;
  /** Provided → row wrapper is measured (variable-height callers). */
  measureElement?: (el: HTMLElement) => number;
  className?: string;
  style?: CSSProperties;
  "aria-label"?: string;
  "data-testid"?: string;
  role?: string;
  /** Rendered when items.length === 0. Otherwise the container renders with zero rows. */
  emptyState?: ReactNode;
  renderItem: (item: T, virtualRow: VirtualItem) => ReactNode;
}

// NB-7 / AC-2 — shared headless virtualizer wrapper. Replaces 4 copies of
// the scrollRef + useVirtualizer + outer/inner div + absolute-positioned
// row scaffolding (pipeline-status-nav, members-card, teams-card,
// team-tile). Exposes scrollToIndex via ref for reveal/scroll-into-view
// callers. The inner row wrapper sets data-index + measureElement ref
// unconditionally — both are no-ops without measureElement.
function VirtualListInner<T>(
  {
    items,
    estimateSize,
    getKey,
    overscan,
    measureElement,
    className,
    style,
    role = "list",
    emptyState,
    renderItem,
    "aria-label": ariaLabel,
    "data-testid": dataTestId,
  }: VirtualListProps<T>,
  ref: React.Ref<VirtualListHandle>,
) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const estimateFn = typeof estimateSize === "function" ? estimateSize : () => estimateSize;
  // TanStack Virtual's API returns instance methods (scrollToIndex, measure,
  // etc.) that close over internal mutable state and can't be memoized by
  // React Compiler without producing stale UI. Opt this component out of
  // memoization — the virtualizer manages its own re-render scheduling.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: estimateFn,
    overscan,
    getItemKey: (i) => getKey(items[i], i),
    measureElement,
  });

  useImperativeHandle(
    ref,
    () => ({
      scrollToIndex: (index, options) => virtualizer.scrollToIndex(index, options),
      getScrollElement: () => scrollRef.current,
    }),
    // virtualizer is a stable instance per-mount from useVirtualizer; declared
    // here so the safety invariant is explicit (and self-documenting if the
    // virtualizer's identity contract ever changes upstream).
    [virtualizer],
  );

  if (items.length === 0 && emptyState !== undefined) return <>{emptyState}</>;

  const hasMeasure = !!measureElement;

  return (
    <div
      ref={scrollRef}
      role={role}
      className={className}
      style={style}
      aria-label={ariaLabel}
      data-testid={dataTestId}
    >
      <div
        style={{ height: `${virtualizer.getTotalSize()}px`, width: "100%", position: "relative" }}
      >
        {virtualizer.getVirtualItems().map((virtualRow) => {
          const item = items[virtualRow.index];
          if (item === undefined) return null;
          return (
            <div
              key={virtualRow.key}
              data-index={virtualRow.index}
              ref={hasMeasure ? virtualizer.measureElement : undefined}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                ...(hasMeasure ? null : { height: `${virtualRow.size}px` }),
                transform: `translateY(${virtualRow.start}px)`,
              }}
            >
              {renderItem(item, virtualRow)}
            </div>
          );
        })}
      </div>
    </div>
  );
}

const VirtualListBase = forwardRef(VirtualListInner);
VirtualListBase.displayName = "VirtualList";
// Cast around forwardRef's generic-erasure so call sites preserve the <T>
// type parameter. displayName is set on the base ref'd component above so
// React DevTools and snapshot tooling see "VirtualList" instead of
// "ForwardRef".
export const VirtualList = VirtualListBase as <T>(
  props: VirtualListProps<T> & { ref?: React.Ref<VirtualListHandle> },
) => ReturnType<typeof VirtualListInner>;
