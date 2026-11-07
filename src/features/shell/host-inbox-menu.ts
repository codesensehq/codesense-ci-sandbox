// SPDX-License-Identifier: MIT
import type {
  InboxMessageRow,
  InboxTeam,
  InboxView,
  RoleQueue,
  SessionListRow,
} from "@/tauri/commands";
import { openRoleEditor } from "@/features/agents/stores/role-editor-store";
import { ROLE_QUEUE_MENU_ITEMS } from "@/features/inbox/lib/role-queue-menu-items";
import { buildTeamKey } from "@/shared/lib/team-key";
import type { HostMenuItem, HostMenuRequest } from "./host-menu-context";

type StoredMessage = InboxMessageRow & { team?: InboxTeam };
export type InboxMenu = {
  items: HostMenuItem[];
  /** Architecture: navigation owns focus after the overlay closes. */
  choose: (id: string) => Promise<void | (() => void)>;
};
const separator = { id: "separator", separator: true };
const teamOf = (value: unknown): InboxTeam | undefined => {
  const t = value as Partial<InboxTeam> | undefined;
  return typeof t?.org === "string" && typeof t.team === "string"
    ? { org: t.org, team: t.team }
    : undefined;
};
const scope = (team?: InboxTeam) => team ?? {};
const runOf = (request: HostMenuRequest) => request.page?.run ?? request.run;
const notify = (request: HostMenuRequest, id: string, waitForPage = false) =>
  request.page?.enabled() ? request.page.notify(id, request.row ?? {}, waitForPage) : undefined;
const pageItems = (request: HostMenuRequest, items: readonly HostMenuItem[]) =>
  request.page?.enabled() ? [...items] : [];

async function conversationMessages(request: HostMenuRequest): Promise<StoredMessage[]> {
  const row = request.row ?? {};
  const queries = Array.isArray(row.conversations)
    ? row.conversations
    : [{ with: row.key ?? (row.role && `#${row.role}`), team: row.team }];
  const rows: StoredMessage[] = [];
  for (const query of queries) {
    const q = query as { with?: string; team?: InboxTeam };
    if (typeof q.with !== "string") continue;
    const team = teamOf(q.team) ?? request.team;
    let before: number | undefined;
    for (;;) {
      const result = (await runOf(request)("inbox", {
        ...scope(team),
        scope: "all",
        conversation: q.with,
        limit: 500,
        ...(before ? { before } : {}),
      })) as InboxView;
      rows.push(
        ...result.messages
          .filter((m) => !m.archivedAtMs && (m.toHuman || m.from === "human"))
          .map((m) => ({ ...m, team })),
      );
      const next = result.messages.at(-1)?.messageId;
      if (!result.hasMore || !next || next === before) break;
      before = next;
    }
  }
  return rows;
}
async function writeBatch(request: HostMenuRequest, verb: string, rows: StoredMessage[]) {
  const batches = new Map<string, StoredMessage[]>();
  for (const m of rows) {
    const key = JSON.stringify(m.team);
    const batch = batches.get(key) ?? [];
    batch.push(m);
    batches.set(key, batch);
  }
  for (const batch of batches.values())
    await runOf(request)(verb, {
      ...scope(batch[0].team),
      messageIds: batch.map((m) => m.messageId),
    });
}
async function terminalItem(request: HostMenuRequest, handle: string | undefined) {
  if (!handle || handle === "human" || handle.startsWith("#") || handle.startsWith("role:"))
    return undefined;
  const sessions = (await runOf(request)("sessions", {}).catch(() => [])) as SessionListRow[];
  const session = sessions.find((s) => s.handle === handle && s.state !== "exited");
  return (
    session && { id: "go-to-terminal", label: `Go to ${session.displayName || handle}’s terminal` }
  );
}
function commonChoose(request: HostMenuRequest, rows: StoredMessage[], handle?: string) {
  return async (id: string): Promise<void | (() => void)> => {
    if (id === "mark-unread") {
      // Architecture: establish the page's explicit hold before a persisted change can refresh it.
      await notify(request, id, true);
      await writeBatch(
        request,
        "markUnread",
        rows.filter((m) => m.toHuman),
      );
    } else if (id === "mark-read") {
      notify(request, id);
      await writeBatch(
        request,
        "markRead",
        rows.filter((m) => m.toHuman),
      );
    } else if (id === "go-to-terminal")
      return () => {
        void runOf(request)("gotoAgent", { agentId: handle });
      };
    else if (id === "archive" && !request.page?.enabled())
      await writeBatch(request, "archive", rows);
    else return () => notify(request, id);
  };
}

