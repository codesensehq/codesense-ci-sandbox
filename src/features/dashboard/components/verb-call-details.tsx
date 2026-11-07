// SPDX-License-Identifier: MIT
import { useState } from "react";

const LABELS: Record<string, string> = {
  target: "Target",
  kind: "Kind",
  storyId: "Story",
  branch: "Branch",
  repo: "Repository",
  baseRepo: "Base repository",
  path: "Path",
  agent: "Agent",
  machineId: "Machine",
  headed: "Headed (false runs in background)",
  sessionMode: "Session mode",
  initialPrompt: "Initial prompt",
  reviewPrompt: "Review prompt",
  operationId: "Operation ID",
  linkedStoryId: "Linked story",
  pullNumber: "Pull request",
  session: "Session",
  sessionId: "Session ID",
  handle: "Handle",
  title: "Title",
  epic: "Epic",
  org: "Organization",
  team: "Team",
  to: "To",
  text: "Text",
  message: "Message",
  prompt: "Prompt",
  messageId: "Message ID",
  inReplyTo: "In reply to",
  replyTo: "Replies go to",
  attachFile: "Attached file",
  contentType: "Content type",
  summary: "Summary",
  key: "Key",
  removeWorktree: "Delete worktree",
  discardUnsaved: "Discard unsaved changes",
};

function entries(value: unknown, path: string[] = []): [string, string][] {
  if (value !== null && typeof value === "object" && !Array.isArray(value)) {
    const fields = Object.entries(value);
    if (fields.length) return fields.flatMap(([key, child]) => entries(child, [...path, key]));
  }
  const label = path.map((key) => LABELS[key] ?? key).join(" · ") || "Arguments";
  return [
    [label, typeof value === "string" ? value : (JSON.stringify(value, null, 2) ?? String(value))],
  ];
}

function ArgumentValue({ label, value }: { label: string; value: string }) {
  const [expanded, setExpanded] = useState(false);
  const long = value.length > 240;
  return (
    <>
      <span className="whitespace-pre-wrap break-words">
        {long && !expanded ? `${value.slice(0, 240)}…` : value}
      </span>
      {long && (
        <button
          type="button"
          className="ml-2 underline"
          aria-label={`${expanded ? "Collapse" : "Expand"} ${label}`}
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded ? "Show less" : "Show more"}
        </button>
      )}
    </>
  );
}

/** Enumerate the whole call so new arguments cannot silently disappear from approvals. */
export function VerbCallDetails({ args }: { args: unknown }) {
  return (
    <section aria-label="Details" className="space-y-2 text-sm">
      <h3 className="font-medium">Details</h3>
      <dl className="max-h-56 space-y-2 overflow-auto rounded-md bg-muted p-3">
        {entries(args).map(([label, value], index) => (
          <div key={index}>
            <dt className="font-medium">{label}</dt>
            <dd>
              <ArgumentValue label={label} value={value} />
            </dd>
          </div>
        ))}
      </dl>
    </section>
  );
}
