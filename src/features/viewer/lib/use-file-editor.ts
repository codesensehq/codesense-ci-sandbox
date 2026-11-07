// SPDX-License-Identifier: MIT
import { fileChangeAffects } from "@/shared/lib/file-change-affects";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { readFile, WORKTREE_WRITE_CONFLICT_PREFIX } from "@/tauri/commands";
import { isFileMissingError } from "@/shared/lib/file-missing-error";

import {
  init,
  isConflict,
  isDirty,
  reduce,
  type Cmd,
  type Event,
  type Lock,
  type Machine,
  type ReadResult,
} from "./buffer-machine";
import { BINARY_MESSAGE, retargetDraft } from "./file-buffer-model";
import { isTooLargeToView, looksBinary, utf8ByteLength } from "./worktree-file-view";

/** The file currently open in an editor inset — its absolute path plus the byte
 *  size from the tree's `FileEntry` (or `null` when only a path is known, e.g.
 *  seeded from a diff or a context viewer selection where the size wasn't
 *  measured). */
export interface SelectedFile {
  path: string;
  size: number | null;
}

/** True when freshly-read disk content can't be shown/merged in-app — binary
 *  (NUL-scanned) or over the edit size cap. The read effect and the reconcile
 *  effect both gate on this so the merge view never mounts CM6 over unviewable
 *  content, and a `reloadFromDisk` never drops binary bytes into the buffer. */
export function isUnviewableDisk(disk: string): boolean {
  return looksBinary(disk) || isTooLargeToView(utf8ByteLength(disk));
}

/** Placeholder copy for a file that isn't valid UTF-8 text. Editing it would
 *  re-encode (and corrupt) whatever bytes don't decode, so it never opens. */
export const NOT_UTF8_MESSAGE = "Not UTF-8 text — open it in an external editor";

/** Placeholder copy for a file over the editor's size limit (or the remote
 *  protocol's). The pane adds the size and the limit when it knows them. */
export const TOO_LARGE_MESSAGE = "File too large to edit";

/** True for a read that found no file (local or remote). */
function isGone(e: unknown): boolean {
  return (
    isFileMissingError(e) ||
    (typeof e === "object" && e !== null && (e as { kind?: unknown }).kind === "notFound")
  );
}

/** True for a read the remote machine refused as over its file-size limit. */
function isBoundsExceeded(e: unknown): boolean {
  return typeof e === "object" && e !== null && (e as { kind?: unknown }).kind === "boundsExceeded";
}

/** True for a read rejected because the bytes aren't UTF-8: the local reader's
 *  `read_to_string` message, or the remote adapter's fatal-decoder error. */
export function isNotUtf8Error(e: unknown): boolean {
  if (typeof e === "object" && e !== null && (e as { kind?: unknown }).kind === "notUtf8") {
    return true;
  }
  const msg = e instanceof Error ? e.message : typeof e === "string" ? e : "";
  return /valid UTF-8|not utf-?8/i.test(msg);
}

/** True for the local writer's refusal of a save whose disk baseline moved. */
export function isLocalWriteConflict(e: unknown): boolean {
  const msg = e instanceof Error ? e.message : typeof e === "string" ? e : "";
  return msg.startsWith(WORKTREE_WRITE_CONFLICT_PREFIX);
}

/** A readable message for any thrown value — including the remote commands'
 *  typed `{kind, message?}` objects, which `String(e)` renders as
 *  "[object Object]". */
export function describeFileError(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  if (typeof e === "object" && e !== null) {
    const { kind, message } = e as { kind?: unknown; message?: unknown };
    if (typeof message === "string" && message) return message;
    switch (kind) {
      case "disconnected":
        return "Can’t reach the remote machine.";
      case "unauthorized":
        return "Workspace access was revoked — reopen the workspace.";
      case "notFound":
        return "The file no longer exists.";
      case "boundsExceeded":
        return "The file is larger than the 4 MiB remote limit.";
      default:
        break;
    }
  }
  return String(e);
}

