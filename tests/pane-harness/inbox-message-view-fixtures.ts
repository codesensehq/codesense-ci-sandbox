// SPDX-License-Identifier: MIT
import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import type { InboxMessageRow } from "../../src/tauri/commands";
import type { MountedPane, PaneFixtures } from "./harness";

export const INBOX_PANE = "src/features/shell/right-pane/__fixtures__/inbox-pane.html";
const team = { org: "fixture", team: "dev" };
export const builder = {
  id: "builder",
  sessionId: "builder",
  handle: "builder",
  displayName: "Builder",
  rowNames: { primary: "Builder", secondary: null, full: "Builder" },
  state: "running",
  alive: true,
  team,
};
const today = new Date();
const at = (day: number, hour: number, minute: number) =>
  new Date(today.getFullYear(), today.getMonth(), today.getDate() + day, hour, minute).getTime();
const incoming = { from: "builder", fromName: "Builder" };
export const outgoing = {
  from: "human",
  fromName: "You",
  to: "builder",
  toName: "Builder",
  toHuman: false,
  conversation: "builder",
};
const row = (id: number, text: string, overrides: Partial<InboxMessageRow>) =>
  inboxFixtureRow(id, text, { contentType: "markdown", unread: false, ...overrides }, team);
export const GROUPING_TEXT =
  "Grouping is in. This is the rule:\n\n```js\nconst GROUP_MS = 5 * 60_000;\nconst grouped = prev?.from === m.from && m.at - prev.at < GROUP_MS;\n```\n\nFull diff is attached.";
const thread = [
  row(
    1,
    "Picked up the Inbox. Plan for today:\n\n1. Bubbles: theirs left, mine right\n2. Five-minute grouping and day labels\n3. Needs-you edge and the collapsed **To** chip",
    { ...incoming, createdAtMs: at(-1, 16, 48) },
  ),
  row(2, "I'll keep paging and the jump-to-latest dock as they are.", {
    ...incoming,
    createdAtMs: at(-1, 16, 49),
  }),
  row(3, "Sounds right. Keep the right-click menus working.", {
    ...outgoing,
    createdAtMs: at(-1, 17, 2),
  }),
  row(4, GROUPING_TEXT, {
    ...incoming,
    createdAtMs: at(0, 9, 31),
    primaryFile: "design/ux-brief.md",
    reactions: [{ emoji: "👍", count: 1, reactors: ["human"] }],
  }),
  row(5, "Nice. Compare against the [native chat captures](https://example.com) before you push.", {
    ...outgoing,
    createdAtMs: at(0, 9, 40),
  }),
  row(6, "And post light + dark for review.", { ...outgoing, createdAtMs: at(0, 9, 41) }),
  row(7, "Captures are up. One open choice before I wire the composer:", {
    ...incoming,
    createdAtMs: at(0, 14, 10),
    unread: true,
  }),
  row(
    8,
    "**Should the composer keep the To chip collapsed when you reply inside a thread?** Collapsed means *To Builder ▾*, and the full picker opens from it.",
    { ...incoming, createdAtMs: at(0, 14, 11), unread: true, kind: "needs-you" },
  ),
];
/** Scrolls the log so message `id`'s bubble starts `offset`px below the log's top. */
export async function scrollBubbleTo(pane: MountedPane, id: number, offset: number) {
  const body = pane.frame.locator(`[data-message-id="${id}"] .bd`);
  await body.evaluate((el, offset) => {
    const log = document.getElementById("log")!;
    log.scrollTop += el.getBoundingClientRect().top - log.getBoundingClientRect().top - offset;
  }, offset);
  return body;
}
export function messageViewFixtures(unread = true): PaneFixtures {
  return {
    width: 900,
    height: 900,
    theme: { id: "codesense", scheme: "light" },
    sessions: [builder],
    inbox: unread
      ? thread
      : thread.map((r) => ({
          ...r,
          message: { ...r.message, unread: false },
          conversation: { ...r.conversation, unread: false },
        })),
  };
}
export const MARKDOWN = `## Markdown message

Here's **bold**, *italic* and \`inline code\`, plus a [link](https://example.com).

- A bullet list
- With a second item
  - and a nested one
    1. deeper numbered

1. Numbered
2. Steps

- [ ] open task
- [x] done task

> A quoted message.

---

\`\`\`js
const status = await codesense.sessions();
\`\`\`

| Story | Status |
|---|---|
| Inbox | in progress |
| Agents | designing |
`;
