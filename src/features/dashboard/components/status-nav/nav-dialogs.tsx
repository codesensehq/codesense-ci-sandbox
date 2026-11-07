// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// RF-12: the pipeline-status-nav DIALOG STACK, relocated verbatim from
// `pipeline-status-nav.tsx` (no behavior change). Purely presentational — the
// orchestrator owns every piece of state and every confirm/close handler; this
// component just wires them into the shared dialog primitives.
// ---------------------------------------------------------------------------
import { ConfirmActionDialog } from "@/features/settings/components/confirm-action-dialog";
import { RenameContentDialog } from "@/features/dashboard/components/rename-content-dialog";
import { WhoCanUseItField } from "@/shared/components/who-can-use-it";
import { QuickOpenDialog } from "@/features/viewer/components/quick-open-dialog";
import { baseName, suffixName } from "@/features/dashboard/lib/structural-ops";
import type { useNavFileActions } from "./use-nav-file-actions";

// RF-12b: collapsed from 32 individual props to a single `actions` bundle. Every
// former prop is a name-identical member of the `useNavFileActions` return, so the
// orchestrator passes `actions={fileActions}` and this component destructures the
// same names it always read — the JSX body below is unchanged.
interface NavDialogsProps {
  actions: ReturnType<typeof useNavFileActions>;
}

