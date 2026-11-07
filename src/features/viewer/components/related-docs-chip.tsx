// SPDX-License-Identifier: MIT
import { revealSlot } from "@/features/shell/slots/reveal-slot";
import { HEADER_BTN } from "./header-button";
import { useCallback, useState } from "react";
import { FileStack, StickyNote, FilePlus2, Loader2, Link2, X } from "lucide-react";
import { toast } from "sonner";

import { DOC_TYPE_ICON } from "@/shared/lib/pipeline-graph/doc-type-icon";
import type { ResolvedSupportingDoc } from "@/shared/lib/pipeline-graph";
import { basename } from "@/shared/lib/basename";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { FloatingMenu } from "@/shared/components/floating-menu";

import { useDocBacklinks } from "./use-doc-backlinks";
import { QuickOpenDialog } from "./quick-open-dialog";
import {
  buildCsLinkToDoc,
  formatCsHref,
  type CsLinkResolveContext,
  type DocIdentity,
} from "../lib/note-links";

/** A candidate file offered in the "Relate a file…" picker (capability 1). */
export interface RelatableFile {
  filePath: string;
  fileName: string;
}

/**
 * SLW-FX-21: the write affordances the host wires into the chip's team
 * supporting-docs section. Its mere presence flips that section from read-only
 * (SW-SN-10 default) to writable — so a read-only / layered source doc omits it
 * (AC-8) and the chip stays exactly as it was. The "My notes" section is never
 * governed by this and stays read-only.
 */
export interface RelateDocActions {
  /** Files pickable in "Relate a file…" — host pre-excludes the open doc + already-related. */
  candidates: RelatableFile[];
  /** Relate the picked file to the open doc (writes the open doc's `supporting_docs`). */
  relate: (docPath: string) => void;
  /** Unlink a supporting doc from the open doc. */
  unlink: (docPath: string) => void;
}

interface RelatedDocsChipProps {
  activeTabPath: string;
  supportingDocs: ResolvedSupportingDoc[];
  docIdentity: DocIdentity;
  resolveCtx: CsLinkResolveContext;
  /** Open a supporting-doc row in the viewer (`store.openFile` bound by the host). */
  onOpenFile: (filePath: string) => void;
  /** SLW-FX-21: when provided, the team supporting-docs section gains a
   *  "Relate a file…" header + per-row unlink. Omitted → the chip is read-only
   *  (today's SW-SN-10 behavior), e.g. for a read-only layered source doc. */
  docActions?: RelateDocActions;
  /** Outer positioning. Defaults to the standalone top-right float; the host can
   *  pass a plain class (e.g. `relative`) to lay it out inside a shared chip row
   *  next to the SLW-NC-2 "Versions" chip. */
  className?: string;
}

/** SW-SN-10: the read-only "Related" chip for an open doc. Forked from
 *  `SupportingDocsPopover` (which stays docs-only for the nav) and extended with a
 *  second private "My notes" section (derived note backlinks) plus a "New note
 *  about this" action. Never writes the repo doc (AC-6). SW-FX-62: hosted in the
 *  docked `ViewerToolbar` band (not a float). SW-FX-79: the shell is the portaled
 *  `FloatingMenu` (Radix `Popover`) — contents + the relate-picker suspension
 *  logic unchanged. */
