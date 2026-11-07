// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import { NotMockedError } from "../mock-router";
import type { MockEventBus } from "../mock-events";
import { applyMockTextChange } from "../text-change";
import { mockScratchSpace } from "./git-sync";
import type {
  Note,
  NoteActor,
  NoteHit,
  NoteMeta,
  NoteRevision,
  NoteRow,
  NoteWritten,
} from "@/tauri/commands";
import { NOTES_CHANGED_EVENT, type NotesChangedPayload } from "@/tauri/events";
import { summarize } from "@/features/notes/lib/note-summary";

/**
 * The notes commands in the mock: an in-memory store of notes with a revision history,
 * per org. The default world is a first look (one empty note). `?notes=demo` seeds the
 * notes the redesign was drawn with — a "Prompts" note whose lines were handed to agents
 * of the `?agents=team` world, reorganized by a "Notes tidy" routine — plus the all-orgs
 * prompts the Agent menu prefills from. `?notesHistory=off` leaves the history commands
 * out, as on an install whose backend predates them.
 */

const MIN = 60_000;
const NOTES_DIR = "/mock/users/1/scratch/notes";
export const MOCK_PROMPTS_DIR = "/mock/users/1/scratch/all-orgs/prompts";

const PROMPTS: Record<string, string> = {
  [`${MOCK_PROMPTS_DIR}/organize-notes.md`]: `---
kind: prompt
---
Organize this note: turn loose lines into tasks, group them under short headings, and file reusable prompts under "Prompt to reuse". Don't delete anything.`,
  [`${MOCK_PROMPTS_DIR}/tidy-notes.md`]: `---
kind: prompt
---
Read today's notes. Turn loose lines into tasks, group them under headings, file prompts into a "Prompts" note. Don't delete anything. Then call signal_done with a 2-line summary.`,
};

/** The all-orgs prompt files the scratch-space listing shows in the demo world. */
export function mockNotePromptFiles(): { path: string; logical: string; kind: "prompt" }[] {
  return Object.keys(PROMPTS).map((path) => ({
    path,
    logical: `prompts/${path.slice(MOCK_PROMPTS_DIR.length + 1)}`,
    kind: "prompt" as const,
  }));
}

const PROMPTS_BODY_AFTER = `Icons are all just off — a pass over every chrome surface before the launch walk.

## This week

- [ ] Remove agent accounts from the setup flow [@Implement retry fix](cs:agent:BUG-142%23dev)
- [ ] The codesense/default drop down needs a redesign — it reads like the Mac menu
- [ ] Inbox icon spacing: needs top/right margin; full-screen CLI icons need the same polish [@Draft the fix spec](cs:agent:BUG-151%23spec)
- [ ] MCP tools are hidden — check [SW-RM-P1](cs:story:SW-RM-P1)
- [x] Messaging human ↔ agent [@Verify on staging](cs:agent:BUG-142%23qa)

## Prompt to reuse

\`\`\`
Run an impeccable pass on the right pane. Keep the status marks from #2032 and the 28px icon buttons.
# report back to the human inbox when done
\`\`\`

## Later

- Routines tab polish
- Agent avatars in the [inbox rail](https://example.com/inbox-rail)
`;

const PROMPTS_BODY_BEFORE = `Icons are all just off — a pass over every chrome surface before the launch walk.

- [ ] Remove agent accounts from the setup flow [@Implement retry fix](cs:agent:BUG-142%23dev)
- the codesense/default drop down needs to be redesigned. need to just put it in the mac menu
- [ ] Inbox icon spacing: needs top/right margin; full-screen CLI icons need the same polish [@Draft the fix spec](cs:agent:BUG-151%23spec)
- mcp tools are hidden.
- [x] Messaging human ↔ agent [@Verify on staging](cs:agent:BUG-142%23qa)

## Prompt to reuse

\`\`\`
Run an impeccable pass on the right pane. Keep the status marks from #2032 and the 28px icon buttons.
# report back to the human inbox when done
\`\`\`

## Later

- Routines tab polish
- Agent avatars in the [inbox rail](https://example.com/inbox-rail)
`;

interface MockNote extends NoteMeta {
  content: string;
  revs: { rev: NoteRevision; content: string }[];
}

const YOU: NoteActor = { kind: "human", name: "You" };
const TIDY: NoteActor = {
  kind: "agent",
  name: "Notes tidy",
  handle: "notes-tidy",
};