async function conversationMenu(request: HostMenuRequest): Promise<InboxMenu | null> {
  const rows = await conversationMessages(request);
  if (!rows.length) return null;
  const handle =
    rows.find((m) => m.toHuman && m.from !== "human")?.from ??
    rows.find((m) => m.to !== "human")?.to;
  const terminal = await terminalItem(request, handle);
  const self = rows.every((m) => m.from === "human" && m.to === "human");
  const readable = rows.filter((m) => m.toHuman && m.from !== "human");
  const items: HostMenuItem[] = pageItems(request, [
    { id: "open", label: "Open" },
    ...(!self ? [{ id: "reply", label: "Reply" }] : []),
  ]);
  if (items.length && (readable.length || terminal)) items.push(separator);
  if (readable.length)
    items.push(
      readable.some((m) => m.unread)
        ? { id: "mark-read", label: "Mark as read" }
        : { id: "mark-unread", label: "Mark as unread" },
    );
  if (terminal) items.push(terminal);
  if (items.length) items.push(separator);
  items.push({ id: "archive", label: "Archive conversation" });
  return { items, choose: commonChoose(request, rows, handle) };
}
// Architecture: Copy text extracts readable HTML content without executable or styling source.
function htmlMessageText(html: string): string {
  const body = new DOMParser().parseFromString(html, "text/html").body;
  body.querySelectorAll("script, style, template").forEach((node) => node.remove());
  body.querySelectorAll("br").forEach((node) => node.replaceWith("\n"));
  body
    .querySelectorAll(
      "p, div, section, article, header, footer, aside, h1, h2, h3, h4, h5, h6, li, pre, blockquote, tr",
    )
    .forEach((node) => node.append("\n\n"));
  return (body.textContent ?? "")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n[ \t]+/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

async function messageMenu(request: HostMenuRequest): Promise<InboxMenu | null> {
  const id = request.row?.messageId;
  if (typeof id !== "number") return null;
  const team = teamOf(request.row?.team) ?? request.team;
  const result = (await runOf(request)("inbox", {
    ...scope(team),
    scope: "all",
    before: id + 1,
    limit: 1,
  })) as InboxView;
  const stored = result.messages.find((m) => m.messageId === id);
  if (!stored) return null;
  const m = { ...stored, team };
  const received = m.from !== "human" && m.toHuman;
  const terminal = received ? await terminalItem(request, m.from) : undefined;
  // The page opens its tapback picker for React…; HTML messages take no reactions there.
  const items = pageItems(request, [
    ...(m.contentType !== "html" ? [{ id: "react", label: "React…" }] : []),
    ...(received ? [{ id: "reply", label: "Reply" }] : []),
  ]);
  items.push({ id: "copy-text", label: "Copy text" });
  if (received) {
    items.push(
      separator,
      m.unread
        ? { id: "mark-read", label: "Mark as read" }
        : { id: "mark-unread", label: "Mark as unread" },
    );
    if (terminal) items.push(terminal);
    if (m.contentType === "html") items.push({ id: "open-in-tab", label: "Open in tab" });
  }
  items.push(separator, { id: "archive", label: "Archive message" });
  const choose = commonChoose(request, [m], m.from);
  return {
    items,
    choose: async (id) => {
      if (id === "copy-text") {
        const text = m.contentType === "html" ? htmlMessageText(m.text) : m.text;
        await navigator.clipboard.writeText(text);
      } else if (id === "open-in-tab")
        return () => {
          void runOf(request)("openMessage", { ...scope(team), messageId: m.messageId });
        };
      else return choose(id);
    },
  };
}
async function attachmentMenu(request: HostMenuRequest): Promise<InboxMenu | null> {
  const path = request.row?.path;
  if (typeof path !== "string" || !path) return null;
  const team = teamOf(request.row?.team) ?? request.team;
  return {
    items: [
      { id: "open", label: "Open" },
      { id: "open-new-tab", label: "Open in new tab" },
      separator,
      { id: "copy-path", label: "Copy path" },
    ],
    choose: async (id) => {
      if (id === "copy-path") await navigator.clipboard.writeText(path);
      else
        return () => {
          void runOf(request)("openFile", { path, ...scope(team), newTab: id === "open-new-tab" });
        };
    },
  };
}
async function roleQueueMenu(request: HostMenuRequest): Promise<InboxMenu | null> {
  const role = request.row?.role;
  if (typeof role !== "string" || !role) return null;
  const team = teamOf(request.row?.team) ?? request.team;
  const result = (await runOf(request)("roles", { ...scope(team), role })) as {
    roles: RoleQueue[];
  };
  if (!result.roles.some((r) => r.role === role)) return null;
  const rows = await conversationMessages(request);
  const items: HostMenuItem[] = ROLE_QUEUE_MENU_ITEMS.filter(
    (item) => item.id !== "open-queue" || request.page?.enabled(),
  ).map((item) => ({ ...item }));
  if (rows.some((m) => m.toHuman && m.unread))
    items.push(separator, { id: "mark-read", label: "Mark as read" });
  items.push(separator, { id: "archive", label: "Archive conversation" });
  const choose = commonChoose(request, rows);
  return {
    items,
    choose: async (id) => {
      if (id === "edit-role" && team)
        return () => {
          void openRoleEditor(buildTeamKey(team.org, team.team), role);
        };
      return choose(id);
    },
  };
}
/** Architecture: one builder per inbox kind; all use the existing host menu renderer. */
export async function buildInboxMenu(request: HostMenuRequest): Promise<InboxMenu | null> {
  const builders = {
    conversation: conversationMenu,
    message: messageMenu,
    attachment: attachmentMenu,
    "role-queue": roleQueueMenu,
  };
  return builders[request.row?.kind as keyof typeof builders]?.(request) ?? null;
}
