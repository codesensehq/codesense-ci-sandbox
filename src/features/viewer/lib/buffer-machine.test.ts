// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  canSave,
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

// Exhaustive model check of the buffer state machine against the spec's
// invariants (EDITOR-BUFFER-STATE-SPEC.md, I1–I8 plus liveness).
//
// An environment model holds the true disk (with a version number), the agent
// that writes it, and the queue of IO results the machine is waiting for. A
// breadth-first search explores every interleaving of user actions, agent
// writes, disk signals and IO deliveries (in any order, stale ones included)
// to a fixed depth, deduplicating canonical states, and checks every step. A
// seeded random walk then goes much deeper.

type Mode = "local" | "remote";
type EnvDisk = { k: "text"; text: string } | { k: "gone" } | { k: "bin" };

interface Pending {
  kind: "read" | "write";
  ticket: number;
  /** A read's sample of disk when it ran, with that disk's version. */
  sample?: { result: ReadResult; v: number };
  text?: string;
  lock?: Lock;
}

interface World {
  m: Machine;
  disk: EnvDisk;
  v: number;
  pending: Pending[];
  /** Every disk version the user was shown (base, disk read, merge). */
  shown: string[];
  mode: Mode;
  tooLarge: boolean;
  removed: boolean;
  /** Versions mirrored alongside the machine, for I8. */
  diskV: number;
  baseV: number;
  theirsV: number;
}

const TEXTS = ["A", "B", "C"];

const hashOf = (mode: Mode, text: string) => (mode === "remote" ? `h:${text}` : null);

function sample(w: World): { result: ReadResult; v: number } {
  if (w.tooLarge) return { result: { k: "unviewable", why: "tooLarge" }, v: w.v };
  if (w.disk.k === "gone") return { result: { k: "gone" }, v: w.v };
  if (w.disk.k === "bin") return { result: { k: "unviewable", why: "binary" }, v: w.v };
  return { result: { k: "snap", text: w.disk.text, hash: hashOf(w.mode, w.disk.text) }, v: w.v };
}

/** Does the env's disk satisfy a write's lock? */
function lockHolds(w: World, lock: Lock): boolean {
  // (An unlocked write, which only a mutation emits, always lands.)
  if ((lock as { k: string }).k === "none") return true;
  if (lock.k === "absent") return w.disk.k === "gone";
  if (w.disk.k !== "text") return false;
  if (lock.k === "hash") return lock.hash === hashOf(w.mode, w.disk.text);
  return lock.text === w.disk.text;
}

function record(w: World): string[] {
  const s = w.m.s;
  const out = new Set(w.shown);
  if ((s.k === "editing" || s.k === "merging") && !s.base.absent) out.add(s.base.text);
  if (s.k === "editing" && s.disk.k === "snap") out.add(s.disk.text);
  if (s.k === "merging") out.add(s.theirs.text);
  return [...out].sort();
}

type Step =
  | { k: "event"; e: Event }
  | { k: "deliver"; index: number; outcome?: "ok" | "fail" }
  | { k: "agent"; disk: EnvDisk };

function label(st: Step): string {
  if (st.k === "event") return JSON.stringify(st.e);
  if (st.k === "agent") return `agent ${JSON.stringify(st.disk)}`;
  return `deliver #${st.index}${st.outcome === "fail" ? " (fail)" : ""}`;
}

interface Violation {
  msg: string;
}

