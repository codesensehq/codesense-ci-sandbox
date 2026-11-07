// SPDX-License-Identifier: MIT
import { agentRefsInMarkdown, stripAgentRefs, type AgentRefAttrs } from "./agent-ref";
import { normalizeLine } from "./note-diff";

/** What the notes list shows under a note's title. */
export interface NoteSummary {
  /** The first lines of its text, joined ("Call the bank · pick up the drone"). */
  snippet: string;
  /** The agents its lines were handed to. */
  refs: AgentRefAttrs[];
}

/** A short preview of a note's body: its first few lines' words. The backend's
 *  records projection builds the same one for every note but the open one. */
export function summarize(markdown: string): NoteSummary {
  const lines = stripAgentRefs(markdown)
    .split("\n")
    .map((l) => normalizeLine(l))
    .filter((l) => l && !/^(```|~~~|---)/.test(l))
    .slice(0, 4);
  const snippet = Array.from(lines.join(" · ")).slice(0, 240).join("");
  return { snippet, refs: agentRefsInMarkdown(markdown) };
}
