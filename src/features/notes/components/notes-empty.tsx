// SPDX-License-Identifier: MIT
import { useRef } from "react";
import { Clock, Plus } from "lucide-react";

import { useFocusWhenShown, useOwningScope } from "@/shared/lib/warm-scopes";

/** The first look at notes in an org: what they are, how to start, the four keys
 *  worth knowing, and the offer of a routine to keep them tidy. */
export function NotesEmpty({
  orgName,
  onNew,
  onRoutine,
}: {
  orgName: string;
  onNew: () => void;
  onRoutine: () => void;
}): React.JSX.Element {
  const newRef = useRef<HTMLButtonElement>(null);
  // A warm team stays mounted, so `autoFocus` does not run again when the team is shown again.
  useFocusWhenShown(useOwningScope(), true, () => newRef.current?.focus({ preventScroll: true }));
  return (
    <div className="nt-empty">
      <h3>Notes for {orgName}</h3>
      <p>
        Private to you and kept on this Mac, with history. Jot plans, prompts and todos, then hand
        any line to an agent.
      </p>
      <button ref={newRef} type="button" className="nt-empty-new" onClick={onNew} autoFocus>
        <Plus size={14} aria-hidden />
        New note
        <kbd className="nt-kbd">⌘N</kbd>
      </button>
      <div className="nt-empty-keys">
        <kbd className="nt-kbd">- [ ]</kbd>
        <span>starts a task</span>
        <kbd className="nt-kbd">/</kbd>
        <span>inserts a heading, code, checklist…</span>
        <kbd className="nt-kbd">[[</kbd>
        <span>links a note or a story</span>
        <kbd className="nt-kbd">⌘↵</kbd>
        <span>sends the line to an agent</span>
      </div>
      <div className="nt-empty-routine">
        <Clock size={14} aria-hidden />
        <span>Want an agent to keep these tidy every evening?</span>
        <button type="button" className="nt-link" onClick={onRoutine}>
          Set up a routine
        </button>
      </div>
    </div>
  );
}