/** Lower-hex SHA-256 of `text`'s UTF-8 bytes — the save baseline the local
 *  writer checks. `undefined` where WebCrypto is unavailable (the save then
 *  falls back to the watcher-only guard rather than failing). */
export async function sha256Hex(text: string): Promise<string | undefined> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) return undefined;
  const digest = await subtle.digest("SHA-256", new TextEncoder().encode(text));
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * The resolved view state for the currently-selected file. The size/binary
 * gates collapse into a single `placeholder` kind — a placeholder file is never
 * editable and has no buffer/save path. `editable` means the content is in
 * `buffer` and the code editor mounts. `removed` (opt-in via
 * {@link FileEditorOptions.treatMissingAsRemoved}) means the open file vanished
 * from disk while the buffer was clean — nothing to lose, so surface a
 * "was removed" empty state instead of a stuck render.
 */
export type EditorViewState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "readError"; message: string }
  | { kind: "placeholder"; message: string }
  | { kind: "removed" }
  | { kind: "editable" };

/**
 * Adapt the content-repo `useFileRefreshStore` signal into the monotonic
 * `changedNonce` {@link useFileEditor} expects — advancing ONLY when `filePath`
 * is implicated in a change burst. The worktree editor gets a per-workspace nonce
 * for free; the content watcher only exposes a per-repo `refreshKey` plus the
 * burst's changed-path Set (`lastChangedContextFile`), so this is where the
 * per-file precision is recovered: an unrelated file's write never triggers a
 * re-read of the open editor. A broad refresh (`changedPaths == null`, the SF-3
 * "can't prove it's untouched" sentinel) always advances. Seeded to the current
 * `refreshKey` so a pre-existing signal never retro-fires on mount (parity with
 * the worktree hook's `reconciledNonceRef`). Uses React's endorsed
 * adjust-state-during-render idiom (same as the hook's `seenPath` reset).
 */
export function useScopedChangeNonce(
  filePath: string,
  refreshKey: number,
  changedPaths: Set<string> | null,
): number {
  const [nonce, setNonce] = useState(0);
  // The last (refreshKey, path) this hook acted on — tracked in state (not a ref)
  // so the adjust-during-render below is React's endorsed pattern, not a
  // write-ref-in-render.
  const [seen, setSeen] = useState({ key: refreshKey, path: filePath });
  if (seen.path !== filePath) {
    // A file switch (keyed insets remount, but be self-safe) rebaselines without
    // firing — the new file didn't just change, we just started watching it.
    setSeen({ key: refreshKey, path: filePath });
  } else if (seen.key !== refreshKey) {
    setSeen({ key: refreshKey, path: filePath });
    if (fileChangeAffects(changedPaths, filePath)) {
      setNonce((n) => n + 1);
    }
  }
  return nonce;
}

/** DESIGN-remote-phase-2.md §3.3 — type-guard for a remote `Conflict` from the
 *  Rust `RemoteSourceError` enum. The hook treats the Conflict path specially:
 *  the merge view's "theirs" seed comes from a follow-up read, not the error
 *  payload (which only carries the observation). */
export function isRemoteConflict(
  e: unknown,
): e is { kind: "conflict"; observation: { hash: string } } {
  return (
    typeof e === "object" &&
    e !== null &&
    (e as { kind?: unknown }).kind === "conflict" &&
    typeof (e as { observation?: unknown }).observation === "object" &&
    (e as { observation: { hash?: unknown } }).observation !== null &&
    typeof (e as { observation: { hash?: unknown } }).observation.hash === "string"
  );
}

/** DESIGN-remote-phase-2.md B-5 — the discriminated union mirroring the Rust
 *  `RemoteSourceError`. Local throws (string / Error / anything else) don't
 *  match; the save catch then falls back to the generic `setSaveError`. */