export function RelatedDocsChip({
  activeTabPath,
  supportingDocs,
  docIdentity,
  resolveCtx,
  onOpenFile,
  docActions,
  className = "relative",
}: RelatedDocsChipProps) {
  const [open, setOpen] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  // While the relate picker overlay is open, suspend dismissal so the popover
  // survives underneath it (the picker owns its own dismissal — backdrop + Escape).
  const handleOpenChange = useCallback(
    (next: boolean) => {
      if (next) setOpen(true);
      else if (!pickerOpen) setOpen(false);
    },
    [pickerOpen],
  );

  // On-demand, non-blocking: the scan only runs while the popover is open.
  const { backlinks, loading } = useDocBacklinks({ enabled: open, activeTabPath, resolveCtx });

  const createNote = useViewerStore((s) => s.createNote);
  const setActiveNote = useViewerStore((s) => s.setActiveNote);

  const openNote = useCallback(
    (id: string) => {
      setOpen(false);
      // Reveal the notes pane FIRST (setActiveNote alone won't open a closed
      // pane), then focus the note (SN-3 reveal seam + SN-2 rail auto-lands it).
      revealSlot("right-pane", { surface: "notes", intent: { noteId: id } });
      void setActiveNote(id);
    },
    [setActiveNote],
  );

  const handleNewNote = useCallback(async () => {
    setOpen(false);
    const docName = basename(activeTabPath);
    const link = buildCsLinkToDoc(docIdentity);
    // Seed a body with a forward link back to this doc so the note is reachable
    // from the doc next time (via the backlink scan). If the identity can't build
    // a link (shouldn't happen for a resolvable doc), leave the body unset so the
    // note keeps its blank template rather than being clobbered with "".
    const body = link ? `[${docName}](${formatCsHref(link)})\n\n` : undefined;
    try {
      const id = await createNote(`Note about ${docName}`, body);
      revealSlot("right-pane", { surface: "notes", intent: { noteId: id } });
      // createNote already activated the new note; re-assert defensively in case a
      // concurrent switch superseded it (setActiveNote no-ops when already active).
      void setActiveNote(id);
    } catch {
      // createNote logs+rethrows on IO failure; surface it rather than leaving an
      // unhandled rejection and a silently-closed popover.
      toast.error("Couldn't create the note");
    }
  }, [activeTabPath, docIdentity, createNote, setActiveNote]);

  const supportingCount = supportingDocs.length;
  const accessibleName = `Related documents and notes${supportingCount > 0 ? `, ${supportingCount}` : ""}`;

  return (
    <div className={className}>
      <FloatingMenu
        open={open}
        onOpenChange={handleOpenChange}
        role="menu"
        align="end"
        trigger={
          <button
            type="button"
            aria-label={accessibleName}
            title={accessibleName}
            aria-expanded={open}
            className={HEADER_BTN}
            onClick={(e) => e.stopPropagation()}
          >
            <FileStack size={12} aria-hidden="true" />
            {supportingCount > 0 && (
              <span className="text-[10.5px] leading-none">{supportingCount}</span>
            )}
            <span className="viewer-header-label leading-none">Related</span>
          </button>
        }
        className="overflow-y-auto"
        style={{ minWidth: 220, maxWidth: 300, maxHeight: 320, color: "var(--foreground)" }}
      >
        {/* SLW-FX-21 cap 1: relate a file to the OPEN doc. Present whenever the
            doc is writable (docActions) — even with zero supporting docs, which
            is exactly when it matters most (the section below is hidden empty). */}
        {docActions && (
          <button
            type="button"
            role="menuitem"
            className="flex items-center gap-2 w-full px-2.5 py-1.5 text-xs hover:bg-black/5"
            onClick={(e) => {
              e.stopPropagation();
              setPickerOpen(true);
            }}
          >
            <Link2
              size={12}
              className="flex-shrink-0"
              style={{ color: "var(--muted-foreground)" }}
            />
            <span>Relate a file…</span>
          </button>
        )}

        {/* Supporting docs — team, from frontmatter. */}
        {supportingCount > 0 && (
          <>
            <SectionHeader label="Supporting docs" hint={`${supportingCount} linked`} />
            {supportingDocs.map((doc) => {
              const typeConfig = DOC_TYPE_ICON[doc.documentType] ?? DOC_TYPE_ICON.doc;
              const TypeIcon = typeConfig.icon;
              return (
                <div
                  key={doc.filePath}
                  className="group flex items-center w-full hover:bg-accent/40"
                >
                  <button
                    type="button"
                    role="menuitem"
                    className="flex items-center gap-2 flex-1 min-w-0 px-3 py-1.5 text-[13px] text-left"
                    onClick={(e) => {
                      e.stopPropagation();
                      setOpen(false);
                      onOpenFile(doc.filePath);
                    }}
                  >
                    <TypeIcon
                      size={13}
                      style={{ color: "var(--muted-foreground)" }}
                      className="flex-shrink-0"
                    />
                    <span className="truncate">{doc.fileName}</span>
                  </button>
                  {/* SLW-FX-21 cap 3: hover-revealed unlink; stopPropagation so it
                      doesn't also open the doc. */}
                  {docActions && (
                    <button
                      type="button"
                      title="Unlink this doc"
                      aria-label={`Unlink ${doc.fileName}`}
                      className="flex-shrink-0 px-1.5 self-stretch opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 focus:opacity-100 transition-opacity"
                      onClick={(e) => {
                        e.stopPropagation();
                        docActions.unlink(doc.filePath);
                      }}
                    >
                      <X size={12} style={{ color: "var(--muted-foreground)" }} />
                    </button>
                  )}
                </div>
              );
            })}
          </>
        )}

        {/* My notes — private, local, derived on-demand. */}
        <SectionHeader
          label="My notes"
          hint={loading ? "scanning…" : `${backlinks.length} notes`}
        />
        {loading ? (
          <div
            className="flex items-center gap-2 px-3 py-1.5 text-[13px]"
            style={{ color: "var(--muted-foreground)" }}
          >
            <Loader2 size={13} className="animate-spin flex-shrink-0" />
            <span>Scanning notes…</span>
          </div>
        ) : backlinks.length > 0 ? (
          backlinks.map((note) => (
            <button
              key={note.id}
              type="button"
              role="menuitem"
              className="flex items-center gap-2 w-full px-3 py-1.5 text-[13px] hover:bg-accent/40"
              onClick={(e) => {
                e.stopPropagation();
                openNote(note.id);
              }}
            >
              <StickyNote
                size={13}
                style={{ color: "var(--muted-foreground)" }}
                className="flex-shrink-0"
              />
              <span className="truncate">{note.title}</span>
            </button>
          ))
        ) : (
          <div className="px-3 py-1.5 text-[13px]" style={{ color: "var(--muted-foreground)" }}>
            No notes link this doc yet.
          </div>
        )}

        {/* New note about this — pre-linked so it's reachable here next time. */}
        <div className="my-1 border-t" style={{ borderColor: "var(--border)" }} />
        <button
          type="button"
          role="menuitem"
          className="flex items-center gap-2 w-full px-2.5 py-1.5 text-xs hover:bg-black/5"
          onClick={(e) => {
            e.stopPropagation();
            void handleNewNote();
          }}
        >
          <FilePlus2
            size={12}
            className="flex-shrink-0"
            style={{ color: "var(--muted-foreground)" }}
          />
          <span>New note about this</span>
        </button>
      </FloatingMenu>

      {/* SLW-FX-21 cap 1: the shared quick-picker over host-supplied candidates.
          Items come from the in-memory nav tree (no disk scan on open); id is the
          file path so onSelect hands the relate action a path directly. */}
      {docActions && (
        <QuickOpenDialog
          open={pickerOpen}
          onClose={() => setPickerOpen(false)}
          onSelect={(filePath) => docActions.relate(filePath)}
          placeholder="Relate a file…"
          emptyLabel="No files to relate"
          items={docActions.candidates.map((f) => ({
            id: f.filePath,
            label: f.fileName,
            sublabel: f.filePath,
          }))}
        />
      )}
    </div>
  );
}

/**
 * SW-VE-16 Clean minimal / IDE-native: a Related popover section header. Uppercase
 * tracked caption on the left (the muted section cap device from Mockup #2), running
 * count on the right. Shared vocabulary with the Metadata panel's Lists group header so
 * the two popovers read as parts of one system.
 */
function SectionHeader({ label, hint }: { label: string; hint: string }) {
  return (
    <div
      className="flex items-baseline justify-between gap-2 px-3 pt-2.5 pb-1"
      style={{ color: "var(--muted-foreground)" }}
    >
      <span className="text-[10.5px] font-semibold uppercase tracking-[0.08em]">{label}</span>
      <span className="text-[11px] font-normal normal-case tracking-normal">{hint}</span>
    </div>
  );
}