function seedDemo(now: number): MockNote[] {
  let n = 0;
  const rev = (
    at: number,
    actor: NoteActor,
    summary: string,
    added: number,
    removed: number,
  ): NoteRevision => ({ commit: `c${++n}`, at, actor, change: "write", summary, added, removed });
  const note = (
    id: string,
    title: string,
    minutesAgo: number,
    content: string,
    pinned = false,
  ): MockNote => {
    const at = now - minutesAgo * MIN;
    return {
      id,
      title,
      pinned,
      lastOpenedAt: at,
      content,
      revs: [{ rev: rev(at, YOU, "Edited by you", content.split("\n").length, 0), content }],
    };
  };
  const prompts = note("prompts", "Prompts", 4, PROMPTS_BODY_AFTER, true);
  prompts.revs = [
    {
      rev: rev(now - 4 * MIN, TIDY, "Notes tidy grouped 3 lines under This week", 4, 3),
      content: PROMPTS_BODY_AFTER,
    },
    {
      rev: rev(
        now - 2 * 60 * MIN,
        { kind: "agent", name: "Verify on staging", handle: "BUG-142#qa" },
        "Verify on staging checked off Messaging human ↔ agent",
        1,
        1,
      ),
      content: PROMPTS_BODY_BEFORE,
    },
    {
      rev: rev(now - 26 * 60 * MIN, YOU, "Edited by you", 9, 2),
      content: PROMPTS_BODY_BEFORE.replace("- [x] Messaging", "- [ ] Messaging"),
    },
  ];
  return [
    prompts,
    note(
      "launch-walk",
      "Launch walk checklist",
      3 * 24 * 60,
      "Signed build\n\nReal forge\n\nOAuth\n\nClone from scratch",
      true,
    ),
    note("home", "when get home:", 62, "- [ ] Call the bank\n- [ ] pick up the drone"),
    note(
      "remote-handoff",
      "Remote machines handoff",
      2 * 24 * 60,
      "Build machine 203.0.113.10 keep running; FX-1..6 in two PRs",
    ),
    note("standup", "Standup", 4 * 24 * 60, "Blocked on RM-0 review"),
  ];
}

function seedFirstLook(now: number): MockNote[] {
  return [
    {
      id: "note-1",
      title: "Note",
      pinned: false,
      lastOpenedAt: now,
      content: "",
      revs: [],
    },
  ];
}

function meta(n: MockNote): NoteMeta {
  const last = n.revs[0]?.rev;
  return {
    id: n.id,
    title: n.title,
    pinned: n.pinned,
    lastOpenedAt: n.lastOpenedAt,
    ...(last ? { updatedAt: last.at, updatedBy: last.actor } : {}),
  };
}

function asNote(n: MockNote): Note {
  return { ...meta(n), content: n.content };
}

/** Pinned first, then most recently changed or opened — the backend's order. */
function recentFirst(notes: MockNote[]): MockNote[] {
  const at = (n: MockNote) => Math.max(n.revs[0]?.rev.at ?? 0, n.lastOpenedAt);
  return [...notes].sort((a, b) => Number(b.pinned) - Number(a.pinned) || at(b) - at(a));
}

/** A `list_notes` row: the note's metadata with the preview the backend builds. */
function row(n: MockNote): NoteRow {
  const { snippet, refs } = summarize(n.content);
  return { ...meta(n), preview: snippet, refs };
}

/** Title and body substring matches, title matches first, each in list order. */
function searchHits(notes: MockNote[], query: string): NoteHit[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  return recentFirst(notes)
    .flatMap((n): NoteHit[] => {
      const inTitle = n.title.toLowerCase().includes(q);
      const line = n.content.split("\n").find((l) => l.toLowerCase().includes(q));
      if (!inTitle && line === undefined) return [];
      const matched = !inTitle ? "text" : line === undefined ? "title" : "both";
      return [{ id: n.id, title: n.title, matched, snippet: line?.trim() ?? n.title }];
    })
    .sort((a, b) => Number(a.matched === "text") - Number(b.matched === "text"));
}

function lineDelta(before: string, after: string): { added: number; removed: number } {
  const a = new Set(before.split("\n"));
  const b = new Set(after.split("\n"));
  return {
    added: [...b].filter((l) => !a.has(l)).length,
    removed: [...a].filter((l) => !b.has(l)).length,
  };
}