export type RemoteSaveError =
  | { kind: "disconnected" }
  | { kind: "unauthorized" }
  | { kind: "notFound" }
  | { kind: "boundsExceeded" }
  | { kind: "conflict"; observation: { hash: string } }
  | { kind: "transportFailure"; message: string };

export function isRemoteSourceError(e: unknown): e is RemoteSaveError {
  if (typeof e !== "object" || e === null) return false;
  const k = (e as { kind?: unknown }).kind;
  switch (k) {
    case "disconnected":
    case "unauthorized":
    case "notFound":
    case "boundsExceeded":
    case "transportFailure":
      return true;
    case "conflict":
      return isRemoteConflict(e);
    default:
      return false;
  }
}

/** DESIGN-remote-phase-2.md §3.1 — the remote side's reads: content with its
 *  observation hash (the remote lock token), and a cheap hash inspect. */
export interface RemoteFileAdapter {
  /** Fetch content + current hash. The hook seeds `lastSaved` and the
   *  optimistic-lock baseline from the returned hash. */
  readWithHash: (path: string) => Promise<{ content: string; hash: string }>;
  /** The file's current hash without transferring it. When present, the open
   *  editor polls it (see {@link FileEditorOptions.pollMs}) so an edit made on
   *  the machine is noticed — the remote path-set watcher cannot see in-place
   *  edits, only files appearing or vanishing. */
  inspectHash?: (path: string) => Promise<string>;
}

/** The one backend difference between the worktree and context editors, plus the
 *  two caller-specific read/vanish policies. The buffer state machine itself
 *  (`buffer-machine.ts`) is identical for both. */
export interface FileEditorOptions {
  /** Persist `content` to disk, refusing it unless disk is still the version
   *  `expectedHash` names (every write carries a lock). Local writers take the
   *  SHA-256 of the base text, or `"absent"` for a file that must not exist yet;
   *  the remote writer takes the observation hash (`undefined`: must not exist)
   *  and returns the new `{ hash }`. */
  write: (path: string, content: string, expectedHash?: string) => Promise<void | { hash: string }>;
  /** DESIGN-remote-phase-2.md §3.1 — supply only for the remote path. Reads
   *  seed `lastSaved` + the optimistic-lock baseline from the returned hash,
   *  so the next write can offer `expectedHash` and the server can detect a
   *  concurrent edit. */
  remote?: RemoteFileAdapter;
  /** Optional warm-cache seed for an instant edit flip (context). Returns viewable
   *  text for `path`, or `null` when there's no usable seed. Absent (worktree):
   *  the hook always opens from a fresh read. A seed is content with an
   *  unverified base: the fresh read checks it like any restored draft. */
  seed?: (path: string) => string | null;
  /** When a watcher-driven re-read finds the open file GONE and the buffer is
   *  clean, resolve to the `removed` state (context) instead of silently keeping
   *  the last content (worktree). A DIRTY buffer never vanishes either way — the
   *  edits are kept so a save recreates the file (never destroy unsaved work). */
  treatMissingAsRemoved?: boolean;
  /** SLW-DV-9: a preserved unsaved-edit draft to seed the editor with on mount —
   *  its `buffer` (edited text) and `lastSaved` (the on-disk baseline it diverged
   *  from). Used when an unfocus unmounted a dirty editor and it's remounting. A
   *  DIRTY restore enters with an unverified base, so disk is checked before any
   *  save (buffer-machine). `null`/absent, or a clean pair, is a no-op — the
   *  editor opens from a normal fresh read. */
  restore?: FileDraft | null;
  /** Remote only: how often to poll `remote.inspectHash` for an external edit
   *  to the open file. Default 2500 ms; 0 disables. */
  pollMs?: number;
}

/** SLW-DV-9: the minimal preserved-edit payload — the edited `buffer` plus the
 *  `lastSaved` baseline it diverged from. Dirtiness and the stale re-arm are
 *  DERIVED from these against fresh disk, so no stale/reconcile machine is
 *  persisted. Shared by {@link FileEditorOptions.restore} and
 *  {@link FileEditor.getDraft}. */
