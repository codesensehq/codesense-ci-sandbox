// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { buildInboxMenu } from "./host-inbox-menu";
import type { HostMenuRequest } from "./host-menu-context";
vi.mock("@/features/agents/stores/role-editor-store", () => ({ openRoleEditor: vi.fn() }));
const team = { org: "fixture", team: "dev" };
const message = {
  messageId: 10,
  from: "Mia",
  fromName: "Mia",
  to: "human",
  toHuman: true,
  text: "Stored body",
  unread: false,
  conversation: "Mia",
  contentType: "markdown",
};
const notify = vi.fn();
const run = vi.fn();
function request(row: Record<string, unknown>, callback = true): HostMenuRequest {
  return {
    type: "inbox",
    row,
    team,
    teamKey: "fixture:dev",
    frame: document.createElement("iframe"),
    origin: "http://frame.test",
    anchorRect: { x: 0, y: 0, width: 0, height: 0 },
    run,
    page: { enabled: () => callback, notify, run },
  };
}
beforeEach(() => {
  notify.mockReset();
  run.mockReset();
  run.mockImplementation(async (verb) =>
    verb === "inbox"
      ? { messages: [message], hasMore: false }
      : verb === "sessions"
        ? [{ handle: "Mia", displayName: "Mia", state: "running" }]
        : verb === "roles"
          ? { roles: [{ role: "docs" }] }
          : { marked: 1 },
  );
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: vi.fn().mockResolvedValue(undefined) },
  });
});
it("conversation resolves stored mail and establishes its unread hold before writing", async () => {
  const req = request({ kind: "conversation", key: "Mia", conversations: [{ with: "Mia", team }] });
  const menu = await buildInboxMenu(req);
  expect(menu?.items.filter((i) => !i.separator).map((i) => i.label)).toEqual([
    "Open",
    "Reply",
    "Mark as unread",
    "Go to Mia’s terminal",
    "Archive conversation",
  ]);
  await menu?.choose("mark-unread");
  expect(notify).toHaveBeenCalledWith("mark-unread", req.row, true);
  expect(notify.mock.invocationCallOrder[0]).toBeLessThan(run.mock.invocationCallOrder.at(-1)!);
  expect(run).toHaveBeenLastCalledWith("markUnread", { ...team, messageIds: [10] });
  const open = await menu?.choose("open");
  expect(notify).toHaveBeenCalledTimes(1);
  if (open) open();
  expect(notify).toHaveBeenLastCalledWith("open", req.row, false);
});
it("message reads the exact stored id, copies its body, and defers navigation until close", async () => {
  const menu = await buildInboxMenu(
    request({ kind: "message", messageId: 10, text: "Tag body is not used" }),
  );
  expect(run).toHaveBeenCalledWith("inbox", { ...team, scope: "all", before: 11, limit: 1 });
  expect(navigator.clipboard.writeText).not.toHaveBeenCalled();
  await menu?.choose("copy-text");
  expect(navigator.clipboard.writeText).toHaveBeenCalledWith("Stored body");
  const go = await menu?.choose("go-to-terminal");
  expect(run).not.toHaveBeenCalledWith("gotoAgent", expect.anything());
  if (go) go();
  expect(run).toHaveBeenLastCalledWith("gotoAgent", { agentId: "Mia" });
});
it("sent and self messages omit reply and incoming read actions", async () => {
  run.mockResolvedValue({
    messages: [{ ...message, from: "human", toHuman: false }],
    hasMore: false,
  });
  const menu = await buildInboxMenu(request({ kind: "message", messageId: 10 }));
  expect(menu?.items.filter((i) => !i.separator).map((i) => i.label)).toEqual([
    "React…",
    "Copy text",
    "Archive message",
  ]);
  run.mockResolvedValue({ messages: [{ ...message, from: "human", to: "human" }], hasMore: false });
  const self = await buildInboxMenu(request({ kind: "conversation", key: "human" }));
  expect(self?.items.map((i) => i.id)).not.toContain("reply");
  expect(self?.items.map((i) => i.id)).not.toContain("mark-unread");
});
it("attachment retains its team and reads no clipboard until activated", async () => {
  const other = { org: "other", team: "docs" };
  const menu = await buildInboxMenu(
    request({ kind: "attachment", path: "notes/a.md", team: other }),
  );
  expect(run).not.toHaveBeenCalled();
  const open = await menu?.choose("open-new-tab");
  if (open) open();
  expect(run).toHaveBeenCalledWith("openFile", { path: "notes/a.md", ...other, newTab: true });
  await menu?.choose("copy-path");
  expect(navigator.clipboard.writeText).toHaveBeenCalledWith("notes/a.md");
});
it("role queue reuses opening actions and omits page-only actions without a listener", async () => {
  const menu = await buildInboxMenu(request({ kind: "role-queue", role: "docs" }));
  expect(menu?.items.slice(0, 2).map((i) => i.label)).toEqual(["Open queue", "Edit role…"]);
  const noPage = await buildInboxMenu(request({ kind: "role-queue", role: "docs" }, false));
  expect(noPage?.items.map((i) => i.id)).not.toContain("open-queue");
  const noReply = await buildInboxMenu(request({ kind: "message", messageId: 10 }, false));
  expect(noReply?.items.map((i) => i.id)).not.toContain("reply");
});
it("unknown rows and missing stored messages fall back to the dashboard", async () => {
  expect(await buildInboxMenu(request({ kind: "unknown" }))).toBeNull();
  run.mockResolvedValue({ messages: [] });
  expect(await buildInboxMenu(request({ kind: "message", messageId: 10 }))).toBeNull();
});

