---
name: cited-retrieval
description: Retrieve durable team evidence with citations for a review or a context search. Use for evidence gathering, without changing messages, stories, or review receipts.
---

# Cited retrieval

Input: `org`, `team`, an inclusive `since_ms` / `until_ms` window, and
optional search text. Return evidence, not a narrative. This is the single
retrieval path for the reviewer and a future `search_brain` entry point;
callers use this contract directly rather than making a second collector.

Every item has this shape (optional keys are omitted):

```json
{
  "source": "commit|run|advance|story|message|inbox",
  "org": "owner/context-repository",
  "team": "team-name",
  "at": "2026-10-20T14:00:00Z",
  "path": "stories/example.md#L23",
  "run_id": "recorded-run-id",
  "sha": "full-commit-sha",
  "id": 42,
  "text": "What the durable record actually says"
}
```

`source` is one enum value, not the pipe-separated example. `at` is RFC3339.
`text` is a short faithful excerpt or description. A commit needs `sha`; a
run needs `run_id`; an advance needs `run_id` or a corroborating `path#Lnn`;
a story needs `path#Lnn`; a message or inbox item needs its durable numeric
`id`. Message items also carry `record: "agent-update"` to distinguish
`list_agent_updates` IDs from inbox IDs. Never invent locators or timestamps.

Return `{items, event_count, from_sha, to_sha, gaps}`. `event_count` counts
distinct window events actually used, not citations or supporting story
files. Deduplicate commits by SHA, runs/advances by their kind and returned
record fields including `ts`, updates by `record,id`, and inbox items by ID.
`from_sha` is the first parent of the oldest window commit; `to_sha` is the
newest window commit. With no window commits use the current scoped history
head for both ends. With no parent use the root commit and disclose the
root boundary in `gaps`. Never make up a range when history is unavailable.

1. Scope `query_context` with `where.orgs: [org]`, `teams: [team]`,
   `layers: ["team"]`. Page `nextCursor` until exhausted. Use it to locate
   relevant stories and documents; read located files with normal read-only
   file tools and record real line numbers. Metadata `mtime` is milliseconds,
   not proof of a status transition. Durable transition evidence comes from
   the run store. Leave `reviews/` out unless the caller deliberately asks
   for a specific prior review; those reviews are never source evidence for
   new claims.
2. Call `context_repo_history` with explicit `org`, `team`, `branch: "main"`
   and `limit: 500`. Pass each `nextCursor` back as `cursor` until a commit's
   `committedAt * 1000` is older than `since_ms`. Filter both window bounds
   yourself. Passing `team` is essential: it fills `files`. Keep the derived
   `agent` from the existing Actor-* / Co-Authored-By interpretation if
   present; an absent agent stays unattributed. Inspect a cited commit with
   `git show` in the `repoPath` the tool returns, reading only this team's
   files. Exclude review/receipt-only commits and review-file changes from
   the evidence; reviews must not summarize themselves.
3. Call `list_run_events` with `since_ms`, `until_ms`, `kinds:
   ["outcome", "advance"]`, `limit: 500`. Its window uses `ts` (epoch ms),
   never `ended_at`. The store spans teams: keep only events whose `story_id`
   resolves unambiguously to this org/team's story via `query_context`.
   Omit unattributable, storyless, or cross-team events. Read more evidence
   with `read_run` when necessary. When a response reaches the limit,
   subdivide the time window and deduplicate inclusive boundaries. If even
   a one-millisecond window reaches the cap, report a retrieval gap rather
   than claiming complete coverage.
4. Read `list_agent_updates` with explicit `org`, `team`, `limit: 500`.
   Filter `createdAtMs` to the window; normalize as `source: "message"`,
   `record: "agent-update"`, `id`. If this bounded update read stops before
   the window, disclose a gap. Do not confuse an update with an inbox ID.

   For inbox questions, call `inbox_list` with the following arguments,
   replacing the example team with the input `org` and `team`:

   ```json
   {"team":{"org":"owner/context-repository","team":"team-name"},"scope":"to-human","kind":"needs-you","limit":500}
   ```

   Then read the same team's retained inbox, including human replies:

   ```json
   {"team":{"org":"owner/context-repository","team":"team-name"},"scope":"all","limit":500}
   ```

   On this second read, omit `kind`, `agentId`, `sessionId` and `role`.
   An answer stays under 64 KB: while `hasMore` is true and the oldest
   listed row is still inside the window, read again with `before` set to
   the last listed `messageId` and combine the pages. Bodies over 2 KB come
   as `textPreview` with `truncated: true`; add `"include":["text"]` when
   the full text is evidence.
   Check its horizon before filtering any rows: the store caps the whole
   inbox at 500 **before** recipient and kind filters. If the first read's
   `total` (the read without `before`) is 500 and `oldest.createdAtMs >= since_ms`, where
   `oldest` is the oldest retained row, return a retrieval gap: the retained
   horizon does not establish coverage of the start of the window. Do not
   publish a complete review. A short or empty needs-you response proves
   nothing about coverage.

   Keep the retained replies for resolution before applying the candidate
   questions' time window. A question is answered when a retained row has
   `from === "human"`, `inReplyTo === question.messageId`, and was written
   by `until_ms`. When `inReplyToTeam` is present, both its org and team must
   match the question's originating team; absence means this queried team.
   A reply to another team's same numeric ID does not resolve this question.
   Read/unread is not answered/unanswered: keep a read question if no human
   reply resolves it. Filter questions by `createdAtMs` to the window,
   discard archived/withdrawn or resolved ones, and normalize the remaining
   unresolved questions as `source: "inbox"`. These reads mark nothing read.
5. Return only verifiable items. Statements about why a choice was made need
   an explicit recorded reason, not an inference from a diff. Story context
   can corroborate an event without increasing `event_count`.

Retrieval is read-only: never call `read_messages` (it marks mail read),
`signal_progress` (it is transient), or a write tool. Report unavailable
sources in `gaps`; don't silently turn a failed read into a quiet day.
