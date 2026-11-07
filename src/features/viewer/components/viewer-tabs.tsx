// SPDX-License-Identifier: MIT
import { privateFile } from "@/shared/lib/private-file";
import { useEffect, useState, useRef } from "react";
import { X, Lock } from "lucide-react";
import { toast } from "sonner";

import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { resolveVirtualTabTooltip } from "@/shared/lib/virtual-viewer-registry";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { useContentSource } from "@/shared/hooks/use-content-source";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import {
  PositionedContextMenu,
  PositionedMenuItem,
} from "@/shared/components/positioned-context-menu";
import { canExportToPdf, exportViewerToPdf } from "../lib/export-pdf";
import { StarterPageMenuItems } from "./starter-page-menu-items";
import { useSummaryHeadings } from "@/shared/hooks/use-summary-headings";
import { isPipelineFileName } from "@/shared/lib/pipeline-graph/pipeline-file-filter";
import { formatStoryDisplayTitle } from "@/shared/lib/pipeline-graph/story-display-title";
import { useStoryDetails } from "@/shared/lib/pipeline-graph/use-story-details";

/**
 * A tab's label: a story file with a frontmatter `title:` reads "<id> — <title>" like its
 * card, nav row and HUD tile; anything else keeps its file name.
 */
function TabTitle({ path, name }: { path: string; name: string }) {
  const fileName = path.split("/").pop() ?? name;
  const isStory = isPipelineFileName(fileName);
  const headings = useSummaryHeadings();
  const { data } = useStoryDetails(isStory ? path : undefined, headings);
  const titleMeta = data?.titleMeta?.trim();
  // The file name stays one hover away once the tab shows the story's title.
  return titleMeta ? (
    <span
      title={
        privateFile(path) ? `Private · only you see it · ${privateFile(path)!.relative}` : fileName
      }
    >
      {formatStoryDisplayTitle({ titleMeta, fileName })}
    </span>
  ) : (
    <>{name}</>
  );
}

interface ViewerTabsProps {
  repoPath: string;
}

/**
 * Horizontal file-tab list for the viewer. Pure tab rendering — no
 * surrounding chrome (no drag region, no height wrapper, no adjacent
 * buttons). Meant to slot into the root `TitleBar`'s middle region.
 *
 * Interactions preserved from the former `ViewerTopNav`:
 * - Click a tab → activate
 * - Middle-click → close
 * - Hover → X-on-right; click X → close
 * - Right-click → context menu (Close / Close Others / Close All)
 * - Click-drag → reorder
 */