export function NavDialogs({ actions }: NavDialogsProps) {
  const {
    // Delete
    deleteTarget,
    deleteBusy,
    deleteMaterialized,
    deleteKeepsInherited,
    deleteFromParent,
    deleteRevealsParent,
    setDeleteTarget,
    confirmDelete,
    // New file
    newFileDir,
    newFileBusy,
    setNewFileDir,
    newFileAudience,
    setNewFileAudience,
    newFileTeams,
    newFileScratchOffFor,
    confirmNewFile,
    // New-file name collision
    newFileCollision,
    setNewFileCollision,
    confirmKeepBothNewFile,
    // Dropped-file (OS ingest) collision queue
    ingestQueue,
    ingestBusy,
    setIngestQueue,
    confirmKeepBothIngest,
    // Move-to picker
    moveTarget,
    moveDirItems,
    setMoveTarget,
    confirmMovePick,
    // Cross-scope move warning
    moveCrossScope,
    moveBusy,
    setMoveCrossScope,
    performMove,
    // Rename
    renameTarget,
    renameKeepsInherited,
    renameFromParent,
    renameBusy,
    setRenameTarget,
    confirmRename,
    // Canvas attach picker (SW-SC-12)
    canvasAttach,
  } = actions;
  return (
    <>
      <ConfirmActionDialog
        open={deleteTarget !== null}
        onOpenChange={(o) => {
          if (!o) setDeleteTarget(null);
        }}
        title="Delete file?"
        description={
          deleteTarget
            ? deleteFromParent
              ? `Delete from ${deleteFromParent}? Every team that inherits it loses it too. The context repo is git-backed, so you can revert this.`
              : `Delete "${baseName(deleteTarget)}" from team content? Workspaces keep their copies until an agent hydrates them. The context repo is git-backed, so you can revert this.${
                  deleteMaterialized
                    ? " This is one of the team's files — its agents may depend on it."
                    : ""
                }${
                  deleteRevealsParent
                    ? ` The ${deleteRevealsParent} version will show here instead.`
                    : ""
                }${deleteKeepsInherited ? ` ${deleteKeepsInherited}` : ""}`
            : ""
        }
        confirmLabel="Delete"
        loadingLabel="Deleting…"
        loading={deleteBusy}
        variant="destructive"
        onConfirm={() => void confirmDelete()}
      />
      {/* SW-VE-6 (AC-1): New file name prompt (reuses the SW-CP-14 name dialog). */}
      <RenameContentDialog
        key={newFileDir ?? "no-new-file"}
        open={newFileDir !== null}
        onOpenChange={(o) => {
          if (!o) setNewFileDir(null);
        }}
        currentName=""
        loading={newFileBusy}
        onConfirm={(name) => void confirmNewFile(name)}
        title="New file"
        description="Create a new empty file in this folder. It opens read-only — switch to edit deliberately."
        confirmLabel="Create"
        loadingLabel="Creating…"
      >
        <WhoCanUseItField
          value={newFileAudience}
          onChange={setNewFileAudience}
          teams={newFileTeams}
          scratchOffFor={newFileScratchOffFor}
        />
      </RenameContentDialog>
      {/* SW-VE-6 (AC-1): name collision → non-destructive "Keep both" / Cancel.
          Keep both creates a numbered copy; the existing file is untouched. */}
      <ConfirmActionDialog
        open={newFileCollision !== null}
        onOpenChange={(o) => {
          if (!o) setNewFileCollision(null);
        }}
        title="File already exists"
        description={
          newFileCollision
            ? `"${newFileCollision.name}" already exists in this folder. Keep both? A numbered copy (e.g. "${suffixName(newFileCollision.name, 2)}") is created — the existing file is left untouched.`
            : ""
        }
        confirmLabel="Keep both"
        loadingLabel="Creating…"
        loading={newFileBusy}
        variant="default"
        onConfirm={() => void confirmKeepBothNewFile()}
      />
      {/* SW-VE-7 (AC-4): dropped-file name collision → non-destructive "Keep both"
          / Cancel (default Cancel). The head of the queue drives one dialog;
          Cancel skips just that file. Reuses the VE-6 collision idiom. */}
      <ConfirmActionDialog
        open={ingestQueue.length > 0}
        onOpenChange={(o) => {
          // Cancel/Escape/overlay → skip this file. GUARD on `!ingestBusy`: while a
          // Keep-both ingest is in flight it owns the single queue-advance, so an
          // Escape mid-flight must NOT also slice (that would double-advance and
          // silently drop the next queued collision).
          if (!o && !ingestBusy) setIngestQueue((q) => q.slice(1));
        }}
        title="File already exists"
        description={
          ingestQueue[0]
            ? `"${ingestQueue[0].name}" already exists in this folder. Keep both? A numbered copy (e.g. "${suffixName(ingestQueue[0].name, 2)}") is added — the existing file is left untouched.`
            : ""
        }
        confirmLabel="Keep both"
        loadingLabel="Adding…"
        loading={ingestBusy}
        variant="default"
        onConfirm={() => void confirmKeepBothIngest()}
      />
      {/* SW-VE-6 (AC-2): Move-to destination-directory picker. */}
      {moveTarget && (
        <QuickOpenDialog
          open
          onClose={() => setMoveTarget(null)}
          onSelect={confirmMovePick}
          placeholder="Move to folder…"
          emptyLabel="No other folders"
          items={moveDirItems}
        />
      )}
      {/* SW-XS-20: moving a file inherited from a parent team moves it in that
          team, for every team that inherits it — confirm first. */}
      <ConfirmActionDialog
        open={moveCrossScope !== null}
        onOpenChange={(o) => {
          if (!o) setMoveCrossScope(null);
        }}
        title={moveCrossScope ? `Move in ${moveCrossScope.team}?` : "Move?"}
        description={
          moveCrossScope
            ? `This moves it in ${moveCrossScope.team} for every team that inherits it.`
            : ""
        }
        confirmLabel="Move"
        loadingLabel="Moving…"
        loading={moveBusy}
        variant="default"
        onConfirm={() => {
          const c = moveCrossScope;
          setMoveCrossScope(null);
          if (c) void performMove(c.from, c.toDir);
        }}
      />
      <RenameContentDialog
        key={renameTarget ?? "none"}
        open={renameTarget !== null}
        onOpenChange={(o) => {
          if (!o) setRenameTarget(null);
        }}
        currentName={renameTarget ? baseName(renameTarget) : ""}
        description={
          renameFromParent
            ? `This renames it in ${renameFromParent} for every team that inherits it.`
            : `Rename this item in team content. Workspaces keep their copies until an agent hydrates them.${
                renameKeepsInherited ? ` ${renameKeepsInherited}` : ""
              }`
        }
        loading={renameBusy}
        onConfirm={(newName) => void confirmRename(newName)}
      />
      {/* SW-SC-12: epic-scoped story picker for "Attach to story…" when the
          canvas's enclosing epic holds 2+ stories. Reuses the file-open
          quick-picker; selection routes back through the attach hook. */}
      {canvasAttach.picker && (
        <QuickOpenDialog
          open
          onClose={canvasAttach.closePicker}
          onSelect={canvasAttach.selectFromPicker}
          placeholder="Search stories..."
          emptyLabel="No stories found"
          items={canvasAttach.picker.stories.map((s) => ({
            id: s.filePath,
            label: s.storyId ?? s.fileName,
            sublabel: s.fileName,
          }))}
        />
      )}
    </>
  );
}
