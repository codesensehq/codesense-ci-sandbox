// SPDX-License-Identifier: MIT
/**
 * The link between a line of a note and the agent it was handed to. In the note's
 * markdown it is an ordinary link with an `@` label and a `cs:agent:` target (the `cs:` link
 * scheme notes already use for `cs:note:` and `cs:story:`):
 *
 *     - [ ] Remove agent accounts from the setup flow [@Jimmy](cs:agent:jimmy)
 *
 * so the file reads as plain text anywhere (and renders as a link in any markdown
 * viewer), while the notes pane draws it as a chip carrying that agent's live status.
 * The target is the agent session's handle, URI-encoded (handles carry `#` and `/`).
 */

export const AGENT_REF_SCHEME = "cs:agent:";

export interface AgentRefAttrs {
  /** The agent session's handle. */
  handle: string;
  /** What the agent was called when the line was handed to it. */
  name: string;
}

/** The markdown for a handoff link. Brackets in the name are dropped so the label
 *  can't end the link early. */
export function formatAgentRef({ handle, name }: AgentRefAttrs): string {
  const label = name.replace(/[[\]]/g, "").trim() || handle;
  return `[@${label}](${AGENT_REF_SCHEME}${encodeURIComponent(handle)})`;
}

/** The handle a `cs:agent:` href names, or null when the href is anything else. */
export function agentHandleOfHref(href: string | null | undefined): string | null {
  if (!href?.startsWith(AGENT_REF_SCHEME)) return null;
  const encoded = href.slice(AGENT_REF_SCHEME.length);
  if (!encoded) return null;
  try {
    return decodeURIComponent(encoded);
  } catch {
    return null;
  }
}

/** The name a link label carries, without its leading `@`. */
export function agentNameOfLabel(label: string): string {
  return label.trim().replace(/^@/, "").trim();
}

const AGENT_REF_IN_MARKDOWN = /\[@([^\]]*)\]\(cs:agent:([^)\s]+)\)/g;

/** Every handoff link in a note's markdown body, in order (a handle once). */
export function agentRefsInMarkdown(markdown: string): AgentRefAttrs[] {
  const seen = new Set<string>();
  const out: AgentRefAttrs[] = [];
  for (const m of markdown.matchAll(AGENT_REF_IN_MARKDOWN)) {
    const handle = agentHandleOfHref(`${AGENT_REF_SCHEME}${m[2]}`);
    if (!handle || seen.has(handle)) continue;
    seen.add(handle);
    out.push({ handle, name: agentNameOfLabel(m[1]) || handle });
  }
  return out;
}

/** The markdown with every handoff link removed (for comparing lines by their text). */
export function stripAgentRefs(markdown: string): string {
  return markdown.replace(AGENT_REF_IN_MARKDOWN, "");
}
