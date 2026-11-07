// SPDX-License-Identifier: MIT
/**
 * The long-log affordances the Mailspool and a Threads conversation share: a "Jump to latest"
 * pill for a reader who scrolled up while mail landed below, and a polite live region that
 * names new mail without moving focus.
 */

/** Shown only while there is something new below; clicking it is the only way it acts. */
export function JumpToLatest({ unseen, onJump }: { unseen: number; onJump: () => void }) {
  if (unseen <= 0) return null;
  return (
    <button
      type="button"
      className="ibx-latest"
      aria-label={`Jump to latest, ${unseen} new ${unseen === 1 ? "message" : "messages"}`}
      onClick={onJump}
    >
      <span>Jump to latest ↓</span>
      <span className="ibx-badge">{unseen} new</span>
    </button>
  );
}

/** Off-screen, polite: assistive tech hears new mail when it lands; focus never moves. */
export function ArrivalsNote({ note }: { note: string }) {
  return (
    <div className="sr-only" role="status" aria-live="polite" data-testid="ibx-arrivals">
      {note}
    </div>
  );
}
