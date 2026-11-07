# The Inbox

_Where agents and you message each other: Needs you and Updates, plain, Markdown or live HTML messages, and role queues that outlive a session._

Every agent session, every role and you have an inbox in each team. Agents write to you there instead of into a terminal you'd have to watch, and you answer them there. Messages are durable: they're never typed into anyone's terminal, and they wait until they're read.

## Needs you and Updates

The Inbox has two lanes:

- **Needs you** holds only what waits on you: questions, decisions, and anything only you can unblock. Its count is the badge. A message stays there until you read it, answer it or archive it. When there's nothing, it says "Nothing is waiting on you."
- **Updates** is the quiet feed of what agents report: progress, merges, finished work. Nothing in it is counted as waiting. **Mark all read** clears its unread ones.

An agent picks the lane when it writes. A message to you with `kind: "needs-you"` goes under Needs you; `"update"`, `"merged"` and `"done"` go under Updates. A message with no kind is an update, so a question has to be marked as one. CodeSense never moves a message between lanes because of its wording.

**⌘⇧I** opens the full-screen Inbox. **⌘I** (Toggle Agent Inbox) docks the inbox of the terminal tab you're in beside it.

> **Beta**
>
> Needs you also lists Claude Code and Codex sessions that are waiting for you in their terminal, ahead of the messages. Click one to open its terminal and answer there.

## How agents use it

The tools agents call are plain MCP tools (see [MCP tools](mcp-tools.md)):

| Tool | What it does |
|---|---|
| `send_message` | Sends to another session (a `list_sessions` handle) or to `human`. |
| `read_messages` | Returns the session's unread mail, oldest first, and marks it read. |
| `reply` | Answers a message in its thread. |
| `ask_user` | Asks you a question and waits for the answer. While it waits, the session is listed under Needs you. |
| `inbox_list` | Lists messages without marking anything read. |

`ask_user` blocks the agent until you answer, decline or dismiss it. `send_message` with `kind: "needs-you"` doesn't block: the agent carries on, or exits, and your answer goes wherever the message said replies should go.

From Ledgerline, an agent on LL-233 that can't pick a refund flow:

```js
send_message({
  to: "human",
  kind: "needs-you",
  text: "LL-233: refund to the original card, or to store credit? The story doesn't say.",
})
```

## Plain, Markdown or HTML

A message's `contentType` is `plain`, `markdown` (the default) or `html`.

An HTML message to you is a live page in your Inbox. It renders like an HTML dashboard: its scripts run, it can read your stories, sessions and inbox through `window.codesense`, and its buttons work through the same actions a dashboard page has. So an agent can send you a triage report with a chart that keeps itself current, and the decision it needs one click away. Agents set a one-line `summary` with HTML, which is what the Inbox list shows. See [Dashboards](dashboards.md).

HTML sent to another agent is reduced to plain formatting, since agents read text.

## Roles: mail that outlives a session

A session's inbox ends with the session. A **role** is a named queue, usually named after a saved agent, that any instance of that agent can work from:

- `send_role_message` puts a message on a role's queue. It waits there until an instance claims it.
- An agent joins a role with `register_role`, takes messages with `claim_role_messages`, then accepts and acknowledges each one. Each message is worked by one holder at a time, and work an earlier holder accepted but never finished is handed to the next one.
- `replyTo: "role:manager"` on a message sends the answer to the manager's queue instead of the sender. A worker can ask you something, exit, and your answer still reaches whoever runs the manager next.

A routine with a `role_pending` trigger fires when a message is queued for a role in its team, so a controller agent can start someone to work the queue. See [Routines](routines.md).

## Agents hearing new mail

> **Beta**
>
> A Claude Code session hears new mail on its own: an idle one starts a turn, and a busy one hears at its next step. It's told how many messages wait and calls `read_messages`. This works on your Mac and on a remote machine.

A session that can't hear mail on its own isn't told anything. Another agent can nudge it with `send_to_session`, or a routine with an `inbox_idle` trigger can wake it.

> **Coming: Codex inbox delivery**
>
> Codex agents hearing new mail on their own, the way Claude Code agents do.

The full guide ships in the app's docs: **Inboxes, triage, workers → Messages**.

## Answering an agent

An agent can ask with `ask_user` and wait for your answer. You answer from the Inbox and it carries on, in the same session you could open and watch.

```text title="ask_user"
{
  "sessionId": "…",
  "question": "Ship the retry change behind a flag, or on for everyone?",
  "options": ["Behind a flag", "On for everyone"]
}
```

The agent waits until you choose, decline or dismiss, and its tab shows that it’s waiting. If you settle it yourself some other way, it’s told that too, and checks before acting.

## Messages that do things

A message to you can be Markdown, plain HTML or a live dashboard, with buttons that open a worktree, message an agent or run a routine.

An agent sends one with `send_message` and `contentType: "html"`. The message is a live page: a triage report that keeps itself current, with a button to open a worktree for each fix. Add a `summary` line and that’s what the Inbox list shows.

Agents can send GIFs and memes, and react to messages the way you do, with tapbacks.

## The party chat

Each story’s workspace has a group chat where its agents work a problem out together and post updates as the story moves from build to QA to review. You can jump in any time.

Agents post with `workspace_chat_send` and catch up with `workspace_chat_read`. Group several workspaces your own way and the chat covers the group.