/** Apply one step; returns the new world, or a violation. */
function apply(w: World, st: Step): World | Violation {
  const opts = { treatMissingAsRemoved: w.removed };
  if (st.k === "agent") {
    return { ...w, disk: st.disk, v: w.v + 1 };
  }
  let event: Event;
  let delivered: Pending | null = null;
  let pending = w.pending;
  let disk = w.disk;
  let v = w.v;
  let wroteV = -1;
  let overwrote: string | null = null;
  if (st.k === "deliver") {
    delivered = w.pending[st.index] ?? null;
    if (!delivered) return { msg: "no such pending" };
    pending = w.pending.filter((_, i) => i !== st.index);
    if (delivered.kind === "read") {
      event = { k: "READ_LANDED", ticket: delivered.ticket, result: delivered.sample!.result };
    } else if (st.outcome === "fail") {
      event = { k: "SAVE_FAILED", ticket: delivered.ticket, message: "boom" };
    } else if (lockHolds(w, delivered.lock!)) {
      overwrote = w.disk.k === "text" ? w.disk.text : null;
      disk = { k: "text", text: delivered.text! };
      v = w.v + 1;
      wroteV = v;
      event = { k: "SAVE_OK", ticket: delivered.ticket, hash: hashOf(w.mode, delivered.text!) };
    } else {
      event = { k: "SAVE_REFUSED", ticket: delivered.ticket };
    }
  } else {
    event = st.e;
  }

  const prev = w.m;
  const { m, cmds } = reduce(prev, event, opts);
  const ps = prev.s;
  const ns = m.s;

  // I2 end to end: a write that landed overwrote only a version the user saw.
  if (overwrote != null && !w.shown.includes(overwrote) && overwrote !== delivered?.text) {
    return { msg: `I2: a write overwrote ${overwrote}, never shown` };
  }

  // I3: a stale result is a no-op.
  if (event.k === "READ_LANDED" && prev.io.read?.ticket !== event.ticket && m !== prev) {
    return { msg: "I3: a stale read changed the machine" };
  }
  if (
    (event.k === "SAVE_OK" || event.k === "SAVE_REFUSED" || event.k === "SAVE_FAILED") &&
    prev.io.write?.ticket !== event.ticket &&
    m !== prev
  ) {
    return { msg: "I3: a stale write result changed the machine" };
  }
  // I3/I4: at most one read and one write; no read starts while a write is out.
  const reads = cmds.filter((c) => c.k === "read").length;
  const writes = cmds.filter((c) => c.k === "write").length;
  if (reads > 1 || writes > 1) return { msg: "I3: more than one command of a kind" };
  if (reads > 0 && m.io.write) return { msg: "I4: a read started while a write is out" };
  if (writes > 0 && m.io.read) return { msg: "I4: a write left a read in flight" };
  for (const c of cmds) {
    if (c.k === "read" && m.io.read?.ticket !== c.ticket) return { msg: "I3: read not tracked" };
    if (c.k === "write" && m.io.write?.ticket !== c.ticket) return { msg: "I3: write not tracked" };
  }

  // I1: disk never replaces a dirty text.
  const userTextEvents = ["EDIT", "RELOAD", "TAKE_THEIRS"];
  const resolveOk =
    event.k === "SAVE_OK" &&
    prev.io.write?.ticket === event.ticket &&
    prev.io.write.kind === "resolve";
  if (isDirty(prev) && !userTextEvents.includes(event.k) && !resolveOk) {
    const pt = (ps as { text: string }).text;
    if (ns.k !== "editing" && ns.k !== "merging")
      return { msg: `I1: dirty text dropped by ${event.k}` };
    if (ns.text !== pt) return { msg: `I1: dirty text replaced by ${event.k}` };
  }

  // I2: every write carries the lock of a version the user was shown.
  for (const c of cmds) {
    if (c.k !== "write") continue;
    if (c.lock.k === "absent" || (c.lock as { k: string }).k === "none") continue;
    const locked = c.lock.k === "text" ? c.lock.text : c.lock.hash.replace(/^h:/, "");
    if (!record({ ...w, m: prev }).includes(locked) && !record({ ...w, m }).includes(locked)) {
      return { msg: `I2: a write locked to ${locked}, never shown` };
    }
    if (w.mode === "remote" && c.lock.k === "text") return { msg: "I2: remote write with no hash" };
  }

  // I5: in a merge, disk events change only diskMoved; theirs only on refresh.
  if (ps.k === "merging" && ns.k === "merging" && ps.theirs.text !== ns.theirs.text) {
    const refresh =
      event.k === "READ_LANDED" &&
      prev.io.read?.ticket === event.ticket &&
      prev.io.read.kind !== "check";
    if (!refresh) return { msg: `I5: theirs changed by ${event.k}` };
  }
  if (ps.k === "merging" && event.k === "DISK_SIGNAL") {
    if (ns.k !== "merging" || ns.text !== ps.text || ns.theirs !== ps.theirs || cmds.length > 0) {
      return { msg: "I5: a disk signal did more than flag a merge" };
    }
  }
  // A placeholder for a file measured too large isn't re-read on every change.
  if (
    ps.k === "noText" &&
    ps.why === "placeholder" &&
    ps.unviewable === "tooLarge" &&
    event.k === "DISK_SIGNAL" &&
    !event.relist &&
    reads > 0
  ) {
    return { msg: "a too-large placeholder was read on a plain disk signal" };
  }
  // I6: leaving a merge emits exactly one read.
  if (ps.k === "merging" && ns.k !== "merging" && reads !== 1) {
    return { msg: `I6: leaving the merge by ${event.k} issued ${reads} reads` };
  }

  // Mirror versions (for I8) and run the commands into the env.
  let { diskV, baseV, theirsV } = w;
  const landedRead =
    event.k === "READ_LANDED" && prev.io.read?.ticket === event.ticket ? delivered!.sample!.v : -1;
  if (landedRead >= 0) {
    if (prev.io.read!.kind === "check") diskV = landedRead;
    else theirsV = landedRead;
  }
  if (wroteV >= 0 && prev.io.write?.ticket === (event as { ticket: number }).ticket) diskV = wroteV;
  if (event.k === "OPEN_MERGE" && ns.k === "merging") theirsV = diskV;
  if ((event.k === "TAKE_THEIRS" || event.k === "CANCEL_MERGE") && ps.k === "merging")
    diskV = theirsV;
  const baseText = (s: Machine["s"]) =>
    s.k === "editing" || s.k === "merging" ? s.base.text : null;
  const baseChanged = baseText(ps) !== baseText(ns);
  if (baseChanged && baseText(ns) != null) {
    const nextBaseV =
      event.k === "SAVE_OK"
        ? wroteV
        : event.k === "TAKE_THEIRS"
          ? theirsV
          : event.k === "READ_LANDED"
            ? landedRead
            : diskV;
    // I8: a clean base only moves forward.
    if (!isDirty({ ...m }) && nextBaseV >= 0 && nextBaseV < baseV && ps.k !== "loading") {
      return { msg: `I8: clean base rolled back (v${nextBaseV} < v${baseV}) by ${event.k}` };
    }
    if (nextBaseV >= 0) baseV = nextBaseV;
  }

  const next: Pending[] = [...pending];
  const wAfter: World = { ...w, m, disk, v };
  for (const c of cmds as Cmd[]) {
    if (c.k === "read") next.push({ kind: "read", ticket: c.ticket, sample: sample(wAfter) });
    else next.push({ kind: "write", ticket: c.ticket, text: c.text, lock: c.lock });
  }
  const out: World = { ...wAfter, pending: next, diskV, baseV, theirsV };
  return { ...out, shown: record(out) };
}