export interface FileDraft {
  buffer: string;
  lastSaved: string;
}

/** The editable-file model behind an editor inset — the read + edit + concurrent-
 *  write-safety state, shared by the source-worktree editor and the context
 *  editor. Returned surface is stable across both callers. */
export interface FileEditor {
  state: EditorViewState;
  buffer: string;
  setBuffer: (next: string) => void;
  dirty: boolean;
  /** Save `content`. Resolves once the save has settled (written, refused, or
   *  failed — failure surfaces via `saveError` and leaves the buffer dirty). A
   *  clean buffer is a no-op; a conflicted one is refused and its banner shown
   *  again; one whose base isn't verified yet waits for the check. */
  save: (content: string) => Promise<void>;
  saveError: string | null;
  /** A write is in flight (header/status "Saving…"). */
  saving: boolean;
  /** The buffer's base: the disk version it is based on (line ending and
   *  indentation detection read it). */
  diskText: string;
  retryRead: () => void;
  /** Disk holds a version that is neither the base nor the buffer (derived; or
   *  the merge is up). Saves are refused until the user reloads or merges. */
  stale: boolean;
  /** The conflict banner: the conflict, unless "Keep editing" dismissed it for
   *  this same disk version. */
  showStaleBanner: boolean;
  /** Take the last disk version read, discarding unsaved edits. Synchronous. */
  reloadFromDisk: () => void;
  /** Dismiss the banner (the conflict, and the refused save, stay). */
  keepEditing: () => void;
  /** The disk-vs-buffer merge view is up. */
  reconciling: boolean;
  /** The disk version the merge shows — frozen until refreshed. */
  reconcileDisk: string;
  /** A merge problem shown inline (unviewable disk, a refused or failed resolve). */
  reconcileError: string | null;
  /** The file changed on disk since the merge opened (its left side is frozen). */
  reconcileDiskMoved: boolean;
  /** Read the latest version into the merge's left side. */
  refreshReconcile: () => void;
  /** Open the merge on the last disk version read (no read). */
  startReconcile: () => void;
  /** Leave the merge, keeping the buffer; disk is checked once. */
  cancelReconcile: () => void;
  /** Adopt the disk version the merge shows; disk is checked once. */
  takeTheirs: () => void;
  /** Write `content` (keep mine, or a hand-merge) locked to the version the
   *  merge shows. Resolves once that write settles. */
  resolveSave: (content: string) => Promise<void>;
  /** The current buffer, its base, and whether it is dirty — read from the
   *  machine directly, so it is safe in an unmount cleanup. */
  getDraft: () => FileDraft & { dirty: boolean; reconciling: boolean };
}

const PLACEHOLDER_MESSAGE = {
  binary: BINARY_MESSAGE,
  tooLarge: TOO_LARGE_MESSAGE,
  notUtf8: NOT_UTF8_MESSAGE,
} as const;

function viewOf(m: Machine, path: string | null): EditorViewState {
  if (path == null) return { kind: "idle" };
  const s = m.s;
  switch (s.k) {
    case "loading":
      return { kind: "loading" };
    case "noText":
      if (s.why === "placeholder") {
        return { kind: "placeholder", message: PLACEHOLDER_MESSAGE[s.unviewable] };
      }
      if (s.why === "readError") return { kind: "readError", message: s.message };
      return { kind: "removed" };
    case "editing":
    case "merging":
      return { kind: "editable" };
  }
}

function textOf(m: Machine): { buffer: string; base: string } {
  const s = m.s;
  if (s.k === "editing" || s.k === "merging") return { buffer: s.text, base: s.base.text };
  if (s.k === "noText" && s.base) return { buffer: s.base.text, base: s.base.text };
  return { buffer: "", base: "" };
}

/** What a read came back with, in the machine's terms. Text the editor
 *  already holds (its buffer or base — e.g. a file the user typed past the
 *  view limit and saved) is never refused as too large. */
