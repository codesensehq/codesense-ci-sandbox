// SPDX-License-Identifier: MIT
/**
 * The editor buffer as one pure state machine (EDITOR-BUFFER-STATE-SPEC.md).
 *
 * `reduce` owns every piece of buffer state: the text, the disk version it is
 * based on, the last disk version read, the merge, and the IO bookkeeping.
 * It returns commands (`read`, `write`) for an IO runner to carry out. Each
 * command carries a ticket, and a result whose ticket is no longer current is
 * dropped without effect.
 *
 * - At most one read and one write are in flight. No read starts while a
 *   write is out, and a write invalidates the read in flight.
 * - The conflict is derived (`isConflict`), never stored, so it cannot stick
 *   or clear at the wrong moment.
 * - Every write carries a lock: the hash (or text) of the disk version the
 *   user was shown, or ABSENT for a file that must not exist yet. The backend
 *   refuses a write over any other version.
 * - Reload is synchronous: it takes the last disk version read.
 * - A merge freezes the disk snapshot it shows. Disk events only set a flag,
 *   and leaving the merge issues exactly one re-check read.
 *
 * The reducer is the same for a local and a remote file. Only the adapter
 * differs: where disk signals come from, and the lock token's form.
 */

/** A disk version that was actually read. `hash` is the remote observation
 *  hash; `null` locally (the local lock is the SHA-256 of the text). */
export interface Snap {
  text: string;
  hash: string | null;
}

export type Unviewable = "binary" | "tooLarge" | "notUtf8";

export type Disk =
  | ({ k: "snap" } & Snap)
  | { k: "gone" }
  | { k: "unviewable"; why: Unviewable }
  | { k: "unknown" };

/** What a read can come back with. */
export type ReadResult = Exclude<Disk, { k: "unknown" }> | { k: "error"; message: string };

/** The disk version the buffer is based on. `verified`: disk was seen equal
 *  to it (or we wrote it). `absent`: the file was deleted under unsaved edits
 *  and must not exist when we save (which recreates it); `text` then keeps the
 *  last version's text for the view (line ending, indentation), and the
 *  buffer counts as dirty whatever it holds. */
export interface Base {
  text: string;
  hash: string | null;
  verified: boolean;
  absent: boolean;
}

export type S =
  | { k: "loading" }
  | { k: "noText"; why: "placeholder"; unviewable: Unviewable; base: Base | null }
  | { k: "noText"; why: "readError"; message: string; base: Base | null }
  | { k: "noText"; why: "removed"; base: Base | null }
  | { k: "editing"; text: string; base: Base; disk: Disk; dismissed: boolean }
  | { k: "merging"; text: string; base: Base; theirs: Snap; diskMoved: boolean };

export interface IO {
  read: { ticket: number; kind: "check" | "refresh" | "refresh-after-refusal" } | null;
  write: { ticket: number; kind: "save" | "resolve"; text: string } | null;
  /** A disk signal arrived while a read or write was out: read once more. */
  rereadWanted: boolean;
  /** A save waits for the write in flight, or for the base to be verified. */
  saveQueued: boolean;
  next: number;
}

export interface Machine {
  s: S;
  io: IO;
  saveError: string | null;
  mergeError: string | null;
}

export type Lock = { k: "hash"; hash: string } | { k: "text"; text: string } | { k: "absent" };

export type Cmd =
  | { k: "read"; ticket: number }
  | { k: "write"; ticket: number; text: string; lock: Lock };

export interface Draft {
  buffer: string;
  lastSaved: string;
}

