// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { currentNotesScope, type NotesScope } from "@/shared/lib/notes-scope";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { notesHistory, notesReadAt, notesRestore, type NoteRevision } from "@/tauri/commands";
import { NOTES_CHANGED_EVENT, type NotesChangedPayload } from "@/tauri/events";

/**
 * A note's saved revisions, for the "who changed this" banner, the review and the
 * history list. The history backend is newer than this pane: on an install without
 * it, the commands are missing and the history UI stays hidden (`available` false)
 * rather than erroring on every note.
 */

/** Whether the history commands answered, learned once per session. */
let historyAvailable: boolean | null = null;

/** Whether a failed call means the command itself is missing. */
export function isMissingCommand(e: unknown): boolean {
  const msg = e instanceof Error ? e.message : String(e);
  return /not mocked|not found|unknown command|not allowed|no such command|not registered/i.test(
    msg,
  );
}

function loadedScope(): NotesScope {
  return useViewerStore.getState().notesLoadedScope ?? currentNotesScope();
}

const SEEN_PREFIX = "cs.notes.seen-rev.";

function readSeen(noteId: string): string | null {
  try {
    return localStorage.getItem(SEEN_PREFIX + noteId);
  } catch {
    return null;
  }
}

function writeSeen(noteId: string, rev: string): void {
  try {
    localStorage.setItem(SEEN_PREFIX + noteId, rev);
  } catch {
    // Storage off: the banner just shows again next time.
  }
}

/** Whether a notes change made a new revision of note `noteId`. */
export function shouldReloadHistory(
  payload: NotesChangedPayload | undefined,
  noteId: string | null,
): boolean {
  return !!payload && payload.id === noteId && !!payload.commit;
}

export interface NoteHistory {
  /** null while the first read is in flight. */
  available: boolean | null;
  /** Newest first. */
  entries: NoteRevision[];
  /** The newest revision, when an agent made it and the person hasn't looked yet. */
  unseenAgentChange: NoteRevision | null;
  /** The revision just before `rev` (what undoing it returns to), if any. */
  revisionBefore: (rev: string) => NoteRevision | null;
  readAt: (rev: string) => Promise<string>;
  /** Make `rev` the body again, then reload the note. */
  restore: (rev: string) => Promise<void>;
  /** Take back only what `rev` changed, keeping every later edit, then reload. */
  undo: (rev: string) => Promise<void>;
  markSeen: (rev: string) => void;
  reload: () => void;
}

export function useNoteHistory(noteId: string | null): NoteHistory {
  const [available, setAvailable] = useState<boolean | null>(historyAvailable);
  const [entries, setEntries] = useState<NoteRevision[]>([]);
  const [seen, setSeen] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const reload = useCallback(() => setTick((t) => t + 1), []);
  const noteRef = useRef(noteId);
  useEffect(() => {
    noteRef.current = noteId;
  });

  useEffect(() => {
    if (!noteId || historyAvailable === false) return;
    let cancelled = false;
    notesHistory(loadedScope(), noteId).then(
      (list) => {
        historyAvailable = true;
        if (cancelled) return;
        setAvailable(true);
        setEntries(list);
        setSeen(readSeen(noteId));
      },
      (e: unknown) => {
        if (cancelled) return;
        if (isMissingCommand(e)) {
          historyAvailable = false;
          setAvailable(false);
        }
        setEntries([]);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [noteId, tick]);

  // A change saved to this note's history: re-read it. A person's save waiting for
  // its quiet period carries no commit yet (an event naming the commit follows), so
  // typing never re-reads history. (The app root reloads the notes themselves when
  // an agent changed them.)
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void listen<NotesChangedPayload>(NOTES_CHANGED_EVENT, (event) => {
      if (shouldReloadHistory(event.payload, noteRef.current)) setTick((t) => t + 1);
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const latest = entries[0] ?? null;
  const unseenAgentChange =
    available && latest && latest.actor.kind === "agent" && latest.commit !== seen ? latest : null;

  const revisionBefore = useCallback(
    (rev: string) => {
      const at = entries.findIndex((e) => e.commit === rev);
      return at >= 0 ? (entries[at + 1] ?? null) : null;
    },
    [entries],
  );

  const readAt = useCallback(
    (rev: string) =>
      noteRef.current
        ? notesReadAt(loadedScope(), noteRef.current, rev).then((at) => at.content)
        : Promise.resolve(""),
    [],
  );

  const bringBack = useCallback(async (rev: string, undo: boolean) => {
    const id = noteRef.current;
    if (!id) return;
    // Land any pending keystrokes first, so they can't save over the restored body.
    await useViewerStore.getState().flushNotes();
    await notesRestore(loadedScope(), id, rev, undo ? { undo: true } : undefined);
    await useViewerStore.getState().loadNotes();
    setTick((t) => t + 1);
  }, []);
  const restore = useCallback((rev: string) => bringBack(rev, false), [bringBack]);
  const undo = useCallback((rev: string) => bringBack(rev, true), [bringBack]);

  const markSeen = useCallback((rev: string) => {
    const id = noteRef.current;
    if (!id) return;
    writeSeen(id, rev);
    setSeen(rev);
  }, []);

  return {
    available,
    entries,
    unseenAgentChange,
    revisionBefore,
    readAt,
    restore,
    undo,
    markSeen,
    reload,
  };
}

/** Test-only: forget what the session learned about the history commands. */
export function resetNoteHistoryAvailabilityForTests(): void {
  historyAvailable = null;
}