function classifyContent(
  content: string,
  hash: string | null,
  held: readonly string[],
): ReadResult {
  if (held.includes(content)) return { k: "snap", text: content, hash };
  if (looksBinary(content)) return { k: "unviewable", why: "binary" };
  if (isTooLargeToView(utf8ByteLength(content))) return { k: "unviewable", why: "tooLarge" };
  return { k: "snap", text: content, hash };
}

/** An open file listed over the view limit is re-read in full at most this
 *  often on local workspace changes (the watcher's nonce is workspace-wide). */
const BIG_REREAD_MS = 5000;

function classifyReadError(e: unknown): ReadResult {
  if (isGone(e)) return { k: "gone" };
  if (isNotUtf8Error(e)) return { k: "unviewable", why: "notUtf8" };
  if (isBoundsExceeded(e)) return { k: "unviewable", why: "tooLarge" };
  return { k: "error", message: describeFileError(e) };
}

/**
 * The editable-file model shared by {@link SourceEditorInset} (worktree) and
 * {@link ContextEditorInset} (context): a thin React shell over the pure
 * buffer state machine (`buffer-machine.ts`). The hook only turns inputs into
 * events (watcher nonce, remote poll, a new listed size, a path move, user
 * actions) and runs the machine's commands (one read, one write, each tagged
 * with a ticket). Every rule about when disk may touch the buffer lives in the
 * machine.
 */
