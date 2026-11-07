// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";
import { usePaneStore } from "./panes/pane-store";
import { useEffect, useRef } from "react";
import { NativeSlotProvider, useNativeSlot } from "../slots/native-slot-context";
import { useViewerStore } from "@/shared/stores/viewer-store";
/**
 * Register the built-in right-pane panels — the user's own surfaces:
 * Notes. Its zero-arg render closure
 * reads its own store. Inbox hosts the active team's resolved dashboard.
 */
import { Inbox, StickyNote } from "lucide-react";

import { ScratchNotePanel } from "@/features/viewer/components/scratch-note-panel";

import { PanePanelBody } from "./panes/pane-panel-body";
import { PaneBadge } from "./panes/pane-chrome";
import { registerPanel } from "./panel-registry";
import { INBOX_PANEL_ID, SCRATCH_PANEL_ID } from "./right-pane-layout";

function ScratchPanelBody() {
  const slot = useNativeSlot();
  const org = useOrgStore((s) => s.activeOrgSlug);
  const activeNoteId = useViewerStore((s) => s.activeNoteId);
  const notes = useViewerStore((s) => s.notes);
  const scope = useViewerStore((s) => s.notesLoadedScope);
  const status = useViewerStore((s) => s.scratchNoteLoadStatus);
  const sharedNote = slot?.state.noteId;
  const intentNote = slot?.intent?.noteId;
  const heading = slot?.intent?.heading;
  const write = slot?.write;
  const ready = !!slot?.ready && status === "loaded" && !!scope && scope.orgSlug === org;
  const hasNote = (id: unknown) => notes.some((n) => n.id === id);
  const sharedPresent = hasNote(sharedNote);
  const intentPresent = hasNote(intentNote);
  const activePresent = hasNote(activeNoteId);
  const lastSelection = useRef<string | null>(null);
  const restoredShared = useRef<unknown>(null);
  const restoring = useRef<string | null>(null);
  useEffect(() => {
    lastSelection.current = null;
    restoredShared.current = null;
  }, [org]);
  useEffect(() => {
    if (!ready) return;
    const sharedChanged = restoredShared.current !== sharedNote;
    // Architecture: restore only a note in the active org's loaded model; intent takes precedence.
    const applyingIntent = typeof intentNote === "string" && intentPresent;
    if (sharedPresent || applyingIntent) restoredShared.current = sharedNote;
    const id = applyingIntent
      ? intentNote
      : sharedChanged && sharedPresent && typeof sharedNote === "string"
        ? sharedNote
        : null;
    if (!id) return;
    lastSelection.current = id;
    restoring.current = id;
    let current = true;
    void useViewerStore
      .getState()
      .setActiveNote(id)
      .then(() => {
        const model = useViewerStore.getState();
        const selected = model.activeNoteId;
        // Architecture: publish the winning local selection and consume even a superseded intent.
        if (
          !current ||
          useOrgStore.getState().activeOrgSlug !== org ||
          model.notesLoadedScope?.orgSlug !== org ||
          !selected ||
          !model.notes.some((note) => note.id === selected)
        )
          return;
        if (applyingIntent || selected !== sharedNote) {
          lastSelection.current = selected;
          write?.({ noteId: selected });
        }
        if (applyingIntent && selected === id && typeof heading === "string")
          model.requestHeadingJump(heading);
      })
      .finally(() => {
        if (current && restoring.current === id) restoring.current = null;
      });
    return () => {
      current = false;
      if (restoring.current === id) restoring.current = null;
    };
  }, [ready, org, sharedNote, sharedPresent, intentNote, intentPresent, write, heading]);
  useEffect(() => {
    if (
      ready &&
      !intentPresent &&
      !restoring.current &&
      write &&
      activeNoteId &&
      activePresent &&
      activeNoteId !== lastSelection.current
    ) {
      lastSelection.current = activeNoteId;
      write({ noteId: activeNoteId });
    }
  }, [activeNoteId, activePresent, write, ready, intentPresent]);
  // Scratch is global and reads its own store; the section header (not the
  // panel) owns collapse/expand now, so no onHeaderClick is wired.
  return (
    <div className="flex flex-col flex-1 min-h-0 overflow-hidden">
      <ScratchNotePanel />
    </div>
  );
}

/** Architecture: normal Notes and its failure fallback share the host UI topic. */
function ScratchSlotBody() {
  const slot = useNativeSlot();
  const intent = usePaneStore((s) => s.pendingIntents[SCRATCH_PANEL_ID]);
  return slot ? (
    <ScratchPanelBody />
  ) : (
    <NativeSlotProvider
      slotId="right-pane"
      panelId={SCRATCH_PANEL_ID}
      viewKey="all"
      intent={intent}
      onIntentDelivered={() =>
        intent && usePaneStore.getState().clearIntent(SCRATCH_PANEL_ID, intent)
      }
    >
      <ScratchPanelBody />
    </NativeSlotProvider>
  );
}

let registered = false;

/** Register the built-in panels once (idempotent). */
export function registerBuiltinPanels(): void {
  if (registered) return;
  registered = true;
  registerPanel({
    id: INBOX_PANEL_ID,
    title: "Inbox",
    icon: Inbox,
    render: () => <PanePanelBody panelId={INBOX_PANEL_ID} />,
    badge: () => <PaneBadge panelId={INBOX_PANEL_ID} />,
    maximizable: true,
  });
  // KEEP IN SYNC with BUILTIN_PANEL_IDS (right-pane-layout.ts), the set the
  // load-time reconcile prunes against.
  registerPanel({
    // SW-RP-3: display title is "Notes" (the id stays SCRATCH_PANEL_ID =
    // "scratch" — it is the persisted layout key + the save_scratch_note
    // contract, neither of which is user-facing).
    id: SCRATCH_PANEL_ID,
    title: "Notes",
    icon: StickyNote,
    render: () => <ScratchSlotBody />,
    // SW-RP-4: Notes is allowed in BOTH modes (same user-global notes surface).
  });
}