export type Event =
  /** A watcher event, a poll hash change, or (`relist`) a new listed size. */
  | { k: "DISK_SIGNAL"; relist?: boolean }
  | { k: "READ_LANDED"; ticket: number; result: ReadResult }
  | { k: "EDIT"; text: string }
  | { k: "SAVE" }
  | { k: "SAVE_OK"; ticket: number; hash: string | null }
  | { k: "SAVE_REFUSED"; ticket: number }
  | { k: "SAVE_FAILED"; ticket: number; message: string }
  | { k: "OPEN_MERGE" }
  | { k: "MERGE_REFRESH" }
  | { k: "RESOLVE"; text: string }
  | { k: "TAKE_THEIRS" }
  | { k: "CANCEL_MERGE" }
  | { k: "RELOAD" }
  | { k: "RETRY" }
  | { k: "DISMISS" }
  /** The path moved under the buffer (the worktree root changed). */
  | { k: "RETARGET"; draft: Draft | null; tooLarge: boolean };

export interface Options {
  /** A clean buffer whose file vanished shows "removed" (context editor). */
  treatMissingAsRemoved: boolean;
}

export const MERGE_UNVIEWABLE = "This file is binary or too large to merge in-app.";
export const MERGE_REFUSED =
  "The file changed again while you were merging — the left side now shows the latest; review and keep yours again.";
export const MERGE_GONE = "The file no longer exists on disk.";
export const GONE_MESSAGE = "The file no longer exists.";
export const SAVE_REFUSED_MESSAGE =
  "Not saved — the file changed on disk. It has been read again; save again to keep your version.";

const freshIO = (): IO => ({
  read: null,
  write: null,
  rereadWanted: false,
  saveQueued: false,
  next: 1,
});

function verifiedBase(snap: Snap): Base {
  return { text: snap.text, hash: snap.hash, verified: true, absent: false };
}

// ── Derived status ───────────────────────────────────────────────────────

/** The buffer holds work disk doesn't: different text, or a file to recreate. */
function dirtyOf(s: { text: string; base: Base }): boolean {
  return s.base.absent || s.text !== s.base.text;
}

export function isDirty(m: Machine): boolean {
  const s = m.s;
  return (s.k === "editing" || s.k === "merging") && dirtyOf(s);
}

/** Dirty, and disk holds a version that is neither the base nor the text (or
 *  disk turned into something the editor can't show). */
export function isConflict(m: Machine): boolean {
  const s = m.s;
  if (s.k !== "editing" || !dirtyOf(s)) return false;
  if (s.disk.k === "unviewable") return true;
  return s.disk.k === "snap" && s.disk.text !== s.base.text && s.disk.text !== s.text;
}

/** Dirty, and not yet known safe to save: disk unknown or the base unverified. */
export function isChecking(m: Machine): boolean {
  const s = m.s;
  if (s.k !== "editing" || !dirtyOf(s) || isConflict(m)) return false;
  return s.disk.k === "unknown" || !s.base.verified;
}

export function canSave(m: Machine): boolean {
  const s = m.s;
  return s.k === "editing" && dirtyOf(s) && !isConflict(m) && s.base.verified && m.io.write == null;
}

export function lockForBase(base: Base): Lock {
  if (base.absent) return { k: "absent" };
  if (base.hash != null) return { k: "hash", hash: base.hash };
  return { k: "text", text: base.text };
}

function lockForSnap(snap: Snap): Lock {
  return snap.hash != null ? { k: "hash", hash: snap.hash } : { k: "text", text: snap.text };
}

// ── Construction ─────────────────────────────────────────────────────────

export interface InitOptions {
  restore: Draft | null;
  seed: string | null;
  tooLarge: boolean;
}

/** The machine for a freshly mounted editor, and the commands to start it. */
export function init(o: InitOptions): { m: Machine; cmds: Cmd[] } {
  const m: Machine = { s: { k: "loading" }, io: freshIO(), saveError: null, mergeError: null };
  return enter(m, o.restore, o.seed, o.tooLarge);
}