it("does not write unread state until the page confirms its hold", async () => {
  let accept!: () => void;
  notify.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        accept = resolve;
      }),
  );
  const menu = await buildInboxMenu(request({ kind: "message", messageId: 10 }));
  const choosing = menu!.choose("mark-unread");
  await Promise.resolve();
  expect(run).not.toHaveBeenCalledWith("markUnread", expect.anything());
  accept();
  await choosing;
  expect(run).toHaveBeenCalledWith("markUnread", { ...team, messageIds: [10] });
});

it("copies readable HTML paragraphs without script, style or template contents", async () => {
  run.mockResolvedValueOnce({
    messages: [
      {
        ...message,
        contentType: "html",
        text: "<style>.secret { color: red }</style><p>First <b>paragraph</b>.</p><script>window.secret = 1</script><template>Hidden template</template><p>Second<br>line.</p>",
      },
    ],
    hasMore: false,
  });
  const menu = await buildInboxMenu(request({ kind: "message", messageId: 10 }));
  await menu?.choose("copy-text");
  expect(navigator.clipboard.writeText).toHaveBeenCalledWith("First paragraph.\n\nSecond\nline.");
});

it("puts React… first on a message and hands it to the page after the menu closes", async () => {
  const req = request({ kind: "message", messageId: 10 });
  const menu = await buildInboxMenu(req);
  expect(menu?.items[0]).toEqual({ id: "react", label: "React…" });
  expect(menu?.items.filter((i) => !i.separator).map((i) => i.label)).toEqual([
    "React…",
    "Reply",
    "Copy text",
    "Mark as unread",
    "Go to Mia’s terminal",
    "Archive message",
  ]);
  const react = await menu?.choose("react");
  expect(notify).not.toHaveBeenCalled();
  if (react) react();
  expect(notify).toHaveBeenCalledWith("react", req.row, false);
  const noPage = await buildInboxMenu(request({ kind: "message", messageId: 10 }, false));
  expect(noPage?.items.map((i) => i.id)).not.toContain("react");
  run.mockImplementation(async (verb) =>
    verb === "inbox" ? { messages: [{ ...message, contentType: "html" }], hasMore: false } : [],
  );
  const html = await buildInboxMenu(request({ kind: "message", messageId: 10 }));
  expect(html?.items.map((i) => i.id)).not.toContain("react");
});
