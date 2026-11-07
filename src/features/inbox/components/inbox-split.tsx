// SPDX-License-Identifier: MIT
import { useRunningAgent } from "@/features/viewer/lib/hand-agents";
import { lazy, Suspense, useEffect, useRef, useState, type ReactNode } from "react";

import {
  cleanupAgentInboxRailTab,
  useAgentInboxRailStore,
  useAgentInboxRailWidth,
} from "@/features/viewer/stores/agent-inbox-rail-store";
import {
  useMinimizedTerminalTabIndex,
  useRunningTerminalTabIndex,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";

import { INBOX_RAIL_WIDTH_DEFAULT, InboxSplitter, inboxRailCssWidth } from "./inbox-splitter";

// The rail (markdown renderer, lenses) loads only once some tab opens its inbox.
const InboxRail = lazy(() => import("./inbox-rail").then((m) => ({ default: m.InboxRail })));

/** Below this the rail drops lens labels and slims its list column. */
const NARROW_RAIL_PX = 470;

/**
 * A tab's body under its caption: the terminal on the left and, when the tab's inbox is open,
 * a splitter and the docked inbox on the right. The terminal keeps the same parent element
 * whether or not the inbox shows, so opening it never remounts (or re-attaches) the terminal.
 */
export function InboxSplit({
  tab,
  teamKey,
  visible = true,
  children,
}: {
  tab: { id: string; sessionId: string };
  teamKey: string;
  visible?: boolean;
  children: ReactNode;
}) {
  const runningAgent = useRunningAgent(tab.sessionId);
  const target = useAgentInboxRailStore((s) => s.bySession[tab.sessionId]);
  const width = useAgentInboxRailWidth(tab.id) ?? INBOX_RAIL_WIDTH_DEFAULT;
  const ref = useRef<HTMLDivElement>(null);
  const [splitWidth, setSplitWidth] = useState(0);

  useEffect(() => {
    const el = ref.current;
    if (!visible || !target || !el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver((entries) => setSplitWidth(entries[0]?.contentRect.width ?? 0));
    ro.observe(el);
    return () => ro.disconnect();
  }, [target, visible]);
  const shown = splitWidth > 0 ? Math.min(width, splitWidth * 0.72) : width;

  return (
    // `flex-1 min-h-0`: under the caption it takes exactly the height left, never `h-full`
    // (that overflowed the tile by one caption height, see `paneBodyView`).
    <div ref={ref} className="ibx-split flex-1 min-h-0" data-testid={`inbox-split-${tab.id}`}>
      <div className="ibx-split-term">{children}</div>
      {target && visible && (
        <div
          data-surface="canvas"
          hidden={!runningAgent}
          className={runningAgent ? "contents" : "hidden"}
        >
          <InboxSplitter tabId={tab.id} width={width} containerRef={ref} />
          <Suspense fallback={<div className="ibx-rail" style={inboxRailCssWidth(width)} />}>
            <InboxRail
              tabId={tab.id}
              sessionId={tab.sessionId}
              teamKey={teamKey}
              composeRequest={target.composeRequest}
              narrow={shown < NARROW_RAIL_PX}
              visible={runningAgent}
              style={inboxRailCssWidth(width)}
            />
          </Suspense>
        </div>
      )}
    </div>
  );
}

/**
 * Drop a closed tab's remembered inbox width and lens so the maps stay bounded. A tab that only
 * left this tile (Hidden, moved to another tile, its workspace minimized) is still running and
 * keeps both.
 */
export function useInboxTabCleanup(tabs: readonly { id: string }[]): void {
  const running = useRunningTerminalTabIndex();
  const minimized = useMinimizedTerminalTabIndex();
  const known = useRef<Set<string>>(new Set());
  useEffect(() => {
    const alive = new Set<string>();
    for (const index of [running, minimized])
      for (const byWorkspace of Object.values(index))
        for (const list of Object.values(byWorkspace)) for (const t of list) alive.add(t.id);
    const here = new Set(tabs.map((t) => t.id));
    const next = new Set(here);
    for (const id of known.current) {
      if (here.has(id)) continue;
      if (alive.has(id)) next.add(id);
      else cleanupAgentInboxRailTab(id);
    }
    known.current = next;
  }, [tabs, running, minimized]);
}