function enter(
  m: Machine,
  draft: Draft | null,
  seed: string | null,
  tooLarge: boolean,
): { m: Machine; cmds: Cmd[] } {
  const dirtyDraft = draft && draft.buffer !== draft.lastSaved ? draft : null;
  if (dirtyDraft) {
    const s: S = {
      k: "editing",
      text: dirtyDraft.buffer,
      base: { text: dirtyDraft.lastSaved, hash: null, verified: false, absent: false },
      disk: tooLarge ? { k: "unviewable", why: "tooLarge" } : { k: "unknown" },
      dismissed: false,
    };
    return tooLarge ? { m: { ...m, s }, cmds: [] } : issueRead({ ...m, s }, "check");
  }
  if (tooLarge) {
    return {
      m: { ...m, s: { k: "noText", why: "placeholder", unviewable: "tooLarge", base: null } },
      cmds: [],
    };
  }
  if (seed != null) {
    const s: S = {
      k: "editing",
      text: seed,
      base: { text: seed, hash: null, verified: false, absent: false },
      disk: { k: "unknown" },
      dismissed: false,
    };
    return issueRead({ ...m, s }, "check");
  }
  return issueRead({ ...m, s: { k: "loading" } }, "check");
}

// ── IO helpers ───────────────────────────────────────────────────────────

function issueRead(m: Machine, kind: NonNullable<IO["read"]>["kind"]): { m: Machine; cmds: Cmd[] } {
  if (m.io.write) return { m: { ...m, io: { ...m.io, rereadWanted: true } }, cmds: [] };
  const ticket = m.io.next;
  return {
    m: { ...m, io: { ...m.io, read: { ticket, kind }, rereadWanted: false, next: ticket + 1 } },
    cmds: [{ k: "read", ticket }],
  };
}

function then(
  a: { m: Machine; cmds: Cmd[] },
  f: (m: Machine) => { m: Machine; cmds: Cmd[] },
): { m: Machine; cmds: Cmd[] } {
  const b = f(a.m);
  return { m: b.m, cmds: [...a.cmds, ...b.cmds] };
}

const same = (m: Machine) => ({ m, cmds: [] as Cmd[] });

/** Save now if allowed, queue it if it must wait, or drop it. */
function trySave(m: Machine): { m: Machine; cmds: Cmd[] } {
  const s = m.s;
  const unqueued = { ...m, io: { ...m.io, saveQueued: false } };
  if (s.k !== "editing" || !dirtyOf(s)) return same(unqueued);
  if (isConflict(m)) {
    // Blocked: the banner comes back so the user sees why.
    return same({ ...unqueued, s: { ...s, dismissed: false } });
  }
  if (m.io.write) return same({ ...m, io: { ...m.io, saveQueued: true } });
  if (!s.base.verified || (s.disk.k === "unknown" && m.io.read)) {
    // Wait for a read to check disk; ask for one if none is out.
    const queued = { ...m, io: { ...m.io, saveQueued: true } };
    return queued.io.read ? same(queued) : issueRead(queued, "check");
  }
  const ticket = m.io.next;
  return {
    m: {
      ...m,
      io: {
        ...m.io,
        // The write invalidates the read in flight; disk is read again after.
        read: null,
        rereadWanted: m.io.rereadWanted || m.io.read != null,
        saveQueued: false,
        write: { ticket, kind: "save", text: s.text },
        next: ticket + 1,
      },
    },
    cmds: [{ k: "write", ticket, text: s.text, lock: lockForBase(s.base) }],
  };
}

/** After a read or write settles: a queued save, then the one re-read a
 *  signal asked for. */
function afterIO(m: Machine): { m: Machine; cmds: Cmd[] } {
  let r = same(m);
  // A queued save goes first: its write re-reads disk once it lands anyway.
  if (r.m.io.saveQueued && !r.m.io.write) {
    const s = r.m.s;
    // A queued save waits while the base is still being checked.
    const waiting =
      s.k === "editing" && (!s.base.verified || s.disk.k === "unknown") && r.m.io.read != null;
    if (!waiting) r = then(r, trySave);
  }
  if (r.m.io.rereadWanted && !r.m.io.read && !r.m.io.write && r.m.s.k !== "merging") {
    r = then(r, (x) => issueRead(x, "check"));
  }
  return r;
}

