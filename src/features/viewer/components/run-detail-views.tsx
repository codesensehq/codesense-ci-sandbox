// SPDX-License-Identifier: MIT
import { memo, useCallback, useId, useMemo, useState, type ReactNode } from "react";

import { RunEvidenceView } from "@/features/viewer/components/run-evidence-view";
import { WeftFindingsView } from "@/features/viewer/components/weft-findings-view";
import { WeftReviewHeader } from "@/features/viewer/components/weft-review-header";
import { WvEndorsementPanel } from "@/features/weft/components/wv-endorsement-panel";
import { useRunEvidence } from "@/features/viewer/hooks/use-run-evidence";
import { useRovingTabs } from "@/shared/hooks/use-roving-tabs";
import type { RunDetailView } from "@/shared/stores/use-agent-result-store";
import { useRunReview } from "@/features/viewer/hooks/use-run-review";
import type { WvPendingAssertionDto } from "@/tauri/commands";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { useWindowVisible } from "@/shared/hooks/use-window-focused";
import { cn } from "@/lib/utils";

/** The §9.1 run-detail tabs, in visual order (`Audit` joins later). The switch is grown, not
 *  forked (ARCH-INV-10). F4 establishes `findings` before `evidence` and F5 appends
 *  `"endorsement"` at the end. */
const RUN_DETAIL_TABS = [
  "output",
  "findings", // SW-WV-F4 (inserted before evidence per ARCHITECTURE.md §4.3)
  "evidence",
  "endorsement", // SW-WV-F5 (appended last per ARCHITECTURE.md §4.3)
] as const;

const TAB_LABEL: Record<RunDetailView, string> = {
  output: "Output",
  findings: "Findings",
  evidence: "Evidence",
  endorsement: "Endorse",
};