const isViolation = (x: World | Violation): x is Violation => "msg" in x && !("m" in x);

/** Every step possible from `w`. */
function steps(w: World): Step[] {
  const out: Step[] = [];
  const s = w.m.s;
  out.push({ k: "event", e: { k: "DISK_SIGNAL" } });
  for (let i = 0; i < w.pending.length; i++) {
    out.push({ k: "deliver", index: i });
    if (w.pending[i]!.kind === "write") out.push({ k: "deliver", index: i, outcome: "fail" });
  }
  for (const t of TEXTS) {
    if (w.disk.k !== "text" || w.disk.text !== t)
      out.push({ k: "agent", disk: { k: "text", text: t } });
  }
  if (w.disk.k !== "gone") out.push({ k: "agent", disk: { k: "gone" } });
  if (w.disk.k !== "bin") out.push({ k: "agent", disk: { k: "bin" } });
  if (s.k === "editing" || s.k === "merging") {
    for (const t of TEXTS) if (s.text !== t) out.push({ k: "event", e: { k: "EDIT", text: t } });
  }
  if (s.k === "editing") {
    out.push({ k: "event", e: { k: "SAVE" } });
    out.push({ k: "event", e: { k: "RELOAD" } });
    out.push({ k: "event", e: { k: "DISMISS" } });
    out.push({ k: "event", e: { k: "OPEN_MERGE" } });
  }
  if (s.k === "merging") {
    out.push({ k: "event", e: { k: "MERGE_REFRESH" } });
    out.push({ k: "event", e: { k: "RESOLVE", text: s.text } });
    out.push({ k: "event", e: { k: "TAKE_THEIRS" } });
    out.push({ k: "event", e: { k: "CANCEL_MERGE" } });
  }
  if (s.k === "noText" && s.why === "readError") out.push({ k: "event", e: { k: "RETRY" } });
  return out;
}