function sameDisk(a: Disk, b: Disk): boolean {
  if (a.k !== b.k) return false;
  if (a.k === "snap" && b.k === "snap") return a.text === b.text && a.hash === b.hash;
  if (a.k === "unviewable" && b.k === "unviewable") return a.why === b.why;
  return true;
}

// ── The reducer ──────────────────────────────────────────────────────────

export function reduce(m: Machine, e: Event, o: Options): { m: Machine; cmds: Cmd[] } {
  const s = m.s;
  switch (e.k) {
    case "DISK_SIGNAL": {
      if (s.k === "merging") {
        return s.diskMoved ? same(m) : same({ ...m, s: { ...s, diskMoved: true } });
      }
      // A measured too-large file is not re-read on every change; a new
      // listed size is what can bring it back.
      if (s.k === "noText" && s.why === "placeholder" && s.unviewable === "tooLarge" && !e.relist) {
        return same(m);
      }
      if (m.io.read || m.io.write) return same({ ...m, io: { ...m.io, rereadWanted: true } });
      return issueRead(m, "check");
    }

    case "READ_LANDED": {
      const read = m.io.read;
      if (!read || read.ticket !== e.ticket) return same(m); // stale: no effect
      const base0: Machine = { ...m, io: { ...m.io, read: null } };
      const r = e.result;
      if (read.kind !== "check") {
        if (s.k !== "merging") return afterIO(base0);
        if (r.k === "snap") {
          return afterIO({
            ...base0,
            s: { ...s, theirs: { text: r.text, hash: r.hash }, diskMoved: false },
            mergeError: read.kind === "refresh-after-refusal" ? MERGE_REFUSED : null,
          });
        }
        const why = r.k === "error" ? r.message : r.k === "gone" ? MERGE_GONE : MERGE_UNVIEWABLE;
        return afterIO({ ...base0, mergeError: why });
      }
      return afterIO(landCheck(base0, r, o));
    }

    case "EDIT": {
      if (s.k !== "editing" && s.k !== "merging") return same(m);
      if (s.text === e.text) return same(m);
      return same({ ...m, s: { ...s, text: e.text } });
    }

    case "SAVE":
      return trySave(m);

    case "SAVE_OK": {
      const w = m.io.write;
      if (!w || w.ticket !== e.ticket) return same(m);
      const snap: Snap = { text: w.text, hash: e.hash };
      const cleared: Machine = { ...m, io: { ...m.io, write: null } };
      if (w.kind === "resolve") {
        if (s.k !== "merging") return afterIO(cleared);
        const next: Machine = {
          ...cleared,
          s: {
            k: "editing",
            text: w.text,
            base: verifiedBase(snap),
            disk: { k: "snap", ...snap },
            dismissed: false,
          },
          io: { ...cleared.io, rereadWanted: false },
          mergeError: null,
          saveError: null,
        };
        // Leaving the merge: exactly one re-check.
        return then(issueRead(next, "check"), afterIO);
      }
      if (s.k !== "editing") return afterIO(cleared);
      // Edits typed while the write was out are saved next (the editor's own
      // ⌘S is swallowed while a save is in flight): a save saves the buffer.
      const typedSince = s.text !== w.text;
      return afterIO({
        ...cleared,
        s: { ...s, base: verifiedBase(snap), disk: { k: "snap", ...snap } },
        io: { ...cleared.io, saveQueued: cleared.io.saveQueued || typedSince },
        saveError: null,
      });
    }

    case "SAVE_REFUSED": {
      const w = m.io.write;
      if (!w || w.ticket !== e.ticket) return same(m);
      // A queued save is dropped: the user decides again once disk is read.
      const cleared: Machine = {
        ...m,
        io: { ...m.io, write: null, rereadWanted: false, saveQueued: false },
      };
      if (w.kind === "resolve") {
        if (s.k !== "merging") return afterIO(cleared);
        return issueRead({ ...cleared, mergeError: MERGE_REFUSED }, "refresh-after-refusal");
      }
      if (s.k !== "editing") return afterIO(cleared);
      // Disk moved under the lock: read it; the result derives the conflict
      // (whose banner explains itself), or clears this note if disk turns out
      // to hold the edits already.
      return then(
        issueRead(
          { ...cleared, s: { ...s, disk: { k: "unknown" } }, saveError: SAVE_REFUSED_MESSAGE },
          "check",
        ),
        afterIO,
      );
    }

    case "SAVE_FAILED": {
      const w = m.io.write;
      if (!w || w.ticket !== e.ticket) return same(m);
      const cleared: Machine = { ...m, io: { ...m.io, write: null, saveQueued: false } };
      if (w.kind === "resolve") return afterIO({ ...cleared, mergeError: e.message });
      return afterIO({ ...cleared, saveError: e.message });
    }

    case "OPEN_MERGE": {
      if (s.k !== "editing" || !isConflict(m)) return same(m);
      if (s.disk.k !== "snap") return same({ ...m, mergeError: MERGE_UNVIEWABLE });
      return same({
        ...m,
        s: {
          k: "merging",
          text: s.text,
          base: s.base,
          theirs: { text: s.disk.text, hash: s.disk.hash },
          // A change already being read (or owed a read) is newer than this.
          diskMoved: m.io.read != null || m.io.rereadWanted,
        },
        io: { ...m.io, read: null, rereadWanted: false },
        mergeError: null,
      });
    }

    case "MERGE_REFRESH": {
      if (s.k !== "merging" || m.io.write) return same(m);
      return issueRead({ ...m, io: { ...m.io, read: null } }, "refresh");
    }

    case "RESOLVE": {
      if (s.k !== "merging" || m.io.write) return same(m);
      const ticket = m.io.next;
      return {
        m: {
          ...m,
          io: {
            ...m.io,
            read: null,
            rereadWanted: false,
            write: { ticket, kind: "resolve", text: e.text },
            next: ticket + 1,
          },
        },
        cmds: [{ k: "write", ticket, text: e.text, lock: lockForSnap(s.theirs) }],
      };
    }

    case "TAKE_THEIRS":
    case "CANCEL_MERGE": {
      if (s.k !== "merging" || m.io.write) return same(m);
      const take = e.k === "TAKE_THEIRS";
      const next: Machine = {
        ...m,
        s: {
          k: "editing",
          text: take ? s.theirs.text : s.text,
          base: take ? verifiedBase(s.theirs) : s.base,
          disk: { k: "snap", ...s.theirs },
          dismissed: false,
        },
        io: { ...m.io, read: null, rereadWanted: false },
        mergeError: null,
        saveError: take ? null : m.saveError,
      };
      // Leaving the merge: exactly one re-check.
      return issueRead(next, "check");
    }

    case "RELOAD": {
      if (s.k !== "editing" || m.io.write) return same(m);
      const d = s.disk;
      const cleared: Machine = {
        ...m,
        saveError: null,
        mergeError: null,
        io: { ...m.io, saveQueued: false },
      };
      if (d.k === "snap") {
        const snap = { text: d.text, hash: d.hash };
        return same({
          ...cleared,
          s: { ...s, text: d.text, base: verifiedBase(snap), dismissed: false },
        });
      }
      if (d.k === "unviewable") {
        return same({
          ...cleared,
          s: { k: "noText", why: "placeholder", unviewable: d.why, base: s.base },
        });
      }
      if (d.k === "gone" && o.treatMissingAsRemoved) {
        return same({ ...cleared, s: { k: "noText", why: "removed", base: s.base } });
      }
      // Gone (worktree), or not read yet: back to the base, and read. Discarding
      // also drops a pending recreate: the base is unverified again, so a later
      // save first checks disk (and recreates only with the "absent" lock).
      const back: Machine = {
        ...cleared,
        s: {
          ...s,
          text: s.base.text,
          base: s.base.absent ? { ...s.base, absent: false, verified: false } : s.base,
          dismissed: false,
        },
      };
      return d.k === "unknown" && !m.io.read ? issueRead(back, "check") : same(back);
    }

    case "RETRY": {
      if (s.k !== "noText" || s.why !== "readError") return same(m);
      return issueRead({ ...m, s: { k: "loading" } }, "check");
    }

    case "DISMISS": {
      if (s.k !== "editing" || s.dismissed) return same(m);
      return same({ ...m, s: { ...s, dismissed: true } });
    }

    case "RETARGET": {
      const reset: Machine = {
        s: { k: "loading" },
        io: { ...freshIO(), next: m.io.next },
        saveError: null,
        mergeError: null,
      };
      return enter(reset, e.draft, null, e.tooLarge);
    }
  }
}