export function useFileEditor(
  selected: SelectedFile | null,
  changedNonce: number,
  opts: FileEditorOptions,
): FileEditor {
  const path = selected?.path ?? null;
  const size = selected?.size ?? null;
  const tooLarge = isTooLargeToView(size);
  const seedFor = opts.seed;
  const seedContent = useMemo(
    () => (path != null && seedFor ? seedFor(path) : null),
    [path, seedFor],
  );
  const pollMs = opts.pollMs ?? 2500;
  const canPoll = opts.remote?.inspectHash != null;

  // The latest inputs, for commands run after an await.
  const live = useRef({ path, tooLarge, opts, size });
  useLayoutEffect(() => {
    live.current = { path, tooLarge, opts, size };
  });

  // The machine. `machineRef` is the source of truth (read synchronously by
  // handlers and cleanups); the state copy drives rendering.
  const [initial] = useState(() =>
    init({ restore: opts.restore ?? null, seed: seedContent, tooLarge }),
  );
  const machineRef = useRef<Machine>(initial.m);
  const [machine, setMachine] = useState<Machine>(initial.m);
  const mountedRef = useRef(true);
  // Callers awaiting a save or resolve: settled once no write is out or owed.
  const waitersRef = useRef<(() => void)[]>([]);

  const settleWaiters = useCallback(() => {
    const m = machineRef.current;
    if (m.io.write || m.io.saveQueued) return;
    const waiting = waitersRef.current;
    waitersRef.current = [];
    for (const w of waiting) w();
  }, []);

  const dispatchRef = useRef<(e: Event) => void>(() => {});
  // The remote hash of the last version read (viewable or not), for the poll:
  // a version already read, even one that can't be shown, isn't read again.
  const lastReadHashRef = useRef<string | null>(null);
  // When (and at what listed size) an over-limit open file was last read in
  // full. Workspace changes don't each re-read it: a remote one is re-read
  // when its hash moved, a local one at most once per BIG_REREAD_MS, with a
  // trailing re-check so the last change is always seen.
  const bigReadRef = useRef<{ size: number | null; at: number } | null>(null);
  const bigTrailRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const run = useCallback((c: Cmd) => {
    const { path: p, tooLarge: big, opts: o, size: listed } = live.current;
    const land = (e: Event) => {
      if (mountedRef.current) dispatchRef.current(e);
    };
    if (p == null) return;
    if (c.k === "read") {
      const cur = machineRef.current.s;
      const editing = cur.k === "editing" || cur.k === "merging";
      const held = editing ? [cur.text, cur.base.text] : [];
      // A file listed over the limit is not read (it is unviewable as listed)
      // unless it is open for editing: then its size may be the user's own.
      if (big && !editing) {
        void Promise.resolve().then(() =>
          land({
            k: "READ_LANDED",
            ticket: c.ticket,
            result: { k: "unviewable", why: "tooLarge" },
          }),
        );
        return;
      }
      const current = () => machineRef.current.io.read?.ticket === c.ticket;
      const noteHash = (h: unknown) => {
        if (typeof h === "string" && current()) lastReadHashRef.current = h;
      };
      const readNow = () => {
        if (big) bigReadRef.current = { size: listed, at: Date.now() };
        const reader: Promise<ReadResult> = o.remote
          ? o.remote.readWithHash(p).then(
              (r) => {
                noteHash(r.hash);
                return classifyContent(r.content, r.hash, held);
              },
              (e: unknown) => {
                noteHash(
                  typeof e === "object" && e !== null ? (e as { hash?: unknown }).hash : null,
                );
                throw e;
              },
            )
          : readFile(p).then((content) => classifyContent(content, null, held));
        reader
          .catch((e: unknown) => classifyReadError(e))
          .then((result) => land({ k: "READ_LANDED", ticket: c.ticket, result }));
      };
      // An over-limit open file already read at this listed size: the version
      // held stands unless disk may have moved since.
      const disk = cur.k === "editing" ? cur.disk : null;
      const prior = bigReadRef.current;
      if (
        big &&
        prior != null &&
        prior.size === listed &&
        disk != null &&
        (disk.k === "snap" || disk.k === "unviewable")
      ) {
        const cached: ReadResult = disk;
        const keep = () => land({ k: "READ_LANDED", ticket: c.ticket, result: cached });
        const inspect = o.remote?.inspectHash;
        if (o.remote) {
          const known = disk.k === "snap" ? disk.hash : lastReadHashRef.current;
          if (!inspect || known == null) {
            readNow();
            return;
          }
          inspect(p).then(
            (h) => (h === known ? keep() : readNow()),
            () => readNow(),
          );
          return;
        }
        const wait = prior.at + BIG_REREAD_MS - Date.now();
        if (wait > 0) {
          if (bigTrailRef.current == null) {
            bigTrailRef.current = setTimeout(() => {
              bigTrailRef.current = null;
              if (mountedRef.current) dispatchRef.current({ k: "DISK_SIGNAL" });
            }, wait);
          }
          void Promise.resolve().then(keep);
          return;
        }
      }
      if (!big) bigReadRef.current = null;
      readNow();
      return;
    }
    const remote = o.remote != null;
    const expected = lockToken(c.lock, remote);
    expected
      .then((token) => o.write(p, c.text, token))
      .then((res) => {
        const hash = res && typeof res === "object" && "hash" in res ? res.hash : null;
        land({ k: "SAVE_OK", ticket: c.ticket, hash });
      })
      .catch((e: unknown) => {
        const refused = remote
          ? isRemoteConflict(e) ||
            (typeof e === "object" && e !== null && (e as { kind?: unknown }).kind === "notFound")
          : isLocalWriteConflict(e);
        if (refused) land({ k: "SAVE_REFUSED", ticket: c.ticket });
        else land({ k: "SAVE_FAILED", ticket: c.ticket, message: saveFailureMessage(e) });
      });
  }, []);

  const dispatch = useCallback(
    (e: Event) => {
      const cur = machineRef.current;
      const { m, cmds } = reduce(cur, e, {
        treatMissingAsRemoved: live.current.opts.treatMissingAsRemoved ?? false,
      });
      if (m !== cur) {
        machineRef.current = m;
        setMachine(m);
      }
      for (const c of cmds) run(c);
      settleWaiters();
    },
    [run, settleWaiters],
  );
  useLayoutEffect(() => {
    dispatchRef.current = dispatch;
  });

  // Mount: start the machine's first commands once (StrictMode remounts keep
  // the reads already out; their tickets are still current).
  const startedRef = useRef(false);
  useEffect(() => {
    mountedRef.current = true;
    if (!startedRef.current) {
      startedRef.current = true;
      for (const c of initial.cmds) run(c);
    }
    return () => {
      mountedRef.current = false;
      if (bigTrailRef.current != null) clearTimeout(bigTrailRef.current);
      bigTrailRef.current = null;
      const waiting = waitersRef.current;
      waitersRef.current = [];
      for (const w of waiting) w();
    };
  }, [initial, run]);

  // The path moved under the buffer (a worktree root move): re-target, carrying
  // unsaved edits as an unverified draft.
  const seenPathRef = useRef(path);
  useLayoutEffect(() => {
    if (seenPathRef.current === path) return;
    seenPathRef.current = path;
    // What was read at the old location says nothing about the new one.
    lastReadHashRef.current = null;
    bigReadRef.current = null;
    if (bigTrailRef.current != null) clearTimeout(bigTrailRef.current);
    bigTrailRef.current = null;
    const m = machineRef.current;
    const t = textOf(m);
    const editable = m.s.k === "editing" || m.s.k === "merging";
    dispatch({ k: "RETARGET", draft: retargetDraft(editable, t.buffer, t.base), tooLarge });
  }, [path, tooLarge, dispatch]);

  // A new listed size (the tree relisted the file) is a disk signal.
  const seenSizeRef = useRef(size);
  useEffect(() => {
    if (seenSizeRef.current === size) return;
    seenSizeRef.current = size;
    dispatch({ k: "DISK_SIGNAL", relist: true });
  }, [size, dispatch]);

  // The watcher's nonce: a change to the open file (or one we can't rule out).
  const seenNonceRef = useRef(changedNonce);
  useEffect(() => {
    if (seenNonceRef.current === changedNonce) return;
    seenNonceRef.current = changedNonce;
    dispatch({ k: "DISK_SIGNAL" });
  }, [changedNonce, dispatch]);

  // Remote: poll the file's hash against the version last read (the remote
  // path-set watcher can't see in-place edits). A move is a disk signal.
  const editableForPoll = machine.s.k === "editing" || machine.s.k === "merging";
  useEffect(() => {
    if (!canPoll || !editableForPoll || path == null || pollMs <= 0) return;
    let cancelled = false;
    let busy = false;
    const known = (): string | null => {
      const s = machineRef.current.s;
      if (s.k === "merging") return s.theirs.hash;
      if (s.k !== "editing") return null;
      if (s.disk.k === "snap") return s.disk.hash;
      return lastReadHashRef.current ?? s.base.hash;
    };
    const tick = () => {
      const inspect = live.current.opts.remote?.inspectHash;
      if (busy || !inspect || machineRef.current.io.write) return;
      if (typeof document !== "undefined" && document.visibilityState === "hidden") return;
      if (known() == null) return;
      busy = true;
      inspect(path)
        .then((hash) => {
          if (cancelled || live.current.path !== path) return;
          const k = known();
          // One read at a time: while one is out (or a write), the machine
          // already reads again when it settles; a failed read is retried by
          // the next tick that still sees the hash moved.
          const io = machineRef.current.io;
          if (k != null && hash !== k && !io.read && !io.write) dispatch({ k: "DISK_SIGNAL" });
        })
        .catch(() => {
          // Disconnected or gone: the save path reports it; polling stays quiet.
        })
        .finally(() => {
          busy = false;
        });
    };
    const id = setInterval(tick, pollMs);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [canPoll, editableForPoll, path, pollMs, dispatch]);

  const waitForIdle = useCallback((): Promise<void> => {
    const m = machineRef.current;
    if (!m.io.write && !m.io.saveQueued) return Promise.resolve();
    return new Promise<void>((resolve) => waitersRef.current.push(resolve));
  }, []);

  const setBuffer = useCallback((next: string) => dispatch({ k: "EDIT", text: next }), [dispatch]);
  const save = useCallback(
    (content: string) => {
      dispatch({ k: "EDIT", text: content });
      dispatch({ k: "SAVE" });
      return waitForIdle();
    },
    [dispatch, waitForIdle],
  );
  const resolveSave = useCallback(
    (content: string) => {
      dispatch({ k: "RESOLVE", text: content });
      return waitForIdle();
    },
    [dispatch, waitForIdle],
  );
  const retryRead = useCallback(() => dispatch({ k: "RETRY" }), [dispatch]);
  const reloadFromDisk = useCallback(() => dispatch({ k: "RELOAD" }), [dispatch]);
  const keepEditing = useCallback(() => dispatch({ k: "DISMISS" }), [dispatch]);
  const startReconcile = useCallback(() => dispatch({ k: "OPEN_MERGE" }), [dispatch]);
  const refreshReconcile = useCallback(() => dispatch({ k: "MERGE_REFRESH" }), [dispatch]);
  const cancelReconcile = useCallback(() => dispatch({ k: "CANCEL_MERGE" }), [dispatch]);
  const takeTheirs = useCallback(() => dispatch({ k: "TAKE_THEIRS" }), [dispatch]);
  const getDraft = useCallback(() => {
    const m = machineRef.current;
    const t = textOf(m);
    return {
      buffer: t.buffer,
      lastSaved: t.base,
      dirty: isDirty(m),
      reconciling: m.s.k === "merging",
    };
  }, []);

  const state = viewOf(machine, path);
  const t = textOf(machine);
  const conflict = isConflict(machine);
  const s = machine.s;
  return {
    state,
    buffer: t.buffer,
    setBuffer,
    dirty: state.kind === "editable" && isDirty(machine),
    save,
    saveError: machine.saveError,
    saving: machine.io.write != null,
    diskText: t.base,
    retryRead,
    stale: conflict || s.k === "merging",
    showStaleBanner: conflict && s.k === "editing" && !s.dismissed,
    reloadFromDisk,
    keepEditing,
    reconciling: s.k === "merging",
    reconcileDisk: s.k === "merging" ? s.theirs.text : "",
    reconcileError: machine.mergeError,
    reconcileDiskMoved: s.k === "merging" && s.diskMoved,
    refreshReconcile,
    startReconcile,
    cancelReconcile,
    takeTheirs,
    resolveSave,
    getDraft,
  };
}

/** The token a write carries for its lock. Remote: the observation hash
 *  (`undefined` asserts the file is absent). Local: the SHA-256 of the base
 *  text, or `"absent"`. */
async function lockToken(lock: Lock, remote: boolean): Promise<string | undefined> {
  switch (lock.k) {
    case "hash":
      return lock.hash;
    case "absent":
      return remote ? undefined : LOCAL_ABSENT_TOKEN;
    case "text": {
      if (remote) throw new Error("No remote lock for this version — reload the file to save.");
      const hash = await sha256Hex(lock.text);
      // Never an unlocked write.
      if (hash == null) throw new Error("Can't lock the save (no SHA-256 available).");
      return hash;
    }
  }
}

/** The local writer's token for "the file must not exist yet". */
export const LOCAL_ABSENT_TOKEN = "absent";

/** A readable message for a write that failed outright (not a lock refusal). */
function saveFailureMessage(e: unknown): string {
  if (typeof e === "object" && e !== null) {
    switch ((e as { kind?: unknown }).kind) {
      case "disconnected":
        return "Can't reach the remote machine — reconnect to save.";
      case "unauthorized":
        return "Workspace authority revoked — reopen the workspace to save.";
      case "boundsExceeded":
        return "Buffer exceeds the 4 MiB remote write cap — reduce the file to save.";
      case "transportFailure": {
        const msg = (e as { message?: unknown }).message;
        return typeof msg === "string" && msg
          ? msg
          : "Transport failure saving to the remote machine.";
      }
      default:
        break;
    }
  }
  return describeFileError(e);
}
