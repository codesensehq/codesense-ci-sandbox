// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { InlineAlert } from "@/components/ui/inline-alert";
import { ConfirmActionDialog } from "@/features/settings/components/confirm-action-dialog";
import { useNowClock } from "@/features/viewer/hooks/use-now-clock";
import type { CommonViewerProps } from "@/shared/lib/viewer-file-type";

import { useRoutinesStore, watchRoutineList } from "../stores/routines-store";
import { RoutineDetailPane } from "./routine-detail";
import { RoutineList } from "./routine-list";
import { RoutinesStatusStrip } from "./routines-status-strip";

/** How often relative times ("in 3h") re-render. Display only: no backend read. */
const CLOCK_MS = 30_000;

/**
 * Retired from the app (SPEC §10): routines live in the Agents tab, and
 * `virtual://routines` now redirects to Agents → Schedule. This surface stays only as
 * the harness of the shared editor's seam tests until the redirect's release ends.
 *
 * The Routines tab (`virtual://routines`): what will run, what did run and why
 * something didn't. The list sits on the left in a fixed order; the selected
 * routine's runs open on the right.
 */
export function RoutinesTab(_props: CommonViewerProps & { filePath: string }) {
  useEffect(() => watchRoutineList(), []);
  const now = useNowClock(CLOCK_MS);
  const actionError = useRoutinesStore((s) => s.actionError);
  const dismiss = useRoutinesStore((s) => s.dismissActionError);
  const notice = useRoutinesStore((s) => s.notice);
  const dismissNotice = useRoutinesStore((s) => s.dismissNotice);
  const pendingLeave = useRoutinesStore((s) => s.pendingLeave);
  const confirmLeave = useRoutinesStore((s) => s.confirmLeave);
  const cancelLeave = useRoutinesStore((s) => s.cancelLeave);
  const creating = useRoutinesStore((s) => s.creating);
  return (
    <div
      data-testid="routines-tab"
      className="flex h-full min-h-0 flex-col bg-background text-foreground"
    >
      <RoutinesStatusStrip />
      {actionError && (
        <InlineAlert
          className="mx-4 mt-2"
          severity="error"
          title="That didn't work"
          action={{ label: "Dismiss", onClick: dismiss }}
        >
          {actionError}
        </InlineAlert>
      )}
      {notice && (
        <InlineAlert
          className="mx-4 mt-2"
          data-testid="routines-notice"
          action={{ label: "Dismiss", onClick: dismissNotice }}
        >
          {notice}
        </InlineAlert>
      )}
      <ConfirmActionDialog
        open={pendingLeave !== null}
        onOpenChange={(open) => {
          if (!open) cancelLeave();
        }}
        title="Discard unsaved changes?"
        description={
          creating
            ? "This new routine isn't saved yet. Leaving drops it."
            : "This routine has edits that aren't saved. Leaving drops them."
        }
        confirmLabel="Discard"
        loadingLabel="Discarding…"
        loading={false}
        onConfirm={confirmLeave}
        testId="routine-discard-confirm"
      />
      <div className="flex min-h-0 flex-1">
        <div className="w-[min(30rem,50%)] shrink-0 overflow-y-auto border-r border-border">
          <RoutineList now={now} />
        </div>
        <div className="min-w-0 flex-1 overflow-y-auto">
          <RoutineDetailPane now={now} />
        </div>
      </div>
    </div>
  );
}