/** A check read landed (the ticket is current and already cleared). */
function landCheck(m: Machine, r: ReadResult, o: Options): Machine {
  const s = m.s;
  if (s.k === "merging") return m; // disk events only set a flag in a merge
  if (s.k === "loading" || s.k === "noText") {
    switch (r.k) {
      case "snap": {
        const snap = { text: r.text, hash: r.hash };
        return {
          ...m,
          s: {
            k: "editing",
            text: r.text,
            base: verifiedBase(snap),
            disk: { k: "snap", ...snap },
            dismissed: false,
          },
        };
      }
      case "unviewable":
        return {
          ...m,
          s: {
            k: "noText",
            why: "placeholder",
            unviewable: r.why,
            base: s.k === "noText" ? s.base : null,
          },
        };
      case "gone":
        if (s.k === "noText") return m;
        return { ...m, s: { k: "noText", why: "readError", message: GONE_MESSAGE, base: null } };
      case "error":
        if (s.k === "noText") return m;
        return { ...m, s: { k: "noText", why: "readError", message: r.message, base: null } };
    }
  }
  // editing
  if (r.k === "error") {
    // Disk couldn't be read; nothing about it is known that wasn't before. A
    // save waiting on the check gets the reason.
    if (m.saveError === SAVE_REFUSED_MESSAGE) {
      return {
        ...m,
        io: { ...m.io, saveQueued: false },
        saveError: `Not saved — the file changed on disk, and reading it again failed (${r.message}).`,
      };
    }
    if (m.io.saveQueued && !s.base.verified) {
      return {
        ...m,
        io: { ...m.io, saveQueued: false },
        saveError: `Couldn’t check the file on disk before saving (${r.message}).`,
      };
    }
    return m;
  }
  const disk: Disk = r;
  const dismissed = sameDisk(s.disk, disk) ? s.dismissed : false;
  const clean = !dirtyOf(s);
  const next = { ...s, disk, dismissed };
  if (disk.k === "snap") {
    const snap = { text: disk.text, hash: disk.hash };
    if (clean) {
      return {
        ...m,
        s: { ...next, text: disk.text, base: verifiedBase(snap) },
        saveError: null,
        mergeError: null,
      };
    }
    if (disk.text === s.text)
      return { ...m, s: { ...next, base: verifiedBase(snap) }, saveError: null };
    if (disk.text === s.base.text) return { ...m, s: { ...next, base: verifiedBase(snap) } };
    return { ...m, s: next };
  }
  if (disk.k === "unviewable") {
    if (clean) {
      return { ...m, s: { k: "noText", why: "placeholder", unviewable: disk.why, base: s.base } };
    }
    return { ...m, s: next };
  }
  // gone
  if (clean) {
    if (o.treatMissingAsRemoved) return { ...m, s: { k: "noText", why: "removed", base: s.base } };
    return { ...m, s: next };
  }
  // The edits stay; a save recreates the file (it must still be absent).
  return { ...m, s: { ...next, base: { ...s.base, hash: null, verified: true, absent: true } } };
}