/** Canonical key: tickets renumbered by order of appearance. */
function key(w: World): string {
  const map = new Map<number, number>();
  const t = (n: number | undefined) => {
    if (n == null) return null;
    if (!map.has(n)) map.set(n, map.size);
    return map.get(n);
  };
  const io = w.m.io;
  return JSON.stringify([
    w.m.s,
    t(io.read?.ticket),
    io.read?.kind,
    t(io.write?.ticket),
    io.write?.kind,
    io.write?.text,
    io.rereadWanted,
    io.saveQueued,
    w.m.saveError,
    w.m.mergeError,
    w.disk,
    w.pending.map((p) => [p.kind, t(p.ticket), p.sample?.result, p.text, p.lock]),
    w.shown,
  ]);
}

/** Deliver everything (oldest first), signal disk, deliver again: rest. */
function quiesce(w0: World): World | Violation {
  let w = w0;
  for (let round = 0; round < 6; round++) {
    let guard = 0;
    while (w.pending.length > 0) {
      if (guard++ > 50) return { msg: "liveness: IO never settles" };
      const r = apply(w, { k: "deliver", index: 0 });
      if (isViolation(r)) return r;
      w = r;
    }
    if (round < 2) {
      const r = apply(w, { k: "event", e: { k: "DISK_SIGNAL" } });
      if (isViolation(r)) return r;
      w = r;
    }
  }
  return w;
}

/** I7 at rest, and liveness: a way back to Clean or saveable. */
function checkRest(w0: World): Violation | null {
  const r = quiesce(w0);
  if (isViolation(r)) return r;
  const w = r;
  const s = w.m.s;
  // A clean placeholder for content (not the listed size) recovers once disk
  // is text again.
  if (s.k === "noText" && s.why === "placeholder" && s.unviewable !== "tooLarge") {
    if (w.disk.k === "text") return { msg: "I7: a content placeholder over text disk at rest" };
  }
  if (s.k === "editing" && !w.tooLarge) {
    const clean = !isDirty(w.m);
    if (w.disk.k === "text") {
      if (clean && s.text !== w.disk.text)
        return { msg: "I7: clean buffer differs from disk at rest" };
      if (
        !clean &&
        !s.base.absent &&
        w.disk.text !== s.base.text &&
        w.disk.text !== s.text &&
        !isConflict(w.m)
      ) {
        return { msg: "I7: dirty buffer over moved disk shows no conflict at rest" };
      }
      if (isConflict(w.m) && (w.disk.text === s.text || w.disk.text === s.base.text)) {
        return { msg: "I7: conflict shown though disk equals the text or base" };
      }
      if (!clean && !s.base.absent && w.disk.text === s.text)
        return { msg: "I7: disk equals the edits, still dirty" };
    }
    if (w.disk.k === "bin" && clean)
      return { msg: "I7: clean buffer over binary disk is editable" };
  }
  // Liveness: leave any merge, then Reload if stuck; must end Clean or saveable.
  let x: World = w;
  const go = (st: Step): Violation | null => {
    const y = apply(x, st);
    if (isViolation(y)) return y;
    const q = quiesce(y);
    if (isViolation(q)) return q;
    x = q;
    return null;
  };
  if (x.m.s.k === "merging") {
    const v = go({ k: "event", e: { k: "TAKE_THEIRS" } });
    if (v) return v;
  }
  if (isDirty(x.m) && !canSave(x.m)) {
    const v = go({ k: "event", e: { k: "RELOAD" } });
    if (v) return v;
  }
  if (isDirty(x.m) && !canSave(x.m)) {
    return { msg: `liveness: stuck dirty and unsaveable: ${JSON.stringify(x.m.s)}` };
  }
  return null;
}

interface Start {
  name: string;
  disk: EnvDisk;
  restore?: { buffer: string; lastSaved: string };
  seed?: string;
  tooLarge?: boolean;
  removed?: boolean;
}

