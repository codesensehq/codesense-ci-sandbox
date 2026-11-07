// SPDX-License-Identifier: MIT
import { listScratchSpace, readFile } from "@/tauri/commands";

/**
 * The words the notes pane prefills when it hands a note to an agent come from the
 * person's own prompts (the all-orgs `prompts/` folder of their scratch space), never
 * from the app: edit the file and the next "Organize" or routine starts from it.
 *
 *   prompts/organize-notes.md  — "Organize with an agent"
 *   prompts/tidy-notes.md      — "Set up a routine"
 *
 * A missing file leaves the prompt to the person.
 */
export const NOTE_PROMPT_FILES = {
  organize: "prompts/organize-notes.md",
  routine: "prompts/tidy-notes.md",
} as const;

export type NotePromptKind = keyof typeof NOTE_PROMPT_FILES;

/** A prompt file's text without its YAML front matter. */
export function promptBody(text: string): string {
  const fm = /^---\r?\n[\s\S]*?\r?\n---\r?\n?/;
  return text.replace(fm, "").trim();
}

/** The person's prompt for `kind`, or "" when they keep none. */
export async function readNotePrompt(kind: NotePromptKind): Promise<string> {
  try {
    const listing = await listScratchSpace(null);
    const file = listing.allOrgs.find(
      (f) => f.kind === "prompt" && f.logical === NOTE_PROMPT_FILES[kind],
    );
    if (!file) return "";
    return promptBody(await readFile(file.path));
  } catch {
    return "";
  }
}

/** Which note an agent is handed: its id and org for the notes tools, and its file
 *  (only used when the note is in no org, where the notes tools can't reach it). */
export interface NoteRef {
  id: string | null;
  org: string | null;
  path: string | null;
}

/** The line that tells an agent which note it is working on. A note in an org is
 *  named for the notes tools, so the agent's edits go through them: each one is
 *  saved to the note's history under the agent's name and shows in the open note at
 *  once, rather than being written behind the open editor's back. */
export function noteContextLine(title: string, note: NoteRef): string {
  if (note.id && note.org) {
    return (
      `The note: “${title}” (note id ${note.id} in org ${note.org}). ` +
      `Read it with notes_read and change it with notes_write (pass that id and org), ` +
      `not by editing its file.`
    );
  }
  return note.path ? `The note: “${title}” (${note.path}).` : `The note: “${title}”.`;
}