export function ViewerTabs({ repoPath }: ViewerTabsProps) {
  // T.4 Phase 5: tab slices are team-keyed; derive teamKey from the
  // active org/team. Action callers still pass `repoPath` so the store's
  // `revealOpenedFile`/`resolveRepo` lookup can find the team's content
  // path.
  const teamKey = useActiveTeamKey();
  const tabs = useViewerStore((s) => s.openTabs?.[teamKey]) ?? [];
  const activeTabPath = useViewerStore((s) => s.activeTab?.[teamKey]) ?? "";
  const setActiveTab = useViewerStore((s) => s.setActiveTab);
  const closeTab = useViewerStore((s) => s.closeTab);
  const closeOtherTabs = useViewerStore((s) => s.closeOtherTabs);
  const closeAllTabs = useViewerStore((s) => s.closeAllTabs);
  const reorderTab = useViewerStore((s) => s.reorderTab);
  const pinTab = useViewerStore((s) => s.pinTab);
  const openFile = useViewerStore((s) => s.openFile);
  const resolvedRepo = useContentSource(repoPath);
  const exportKeystroke = useEffectiveShortcut("viewer.export-pdf");
  const exportHint = formatKeystrokeDisplay(exportKeystroke);

  const [tabContextMenu, setTabContextMenu] = useState<{
    x: number;
    y: number;
    path: string;
  } | null>(null);
  const dragState = useRef<{ idx: number; startX: number } | null>(null);
  const [dragOverIdx, setDragOverIdx] = useState<number | null>(null);
  const tabRefs = useRef<(HTMLDivElement | null)[]>([]);
  const cachedRects = useRef<(DOMRect | null)[]>([]);

  const cancelDrag = () => {
    dragState.current = null;
    cachedRects.current = [];
    setDragOverIdx(null);
  };

  // NB-7 / AC-7: NB-3's drag rects cache + pointer-state ref are normally
  // cleared on PointerUp / PointerLeave. Unmounting the component mid-drag
  // (window close, tab swap mid-pointer-move) leaves them alive on the
  // ref objects. The cleanup nulls all three ref slots so closures from
  // any in-flight event handlers don't keep DOMRect arrays GC-pinned and
  // tabRefs (per-row detached DOM nodes) get released.
  useEffect(
    () => () => {
      cachedRects.current = [];
      dragState.current = null;
      tabRefs.current = [];
    },
    [],
  );

  const activeIdx = tabs.findIndex((t) => t.path === activeTabPath);

  return (
    <>
      <div
        className="flex-1 flex items-end gap-0 mx-1 overflow-hidden min-w-0"
        onPointerMove={(e) => {
          if (!dragState.current) return;
          const dx = Math.abs(e.clientX - dragState.current.startX);
          if (dx < 5) return; // dead zone
          for (let i = 0; i < cachedRects.current.length; i++) {
            const rect = cachedRects.current[i];
            if (!rect) continue;
            if (e.clientX >= rect.left && e.clientX <= rect.right) {
              setDragOverIdx(i);
              return;
            }
          }
        }}
        onPointerUp={() => {
          if (dragState.current && dragOverIdx !== null && dragState.current.idx !== dragOverIdx) {
            reorderTab(teamKey, dragState.current.idx, dragOverIdx);
          }
          cancelDrag();
        }}
        onPointerLeave={() => {
          cancelDrag();
        }}
      >
        {/* eslint-disable react-hooks/refs -- dragState ref read during render is intentional for drag tracking without extra re-renders */}
        {tabs.map((tab, idx) => {
          const privateInfo = privateFile(tab.path);
          const isActive = tab.path === activeTabPath;
          const hideRightDivider = isActive || idx === activeIdx - 1;

          return (
            <div
              key={tab.path}
              ref={(el) => {
                tabRefs.current[idx] = el;
              }}
              className={`min-w-0 flex-1 basis-0 max-w-fit flex items-end ${
                dragOverIdx === idx && dragState.current && dragState.current.idx !== idx
                  ? "border-l-2 border-accent"
                  : ""
              } ${!isActive && !hideRightDivider ? "border-r" : ""}`}
              style={{
                borderColor: !isActive ? "var(--cs-tab-border, var(--border))" : undefined,
              }}
            >
              <button
                className={`group flex items-center gap-1 px-3 text-xs min-w-0 flex-1 ${
                  isActive
                    ? "cs-tab-item-active font-medium h-8 rounded-t-md border-t border-x -mb-[1px] relative z-10"
                    : "cs-tab-item h-7 mb-0.5 mx-0.5"
                }`}
                style={{
                  borderColor: isActive ? "var(--cs-tab-border, var(--border))" : undefined,
                }}
                title={
                  privateInfo
                    ? `Private · only you see it · ${privateInfo.relative}`
                    : (resolveVirtualTabTooltip(tab.path) ?? undefined)
                }
                aria-label={privateInfo ? `${tab.name}, private` : undefined}
                onClick={() => setActiveTab(teamKey, repoPath, tab.path)}
                // SW-FX-10: double-click = open-or-switch (consistent with
                // left-nav rows and CLI workspace tabs). `openFile` pins a
                // preview tab on commit; an explicit pin affordance also lives in
                // the tab context menu (Decision A).
                onDoubleClick={() => openFile(teamKey, repoPath, tab.path)}
                onPointerDown={(e) => {
                  if (e.button === 0) {
                    dragState.current = { idx, startX: e.clientX };
                    // Cache rects at drag start — avoids forced layout per pointer move.
                    // Drive length from `tabs` so stale slots in `tabRefs.current` (never
                    // truncated when a tab closes) don't seed phantom entries.
                    cachedRects.current = tabs.map(
                      (_, i) => tabRefs.current[i]?.getBoundingClientRect() ?? null,
                    );
                  }
                  if (e.button === 1) {
                    e.preventDefault();
                    closeTab(teamKey, repoPath, tab.path, { user: true });
                  }
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setTabContextMenu({ x: e.clientX, y: e.clientY, path: tab.path });
                }}
              >
                {privateInfo && (
                  <Lock size={10} className="shrink-0 text-muted-foreground" aria-hidden />
                )}
                <span className={`truncate ${tab.preview ? "italic" : ""}`}>
                  <TabTitle path={tab.path} name={tab.name} />
                </span>
                <span
                  role="button"
                  aria-label="Close tab"
                  className="ml-1 opacity-0 group-hover:opacity-100 hover:!opacity-100"
                  onClick={(e) => {
                    e.stopPropagation();
                    closeTab(teamKey, repoPath, tab.path, { user: true });
                  }}
                >
                  <X size={12} />
                </span>
              </button>
            </div>
          );
        })}
        {/* eslint-enable react-hooks/refs */}
      </div>

      {tabContextMenu && (
        <PositionedContextMenu
          position={tabContextMenu}
          onClose={() => setTabContextMenu(null)}
          minWidth="140px"
          label="Tab actions"
        >
          <PositionedMenuItem
            onClick={() => {
              closeTab(teamKey, repoPath, tabContextMenu.path, { user: true });
              setTabContextMenu(null);
            }}
          >
            Close
          </PositionedMenuItem>
          <PositionedMenuItem
            onClick={() => {
              closeOtherTabs(teamKey, repoPath, tabContextMenu.path);
              setTabContextMenu(null);
            }}
          >
            Close Others
          </PositionedMenuItem>
          <PositionedMenuItem
            onClick={() => {
              closeAllTabs(teamKey);
              setTabContextMenu(null);
            }}
          >
            Close All
          </PositionedMenuItem>
          {/* SW-FX-10: pin's former double-click home is now open-and-focus,
              so keep an explicit pin affordance here. Only meaningful for a
              preview (italic) tab — pinning makes it permanent. */}
          {tabs.find((t) => t.path === tabContextMenu.path)?.preview && (
            <PositionedMenuItem
              onClick={() => {
                pinTab(teamKey, tabContextMenu.path);
                setTabContextMenu(null);
              }}
            >
              Pin tab
            </PositionedMenuItem>
          )}
          <StarterPageMenuItems
            path={tabContextMenu.path}
            isFile
            onClose={() => setTabContextMenu(null)}
          />
          {canExportToPdf(tabContextMenu.path) && (
            <>
              <div role="separator" className="my-1 h-px bg-border" />
              <PositionedMenuItem
                className="flex items-center justify-between"
                onClick={() => {
                  // Match `repo-viewer-view.tsx` handleExportPdf guard:
                  // surface a toast on transient state (empty
                  // teamContentPath from non-synthetic / legacy repo
                  // entries) instead of letting buildViewerUrl produce
                  // a malformed URL silently.
                  if (resolvedRepo?.teamContentPath) {
                    void exportViewerToPdf(tabContextMenu.path, resolvedRepo.teamContentPath);
                  } else {
                    toast.error("Couldn't export — content not ready");
                  }
                  setTabContextMenu(null);
                }}
              >
                <span>Export to PDF</span>
                <span className="ml-4 text-xs text-muted-foreground">{exportHint}</span>
              </PositionedMenuItem>
            </>
          )}
        </PositionedContextMenu>
      )}
    </>
  );
}