const STARTS: Start[] = [
  { name: "fresh open", disk: { k: "text", text: "A" } },
  { name: "fresh open, file too large", disk: { k: "text", text: "A" }, tooLarge: true },
  { name: "fresh open, binary", disk: { k: "bin" } },
  { name: "fresh open, context (removed)", disk: { k: "text", text: "A" }, removed: true },
  { name: "clean seed, disk agrees", disk: { k: "text", text: "A" }, seed: "A" },
  { name: "clean seed, disk moved", disk: { k: "text", text: "B" }, seed: "A" },
  {
    name: "draft matching disk",
    disk: { k: "text", text: "B" },
    restore: { buffer: "B", lastSaved: "A" },
  },
  {
    name: "draft matching its base",
    disk: { k: "text", text: "A" },
    restore: { buffer: "B", lastSaved: "A" },
  },
  {
    name: "draft diverged",
    disk: { k: "text", text: "C" },
    restore: { buffer: "B", lastSaved: "A" },
  },
  { name: "draft, file gone", disk: { k: "gone" }, restore: { buffer: "B", lastSaved: "A" } },
  {
    name: "draft, file too large",
    disk: { k: "text", text: "A" },
    restore: { buffer: "B", lastSaved: "A" },
    tooLarge: true,
  },
];

function startWorld(st: Start, mode: Mode): World {
  const { m, cmds } = init({
    restore: st.restore ?? null,
    seed: st.seed ?? null,
    tooLarge: st.tooLarge ?? false,
  });
  const w: World = {
    m,
    disk: st.disk,
    v: 0,
    pending: [],
    shown: [],
    mode,
    tooLarge: st.tooLarge ?? false,
    removed: st.removed ?? false,
    diskV: -1,
    baseV: -1,
    theirsV: -1,
  };
  for (const c of cmds) {
    if (c.k === "read") w.pending.push({ kind: "read", ticket: c.ticket, sample: sample(w) });
  }
  return { ...w, shown: record(w) };
}

/** BFS to `depth`, checking every step and every reached state at rest. */
function explore(st: Start, mode: Mode, depth: number): { states: number; failure: string | null } {
  const start = startWorld(st, mode);
  const seen = new Set<string>([key(start)]);
  let frontier: { w: World; trail: string[] }[] = [{ w: start, trail: [] }];
  let states = 1;
  const rest0 = checkRest(start);
  if (rest0) return { states, failure: `${st.name}: ${rest0.msg} at start` };
  for (let d = 0; d < depth; d++) {
    const nextFrontier: typeof frontier = [];
    for (const { w, trail } of frontier) {
      for (const step of steps(w)) {
        const r = apply(w, step);
        const t = [...trail, label(step)];
        if (isViolation(r)) return { states, failure: `${st.name}: ${r.msg}\n  ${t.join("\n  ")}` };
        const k = key(r);
        if (seen.has(k)) continue;
        seen.add(k);
        states++;
        const rest = checkRest(r);
        if (rest) return { states, failure: `${st.name}: ${rest.msg}\n  ${t.join("\n  ")}` };
        nextFrontier.push({ w: r, trail: t });
      }
    }
    frontier = nextFrontier;
  }
  return { states, failure: null };
}

function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function walk(seed: number, mode: Mode, depth: number): string | null {
  const r = rng(seed);
  const st = STARTS[Math.floor(r() * STARTS.length)]!;
  let w = startWorld(st, mode);
  const trail: string[] = [];
  for (let i = 0; i < depth; i++) {
    const options = steps(w);
    const step = options[Math.floor(r() * options.length)]!;
    trail.push(label(step));
    const n = apply(w, step);
    if (isViolation(n)) return `${st.name}: ${n.msg}\n  ${trail.join("\n  ")}`;
    w = n;
  }
  const rest = checkRest(w);
  return rest ? `${st.name}: ${rest.msg}\n  ${trail.join("\n  ")}` : null;
}

const DEPTH = Number(process.env.MODEL_DEPTH ?? 8);
const WALKS = Number(process.env.MODEL_WALKS ?? 20000);

describe("buffer machine — exhaustive model check (I1–I8, liveness)", () => {
  for (const mode of ["local", "remote"] as const) {
    for (const st of STARTS) {
      it(`${mode}: ${st.name} (depth ${DEPTH})`, () => {
        const { failure, states } = explore(st, mode, DEPTH);
        expect(failure).toBeNull();
        // The search really branched (a vacuous pass would explore nothing).
        expect(states).toBeGreaterThan(st.tooLarge ? 3 : 100);
      }, 120_000);
    }
  }
});

