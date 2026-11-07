import "../../../src/index.css";
import React from "react";
import { createRoot } from "react-dom/client";
import { HostMenuLayer } from "../../../src/features/shell/host-menu-layer";
import { VerbConfirmationProvider } from "../../../src/features/dashboard/components/verb-confirmation-provider";
import { SlotDashboardHost } from "../../../src/features/shell/slots/slot-dashboard-host";
import { installFakeTauri } from "./fake-tauri";
import { SLOT_GRANT_CEILINGS } from "../../../src/features/dashboard/lib/verb-registry.generated";
import { useOrgStore } from "../../../src/shared/stores/org-store";

const team = { org: "fixture", team: "dev" };
const otherTeam = { org: "fixture", team: "web" };
const crossTeam = new URLSearchParams(location.search).has("cross-team");
const api = installFakeTauri({
  kind: "builtin",
  effectiveGrant: crossTeam ? [...SLOT_GRANT_CEILINGS["right-pane"]] : undefined,
  sessions: [
    {
      id: "Mia",
      sessionId: "mia-session",
      handle: "Mia",
      displayName: "Mia",
      rowNames: { primary: "Mia" },
      state: "running",
      alive: true,
      kind: "cli",
      launchTeamKey: crossTeam ? "fixture:web" : "fixture:dev",
      team: crossTeam ? otherTeam : team,
      role: "docs",
    },
  ],
});
if (crossTeam)
  useOrgStore.setState({
    orgs: [{ slug: "fixture", teams: [{ name: "dev" }, { name: "web" }] }] as never,
  });
const rows = [
  {
    messageId: 1,
    conversation: "Mia",
    from: "Mia",
    to: "human",
    text: "A plain message to select",
    primaryFile: "notes/a.md",
    onClick: { type: "goto-agent", agentId: "Mia" },
    contentType: "plain",
  },
  {
    messageId: 2,
    conversation: "Mia",
    from: "human",
    to: "Mia",
    text: "My answer",
    contentType: "plain",
  },
  {
    messageId: 3,
    conversation: "Mia",
    from: "Mia",
    to: "human",
    text: "<p id='message-body'>Nested message body</p><style>.secret { color: red }</style><script>window.hiddenCopy = 1</script><template>Hidden template</template><p>Second<br>line.</p><input id='message-field' value='Nested field'>",
    contentType: "html",
  },
  {
    messageId: 4,
    conversation: "#docs",
    from: "human",
    to: "role:docs",
    text: "Role work",
    contentType: "plain",
  },
].map((m) => ({
  ...m,
  team: crossTeam && m.conversation === "Mia" ? otherTeam : team,
  fromName: m.from,
  toName: m.to,
  createdAtMs: 1700000000000 + m.messageId * 1000,
  toHuman: m.to === "human",
  unread: false,
  kind: "update",
  inReplyTo: null,
  archivedAtMs: null as number | null,
}));
api.inboxRows = rows;
const base = api.__TAURI_INTERNALS__.invoke;
api.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
  if (command === "mint_frame") {
    const record = await base(command, args);
    if (args.args.message) {
      record.message = args.args.message;
      record.parentFrame = args.args.parentFrame;
      record.kind = "message";
      record.grant.principal = "Mia";
    }
    return record;
  }
  if (command === "agent_read_roles")
    return { roles: [{ role: "docs", roleName: "Docs", registrations: [] }] };
  if (command === "get_human_status") return { status: "online", note: null };
  if (command === "agent_read_inbox" || command === "list_inbox_rollup") {
    api.calls.push([command, args]);
    const q = args.query ?? {};
    const all = rows.filter(
      (m) =>
        !m.archivedAtMs && (!args.team || JSON.stringify(m.team) === JSON.stringify(args.team)),
    );
    const messages = all
      .filter(
        (m) =>
          (!q.conversation || m.conversation === q.conversation) &&
          (!q.before || m.messageId < q.before) &&
          (q.scope !== "to-human" || m.toHuman),
      )
      .sort((a, b) => b.messageId - a.messageId);
    const conversations = [
      ...new Set(all.map((m) => JSON.stringify([m.team, m.conversation]))),
    ].map((key) => {
      const ms = all.filter((m) => JSON.stringify([m.team, m.conversation]) === key);
      const last = ms.at(-1)!;
      return {
        with: last.conversation,
        team: last.team,
        unread: ms.filter((m) => m.toHuman && m.unread).length,
        messageCount: ms.length,
        lastAt: last.createdAtMs,
        lastMessageId: last.messageId,
        lastPreview: last.text,
      };
    });
    return {
      messages: messages.slice(0, q.limit ?? 500).map((m) =>
        command === "list_inbox_rollup"
          ? {
              ...m,
              id: m.messageId,
              toRole: m.to.startsWith("role:") ? m.to.slice(5) : null,
              onClick: m.onClick ? { type: "goto-agent", agent_id: m.onClick.agentId } : null,
            }
          : m,
      ),
      conversations,
      hasMore: messages.length > (q.limit ?? 500),
      humanUnread: all.filter((m) => m.toHuman && m.unread).length,
    };
  }
  if (
    [
      "mark_inbox_messages_read",
      "mark_inbox_messages_unread",
      "mark_inbox_messages_archived",
    ].includes(command)
  ) {
    api.calls.push([command, args]);
    let changed = 0;
    for (const m of rows.filter(
      (m) =>
        m.toHuman &&
        args.messageIds.includes(m.messageId) &&
        (!args.team || JSON.stringify(args.team) === JSON.stringify(m.team)),
    )) {
      const unread = command === "mark_inbox_messages_unread";
      if (m.unread !== unread) changed++;
      m.unread = unread;
      if (command === "mark_inbox_messages_archived") m.archivedAtMs = Date.now();
    }
    api.emit("agent-inbox-changed", {});
    return changed;
  }
  return base(command, args);
};
createRoot(document.getElementById("root")!).render(
  <VerbConfirmationProvider>
    <HostMenuLayer>
      <div style={{ width: 900, height: 700 }}>
        <SlotDashboardHost
          slotId="right-pane"
          contentRoot="/fixture"
          visible
          entry={{
            id: "inbox",
            title: "Inbox",
            path: "/fixture/inbox-pane.html",
            panesDir: "/fixture",
            scope: "built-in",
            dashboardRef: { scope: "built-in", id: "inbox" },
          }}
        />
      </div>
    </HostMenuLayer>
  </VerbConfirmationProvider>,
);