/** The run's retained tabs and action header consume one scoped review projection. */
export function RunDetailViews({
  runId,
  storyId,
  defaultView,
  output,
  onOpenDiff,
  teamKey: providedTeamKey,
  visible = true,
}: {
  runId: string;
  storyId: string | null;
  defaultView: RunDetailView;
  output: ReactNode;
  /** SW-WV-F4: open a finding's file in the viewer (the diff overlay activates there).
   *  Absent → the Findings rows render without diff chevrons (no dead buttons). */
  onOpenDiff?: (path: string) => void;
  /** The owning team, independent of whichever team is selected later. */
  teamKey?: string;
  visible?: boolean;
}): React.JSX.Element {
  const [view, setView] = useState<RunDetailView>(defaultView);
  // Evidence is lazy, and opened bodies stay mounted so tab switches retain user state.
  const [findingsOpened, setFindingsOpened] = useState(defaultView === "findings");
  const [evidenceOpened, setEvidenceOpened] = useState(defaultView === "evidence");
  const [endorsementOpened, setEndorsementOpened] = useState(defaultView === "endorsement");
  const activate = useCallback((next: RunDetailView) => {
    setView(next);
    if (next === "findings") setFindingsOpened(true);
    if (next === "evidence") setEvidenceOpened(true);
    if (next === "endorsement") setEndorsementOpened(true);
  }, []);

  const activeTeam = useActiveTeamKey();
  const windowVisible = useWindowVisible();
  const teamKey = providedTeamKey ?? activeTeam;
  const shown = visible && windowVisible && (!providedTeamKey || teamKey === activeTeam);
  const endorsements = useRunReview("runEndorsements", runId, null, { teamKey, visible: shown });
  const assertions = useMemo(
    () =>
      endorsements.snapshot?.snapshot
        .map((row) => row.assertion as WvPendingAssertionDto)
        .sort((a, b) => a.id - b.id),
    [endorsements.snapshot],
  );
  const pendingCount = assertions?.length ?? 0;
  const endorseHidden = pendingCount === 0;
  const evidenceNeeded = evidenceOpened || findingsOpened || pendingCount > 0;
  const evidenceVisible = shown && (view === "evidence" || view === "findings" || pendingCount > 0);
  const evidence = useRunEvidence(evidenceNeeded ? storyId : null, evidenceNeeded ? runId : null, {
    teamKey,
    visible: evidenceVisible,
  });
  const evidenceReady = evidence.state.kind === "ready" ? evidence.state.evidence : null;

  const { tabProps } = useRovingTabs(RUN_DETAIL_TABS, view, activate);

  const id = useId();
  const tabId = (v: RunDetailView) => `${id}-${v}-tab`;
  const panelId = (v: RunDetailView) => `${id}-${v}-panel`;

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="run-detail-views">
      {/* SW-WV-F4 (AC-1): the single action header — verdict chip + worker summary + Ratify / Send
          back — above the tabs. Renders ONLY when the run has pending assertions (the header itself
          returns null otherwise); the status/summary come from the already-fetched evidence. */}
      <WeftReviewHeader
        assertions={assertions ?? EMPTY_ASSERTIONS}
        header={evidenceReady?.header ?? null}
      />
      <div
        role="tablist"
        aria-label="Run detail views"
        className="flex h-9 shrink-0 items-stretch gap-3 border-b border-border px-4"
      >
        {RUN_DETAIL_TABS.map((v) => {
          const isEndorse = v === "endorsement";
          const hidden = isEndorse && endorseHidden;
          const rp = tabProps(v);
          const label =
            isEndorse && pendingCount && pendingCount > 0
              ? `Endorse (${pendingCount})`
              : TAB_LABEL[v];
          return (
            <button
              key={v}
              {...rp}
              // AC-6: a hidden Endorse tab is non-focusable and aria-hidden so roving /
              // AT never lands on a tab with nothing to endorse.
              tabIndex={hidden ? -1 : rp.tabIndex}
              aria-hidden={hidden || undefined}
              id={tabId(v)}
              type="button"
              aria-controls={panelId(v)}
              onClick={() => {
                if (hidden) return;
                activate(v);
              }}
              className={cn(
                "relative px-0.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                "aria-selected:font-medium aria-selected:text-foreground aria-selected:shadow-[inset_0_-2px_0_currentColor]",
                // A hidden Endorse tab collapses out of view but stays in the DOM
                // (aria-hidden + non-focusable) so the append-only tab order holds.
                hidden && "pointer-events-none w-0 overflow-hidden p-0 opacity-0",
              )}
            >
              {label}
            </button>
          );
        })}
      </div>
      {/* AC-6: reveal is announced without stealing focus. Roving keeps focus on the active tab (WAI-ARIA
          tabs); switching to Evidence flips this polite line "" → "Evidence view" so AT users hear the
          reveal, and the tabpanel is Tab-reachable (tabIndex 0) — never dropped into silent content. */}
      <span aria-live="polite" className="sr-only">
        {view === "findings"
          ? "Findings view"
          : view === "evidence"
            ? "Evidence view"
            : view === "endorsement"
              ? "Endorsement view"
              : ""}
      </span>
      <div
        id={panelId("output")}
        role="tabpanel"
        aria-labelledby={tabId("output")}
        hidden={view !== "output"}
        className="min-h-0 flex-1"
      >
        {output}
      </div>
      <div
        id={panelId("findings")}
        role="tabpanel"
        aria-labelledby={tabId("findings")}
        hidden={view !== "findings"}
        tabIndex={0}
        className="min-h-0 flex-1 overflow-auto focus-visible:outline-none"
      >
        {findingsOpened && (
          <MemoFindings
            visible={shown && view === "findings"}
            perNodeResults={evidenceReady?.perNodeResults ?? EMPTY_RESULTS}
            onOpenDiff={onOpenDiff ?? (() => {})}
          />
        )}
      </div>
      <div
        id={panelId("evidence")}
        role="tabpanel"
        aria-labelledby={tabId("evidence")}
        hidden={view !== "evidence"}
        tabIndex={0}
        className="min-h-0 flex-1 overflow-auto focus-visible:outline-none"
      >
        <MemoEvidence
          visible={shown && view === "evidence"}
          state={evidence.state}
          onReload={evidence.reload}
        />
      </div>
      <div
        id={panelId("endorsement")}
        role="tabpanel"
        aria-labelledby={tabId("endorsement")}
        hidden={view !== "endorsement"}
        tabIndex={0}
        className="min-h-0 flex-1 overflow-auto focus-visible:outline-none"
      >
        {endorsementOpened && (
          <MemoEndorsements
            visible={shown && view === "endorsement"}
            assertions={assertions}
            error={endorsements.error}
          />
        )}
      </div>
    </div>
  );
}

const EMPTY_ASSERTIONS: WvPendingAssertionDto[] = [];
const EMPTY_RESULTS: import("@/shared/lib/run-evidence").PerNodeResult[] = [];
// Hidden bodies retain their rendered state; the visible header can still consume the shared view.
const MemoFindings = memo(
  ({
    visible: _visible,
    ...props
  }: React.ComponentProps<typeof WeftFindingsView> & { visible: boolean }) => (
    <WeftFindingsView {...props} />
  ),
  (_before, after) => !after.visible,
);
const MemoEvidence = memo(
  ({
    visible: _visible,
    ...props
  }: React.ComponentProps<typeof RunEvidenceView> & { visible: boolean }) => (
    <RunEvidenceView {...props} />
  ),
  (_before, after) => !after.visible,
);
const MemoEndorsements = memo(
  ({
    visible: _visible,
    ...props
  }: React.ComponentProps<typeof WvEndorsementPanel> & { visible: boolean }) => (
    <WvEndorsementPanel {...props} />
  ),
  (_before, after) => !after.visible,
);