describe("buffer machine — seeded random walks (depth 40)", () => {
  for (const mode of ["local", "remote"] as const) {
    it(`${mode}: ${WALKS} walks hold every invariant`, () => {
      for (let seed = 1; seed <= WALKS; seed++) {
        const failure = walk(seed * 2654435761 + (mode === "remote" ? 7 : 0), mode, 40);
        expect(failure, `seed ${seed}`).toBeNull();
      }
    }, 300_000);
  }
});

describe("buffer machine — spec examples", () => {
  const opts = { treatMissingAsRemoved: false };
  const run = (m: Machine, ...es: Event[]) => {
    let cur = m;
    const cmds: Cmd[] = [];
    for (const e of es) {
      const r = reduce(cur, e, opts);
      cur = r.m;
      cmds.push(...r.cmds);
    }
    return { m: cur, cmds };
  };
  const opened = (text: string) => {
    const { m } = init({ restore: null, seed: null, tooLarge: false });
    return run(m, { k: "READ_LANDED", ticket: 1, result: { k: "snap", text, hash: null } }).m;
  };

  it("Reload is synchronous from the last disk version read", () => {
    let m = opened("A");
    m = run(m, { k: "EDIT", text: "mine" }, { k: "DISK_SIGNAL" }).m;
    const ticket = m.io.read!.ticket;
    m = run(m, { k: "READ_LANDED", ticket, result: { k: "snap", text: "theirs", hash: null } }).m;
    expect(isConflict(m)).toBe(true);
    const r = run(m, { k: "RELOAD" });
    expect(r.cmds).toEqual([]);
    expect(r.m.s).toMatchObject({ k: "editing", text: "theirs" });
    expect(isDirty(r.m)).toBe(false);
  });

  it("a save is refused while in conflict, and the banner comes back", () => {
    let m = opened("A");
    m = run(m, { k: "EDIT", text: "mine" }, { k: "DISK_SIGNAL" }).m;
    m = run(m, {
      k: "READ_LANDED",
      ticket: m.io.read!.ticket,
      result: { k: "snap", text: "theirs", hash: null },
    }).m;
    m = run(m, { k: "DISMISS" }).m;
    const r = run(m, { k: "SAVE" });
    expect(r.cmds).toEqual([]);
    expect(r.m.s).toMatchObject({ dismissed: false });
  });

  it("every write is locked: local to the base text, absent after a delete", () => {
    let m = opened("A");
    let r = run(m, { k: "EDIT", text: "mine" }, { k: "SAVE" });
    expect(r.cmds).toContainEqual(
      expect.objectContaining({ k: "write", lock: { k: "text", text: "A" } }),
    );
    m = opened("A");
    m = run(m, { k: "EDIT", text: "mine" }, { k: "DISK_SIGNAL" }).m;
    m = run(m, { k: "READ_LANDED", ticket: m.io.read!.ticket, result: { k: "gone" } }).m;
    r = run(m, { k: "SAVE" });
    expect(r.cmds).toContainEqual(expect.objectContaining({ k: "write", lock: { k: "absent" } }));
  });

  it("a merge freezes its disk; leaving re-checks exactly once", () => {
    let m = opened("A");
    m = run(m, { k: "EDIT", text: "mine" }, { k: "DISK_SIGNAL" }).m;
    m = run(m, {
      k: "READ_LANDED",
      ticket: m.io.read!.ticket,
      result: { k: "snap", text: "theirs", hash: null },
    }).m;
    m = run(m, { k: "OPEN_MERGE" }).m;
    const signalled = run(m, { k: "DISK_SIGNAL" }, { k: "DISK_SIGNAL" });
    expect(signalled.cmds).toEqual([]);
    expect(signalled.m.s).toMatchObject({
      k: "merging",
      diskMoved: true,
      theirs: { text: "theirs" },
    });
    const left = run(signalled.m, { k: "CANCEL_MERGE" });
    expect(left.cmds.filter((c) => c.k === "read")).toHaveLength(1);
  });

  it("a restored draft can't save until disk has been checked", () => {
    const { m, cmds } = init({
      restore: { buffer: "mine", lastSaved: "A" },
      seed: null,
      tooLarge: false,
    });
    expect(cmds).toEqual([{ k: "read", ticket: 1 }]);
    const r = run(m, { k: "SAVE" });
    expect(r.cmds).toEqual([]); // queued behind the check
    const checked = run(r.m, {
      k: "READ_LANDED",
      ticket: 1,
      result: { k: "snap", text: "A", hash: null },
    });
    expect(checked.cmds).toContainEqual(expect.objectContaining({ k: "write", text: "mine" }));
  });

  it("a deleted file's buffer stays dirty (recreatable) even back at its last text", () => {
    let m = opened("A");
    m = run(m, { k: "EDIT", text: "mine" }, { k: "DISK_SIGNAL" }).m;
    m = run(m, { k: "READ_LANDED", ticket: m.io.read!.ticket, result: { k: "gone" } }).m;
    m = run(m, { k: "EDIT", text: "A" }).m;
    expect(isDirty(m)).toBe(true);
    expect(canSave(m)).toBe(true);
    const r = run(m, { k: "SAVE" });
    expect(r.cmds).toContainEqual(expect.objectContaining({ k: "write", lock: { k: "absent" } }));
    // Reload discards the recreate too, and a later save checks disk first.
    const back = run(m, { k: "RELOAD" }).m;
    expect(isDirty(back)).toBe(false);
    const edited = run(back, { k: "EDIT", text: "again" }).m;
    expect(canSave(edited)).toBe(false);
    const queued = run(edited, { k: "SAVE" });
    expect(queued.cmds).toContainEqual(expect.objectContaining({ k: "read" }));
    expect(queued.cmds).not.toContainEqual(expect.objectContaining({ k: "write" }));
    const landed = run(queued.m, {
      k: "READ_LANDED",
      ticket: queued.m.io.read!.ticket,
      result: { k: "gone" },
    });
    expect(landed.cmds).toContainEqual(
      expect.objectContaining({ k: "write", text: "again", lock: { k: "absent" } }),
    );
  });

  it("a save queued behind a refused save's failed re-read is dropped, not sent", () => {
    let m = opened("A");
    m = run(m, { k: "EDIT", text: "mine" }, { k: "SAVE" }).m;
    m = run(m, { k: "SAVE_REFUSED", ticket: m.io.write!.ticket }).m;
    m = run(m, { k: "SAVE" }).m; // pressed again while the re-read is out
    const r = run(m, {
      k: "READ_LANDED",
      ticket: m.io.read!.ticket,
      result: { k: "error", message: "EIO" },
    });
    expect(r.cmds).not.toContainEqual(expect.objectContaining({ k: "write" }));
    expect(r.m.io.saveQueued).toBe(false);
    expect(r.m.saveError).toMatch(/reading it again failed/);
  });

  it("Reload clears an inline merge error", () => {
    let m = opened("A");
    m = run(m, { k: "EDIT", text: "mine" }, { k: "DISK_SIGNAL" }).m;
    m = run(m, {
      k: "READ_LANDED",
      ticket: m.io.read!.ticket,
      result: { k: "unviewable", why: "binary" },
    }).m;
    m = run(m, { k: "OPEN_MERGE" }).m;
    expect(m.mergeError).not.toBeNull();
    m = run(m, { k: "RELOAD" }).m;
    expect(m.mergeError).toBeNull();
  });

  it("a refused save whose re-read fails says the re-read failed", () => {
    let m = opened("A");
    const w = run(m, { k: "EDIT", text: "mine" }, { k: "SAVE" });
    m = w.m;
    m = run(m, { k: "SAVE_REFUSED", ticket: m.io.write!.ticket }).m;
    m = run(m, {
      k: "READ_LANDED",
      ticket: m.io.read!.ticket,
      result: { k: "error", message: "EIO" },
    }).m;
    expect(m.saveError).toMatch(/reading it again failed/);
  });

  it("a merge opened while a change is being read says disk moved", () => {
    let m = opened("A");
    m = run(m, { k: "EDIT", text: "mine" }, { k: "DISK_SIGNAL" }).m;
    m = run(m, {
      k: "READ_LANDED",
      ticket: m.io.read!.ticket,
      result: { k: "snap", text: "theirs", hash: null },
    }).m;
    m = run(m, { k: "DISK_SIGNAL" }).m; // another change, its read out
    m = run(m, { k: "OPEN_MERGE" }).m;
    expect(m.s).toMatchObject({ k: "merging", diskMoved: true });
  });
});