export function notesHandlers(events: MockEventBus, search = ""): HandlerRegistry {
  const params = new URLSearchParams(search);
  const demo = params.get("notes") === "demo";
  const withHistory = params.get("notesHistory") !== "off";
  const now = Date.now();
  const byOrg = new Map<string, MockNote[]>();
  const notesOf = (args: Record<string, unknown>): MockNote[] => {
    const org = (args.orgSlug as string | null) ?? "";
    let list = byOrg.get(org);
    if (!list) {
      list = demo ? seedDemo(now) : seedFirstLook(now);
      byOrg.set(org, list);
    }
    return list;
  };
  const find = (args: Record<string, unknown>): MockNote => {
    const note = notesOf(args).find((n) => n.id === args.id);
    if (!note) throw new Error(`note not found: ${String(args.id)}`);
    return note;
  };
  let revSeq = 1000;
  const saveRev = (
    note: MockNote,
    actor: NoteActor,
    change: NoteRevision["change"],
    summary: string,
  ) => {
    const before = note.revs[0]?.content ?? "";
    const { added, removed } = lineDelta(before, note.content);
    note.revs.unshift({
      rev: { commit: `c${++revSeq}`, at: Date.now(), actor, change, summary, added, removed },
      content: note.content,
    });
  };

  const written = (
    args: Record<string, unknown>,
    note: MockNote,
    changed: boolean,
    merged = false,
    commit: string | null = null,
    overlaps: NoteWritten["overlaps"] = [],
  ): NoteWritten => ({
    org: (args.orgSlug as string | null) ?? "",
    note: asNote(note),
    changed,
    commit,
    merged,
    overlaps,
  });

  const handlers: HandlerRegistry = {
    list_notes: (args) => recentFirst(notesOf(args)).map(row),
    read_note: (args) => asNote(find(args)),
    search_notes: (args) => searchHits(notesOf(args), String(args.query ?? "")),
    create_note: (args) => {
      const note: MockNote = {
        id: `note-${Math.random().toString(36).slice(2, 8)}`,
        title: String(args.title ?? "Untitled"),
        pinned: args.pinned === true,
        lastOpenedAt: Date.now(),
        content: typeof args.content === "string" ? args.content : "",
        revs: [],
      };
      if (note.content !== "") saveRev(note, YOU, "create", "Created by you");
      notesOf(args).push(note);
      return written(args, note, true);
    },
    write_note: (args) => {
      const note = find(args);
      let content = String(args.content ?? "");
      const merged =
        typeof args.base === "string" && args.base !== note.content && content !== note.content;
      let overlaps: NoteWritten["overlaps"] = [];
      if (merged) {
        ({ body: content, overlaps } = applyMockTextChange(
          args.base as string,
          content,
          note.content,
        ));
      }
      if (content === note.content) return written(args, note, false, merged, null, overlaps);
      note.content = content;
      // One revision per burst of typing, the way a debounced save lands.
      const last = note.revs[0];
      if (last && last.rev.actor.kind === "human" && Date.now() - last.rev.at < 5 * MIN) {
        last.content = note.content;
        last.rev.at = Date.now();
      } else {
        saveRev(note, YOU, "write", "Edited by you");
      }
      return written(args, note, true, merged, null, overlaps);
    },
    update_note_meta: (args) => {
      const note = find(args);
      const changed =
        (typeof args.title === "string" && args.title !== note.title) ||
        (typeof args.pinned === "boolean" && args.pinned !== note.pinned);
      if (typeof args.title === "string") note.title = args.title;
      if (typeof args.pinned === "boolean") note.pinned = args.pinned;
      if (typeof args.lastOpenedAt === "number") note.lastOpenedAt = args.lastOpenedAt;
      return written(args, note, changed);
    },
    delete_note: (args) => {
      const note = find(args);
      const list = notesOf(args);
      const at = list.findIndex((n) => n.id === args.id);
      if (at >= 0) list.splice(at, 1);
      if (list.length === 0) list.push(...seedFirstLook(Date.now()));
      return written(args, note, true);
    },
    reap_empty_scratch_notes: (args) => notesOf(args).map(meta),
    notes_dir_path: () => NOTES_DIR,
    ...(demo
      ? {
          list_scratch_space: (args: Record<string, unknown>) => {
            const listing = mockScratchSpace((args.orgSlug as string | null) ?? null);
            return { ...listing, allOrgs: [...listing.allOrgs, ...mockNotePromptFiles()] };
          },
        }
      : {}),
    read_file: (args) => {
      const path = String(args.path ?? "");
      if (path in PROMPTS) return PROMPTS[path];
      throw new NotMockedError(`read_file ${path}`);
    },
  };
  if (!withHistory) return handlers;
  return {
    ...handlers,
    notes_history: (args) => find(args).revs.map((r) => ({ ...r.rev })),
    notes_read_at: (args) => {
      const note = find(args);
      const r = note.revs.find((x) => x.rev.commit === args.commit);
      if (!r) throw new Error(`revision not found: ${String(args.commit)}`);
      return { id: note.id, commit: r.rev.commit, title: note.title, content: r.content };
    },
    notes_restore: (args) => {
      const note = find(args);
      const at = note.revs.findIndex((x) => x.rev.commit === args.commit);
      if (at < 0) throw new Error(`revision not found: ${String(args.commit)}`);
      // Undo brings back the revision before the one named (the real backend takes
      // back only that revision's lines).
      const r = args.undo ? note.revs[at + 1] : note.revs[at];
      if (!r) throw new Error(`nothing before revision ${String(args.commit)}`);
      note.content = r.content;
      saveRev(note, YOU, "restore", "Restored an earlier version");
      note.revs[0].rev.restoredFrom = r.rev.commit;
      const payload: NotesChangedPayload = {
        orgSlug: (args.orgSlug as string | null) ?? null,
        id: note.id,
        actor: YOU,
        kind: "restore",
      };
      events.emit(NOTES_CHANGED_EVENT, payload);
      return written(args, note, true, false, note.revs[0].rev.commit);
    },
  };
}
