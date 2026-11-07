// SPDX-License-Identifier: MIT
import { recipePane } from "../../../../../tests/support/builtin-pane-recipe";
import {
  bootPaneDoc,
  listing as paneListing,
  stubTauri as sharedStubTauri,
  el,
  type,
  pick,
  type PaneDoc,
} from "./pane-seam-helpers";
vi.mock("@/features/dashboard/lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { flushFrame, hasLiveFrames } from "@/features/dashboard/lib/frame-lifecycle";
import { frameFixture, frameEventClass } from "../../../../../tests/support/fixtures/frame";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  readFileSync,
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  renameSync,
  realpathSync,
  existsSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Seam: a pane runs behind the REAL `window.codesense` shim (lifted from viewer.rs)
// and talks over postMessage to the REAL right-pane host, pane body, HtmlViewer,
// bridge, action gate, stores and Tauri command wrappers. Only the Tauri IPC and
// its events are stubbed, standing in for the backend (whose path validation and
// byte-preserving write have their own Rust tests).
const { tauriInvoke, listeners } = vi.hoisted(() => ({
  tauriInvoke: vi.fn(),
  listeners: new Map<string, Set<(e: { payload: unknown }) => void>>(),
}));
vi.mock("@tauri-apps/api/core", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tauri-apps/api/core")>();
  return { ...actual, invoke: (cmd: string, args?: unknown) => tauriInvoke(cmd, args) };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, cb: (e: { payload: unknown }) => void) => {
    const set = listeners.get(event) ?? new Set();
    set.add(cb);
    listeners.set(event, set);
    return () => set.delete(cb);
  }),
  emit: vi.fn(async () => {}),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("@/features/viewer/components/scratch-note-panel", () => ({
  ScratchNotePanel: () => <div data-testid="scratch-note-panel" />,
}));

import {
  SHORTCUT_ACTIONS,
  allShortcutActions,
  dashboardCommandActionId,
  findShortcutAction,
  resolveBindings,
  setDynamicShortcutActions,
} from "@/shared/lib/shortcuts/registry";
import {
  runDashboardCommand,
  syncDashboardCommands,
} from "@/features/dashboard/lib/dashboard-commands";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import { matchShortcut } from "@/shared/hooks/use-shortcut-handler";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { revealSlot, resolveSlot, resolveInboxDashboard } from "@/features/shell/slots/reveal-slot";
function activateInbox(target?: { conversation?: string; team?: { org: string; team: string } }) {
  revealSlot("right-pane", {
    surface: "inbox",
    ...(target
      ? {
          intent: {
            conversation: target.conversation,
            org: target.team?.org ?? useOrgStore.getState().activeOrgSlug,
            team: target.team?.team ?? useOrgStore.getState().activeTeamName,
            focusComposer: true,
          },
        }
      : {}),
  });
}
const resolveTarget = (target?: { conversation?: string; team?: { org: string; team: string } }) =>
  resolveSlot("right-pane", {
    surface: "inbox",
    intent: target?.team ? { org: target.team.org, team: target.team.team } : undefined,
  }).dashboard?.panelId ?? null;

import { ScratchNoteWidthDivider } from "@/features/viewer/components/scratch-note-width-divider";
import { InboxRail } from "@/features/inbox/components/inbox-rail";
import { TooltipProvider } from "@/components/ui/tooltip";
import { RightPaneLayout } from "@/features/viewer/components/right-pane-layout";
import { usePaneStore } from "./pane-store";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { registerTerminalSection } from "@/features/viewer/lib/terminal-section-registry";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import type { PaneListing } from "@/tauri/commands";
import { AGENT_INBOX_CHANGED_EVENT, PANE_OPEN_EVENT } from "@/tauri/events";
import type { Org } from "@/shared/types/domain";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";

import { getPanel } from "../panel-registry";
import { defaultRightPaneLayout, INBOX_PANEL_ID } from "../right-pane-layout";
import { PanePanelsMount } from "./use-register-pane-panels";

const PORT = 9322;
const PaneMessageEvent = frameEventClass(PORT);
const ORIGIN = `http://f-test.localhost:${PORT}`;
const ORG = "acme/api";
const TEAM = "backend";
const ROOT = "/h/users/42/orgs/acme/api";
const CONTENT = `${ROOT}/${TEAM}/content`;
const SOURCE_PANEL = `pane:${ORG}:${TEAM}/inbox`;
const PANEL = INBOX_PANEL_ID;
const STORY = `${CONTENT}/e/SW-1.md`;

const PANE = readFileSync(resolve(__dirname, "../__fixtures__/inbox-pane.html"), "utf8");
/** The pane's declared commands, as discovery reads its `codesense:commands` meta. */
const PANE_COMMANDS = JSON.parse(
  /name="codesense:commands"\s+content='([^']*)'/.exec(PANE)![1],
) as import("@/tauri/commands").DashboardCommand[];
let messages: Record<string, any>[];
let sessions: Record<string, any>[];
let appState: string;
let archiveFails: boolean;
let sendInvalid: boolean;
let panePresent = true;
let inboxPreview = false;
function profileChoice() {
  const ref = listing().panes[0]?.dashboardRef;
  return {
    id: "default",
    name: "Default",
    posture: "",
    slots: { "right-pane": inboxPreview && ref ? [ref] : [] },
    cliLayout: "default",
    chrome: {},
  };
}
function setPreview(enabled: boolean) {
  inboxPreview = enabled;
  useDashboardProfileStore.setState({ org: ORG, profile: profileChoice() });
}
let crossTeam = false;
let teamScopedCrossTeam = false;
let narrowedInboxGrant = false;
let crossSurface = "human-inbox";
let rollup: Record<string, any>[] = [];
let archiveFailTeam: string | null = null;
const PRIVATE_PANEL = `pane:private:${ORG}/inbox`;
const row = (id: number, from = "Mia", to: string | null = "human", extra = {}) => ({
  messageId: id,
  from,
  fromName: from,
  to,
  toName: to,
  text: "Message " + id,
  createdAtMs: id * 1000,
  inReplyTo: null,
  toHuman: to === "human",
  unread: true,
  contentType: "markdown",
  ...extra,
});

function inboxView(rows: Record<string, any>[], query: Record<string, any> = {}) {
  const keyed = rows.map((m): Record<string, any> => {
    let root = m;
    const seen = new Set();
    while (root.inReplyTo && !seen.has(root)) {
      seen.add(root);
      const parent = rows.find(
        (r) =>
          (r.messageId ?? r.id) === root.inReplyTo &&
          JSON.stringify(r.team) === JSON.stringify(root.team),
      );
      if (!parent) break;
      root = parent;
    }
    const conversation =
      m.conversation ??
      (root.from === "human"
        ? root.toRole
          ? "#" + root.toRole
          : root.delivery || root.to?.startsWith("role:")
            ? "#" + root.to.replace(/^role:/, "").replace(/^#/, "")
            : root.to
        : root.from);
    return { ...m, conversation };
  });
  const summaries = new Map<string, any>();
  for (const m of keyed.filter(
    (m) => !m.archivedAtMs && (m.from === "human" || m.to === "human"),
  )) {
    const team = m.team ?? { org: ORG, team: TEAM };
    const key = JSON.stringify([team, m.conversation]);
    const c = summaries.get(key) ?? {
      with: m.conversation,
      team,
      unread: 0,
      messageCount: 0,
      lastAt: 0,
    };
    c.messageCount++;
    if (m.to === "human" && m.from !== "human" && m.unread) c.unread++;
    if (m.createdAtMs >= c.lastAt)
      Object.assign(c, {
        lastAt: m.createdAtMs,
        lastMessageId: m.messageId ?? m.id,
        lastPreview: m.summary || m.text.split("\n")[0],
      });
    summaries.set(key, c);
  }
  const filtered = keyed
    .filter(
      (m) =>
        (!query.scope ||
          query.scope === "all" ||
          (query.scope === "to-agents" ? !m.toHuman : m.toHuman)) &&
        (!query.kind || m.kind === query.kind) &&
        (!query.conversation || m.conversation === query.conversation) &&
        (!query.before || (m.messageId ?? m.id) < query.before),
    )
    .sort((a, b) => (b.messageId ?? b.id) - (a.messageId ?? a.id));
  return {
    messages: filtered.slice(0, query.limit ?? 500),
    conversations: [...summaries.values()],
    hasMore: filtered.length > (query.limit ?? 500),
    humanUnread: keyed.filter((m) => m.toHuman && m.unread && !m.archivedAtMs).length,
  };
}

let inheritedInbox = false;
function listing(): PaneListing {
  const base = paneListing({
    org: ORG,
    team: inheritedInbox ? "platform" : TEAM,
    root: ROOT,
    id: "inbox",
    title: "Inbox",
    privateCopy: crossTeam && !teamScopedCrossTeam,
    surface: crossSurface,
    present: panePresent,
  });
  // The commands declaration is what syncs the Inbox's commands to the host registry.
  return {
    ...base,
    ...(inheritedInbox ? { contentRoot: CONTENT, chain: [TEAM, "platform"] } : {}),
    panes: [
      ...base.panes,
      ...(inheritedInbox && base.panes[0]
        ? [
            {
              ...base.panes[0],
              id: "alternate",
              title: "Alternate Inbox",
              path: `${ROOT}/platform/panes/alternate.html`,
              panelId: `pane:${ORG}:platform/alternate`,
              dashboardRef: { scope: "team" as const, org: ORG, team: "platform", id: "alternate" },
            },
          ]
        : []),
    ].map((pane) => ({
      ...pane,
      declaration: {
        slots: [],
        surface: "human-inbox",
        capabilities: [],
        commands: PANE_COMMANDS,
        badge: /name="codesense:badge"\s+content="live"/.test(PANE) ? "live" : undefined,
      },
    })),
  };
}

let hubRev = 0;
const hubSubscriptions = new Map<string, { viewKey: string; rev: number }>();
function projectedCounts() {
  const all = crossTeam ? rollup : messages;
  const recipients: Record<string, number> = {};
  for (const m of all)
    if (m.unread && !m.archivedAtMs && (m.to !== "human" || m.from !== "human"))
      recipients[m.to] = (recipients[m.to] ?? 0) + 1;
  return {
    id: "counts:test",
    rowType: "counts",
    team: { org: ORG, team: TEAM },
    humanUnread: recipients.human ?? 0,
    recipients,
    needsYouBySender: {},
    sessions: Object.fromEntries(
      sessions.map((row) => [
        row.handle,
        {
          unread: recipients[row.handle] ?? 0,
          groupUnread: row.groupUnread ?? 0,
          groupReadId: row.groupReadId ?? 0,
        },
      ]),
    ),
  };
}
function inboxChanged() {
  hubRev++;
  for (const [subscriptionId, sub] of hubSubscriptions) {
    for (const cb of listeners.get("codesense:subscription-delta") ?? [])
      cb({
        payload: {
          subscriptionId,
          baseRev: sub.rev,
          delta: {
            topic: "inbox",
            viewKey: sub.viewKey,
            rev: hubRev,
            upserts: [projectedCounts()],
            removes: [],
          },
        },
      });
    sub.rev = hubRev;
  }
  for (const cb of listeners.get(AGENT_INBOX_CHANGED_EVENT) ?? []) cb({ payload: {} });
}
const mintedFrames = new Map<string, import("@/tauri/commands").FrameRecord>();
let childSequence = 0;
let uniquePaneFrames = false;
let paneSequence = 0;
function stubTauri(cmd: string, args: Record<string, any> | undefined): unknown {
  return sharedStubTauri(cmd, args, PORT, inboxIpc);
}
function inboxIpc(cmd: string, args: Record<string, any> | undefined): unknown {
  if (cmd === "mint_frame") {
    const request = args!.args as import("@/tauri/commands").MintFrameArgs;
    const frame = frameFixture(request, PORT);
    frame.kind = "team";
    frame.grant = {
      ...frame.grant,
      // The listed dashboard's key, as Rust mints it, so its commands reach this frame.
      dashboard: listing().panes[0]?.dashboardRef
        ? dashboardRefKey(listing().panes[0].dashboardRef!)
        : frame.grant.dashboard,
      scope: crossTeam ? "open" : "team",
      team: { org: ORG, team: inheritedInbox ? "platform" : TEAM },
    };
    frame.effectiveGrant =
      crossTeam && !narrowedInboxGrant ? ["inbox:read:all-teams"] : ["inbox:read:team"];
    if (uniquePaneFrames && !request.parentFrame) {
      frame.frameId = `pane-${++paneSequence}`;
      frame.origin = `http://f-${frame.frameId}.localhost:${PORT}`;
      frame.token = `secret-${frame.frameId}`;
      frame.grant.frameId = frame.frameId;
    }
    if (request.parentFrame) {
      frame.frameId = `child-${++childSequence}`;
      frame.origin = `http://f-${frame.frameId}.localhost:${PORT}`;
      frame.token = `secret-${frame.frameId}`;
      frame.grant = {
        ...frame.grant,
        frameId: frame.frameId,
        principal: "Mia",
        team: request.message!,
        slot: "message",
      };
    }
    mintedFrames.set(frame.origin, frame);
    return Promise.resolve(frame);
  }
  if (cmd === "resolve_frame_grant") {
    const frame = mintedFrames.get(args!.origin);
    if (
      !frame ||
      frame.token !== args!.token ||
      (frame.parentFrame &&
        ![...mintedFrames.values()].some((r) => r.frameId === frame.parentFrame))
    )
      throw new Error("denied: frame identity");
    return Promise.resolve(frame.grant);
  }
  if (cmd === "release_frame") {
    for (const [origin, record] of mintedFrames)
      if (record.frameId === args!.frameId || record.parentFrame === args!.frameId)
        mintedFrames.delete(origin);
    return Promise.resolve(null);
  }
  switch (cmd) {
    case "check_path_exists":
      return useOrgStore
        .getState()
        .orgs.some((org) =>
          org.teams.some(
            (team) => pathToTeamContentDirSync(org.org_root, team.name) === args?.path,
          ),
        );
    case "get_http_api_port":
      return PORT;
    case "avatar_packs_list_for_team":
      return { packs: [], agent_avatars: {}, warnings: [] };
    case "list_inbox_rollup":
      return {
        humanUnread: projectedCounts().humanUnread,
        messages: [...new Set(rollup.map((m) => JSON.stringify(m.team)))]
          .flatMap(
            (team) => inboxView(rollup.filter((m) => JSON.stringify(m.team) === team)).messages,
          )
          .map(({ messageId: _id, toHuman: _human, ...message }) => message),
        conversations: inboxView(rollup).conversations,
        roles: [{ role: "docs", holder: null, pending: 0, note: "" }],
      };
    case "list_session_inbox":
      return { handle: "Mia", roles: [], messages: [] };
    case "read_dashboard_profile":
      return profileChoice();
    case "list_panes":
      return listing();
    case "live_hub_unsubscribe":
      return null;
    case "bridge_doc_context":
      return {
        org: ORG,
        team: crossTeam && !teamScopedCrossTeam ? null : TEAM,
        layer: crossTeam && !teamScopedCrossTeam ? "private" : "team",
        contentRoot: CONTENT,
      };
    case "live_hub_subscribe": {
      if (args?.topic === "inbox" && args?.subscriptionId)
        hubSubscriptions.set(args.subscriptionId, { viewKey: args.viewKey, rev: hubRev });
      return {
        topic: args?.topic,
        viewKey: args?.viewKey,
        rev: args?.topic === "inbox" ? hubRev : 0,
        snapshot:
          args?.topic === "inbox"
            ? [projectedCounts()]
            : (inboxIpc("agent_read_sessions", {}) as Record<string, unknown>[]).map((row) => ({
                id: row.handle,
                ...row,
              })),
      };
    }
    case "agent_read_sessions":
      return sessions;
    case "agent_definitions_prepare":
      return { topic: "agentDefinitions", viewKey: "all", rev: 0, snapshot: [] };
    case "agent_read_roles":
      return { roles: [{ role: "docs", roleName: "Docs", registrations: [] }] };
    case "get_human_status":
      return { status: "online", note: null };
    case "set_human_status":
      return { status: args?.status, note: args?.note };
    case "agent_read_inbox":
      return inboxView(
        crossTeam
          ? rollup
              .map(
                (m): Record<string, any> => ({
                  ...m,
                  messageId: m.id,
                  to: m.toRole ? "role:" + m.toRole : m.to,
                  toHuman: m.to === "human",
                }),
              )
              .filter(
                (m) =>
                  !args?.team || (m.team.org === args.team.org && m.team.team === args.team.team),
              )
          : messages,
        args?.query,
      );
    case "mark_inbox_messages_unread":
    case "mark_inbox_messages_read": {
      let count = 0;
      const unread = cmd === "mark_inbox_messages_unread";
      for (const m of crossTeam
        ? rollup.filter((m) => m.team.org === args?.team.org && m.team.team === args?.team.team)
        : messages)
        if (
          (m.toHuman ?? m.to === "human") &&
          args?.messageIds.includes(m.messageId ?? m.id) &&
          m.unread !== unread
        ) {
          m.unread = unread;
          count++;
        }
      inboxChanged();
      return count;
    }
    case "mark_inbox_messages_archived": {
      if (archiveFails || args?.team.team === archiveFailTeam) throw new Error("Store unavailable");
      let count = 0;
      for (const m of crossTeam
        ? rollup.filter((m) => m.team.org === args?.team.org && m.team.team === args?.team.team)
        : messages) {
        if (
          (m.toHuman ?? m.to === "human") &&
          !m.archivedAtMs &&
          args?.messageIds.includes(m.messageId ?? m.id)
        ) {
          m.archivedAtMs = Date.now();
          m.unread = false;
          count++;
        }
      }
      inboxChanged();
      return count;
    }
    case "reply_as_human":
    case "send_inbox_as_human":
      return sendInvalid
        ? { status: "invalid", message: "Recipient ended" }
        : { status: "sent", messageId: 50 };
    case "local_profile_status":
      return { active: null };
    case "load_app_state":
      return appState;
    case "save_app_state":
      appState = args?.state;
      return null;
    case "bridge_resolve_open_target":
      return { path: STORY, org: ORG, team: TEAM, contentRoot: CONTENT };
    default:
      return null;
  }
}

const onFileSelect = vi.fn();
const mockFetch = vi.fn();
globalThis.fetch = mockFetch;

beforeEach(() => {
  inheritedInbox = false;
  liveHub.dispose();
  mintedFrames.clear();
  childSequence = 0;
  uniquePaneFrames = false;
  paneSequence = 0;
  listeners.clear();
  hubSubscriptions.clear();
  hubRev = 0;
  onFileSelect.mockClear();
  messages = [row(1), row(2, "Ori")];
  sessions = [
    { handle: "Mia", displayName: "Mia", state: "running", role: "docs" },
    { handle: "Ori", displayName: "Ori", state: "idle" },
  ];
  appState = "{}";
  archiveFails = false;
  sendInvalid = false;
  panePresent = true;
  crossTeam = false;
  teamScopedCrossTeam = false;
  narrowedInboxGrant = false;
  crossSurface = "human-inbox";
  setPreview(false);
  rollup = [];
  archiveFailTeam = null;
  usePaneStore.setState({ pendingIntents: {}, reloadKeys: {} });
  useAgentListProjectionStore.setState({ sessions: [] });
  tauriInvoke.mockReset();
  tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) =>
    stubTauri(cmd, args),
  );
  mockFetch.mockImplementation(async () => ({ ok: true, status: 200, statusText: "" }));
  useOrgStore.setState({
    orgs: [
      {
        slug: ORG,
        org_root: ROOT,
        teams: [{ name: TEAM }, { name: "web" }, { name: "empty" }],
      } as unknown as Org,
    ],
    activeOrgSlug: ORG,
    activeTeamName: TEAM,
  });
  useViewerStore.setState({
    settings: { ...useViewerStore.getState().settings },
    rightPaneLayout: defaultRightPaneLayout(),
    rightPaneOpen: false,
    maximizedPanel: {},
    openTabs: {},
    activeTab: {},
  });
});

const docs: PaneDoc[] = [];
afterEach(async () => {
  cleanup();
  for (const p of docs.splice(0)) await p.win.happyDOM.close();
});

it("does not leak the candidate focus wrapper through happy-dom's shared element prototype", async () => {
  const originalFocus = HTMLElement.prototype.focus;
  const frame = document.createElement("iframe");
  frame.src = `${ORIGIN}/pane/candidate?codesenseCandidate=1`;
  const candidate = bootPaneDoc(frame, "<textarea></textarea>");
  try {
    expect(HTMLElement.prototype.focus).toBe(originalFocus);
  } finally {
    await candidate.win.happyDOM.close();
  }
});

async function openInbox(
  waitForRows = true,
  html = PANE,
  beforeReady?: () => Promise<void>,
  repoPath = "",
): Promise<PaneDoc> {
  const panel = PANEL;
  render(
    <TooltipProvider>
      <PanePanelsMount />
      <RightPaneLayout repoPath={repoPath} onClose={() => {}} onFileSelect={onFileSelect} />
    </TooltipProvider>,
  );
  await waitFor(() => expect(getPanel(panel)).toBeDefined());
  await act(async () => useViewerStore.getState().revealPanelById(panel));
  const frame = () => {
    const f = screen.getByTestId("pane-body").querySelector("[data-pane-current] iframe");
    if (!f) throw new Error("no pane iframe");
    return f;
  };
  await waitFor(() => expect(frame().getAttribute("src")).toContain("/pane/"));
  await beforeReady?.();
  const current = frame() as HTMLIFrameElement;
  const pane = bootPaneDoc(current, html, mintedFrames.get(new URL(current.src).origin));
  docs.push(pane);
  if (waitForRows)
    await waitFor(() => expect(pane.doc.querySelectorAll(".ib-row")).toHaveLength(2));
  return pane;
}

const calls = (cmd: string) => tauriInvoke.mock.calls.filter((c) => c[0] === cmd);
async function openThread(p: PaneDoc, name = "Mia") {
  const row = [...p.doc.querySelectorAll<HTMLElement>(".ib-row")].find((r) =>
    r.textContent?.includes(name),
  )!;
  await act(async () => row.click());
}

async function menuAction(p: PaneDoc, itemId: string, row: Record<string, unknown>) {
  await act(async () =>
    p.win.dispatchEvent(
      new p.win.MessageEvent("message", {
        source: p.win.parent,
        data: { type: "codesense:menu-action", itemId, row },
      }),
    ),
  );
}

describe("human inbox pane through the real bridge", () => {
  it("Reply from an older message uses that message's own reply target and provenance", async () => {
    messages.push(row(3, "Mia", "human", { inReplyTo: 1, replyTo: "Ori" }));
    const p = await openInbox();
    await openThread(p);
    await act(async () =>
      p.win.dispatchEvent(
        new p.win.MessageEvent("message", {
          source: p.win.parent,
          data: {
            type: "codesense:menu-action",
            itemId: "reply",
            row: { kind: "message", messageId: 1 },
          },
        }),
      ),
    );
    expect(el(p, "recipient").textContent).toBe("Mia");
    type(p, "Reply to the first message");
    fireEvent.click(el(p, "send"));
    await waitFor(() =>
      expect(calls("reply_as_human").at(-1)?.[1]).toMatchObject({
        messageId: 1,
        text: "Reply to the first message",
      }),
    );
  });

  it("archives a sent message through the existing saved hiding path across refresh", async () => {
    messages.push(row(3, "human", "Mia", { inReplyTo: 1 }));
    const p = await openInbox();
    await openThread(p);
    await waitFor(() => expect(el(p, "message-3")).not.toBeNull());
    await act(async () =>
      p.win.dispatchEvent(
        new p.win.MessageEvent("message", {
          source: p.win.parent,
          data: {
            type: "codesense:menu-action",
            itemId: "archive",
            row: { kind: "message", messageId: 3 },
          },
        }),
      ),
    );
    await waitFor(() => expect(p.doc.getElementById("message-3")).toBeNull());
    const prefs = Object.values(JSON.parse(appState).dashboardPrefs)[0] as {
      data: { hiddenSent: number[] };
    };
    expect(prefs.data.hiddenSent).toContain(3);
    await act(async () => inboxChanged());
    expect(p.doc.getElementById("message-3")).toBeNull();
    expect(messages.find((m) => m.messageId === 1)?.archivedAtMs).toBeFalsy();
  });

  it("holds explicit unread across refresh, passive bottom scroll and hide/show until Jump to latest", async () => {
    const p = await openInbox();
    await openThread(p);
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    const row = JSON.parse(p.doc.querySelector(".ib-row")!.getAttribute("data-codesense-row")!);
    expect(row.kind).toBe("conversation");
    const m = messages.find((m) => m.from === "Mia")!;
    await act(async () => {
      p.win.dispatchEvent(
        new p.win.MessageEvent("message", {
          source: p.win.parent,
          data: {
            type: "codesense:menu-action",
            itemId: "mark-unread",
            row: { kind: "message", messageId: m.messageId, team: { org: ORG, team: TEAM } },
          },
        }),
      );
      await p.win.codesense.act("markUnread", { messageIds: [m.messageId] });
    });
    await waitFor(() => expect(calls("mark_inbox_messages_unread")).toHaveLength(1));
    await act(async () => {
      el(p, "log").dispatchEvent(new p.win.Event("scroll") as unknown as Event);
      inboxChanged();
    });
    await act(async () =>
      p.win.dispatchEvent(
        new p.win.MessageEvent("message", {
          source: p.win.parent,
          data: { type: "codesense:visibility", visible: false },
        }),
      ),
    );
    await act(async () =>
      p.win.dispatchEvent(
        new p.win.MessageEvent("message", {
          source: p.win.parent,
          data: { type: "codesense:visibility", visible: true },
        }),
      ),
    );
    await waitFor(() => expect(el(p, "latest-count").textContent).toBe("Marked unread"));
    expect(m.unread).toBe(true);
    expect(calls("mark_inbox_messages_read")).toHaveLength(1);
    await act(async () => el(p, "latest").click());
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(2));
    expect(m.unread).toBe(false);
  });

  it("preserves an explicit unread hold when restoring a stacked thread", async () => {
    const p = await openInbox();
    p.win.innerWidth = 600;
    p.win.dispatchEvent(new p.win.Event("resize") as never);
    await openThread(p);
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    await menuAction(p, "mark-unread", { kind: "message", messageId: 1 });
    await act(async () => p.win.codesense.act("markUnread", { messageIds: [1] }));
    const toggle = () =>
      act(async () =>
        p.win.dispatchEvent(
          new p.win.MessageEvent("message", {
            source: p.win.parent,
            data: { type: "codesense:command", id: "toggle-list" },
          }),
        ),
      );
    await toggle();
    expect(el(p, "inbox").dataset.view).toBe("list");
    await toggle();
    expect(el(p, "inbox").dataset.view).toBe("thread");
    expect(el(p, "latest-count").textContent).toBe("Marked unread");
    expect(messages.find((m) => m.messageId === 1)?.unread).toBe(true);
    expect(calls("mark_inbox_messages_read")).toHaveLength(1);
    await menuAction(p, "open", { kind: "conversation", key: "Mia" });
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(2));
  });

  it("tags the thread header with its conversation and the terminal chip with its own target", async () => {
    messages[0].onClick = { type: "goto-agent", agentId: "Ori" };
    const p = await openInbox();
    await openThread(p);
    const tag = (node: Element) => JSON.parse(node.getAttribute("data-codesense-row")!);
    expect(tag(el(p, "thread-head"))).toMatchObject({ kind: "conversation", key: "Mia" });
    expect(tag(p.doc.querySelector("#thread-avatar .av")!)).toEqual({
      kind: "agent",
      agentId: "Mia",
    });
    const chip = [...p.doc.querySelectorAll("#message-1 .ib-chip")].find((n) =>
      n.textContent?.startsWith("Go to"),
    )!;
    expect(tag(chip)).toEqual({ kind: "agent", agentId: "Ori" });
    fireEvent.click(el(p, "new-message"));
    expect(el(p, "thread-head").hasAttribute("data-codesense-row")).toBe(false);
  });

  it("keeps the team-safe team copy single-team while using the app-scope overlay", async () => {
    const p = await openInbox();
    expect(el(p, "inbox").dataset.mode).toBeUndefined();
    expect(el(p, "team-filter").hidden).toBe(true);
    expect(calls("agent_read_inbox").length).toBeGreaterThan(0);
    expect(
      screen.getByTestId(`${PANEL}-fullscreen-shell`).getAttribute("data-maximize-scope"),
    ).toBe("app");
    const read = vi.spyOn(p.win.codesense, "inboxRollup");
    await act(async () => inboxChanged());
    await new Promise((r) => setTimeout(r, 80));
    expect(read).not.toHaveBeenCalled();
  });

  it("delivers a conversation intent through ready and focuses the real pane composer", async () => {
    revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Mia", team: TEAM } });
    const p = await openInbox();
    await waitFor(() =>
      expect(p.doc.querySelector('[id="conversation-Mia"]')?.getAttribute("aria-selected")).toBe(
        "true",
      ),
    );
    expect(p.doc.activeElement).toBe(el(p, "draft"));
  });

  it.each(["shortcut", "rail"])("opens the real pane from the %s entry point", async (entry) => {
    setPreview(true);
    render(
      <TooltipProvider>
        <PanePanelsMount />
        <InboxRail
          tabId="mia-tab"
          sessionId="Mia"
          teamKey={`${ORG}:${TEAM}`}
          composeRequest={0}
          narrow
        />
        <RightPaneLayout repoPath="" onClose={() => {}} onFileSelect={onFileSelect} />
      </TooltipProvider>,
    );
    await waitFor(() => expect(getPanel(PANEL)).toBeDefined());
    if (entry === "shortcut")
      SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-inbox-rollup")!.run();
    else fireEvent.click(await screen.findByTestId("inbox-rail-open-inbox-pane"));
    await waitFor(() => expect(useViewerStore.getState().rightPaneOpen).toBe(true));
    await waitFor(() =>
      expect(
        screen.getByTestId("pane-body").querySelector("iframe")?.getAttribute("src"),
      ).toContain("/pane/"),
    );
    const frame = screen.getByTestId("pane-body").querySelector("iframe")!;
    const p = bootPaneDoc(frame, PANE);
    docs.push(p);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(2));
    if (entry === "rail") {
      await waitFor(() =>
        expect(el(p, "conversation-Mia").getAttribute("aria-selected")).toBe("true"),
      );
      expect(p.doc.activeElement).toBe(el(p, "draft"));
      expect(el(p, "log").querySelector(".ib-target")).toBeNull();
    } else {
      act(() => SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-inbox-rollup")!.run());
      expect(useViewerStore.getState().rightPaneOpen).toBe(false);
      act(() => SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-inbox-rollup")!.run());
      expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    }
  });

  it("keeps only the newest intent across a reload before ready and buffers it for a late subscriber exactly once", async () => {
    uniquePaneFrames = true;
    revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Mia" } });
    revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Ori" } });
    // The pane never reports ready here, so the old frame stays until the new one goes live.
    const delayed = PANE.replace("cs.ready?.();", "").replace(
      "cs.onIntent(applyIntent);",
      `window.intentDeliveries = [];
       window.subscribeIntent = () => cs.onIntent(intent => {
         window.intentDeliveries.push(intent);
         applyIntent(intent);
       });`,
    );
    let initialFrame!: HTMLIFrameElement;
    const p = await openInbox(true, delayed, async () => {
      initialFrame = screen.getByTestId("pane-body").querySelector("iframe")!;
      act(() =>
        usePaneStore.setState((s) => ({
          reloadKeys: { ...s.reloadKeys, [PANEL]: (s.reloadKeys[PANEL] ?? 0) + 1 },
        })),
      );
      await waitFor(() => {
        const frame = screen
          .getByTestId("pane-body")
          .querySelector<HTMLIFrameElement>("[data-pane-current] iframe");
        expect(frame).not.toBe(initialFrame);
        expect(frame?.getAttribute("src")).toContain("/pane/");
      });
      // Double-buffered save: the old frame stays attached until the candidate presents.
      expect(initialFrame.isConnected).toBe(true);
    });
    await waitFor(() => expect(initialFrame.isConnected).toBe(false));
    await waitFor(() => expect(usePaneStore.getState().pendingIntents[PANEL]).toBeUndefined());
    expect(el(p, "inbox").dataset.view).toBe("list");
    p.win.eval("window.subscribeIntent()()");
    expect(p.win.eval("window.intentDeliveries")).toEqual([{ conversation: "Ori" }]);
    expect(el(p, "conversation-Ori").getAttribute("aria-selected")).toBe("true");
    await openThread(p, "Mia");
    p.win.eval("window.subscribeIntent()");
    expect(p.win.eval("window.intentDeliveries")).toHaveLength(1);
    expect(el(p, "conversation-Mia").getAttribute("aria-selected")).toBe("true");
    p.win.eval(
      "codesense.onFlush(() => new Promise(resolve => { window.finishFlush = resolve; }))",
    );
    const previousFrame = screen.getByTestId("pane-body").querySelector("iframe")!;
    act(() =>
      usePaneStore.setState((s) => ({
        reloadKeys: { ...s.reloadKeys, [PANEL]: (s.reloadKeys[PANEL] ?? 0) + 1 },
      })),
    );
    await waitFor(() => {
      const frame = screen
        .getByTestId("pane-body")
        .querySelector<HTMLIFrameElement>("[data-pane-current] iframe");
      expect(frame).not.toBe(previousFrame);
      expect(frame?.getAttribute("src")).toContain("/pane/");
    });
    expect(previousFrame.isConnected).toBe(true);
    const outgoing = previousFrame.closest("[data-pane-retained]")!;
    expect(outgoing).not.toBeNull();
    expect(outgoing).not.toHaveAttribute("aria-hidden", "true");
    expect(outgoing).not.toHaveAttribute("inert");
    expect(previousFrame.style.visibility).not.toBe("hidden");
    expect(previousFrame.inert).toBeFalsy();
    const frame = screen
      .getByTestId("pane-body")
      .querySelector<HTMLIFrameElement>("[data-pane-current] iframe")!;
    const reloaded = bootPaneDoc(frame, PANE, mintedFrames.get(new URL(frame.src).origin));
    docs.push(reloaded);
    await waitFor(() => expect(reloaded.doc.querySelectorAll(".ib-row")).toHaveLength(2));
    await waitFor(() => {
      expect(previousFrame.style.visibility).toBe("hidden");
      expect(previousFrame.inert).toBe(true);
      expect(p.win.eval("typeof window.finishFlush")).toBe("function");
    });
    expect(previousFrame.isConnected).toBe(true);
    await act(async () => p.win.eval("window.finishFlush()"));
    await waitFor(() => expect(previousFrame.isConnected).toBe(false));
    expect(el(reloaded, "inbox").dataset.view).toBe("list");
  });

  it("targets a message through pane_open and keeps its tint through read redraws", async () => {
    const p = await openInbox();
    const scroll = vi.spyOn(p.win.HTMLElement.prototype, "scrollIntoView");
    for (const cb of listeners.get(PANE_OPEN_EVENT) ?? [])
      cb({
        payload: {
          panelId: SOURCE_PANEL,
          path: "",
          intent: { messageId: 1, conversation: "Ori", team: TEAM },
        },
      });
    await waitFor(() => expect(el(p, "message-1").classList.contains("ib-target")).toBe(true));
    expect(
      scroll.mock.calls.some(([opts]) => typeof opts === "object" && opts.block === "center"),
    ).toBe(true);
    expect(el(p, "conversation-Mia").getAttribute("aria-selected")).toBe("true");
    expect(p.doc.activeElement).toBe(el(p, "draft"));
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    expect(el(p, "message-1").classList.contains("ib-target")).toBe(true);
    await waitFor(
      () => expect(el(p, "message-1").classList.contains("ib-target-fading")).toBe(true),
      { timeout: 1800 },
    );
    await waitFor(() => expect(el(p, "message-1").classList.contains("ib-target")).toBe(false));
  });

  it("does not read an intent-opened thread while the pane is hidden", async () => {
    const p = await openInbox();
    act(() => {
      useViewerStore.setState({ rightPaneOpen: false });
    });
    await new Promise((r) => setTimeout(r, 30));
    act(() => usePaneStore.getState().setIntent(PANEL, { conversation: "Mia" }));
    await new Promise((r) => setTimeout(r, 30));
    expect(el(p, "conversation-Mia").getAttribute("aria-selected")).toBe("false");
    expect(calls("mark_inbox_messages_read")).toHaveLength(0);
    act(() => useViewerStore.setState({ rightPaneOpen: true }));
    await waitFor(() =>
      expect(el(p, "conversation-Mia").getAttribute("aria-selected")).toBe("true"),
    );
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
  });

  it("falls back quietly for unknown messages, ignores another team, and addresses a new conversation", async () => {
    const p = await openInbox();
    act(() => revealSlot("right-pane", { dashboard: PANEL, intent: { messageId: 900 } }));
    await new Promise((r) => setTimeout(r, 30));
    expect(el(p, "inbox").dataset.view).toBe("list");
    act(() =>
      revealSlot("right-pane", {
        dashboard: PANEL,
        intent: { messageId: 900, conversation: "Ori", focusComposer: false },
      }),
    );
    await waitFor(() =>
      expect(el(p, "conversation-Ori").getAttribute("aria-selected")).toBe("true"),
    );
    expect(p.doc.activeElement).not.toBe(el(p, "draft"));
    type(p, "Keep this draft");
    act(() =>
      revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Mia", team: "web" } }),
    );
    await new Promise((r) => setTimeout(r, 30));
    expect(el(p, "conversation-Ori").getAttribute("aria-selected")).toBe("true");
    sessions.push({ handle: "Lee", state: "running" });
    act(() =>
      revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Lee", team: TEAM } }),
    );
    await waitFor(() => expect(el(p, "thread-name").textContent).toBe("New message"));
    expect(el(p, "recipient").textContent).toBe("Lee");
    expect(p.doc.activeElement).toBe(el(p, "draft"));
    act(() => revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Ori" } }));
    await waitFor(() =>
      expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Keep this draft"),
    );
    expect(screen.queryByTestId("pane-dev-banner")).toBeNull();
  });

  it("keeps the newest pane_open when listing requests finish in reverse order", async () => {
    const p = await openInbox();
    const listingReplies: Array<(value: PaneListing) => void> = [];
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) =>
      cmd === "list_panes"
        ? new Promise<PaneListing>((resolve) => listingReplies.push(resolve))
        : stubTauri(cmd, args),
    );
    for (const conversation of ["Mia", "Ori"])
      for (const cb of listeners.get(PANE_OPEN_EVENT) ?? [])
        cb({ payload: { panelId: SOURCE_PANEL, path: "", intent: { conversation } } });
    expect(listingReplies).toHaveLength(2);
    await act(async () => listingReplies[1](listing()));
    await waitFor(() =>
      expect(el(p, "conversation-Ori").getAttribute("aria-selected")).toBe("true"),
    );
    await act(async () => listingReplies[0](listing()));
    await new Promise((r) => setTimeout(r, 60));
    expect(el(p, "conversation-Ori").getAttribute("aria-selected")).toBe("true");
  });

  it("keeps separate drafts for unseen agents opened by intent", async () => {
    sessions.push({ handle: "Lee", state: "running" }, { handle: "Kim", state: "running" });
    const p = await openInbox();
    act(() => revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Lee" } }));
    await waitFor(() => expect(el(p, "recipient").textContent).toBe("Lee"));
    type(p, "Lee draft");
    act(() => revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Kim" } }));
    await waitFor(() => expect(el(p, "recipient").textContent).toBe("Kim"));
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("");
    type(p, "Kim draft");
    act(() => revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Lee" } }));
    await waitFor(() => expect(el(p, "recipient").textContent).toBe("Lee"));
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Lee draft");
    fireEvent.click(el(p, "send"));
    await waitFor(() =>
      expect(calls("send_inbox_as_human").at(-1)?.[1]).toMatchObject({
        to: "@Lee",
        text: "Lee draft",
      }),
    );
    act(() => revealSlot("right-pane", { dashboard: PANEL, intent: { conversation: "Kim" } }));
    await waitFor(() => expect(el(p, "recipient").textContent).toBe("Kim"));
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Kim draft");
  });

  it.each(["scheduled", "running", "hidden"])(
    "holds a new-message intent while refresh is %s",
    async (state) => {
      const p = await openInbox();
      await new Promise((r) => setTimeout(r, 60));
      if (state === "hidden") {
        act(() => useViewerStore.setState({ rightPaneOpen: false }));
        await new Promise((r) => setTimeout(r, 30));
      }
      let releaseInbox!: () => void;
      const gate = new Promise<void>((resolve) => {
        releaseInbox = resolve;
      });
      const before = calls("agent_read_inbox").length;
      tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "agent_read_inbox") await gate;
        return stubTauri(cmd, args);
      });
      messages.push(row(5, "Lee"));
      inboxChanged();
      if (state === "running")
        await waitFor(() => expect(calls("agent_read_inbox").length).toBeGreaterThan(before));
      act(() => revealSlot("right-pane", { dashboard: PANEL, intent: { messageId: 5 } }));
      await waitFor(() => expect(calls("agent_read_inbox").length).toBeGreaterThan(before));
      await act(async () => releaseInbox());
      await waitFor(() =>
        expect(el(p, "conversation-Lee").getAttribute("aria-selected")).toBe("true"),
      );
      expect(el(p, "message-5").classList.contains("ib-target")).toBe(true);
      expect(p.doc.activeElement).toBe(el(p, "draft"));
    },
  );

  it("reads one visible open in a batch, never agent mail, and updates the team badge", async () => {
    messages.push(
      row(3, "human", "Mia", { inReplyTo: 1 }),
      row(4, "Mia", "Other", { inReplyTo: 1 }),
    );
    function Edge() {
      const open = useViewerStore((s) => s.rightPaneOpen);
      return (
        <ScratchNoteWidthDivider
          containerRef={{ current: null }}
          collapsed={!open}
          onToggle={() => useViewerStore.setState({ rightPaneOpen: true })}
        />
      );
    }
    render(
      <TooltipProvider>
        <Edge />
      </TooltipProvider>,
    );
    const p = await openInbox();
    expect(calls("mark_inbox_messages_read")).toHaveLength(0);
    await waitFor(() => expect(usePaneStore.getState().badges[PANEL]?.count).toBe(2));
    await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
    const edge = screen.getByTestId("scratch-note-width-divider");
    expect(edge.querySelector("[data-badge-count]")?.getAttribute("data-badge-count")).toBe("2");
    expect(edge.getAttribute("aria-label")).toContain("Inbox 2 unread");
    fireEvent.keyDown(edge, { key: "Enter" });
    await openThread(p);
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    expect(calls("mark_inbox_messages_read")[0][1]).toEqual({
      team: { org: ORG, team: TEAM },
      messageIds: [1],
    });
    expect(messages.find((m) => m.messageId === 4)?.unread).toBe(true);
    expect(el(p, "log").textContent).toContain("New");
    await openThread(p, "Ori");
    await waitFor(() => expect(usePaneStore.getState().badges[PANEL]).toBeUndefined());
    await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
    expect(edge.querySelector("[data-badge-count]")).toBeNull();
  });

  it("keeps full refreshes and read actions paused while hidden and catches up when shown", async () => {
    const p = await openInbox();
    await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
    await new Promise((r) => setTimeout(r, 30));
    const beforeRoles = calls("agent_read_roles").length;
    const beforePresence = calls("get_human_status").length;
    messages.push(row(5));
    inboxChanged();
    await openThread(p);
    await new Promise((r) => setTimeout(r, 60));
    expect(calls("mark_inbox_messages_read")).toHaveLength(0);
    expect(calls("agent_read_roles")).toHaveLength(beforeRoles);
    expect(calls("get_human_status")).toHaveLength(beforePresence);
    await act(async () => useViewerStore.setState({ rightPaneOpen: true }));
    await waitFor(() =>
      expect(
        [...new Set(calls("mark_inbox_messages_read").flatMap(([, args]) => args.messageIds))].sort(
          (a, b) => a - b,
        ),
      ).toEqual([1, 5]),
    );
  });

  it("marks the next visible open after mail arrives while its thread is hidden", async () => {
    const p = await openInbox();
    await openThread(p);
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
    await new Promise((r) => setTimeout(r, 30));
    messages.push(row(5));
    inboxChanged();
    await new Promise((r) => setTimeout(r, 60));
    expect(calls("mark_inbox_messages_read")).toHaveLength(1);
    await act(async () => useViewerStore.setState({ rightPaneOpen: true }));
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(2));
    expect(calls("mark_inbox_messages_read")[1][1].messageIds).toEqual([5]);
    await new Promise((r) => setTimeout(r, 60));
    expect(calls("mark_inbox_messages_read")).toHaveLength(2);
  });

  it("keeps a newer selection and its draft when an earlier archive fails", async () => {
    const p = await openInbox();
    await openThread(p);
    let rejectArchive!: (reason: Error) => void;
    tauriInvoke.mockImplementation((cmd: string, args?: Record<string, unknown>) =>
      cmd === "mark_inbox_messages_archived"
        ? new Promise((_, reject) => {
            rejectArchive = reject;
          })
        : stubTauri(cmd, args),
    );
    fireEvent.click(el(p, "archive-thread"));
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await openThread(p, "Ori");
    type(p, "For Ori");
    await waitFor(() => expect(calls("mark_inbox_messages_archived")).toHaveLength(1));
    await act(async () => rejectArchive(new Error("Store unavailable")));
    await waitFor(() => expect(el(p, "list-notice").textContent).toContain("Store unavailable"));
    expect(el(p, "thread-name").textContent).toBe("Ori");
    expect(el(p, "recipient").textContent).toBe("Ori");
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("For Ori");
    fireEvent.click(el(p, "send"));
    await waitFor(() =>
      expect(calls("reply_as_human").at(-1)?.[1]).toMatchObject({ messageId: 2, text: "For Ori" }),
    );
  });

  it("waits for persisted drafts before offering conversations or a new composer", async () => {
    appState = JSON.stringify({
      dashboardPrefs: {
        [`dashboard:team:${ORG}:${TEAM}/inbox`]: {
          data: { drafts: { Mia: { text: "Saved Mia draft", to: "Mia", at: 1 } } },
          at: Date.now(),
        },
      },
    });
    let releasePrefs!: () => void;
    const gate = new Promise<void>((resolve) => {
      releasePrefs = resolve;
    });
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "load_app_state") await gate;
      return stubTauri(cmd, args);
    });
    const p = await openInbox(false);
    await waitFor(() => expect(calls("live_hub_subscribe").length).toBeGreaterThan(0));
    await new Promise((r) => setTimeout(r, 60));
    expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(0);
    expect((el(p, "new-message") as HTMLButtonElement).disabled).toBe(true);
    await act(async () => releasePrefs());
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(2));
    await openThread(p);
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Saved Mia draft");
    await openThread(p, "Ori");
    await openThread(p);
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Saved Mia draft");
  });

  it("still opens conversations if preferences cannot be read", async () => {
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "load_app_state") throw new Error("Prefs unavailable");
      return stubTauri(cmd, args);
    });
    const p = await openInbox();
    expect((el(p, "new-message") as HTMLButtonElement).disabled).toBe(false);
    await openThread(p);
    type(p, "Can still reply");
    expect((el(p, "send") as HTMLButtonElement).disabled).toBe(false);
  });

  it("rolls a failed optimistic archive back and retries", async () => {
    const p = await openInbox();
    await openThread(p);
    archiveFails = true;
    fireEvent.click(el(p, "archive-thread"));
    await waitFor(() => expect(el(p, "list-notice").textContent).toContain("Store unavailable"));
    expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(2);
    archiveFails = false;
    fireEvent.click(el(p, "list-notice").querySelector("button")!);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    inboxChanged();
    await new Promise((r) => setTimeout(r, 80));
    expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1);
  });

  it("shows a new inbound reply after its earlier conversation was archived", async () => {
    messages.push(row(3, "human", "Mia", { inReplyTo: 1 }));
    const p = await openInbox();
    await openThread(p);
    fireEvent.click(el(p, "archive-thread"));
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    expect(messages.find((m) => m.messageId === 3)?.archivedAtMs).toBeUndefined();
    messages.push(row(5, "Mia", "human", { inReplyTo: 1, text: "A follow-up question" }));
    inboxChanged();
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(2));
    await openThread(p);
    expect(el(p, "log").textContent).toContain("A follow-up question");
    expect(el(p, "log").textContent).not.toContain("Message 1");
    expect(el(p, "log").textContent).not.toContain("Message 3");
  });

  it("keeps per-thread text and recipients across switches and a reload", async () => {
    const p = await openInbox();
    await openThread(p);
    type(p, "Mia draft");
    await pick(p, "#docs");
    await openThread(p, "Ori");
    type(p, "Ori draft");
    await openThread(p);
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Mia draft");
    expect(el(p, "recipient").textContent).toContain("#docs");
    await waitFor(() => expect(appState).toContain("Ori draft"));
    cleanup();
    const reloaded = await openInbox();
    await openThread(reloaded);
    expect((el(reloaded, "draft") as HTMLTextAreaElement).value).toBe("Mia draft");
    expect(el(reloaded, "recipient").textContent).toContain("#docs");
  });

  it("replies and sends to a different recipient with the parent, keeping invalid drafts", async () => {
    const p = await openInbox();
    await openThread(p);
    type(p, "Answer");
    fireEvent.click(el(p, "send"));
    await waitFor(() => expect(calls("reply_as_human")).toHaveLength(1));
    expect(calls("reply_as_human")[0][1]).toMatchObject({ messageId: 1, text: "Answer" });
    await waitFor(() => expect((el(p, "draft") as HTMLTextAreaElement).value).toBe(""));
    type(p, "Reroute");
    await pick(p, "#docs");
    sendInvalid = true;
    await waitFor(() => expect(el(p, "send")).not.toHaveAttribute("disabled"));
    fireEvent.click(el(p, "send"));
    await waitFor(() => expect(el(p, "composer-error").textContent).toContain("Not sent"));
    expect(calls("send_inbox_as_human").at(-1)?.[1]).toMatchObject({
      to: "#docs",
      text: "Reroute",
      inReplyTo: 1,
    });
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Reroute");
  });

  it.each(["Mia", "#docs", "You"])("sends a new message to %s", async (name) => {
    const p = await openInbox();
    fireEvent.click(el(p, "new-message"));
    await pick(p, name);
    type(p, "Hello");
    fireEvent.click(el(p, "send"));
    await waitFor(() => expect(calls("send_inbox_as_human")).toHaveLength(1));
    expect(calls("send_inbox_as_human")[0][1]).toMatchObject({
      to: name === "You" ? "human" : name === "Mia" ? "@Mia" : name,
      text: "Hello",
      inReplyTo: null,
    });
  });

  it("derives queued and ended state from live sessions and offers the ended session's role", async () => {
    const p = await openInbox();
    await openThread(p);
    type(p, "Keep this");
    sessions[0].state = "offline";
    await act(async () => {
      await p.win.codesense.sessions();
    });
    // Push the same event the host sends after a roster change.
    p.win.dispatchEvent(
      new p.win.MessageEvent("message", {
        source: p.win.parent as never,
        origin: "tauri://localhost",
        data: {
          token: "frame-token",
          type: "codesense:delta",
          topic: "sessions",
          viewKey: "all",
          rev: sessions[0].state === "offline" ? 1 : 2,
          upserts: sessions.map((row) => ({ id: row.handle, ...row })),
          removes: [],
        },
      }) as never,
    );
    await waitFor(() => expect(el(p, "send").textContent).toContain("Queue"));
    sessions[0].state = "exited";
    p.win.dispatchEvent(
      new p.win.MessageEvent("message", {
        source: p.win.parent as never,
        origin: "tauri://localhost",
        data: {
          token: "frame-token",
          type: "codesense:delta",
          topic: "sessions",
          viewKey: "all",
          rev: sessions[0].state === "offline" ? 1 : 2,
          upserts: sessions.map((row) => ({ id: row.handle, ...row })),
          removes: [],
        },
      }) as never,
    );
    await waitFor(() => expect((el(p, "send") as HTMLButtonElement).disabled).toBe(true));
    expect(el(p, "fallback").textContent).toContain("#docs");
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Keep this");
  });
  it("bounds Unicode drafts to 16KB, dropping the oldest draft before trimming the current one", async () => {
    const p = await openInbox();
    await openThread(p, "Ori");
    type(p, "Old draft");
    await openThread(p);
    type(p, "📝".repeat(12000));
    await waitFor(() => expect(appState).toContain("dashboardPrefs"));
    const data = Object.values(JSON.parse(appState).dashboardPrefs)[0] as {
      data: { drafts: Record<string, { text: string }> };
    };
    expect(new TextEncoder().encode(JSON.stringify(data.data)).length).toBeLessThanOrEqual(16384);
    expect(data.data.drafts.Ori).toBeUndefined();
    expect(data.data.drafts.Mia.text.length).toBeGreaterThan(0);
    expect(data.data.drafts.Mia.text.length).toBeLessThan(24000);
  });

  it("renders markdown safely, keeps HTML out of the frame, and opens attachments through the host", async () => {
    messages[0].text =
      "**Bold** and `code` [docs](https://example.com)\n<script>bad()</script>\n[unsafe](javascript:alert(1))";
    messages[0].primaryFile = "e/SW-1.md";
    messages[1].contentType = "html";
    messages[1].text = "<h1>Private html body</h1>";
    messages[1].summary = "Status dashboard";
    const p = await openInbox();
    await openThread(p);
    expect(el(p, "log").querySelector("strong")?.textContent).toBe("Bold");
    expect(el(p, "log").querySelector("script")).toBeNull();
    expect(el(p, "log").querySelectorAll("a")).toHaveLength(1);
    expect(el(p, "log").querySelector("a")?.href).toBe("https://example.com/");
    fireEvent.click(el(p, "log").querySelector("button.ib-chip")!);
    await waitFor(() => expect(onFileSelect).toHaveBeenCalled());
    await openThread(p, "Ori");
    expect(el(p, "log").textContent).not.toContain("Private html body");
    await waitFor(() => expect(el(p, "log").querySelector("iframe")).not.toBeNull());
    fireEvent.click(el(p, "log").querySelector("button")!);
    await waitFor(() => expect(useViewerStore.getState().openTabs).not.toEqual({}));
  });

  it("shows role delivery receipts and reply-to targets", async () => {
    messages[0].replyTo = "role:docs";
    messages.push(
      row(3, "human", "docs", { inReplyTo: 1, delivery: { state: "reading", instance: "Ori" } }),
    );
    const p = await openInbox();
    await openThread(p);
    expect(el(p, "recipient").textContent).toBe("#docs");
    expect(el(p, "recipient-state").textContent).toContain("Queues for the role");
    expect(el(p, "log").textContent).toContain("Reading · Ori");
    type(p, "Role reply");
    fireEvent.click(el(p, "send"));
    await waitFor(() => expect(calls("reply_as_human")).toHaveLength(1));
  });

  it("supports the 640px split, list arrows, Enter, menu-first Escape and command-Enter", async () => {
    const p = await openInbox();
    p.win.innerWidth = 639;
    p.win.dispatchEvent(new p.win.Event("resize") as never);
    expect(el(p, "inbox").dataset.layout).toBe("stack");
    el(p, "conversations").focus();
    fireEvent.keyDown(el(p, "conversations"), { key: "ArrowDown" });
    fireEvent.keyDown(el(p, "conversations"), { key: "Enter" });
    expect(p.doc.activeElement).toBe(el(p, "draft"));
    type(p, "Keyboard answer");
    fireEvent.click(el(p, "recipient"));
    fireEvent.keyDown(el(p, "recipient-search"), { key: "Escape" });
    expect(el(p, "picker").hidden).toBe(true);
    el(p, "draft").focus();
    fireEvent.keyDown(el(p, "draft"), { key: "Escape" });
    expect(p.doc.activeElement).toBe(el(p, "back"));
    expect(p.win.getComputedStyle(el(p, "back") as never).display).not.toBe("none");
    expect(p.win.getComputedStyle(p.doc.querySelector(".ib-list") as never).display).toBe("none");
    expect(el(p, "inbox").dataset.view).toBe("thread");
    fireEvent.keyDown(el(p, "back"), { key: "Escape" });
    expect(el(p, "inbox").dataset.view).toBe("list");
    fireEvent.keyDown(el(p, "conversations"), { key: " " });
    fireEvent.keyDown(el(p, "draft"), { key: "Enter", metaKey: true });
    await waitFor(() => expect(calls("reply_as_human")).toHaveLength(1));
    p.win.innerWidth = 640;
    p.win.dispatchEvent(new p.win.Event("resize") as never);
    expect(el(p, "inbox").dataset.layout).toBe("split");
  });

  it("composer keys: Enter and command or control Enter send; Shift+Enter and IME composition do not", async () => {
    const p = await openInbox();
    await openThread(p);
    type(p, "Texting keys");
    const draft = el(p, "draft");
    // Shift+Enter is a new line: the pane leaves the event alone and nothing is sent.
    expect(fireEvent.keyDown(draft, { key: "Enter", shiftKey: true })).toBe(true);
    expect(fireEvent.keyDown(draft, { key: "Enter", isComposing: true })).toBe(true);
    expect(fireEvent.keyDown(draft, { key: "Enter", keyCode: 229 })).toBe(true);
    expect(fireEvent.keyDown(draft, { key: "Enter", altKey: true })).toBe(true);
    expect(calls("reply_as_human")).toHaveLength(0);
    expect(fireEvent.keyDown(draft, { key: "Enter" })).toBe(false);
    await waitFor(() => expect(calls("reply_as_human")).toHaveLength(1));
    type(p, "Second");
    fireEvent.keyDown(el(p, "draft"), { key: "Enter", ctrlKey: true });
    await waitFor(() => expect(calls("reply_as_human")).toHaveLength(2));
    type(p, "Third");
    fireEvent.keyDown(el(p, "draft"), { key: "Enter", metaKey: true, shiftKey: true });
    await waitFor(() => expect(calls("reply_as_human")).toHaveLength(3));
    expect(p.doc.body.textContent).toContain("Enter to send");
  });

  it("updates presence using the existing status action", async () => {
    const p = await openInbox();
    fireEvent.click(el(p, "presence"));
    const busy = [...el(p, "presence-options").querySelectorAll("button")].find(
      (b) => b.textContent === "Busy",
    )!;
    fireEvent.click(busy);
    fireEvent.input(el(p, "presence-note"), { target: { value: "Deep work" } });
    fireEvent.submit(el(p, "presence-menu"));
    await waitFor(() =>
      expect(calls("set_human_status")[0]?.[1]).toMatchObject({
        status: "busy",
        note: "Deep work",
      }),
    );
    await waitFor(() => expect(el(p, "presence").textContent).toContain("Deep work"));
  });
});

describe("cross-team inbox pane through the real bridge", () => {
  beforeEach(() => {
    crossTeam = true;
    rollup = [
      { ...row(1), id: 1, team: { org: ORG, team: TEAM }, toRole: null },
      { ...row(1, "Ori"), id: 1, team: { org: ORG, team: "web" }, toRole: null },
    ];
  });
  it.each(["inbox", "human-inbox"])(
    "the real rail opens a chosen %s dashboard across teams with its conversation intent",
    async (surface) => {
      crossSurface = surface;
      const base = tauriInvoke.getMockImplementation()!;
      tauriInvoke.mockImplementation(async (cmd, args) =>
        cmd === "list_session_inbox" ? { handle: "Ori", roles: [], messages: [] } : base(cmd, args),
      );
      const p = await openInbox();
      setPreview(true);
      render(
        <TooltipProvider>
          <InboxRail
            tabId="ori-tab"
            sessionId="Ori"
            teamKey={`${ORG}:web`}
            composeRequest={0}
            narrow
          />
        </TooltipProvider>,
      );
      const button = await screen.findByTestId("inbox-rail-open-inbox-pane");
      fireEvent.click(button);
      await waitFor(() => expect(el(p, "thread-name").textContent).toBe("Ori"));
      expect(el(p, "thread-team").textContent).toBe("web");
    },
  );
  async function chooseFilter(p: PaneDoc, name: string) {
    fireEvent.click(el(p, "team-filter"));
    fireEvent.click(
      [...p.doc.querySelectorAll<HTMLElement>("#filter-menu button")].find((b) =>
        b.textContent?.includes(name),
      )!,
    );
  }
  async function chooseTeam(p: PaneDoc, name: string) {
    fireEvent.click(el(p, "composer-team").querySelector("button")!);
    expect(el(p, "team-menu").hidden).toBe(false);
    fireEvent.click(
      [...p.doc.querySelectorAll<HTMLElement>("#team-menu button")].find((b) =>
        b.textContent?.includes(name),
      )!,
    );
  }

  it("keeps unread through a team-filter restore and tags cross-team avatars with the live handle", async () => {
    const p = await openInbox();
    await openThread(p, "Ori");
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    const header = JSON.parse(el(p, "thread-head").getAttribute("data-codesense-row")!);
    expect(header.key).toBe(JSON.stringify([ORG, "web", "Ori"]));
    for (const av of [
      p.doc.querySelector("#thread-avatar .av"),
      p.doc.querySelector('.ib-row[data-codesense-row*="Ori"] .av'),
    ])
      expect(JSON.parse(av!.getAttribute("data-codesense-row")!)).toEqual({
        kind: "agent",
        agentId: "Ori",
      });
    await menuAction(p, "mark-unread", header);
    await act(async () =>
      p.win.codesense.act("markUnread", { org: ORG, team: "web", messageIds: [1] }),
    );
    await chooseFilter(p, "web");
    expect(el(p, "latest-count").textContent).toBe("Marked unread");
    expect(rollup.find((m) => m.from === "Ori")?.unread).toBe(true);
    expect(calls("mark_inbox_messages_read")).toHaveLength(1);
    await menuAction(p, "reply", header);
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(2));
  });
  it("excludes archived teams from the recipient directory without blocking mail", async () => {
    useOrgStore.setState({
      orgs: useOrgStore.getState().orgs.map((org) => ({
        ...org,
        teams: [...org.teams, { name: "retired", archived: true } as never],
      })),
    });
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, any>) => {
      if (cmd === "agent_read_roles" && args?.team.team === "retired")
        throw new Error("Team is not synced");
      return stubTauri(cmd, args);
    });
    const p = await openInbox();
    fireEvent.click(el(p, "new-message"));
    fireEvent.click(el(p, "composer-team").querySelector("button")!);
    expect(el(p, "team-menu").textContent).not.toContain("retired");
  });

  it("refreshes the directory after teams are removed or added while mounted", async () => {
    const p = await openInbox();
    const oldReads = calls("agent_read_roles").length;
    useOrgStore.setState({
      orgs: useOrgStore.getState().orgs.map((org) => ({
        ...org,
        teams: [...org.teams.filter((team) => team.name !== "empty"), { name: "fresh" } as never],
      })),
    });
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, any>) => {
      if (cmd === "agent_read_roles" && args?.team.team === "empty")
        throw new Error("Team is not synced");
      return stubTauri(cmd, args);
    });
    rollup.push({ ...row(2, "New sender"), id: 2, team: { org: ORG, team: "fresh" } });
    inboxChanged();
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(3));
    expect(
      calls("agent_read_roles")
        .slice(oldReads)
        .map((call) => call[1].team.team),
    ).not.toContain("empty");
    fireEvent.click(el(p, "new-message"));
    fireEvent.click(el(p, "composer-team").querySelector("button")!);
    expect(el(p, "team-menu").textContent).toContain("fresh");
    expect(el(p, "team-menu").textContent).not.toContain("empty");
  });

  it("keeps the keyboard's filter option focused when live mail redraws it", async () => {
    const p = await openInbox();
    fireEvent.click(el(p, "team-filter"));
    const web = [...el(p, "filter-menu").querySelectorAll<HTMLElement>("button")].find((b) =>
      b.textContent?.includes("web"),
    )!;
    web.focus();
    const oldReads = calls("list_inbox_rollup").length;
    inboxChanged();
    await waitFor(() => expect(calls("list_inbox_rollup").length).toBeGreaterThan(oldReads));
    await waitFor(() => expect(p.doc.activeElement).not.toBe(web));
    expect(el(p, "filter-menu").contains(p.doc.activeElement)).toBe(true);
    expect(p.doc.activeElement?.textContent).toContain("web");
    fireEvent.keyDown(p.doc.activeElement!, { key: "ArrowUp" });
    expect(p.doc.activeElement?.textContent).toContain(TEAM);
  });

  it("keeps a session recipient when its checked composer team is selected again", async () => {
    const p = await openInbox();
    fireEvent.click(el(p, "new-message"));
    await pick(p, "Mia");
    type(p, "Keep the recipient");
    await chooseTeam(p, TEAM);
    expect(el(p, "recipient").textContent).toBe("Mia");
    expect((el(p, "send") as HTMLButtonElement).disabled).toBe(false);
  });

  it("updates the merged role holder's team when choosing the reply team", async () => {
    rollup = [
      { ...row(1, "human", null), id: 1, team: { org: ORG, team: TEAM }, toRole: "docs" },
      { ...row(1, "human", null), id: 1, team: { org: ORG, team: "web" }, toRole: "docs" },
    ];
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, any>) => {
      if (cmd === "agent_read_roles")
        return {
          roles: [
            {
              role: "docs",
              registrations: [{ live: true, instanceName: args?.team.team + " holder" }],
            },
          ],
        };
      return stubTauri(cmd, args);
    });
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await openThread(p, "#docs");
    expect(el(p, "thread-meta").textContent).toContain("web holder holds it in web");
    await chooseTeam(p, TEAM);
    expect(el(p, "thread-meta").textContent).toContain(TEAM + " holder holds it in " + TEAM);
  });

  it("bounds archived outgoing keys so drafts and filter preferences still save", async () => {
    rollup = Array.from({ length: 500 }, (_, index) => ({
      ...row(index + 1, "human", "Mia"),
      id: index + 1,
      team: { org: ORG, team: TEAM },
    }));
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await openThread(p);
    fireEvent.click(el(p, "archive-thread"));
    await waitFor(() => expect(calls("mark_inbox_messages_archived")).toHaveLength(1));
    await waitFor(() => expect(appState).toContain("hiddenSent"));
    fireEvent.click(el(p, "new-message"));
    type(p, "Still saved after many archives");
    await waitFor(() => expect(appState).toContain("Still saved after many archives"));
    const prefs = Object.values(JSON.parse(appState).dashboardPrefs)[0] as { data: unknown };
    expect(new TextEncoder().encode(JSON.stringify(prefs.data)).length).toBeLessThanOrEqual(16384);
    expect(el(p, "composer-error").hidden).toBe(true);
  });

  it("loads the rollup, labels both teams, and filters through the real pane", async () => {
    const p = await openInbox();
    expect(calls("agent_read_inbox")).toHaveLength(0);
    await waitFor(() => expect(usePaneStore.getState().badges[PANEL]?.count).toBe(2));
    expect(el(p, "inbox").dataset.mode).toBe("all");
    expect(p.doc.querySelectorAll(".ib-row .ib-team")).toHaveLength(2);
    await chooseFilter(p, "web");
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    expect(p.doc.querySelector(".ib-row")?.textContent).toContain("Ori");
    expect(el(p, "inbox").dataset.filter).toBe("one");
    expect(
      screen.getByTestId(`${PANEL}-fullscreen-shell`).getAttribute("data-maximize-scope"),
    ).toBe("app");
  });
  it("routes reads, replies, recipient overrides and archives to the row's team despite duplicate IDs", async () => {
    const p = await openInbox();
    await openThread(p, "Ori");
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    expect(calls("mark_inbox_messages_read")[0][1]).toEqual({
      team: { org: ORG, team: "web" },
      messageIds: [1],
    });
    expect(rollup[0].unread).toBe(true);
    expect(el(p, "thread-team").textContent).toBe("web");
    expect(el(p, "composer-team").querySelector("button")).toBeNull();
    type(p, "Reply in web");
    fireEvent.submit(el(p, "composer"));
    await waitFor(() => expect(calls("reply_as_human")).toHaveLength(1));
    expect(calls("reply_as_human")[0][1]).toMatchObject({
      team: { org: ORG, team: "web" },
      messageId: 1,
      text: "Reply in web",
    });
    await waitFor(() => expect((el(p, "draft") as HTMLTextAreaElement).value).toBe(""));
    await pick(p, "#docs");
    type(p, "Role override");
    fireEvent.submit(el(p, "composer"));
    await waitFor(() => expect(calls("send_inbox_as_human")).toHaveLength(1));
    expect(calls("send_inbox_as_human")[0][1]).toMatchObject({
      team: { org: ORG, team: "web" },
      inReplyTo: 1,
      to: "#docs",
    });
    fireEvent.click(el(p, "archive-thread"));
    await waitFor(() => expect(calls("mark_inbox_messages_archived")).toHaveLength(1));
    expect(calls("mark_inbox_messages_archived")[0][1]).toEqual({
      team: { org: ORG, team: "web" },
      messageIds: [1],
    });
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    expect(p.doc.querySelector(".ib-row")?.textContent).toContain("Mia");
  });

  it("starts in the filtered team, keeps drafts while choosing a no-mail team, and sends there", async () => {
    const p = await openInbox();
    await chooseFilter(p, "web");
    fireEvent.click(el(p, "new-message"));
    expect(el(p, "composer-team").textContent).toBe("inweb");
    await pick(p, "Ori");
    type(p, "Draft survives the team change");
    await chooseTeam(p, "empty");
    expect(el(p, "recipient").textContent).toBe("Choose recipient");
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Draft survives the team change");
    await pick(p, "#docs");
    await chooseTeam(p, "web");
    expect(el(p, "recipient").textContent).toBe("#docs");
    await chooseTeam(p, "empty");
    fireEvent.submit(el(p, "composer"));
    await waitFor(() => expect(calls("send_inbox_as_human")).toHaveLength(1));
    expect(calls("send_inbox_as_human")[0][1]).toMatchObject({
      team: { org: ORG, team: "empty" },
      to: "#docs",
      text: "Draft survives the team change",
      inReplyTo: null,
    });
  });

  it("quotes the same team's parent when merged role messages share local IDs", async () => {
    rollup = [TEAM, "web"].flatMap((name) => {
      const team = { org: ORG, team: name };
      return [
        {
          ...row(1, "human", null, { text: name + " question", unread: false }),
          id: 1,
          team,
          toRole: "docs",
        },
        {
          ...row(3, "human", null, { text: name + " follow-up", unread: false }),
          id: 3,
          team,
          toRole: "docs",
        },
        {
          ...row(4, "Mia", "human", { text: name + " answer", inReplyTo: 1, unread: false }),
          id: 4,
          team,
        },
      ];
    });
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await openThread(p, "#docs");
    const answer = [...el(p, "log").querySelectorAll(".ib-msg")].find((m) =>
      m.textContent?.includes("web answer"),
    )!;
    const quote = answer.querySelector<HTMLButtonElement>(".ib-quote")!;
    expect(quote.textContent).toContain("web question");
    fireEvent.click(quote);
    expect(el(p, "log").querySelector(".ib-target")?.textContent).toContain("web question");
  });

  it("reads older unread role mail even when another team's loaded message shares its ID", async () => {
    rollup = [
      {
        ...row(2, "Mia", "human", { conversation: "#docs" }),
        id: 2,
        team: { org: ORG, team: TEAM },
      },
      ...Array.from({ length: 501 }, (_, n) => ({
        ...row(n + 2, "Ori", "human", { conversation: "#docs", unread: n === 0 }),
        id: n + 2,
        team: { org: ORG, team: "web" },
      })),
    ];
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await openThread(p, "#docs");
    await waitFor(() => expect(rollup.every((m) => !m.unread)).toBe(true));
    expect(
      calls("mark_inbox_messages_read").some(
        ([, args]) => args.team.team === "web" && args.messageIds.includes(2),
      ),
    ).toBe(true);
    await waitFor(() => expect(usePaneStore.getState().badges[PANEL]).toBeUndefined());
  });

  it("merges role and You rows, tags each message, batches reads, and rolls back only the failed archive team", async () => {
    rollup = [
      { ...row(1, "human", null), id: 1, team: { org: ORG, team: TEAM }, toRole: "docs" },
      {
        ...row(1, "human", null),
        id: 1,
        team: { org: ORG, team: "web" },
        toRole: "docs",
        createdAtMs: 2000,
      },
      { ...row(2, "Mia"), id: 2, team: { org: ORG, team: TEAM }, inReplyTo: 1 },
      { ...row(2, "Ori"), id: 2, team: { org: ORG, team: "web" }, inReplyTo: 1, createdAtMs: 3000 },
      { ...row(3, "human"), id: 3, team: { org: ORG, team: TEAM } },
      { ...row(3, "human"), id: 3, team: { org: ORG, team: "web" } },
    ];
    const p = await openInbox();
    const rows = [...p.doc.querySelectorAll(".ib-row")];
    expect(rows.filter((r) => r.textContent?.includes("#docs"))).toHaveLength(1);
    expect(rows.filter((r) => r.textContent?.includes("You"))).toHaveLength(1);
    await openThread(p, "#docs");
    expect(el(p, "thread-team").textContent).toBe("2 teams");
    expect(el(p, "log").querySelectorAll(".mh .ib-team")).toHaveLength(4);
    expect(el(p, "composer-team").textContent).toContain("web");
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(2));
    expect(calls("mark_inbox_messages_read").map((c) => c[1])).toEqual(
      expect.arrayContaining([
        { team: { org: ORG, team: TEAM }, messageIds: [2] },
        { team: { org: ORG, team: "web" }, messageIds: [2] },
      ]),
    );
    archiveFailTeam = "web";
    fireEvent.click(el(p, "archive-thread"));
    await waitFor(() =>
      expect(el(p, "list-notice").textContent).toContain("web: archive: Store unavailable"),
    );
    expect(calls("mark_inbox_messages_archived").map((c) => c[1])).toEqual(
      expect.arrayContaining([
        { team: { org: ORG, team: TEAM }, messageIds: [1, 2] },
        { team: { org: ORG, team: "web" }, messageIds: [1, 2] },
      ]),
    );
    await waitFor(() => expect(el(p, "log").querySelectorAll("article")).toHaveLength(2));
    expect(el(p, "log").textContent).not.toContain("Mia");
    archiveFailTeam = null;
    fireEvent.click(el(p, "list-notice").querySelector("button")!);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    expect(p.doc.querySelector(".ib-row")?.textContent).toContain("You");
  });

  it("archives only the filtered role team and falls back to All teams when it empties", async () => {
    rollup = [
      { ...row(1, "human", null), id: 1, team: { org: ORG, team: TEAM }, toRole: "docs" },
      { ...row(1, "human", null), id: 1, team: { org: ORG, team: "web" }, toRole: "docs" },
    ];
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await chooseFilter(p, "web");
    await openThread(p, "#docs");
    fireEvent.click(el(p, "archive-thread"));
    await waitFor(() => expect(calls("mark_inbox_messages_archived")).toHaveLength(1));
    expect(calls("mark_inbox_messages_archived")[0][1]).toEqual({
      team: { org: ORG, team: "web" },
      messageIds: [1],
    });
    await waitFor(() => expect(el(p, "inbox").dataset.filter).toBe("all"));
    expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1);
  });

  it("keeps invalid sends inline and restores a failed archive in its team", async () => {
    const p = await openInbox();
    await chooseFilter(p, "web");
    await openThread(p, "Ori");
    sendInvalid = true;
    type(p, "Keep this draft");
    fireEvent.submit(el(p, "composer"));
    await waitFor(() => expect(el(p, "composer-error").textContent).toContain("Recipient ended"));
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Keep this draft");
    archiveFails = true;
    fireEvent.click(el(p, "archive-thread"));
    await waitFor(() => expect(el(p, "list-notice").textContent).toContain("Store unavailable"));
    expect(el(p, "inbox").dataset.filter).toBe("one");
    expect(p.doc.querySelector(".ib-row")?.getAttribute("aria-selected")).toBe("true");
  });

  it("opens the selected installed inbox when its team source is listed first", () => {
    const source = listing().panes[0];
    const copy = {
      ...source,
      id: "installed-inbox",
      scope: "private" as const,
      panelId: PRIVATE_PANEL,
      dashboardRef: {
        scope: "private" as const,
        org: ORG,
        id: "installed-inbox",
        installedFrom: source.dashboardRef,
      },
    };
    useDashboardProfileStore.setState({
      org: ORG,
      profile: { ...profileChoice(), slots: { "right-pane": [copy.dashboardRef] } },
    });
    usePaneStore.setState({
      teamKey: `${ORG}:${TEAM}`,
      listing: { ...listing(), panes: [source, copy] },
    });
    expect(resolveTarget()).toBe(PRIVATE_PANEL);
  });

  it("prefers the private copy for other-team entry points and applies the org-qualified intent", async () => {
    const p = await openInbox();
    setPreview(true);
    const state = usePaneStore.getState();
    const privateEntry = state.listing!.panes[0];
    act(() =>
      usePaneStore.setState({
        listing: {
          ...state.listing!,
          panes: [
            {
              ...privateEntry,
              panelId: SOURCE_PANEL,
              scope: "team",
              team: TEAM,
              surface: undefined,
            },
            privateEntry,
          ],
        },
      }),
    );
    expect(resolveTarget()).toBe(PRIVATE_PANEL);
    expect(resolveTarget({ conversation: "Ori", team: { org: ORG, team: "web" } })).toBe(
      PRIVATE_PANEL,
    );
    await act(async () => activateInbox({ conversation: "Ori", team: { org: ORG, team: "web" } }));
    await waitFor(() => expect(el(p, "thread-name").textContent).toBe("Ori"));
    expect(el(p, "thread-team").textContent).toBe("web");
    expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(2);
    expect(p.doc.activeElement).toBe(el(p, "draft"));
  });

  it.each(["closed", "background tab"])(
    "keeps the badge live while %s without rendering, then catches up",
    async (hiddenBy) => {
      function Edge() {
        const open = useViewerStore((state) => state.rightPaneOpen);
        return (
          <ScratchNoteWidthDivider
            containerRef={{ current: null }}
            collapsed={!open}
            onToggle={() => useViewerStore.setState({ rightPaneOpen: true })}
          />
        );
      }
      render(
        <TooltipProvider>
          <Edge />
        </TooltipProvider>,
      );
      const p = await openInbox();
      const frame = screen.getByTestId("pane-body").querySelector("iframe");
      const layout = useViewerStore.getState().rightPaneLayout;
      const publishBadge = vi.spyOn(p.win.codesense, "setBadge");
      await new Promise((r) => setTimeout(r, 80));
      const ownReads = vi.spyOn(p.win.codesense, "inboxRollup");
      await act(async () => {
        inboxChanged();
        inboxChanged();
        inboxChanged();
      });
      await waitFor(() => expect(ownReads).toHaveBeenCalledTimes(1));
      await act(async () =>
        useViewerStore.setState(
          hiddenBy === "closed"
            ? { rightPaneOpen: false }
            : {
                rightPaneLayout: {
                  sections: [
                    {
                      id: "tabs",
                      group: { panelIds: [PANEL, "scratch"], activePanelId: "scratch" },
                      collapsed: false,
                      size: 1,
                    },
                  ],
                },
              },
        ),
      );
      publishBadge.mockClear();
      expect(usePaneStore.getState().badges[PANEL]?.count).toBe(2);
      const edge = screen.getByTestId("scratch-note-width-divider");
      const listBefore = p.doc.querySelector(".ib-rows")!.innerHTML;
      const threadBefore = el(p, "log").innerHTML;
      const hidden = ownReads.mock.calls.length;
      rollup.push({ ...row(4, "NewAgent"), id: 4, team: { org: ORG, team: "web" } });
      await act(async () => {
        for (let i = 0; i < 50; i++) inboxChanged();
      });
      await waitFor(() => expect(usePaneStore.getState().badges[PANEL]?.count).toBe(3), {
        timeout: 2500,
      });
      expect(publishBadge.mock.calls.length).toBeGreaterThanOrEqual(1);
      expect(publishBadge.mock.calls.length).toBeLessThanOrEqual(2);
      expect(ownReads.mock.calls.length - hidden).toBeLessThanOrEqual(2);
      expect(p.doc.querySelector(".ib-rows")!.innerHTML).toBe(listBefore);
      expect(el(p, "log").innerHTML).toBe(threadBefore);
      expect(screen.getByTestId("pane-body").querySelector("iframe")).toBe(frame);
      if (hiddenBy === "closed") {
        expect(edge.querySelector("[data-badge-count]")?.getAttribute("data-badge-count")).toBe(
          "3",
        );
        expect(edge.getAttribute("aria-label")?.toLowerCase()).not.toContain("last known");
      }
      await act(async () => {
        rollup.forEach((message) => {
          message.unread = false;
        });
        inboxChanged();
      });
      await waitFor(() => expect(usePaneStore.getState().badges[PANEL]).toBeUndefined(), {
        timeout: 2500,
      });
      expect(p.doc.querySelector(".ib-rows")!.innerHTML).toBe(listBefore);
      if (hiddenBy === "closed") expect(edge.querySelector("[data-badge-count]")).toBeNull();
      await act(async () =>
        useViewerStore.setState({ rightPaneOpen: true, rightPaneLayout: layout }),
      );
      await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(3));
      await waitFor(() => expect(usePaneStore.getState().badges[PANEL]).toBeUndefined());
      expect(ownReads.mock.calls.length).toBeGreaterThan(hidden);
      await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
      expect(edge.querySelector("[data-badge-count]")).toBeNull();
    },
  );

  it("keeps a persisted team filter through an empty first load, reopen and page reload", async () => {
    const key = `dashboard:private:${ORG}/inbox`;
    appState = JSON.stringify({
      dashboardPrefs: {
        [key]: { data: { filterTeam: { org: ORG, team: "web" } }, at: Date.now() },
      },
    });
    const initial = rollup;
    rollup = [];
    const p = await openInbox(false);
    await waitFor(() => expect(el(p, "team-filter").hidden).toBe(false));
    expect(el(p, "team-filter").textContent).toContain("web");
    expect(JSON.parse(appState).dashboardPrefs[key].data.filterTeam).toEqual({
      org: ORG,
      team: "web",
    });
    rollup = initial;
    await act(async () => inboxChanged());
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
    await act(async () => useViewerStore.setState({ rightPaneOpen: true }));
    expect(el(p, "team-filter").textContent).toContain("web");
    cleanup();
    const reloaded = await openInbox(false);
    await waitFor(() => expect(reloaded.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    expect(el(reloaded, "team-filter").textContent).toContain("web");
  });

  it("leaves All teams unchanged when an intent's conversation is already visible", async () => {
    const p = await openInbox();
    setPreview(true);
    const saves = calls("save_app_state").length;
    await act(async () => activateInbox({ conversation: "Ori", team: { org: ORG, team: "web" } }));
    await waitFor(() => expect(el(p, "thread-name").textContent).toBe("Ori"));
    expect(el(p, "team-filter").textContent).toContain("All teams");
    expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(2);
    expect(calls("save_app_state")).toHaveLength(saves);
  });

  it.each([false, true])(
    "switches a hidden cross-team target without saving it (reduced motion=%s)",
    async (reduced) => {
      const p = await openInbox(true, PANE, undefined, `workspace:${ORG}:${TEAM}:owner/repo`);
      setPreview(true);
      await chooseFilter(p, TEAM);
      const key = `dashboard:private:${ORG}/inbox`;
      await waitFor(() =>
        expect(JSON.parse(appState).dashboardPrefs[key].data.filterTeam).toEqual({
          org: ORG,
          team: TEAM,
        }),
      );
      vi.spyOn(p.win, "matchMedia").mockReturnValue({ matches: reduced } as never);
      await act(async () =>
        revealSlot("right-pane", {
          surface: "inbox",
          presentation: reduced ? "overlay" : "docked",
          intent: { conversation: "Ori", org: ORG, team: "web", focusComposer: true },
        }),
      );
      await waitFor(() => expect(el(p, "thread-name").textContent).toBe("Ori"));
      expect(el(p, "thread-team").textContent).toBe("web");
      expect(el(p, "team-filter").textContent).toContain("web");
      expect(el(p, "filter-status").textContent).toBe("Showing web");
      expect(el(p, "filter-status").getAttribute("aria-live")).toBe("polite");
      expect(el(p, "team-filter").classList.contains("ib-filter-changed")).toBe(!reduced);
      expect(p.doc.activeElement).toBe(el(p, "draft"));
      expect(screen.getByTestId(`${PANEL}-fullscreen-shell`).dataset.fullscreen).toBe(
        String(reduced),
      );
      type(p, "Keep the user's filter");
      await waitFor(() =>
        expect(JSON.stringify(JSON.parse(appState).dashboardPrefs[key].data.drafts)).toContain(
          "Keep the user's filter",
        ),
      );
      expect(JSON.parse(appState).dashboardPrefs[key].data.filterTeam).toEqual({
        org: ORG,
        team: TEAM,
      });
      cleanup();
      const reloaded = await openInbox(false);
      await waitFor(() => expect(el(reloaded, "team-filter").textContent).toContain(TEAM));
    },
  );

  it("restores a saved org-qualified filter, keeps unread elsewhere visible, and persists changes", async () => {
    appState = JSON.stringify({
      dashboardPrefs: {
        [`dashboard:private:${ORG}/inbox`]: {
          data: { filterTeam: { org: ORG, team: "web" } },
          at: Date.now(),
        },
      },
    });
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    expect(el(p, "team-filter").getAttribute("aria-label")).toContain("unread mail in other teams");
    expect(p.doc.querySelector(".ib-row")?.textContent).toContain("Ori");
    await chooseFilter(p, TEAM);
    await waitFor(() =>
      expect(
        JSON.parse(appState).dashboardPrefs[`dashboard:private:${ORG}/inbox`].data.filterTeam,
      ).toEqual({ org: ORG, team: TEAM }),
    );
    fireEvent.click(el(p, "new-message"));
    expect(el(p, "composer-team").textContent).toContain(TEAM);
  });

  it("distinguishes identically named teams and handles in entry-point intents", async () => {
    const otherOrg = "other/org";
    useOrgStore.setState({
      orgs: [
        ...useOrgStore.getState().orgs,
        { slug: otherOrg, org_root: "/h/users/42/orgs/other/org", teams: [{ name: TEAM }] } as Org,
      ],
    });
    rollup[1] = { ...row(1), id: 1, team: { org: otherOrg, team: TEAM } };
    const p = await openInbox();
    const labels = [...p.doc.querySelectorAll(".ib-row .ib-team")].map((n) => n.textContent);
    expect(labels).toEqual(expect.arrayContaining([ORG + " › " + TEAM, otherOrg + " › " + TEAM]));
    setPreview(true);
    await act(async () =>
      activateInbox({ conversation: "Mia", team: { org: otherOrg, team: TEAM } }),
    );
    await waitFor(() => expect(el(p, "thread-team").textContent).toBe(otherOrg + " › " + TEAM));
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    expect(calls("mark_inbox_messages_read")[0][1]).toEqual({
      team: { org: otherOrg, team: TEAM },
      messageIds: [1],
    });
    expect(resolveTarget({ team: { org: "missing", team: TEAM } })).toBeNull();
  });
  it("reinitializes the composer and reads the visible mail when an open role changes team filters", async () => {
    rollup = [
      { ...row(1, "human", null), id: 1, team: { org: ORG, team: TEAM }, toRole: "docs" },
      { ...row(1, "human", null), id: 1, team: { org: ORG, team: "web" }, toRole: "docs" },
      { ...row(2, "Mia"), id: 2, team: { org: ORG, team: TEAM }, inReplyTo: 1 },
      { ...row(2, "Ori"), id: 2, team: { org: ORG, team: "web" }, inReplyTo: 1 },
    ];
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await chooseFilter(p, "web");
    await openThread(p, "#docs");
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    await pick(p, "Ori");
    type(p, "Keep the text");
    await chooseFilter(p, TEAM);
    expect(el(p, "composer-team").textContent).toBe("in" + TEAM);
    expect(el(p, "recipient").textContent).toBe("#docs");
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Keep the text");
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(2));
    expect(calls("mark_inbox_messages_read")[1][1]).toEqual({
      team: { org: ORG, team: TEAM },
      messageIds: [2],
    });
    fireEvent.submit(el(p, "composer"));
    await waitFor(() => expect(calls("send_inbox_as_human")).toHaveLength(1));
    expect(calls("send_inbox_as_human")[0][1]).toMatchObject({
      team: { org: ORG, team: TEAM },
      to: "#docs",
      inReplyTo: 2,
    });
  });

  it("lists a live session with no mail only in the team supplied by its terminal metadata", async () => {
    sessions.push({
      handle: "Fresh",
      displayName: "Fresh",
      state: "running",
      team: { org: ORG, team: "empty" },
    });
    useAgentListProjectionStore.setState({
      sessions: [{ handle: "Fresh", sessionId: null, launchTeamKey: `${ORG}:empty` } as never],
    });
    const p = await openInbox();
    fireEvent.click(el(p, "new-message"));
    await chooseTeam(p, "empty");
    fireEvent.click(el(p, "recipient"));
    expect(el(p, "picker-options").textContent).toContain("Fresh");
    await pick(p, "Fresh");
    await chooseTeam(p, "web");
    fireEvent.click(el(p, "recipient"));
    expect(el(p, "picker-options").textContent).not.toContain("Fresh");
  });

  it("carries attachment and terminal actions from an other-team rollup message", async () => {
    rollup[1].primaryFile = "docs/review.md";
    rollup[1].onClick = { type: "goto-agent", agent_id: "reviewer" };
    const p = await openInbox();
    await openThread(p, "Ori");
    const attachment = [...el(p, "log").querySelectorAll<HTMLElement>("button")].find(
      (b) => b.textContent === "docs/review.md",
    )!;
    expect(attachment).toBeDefined();
    fireEvent.click(attachment);
    await waitFor(() => expect(calls("bridge_resolve_open_target")).toHaveLength(1));
    expect(calls("bridge_resolve_open_target")[0][1]).toEqual({
      frameId: "test",
      page: `${ROOT}/web/content/inbox.html`,
      path: "docs/review.md",
      team: "web",
    });
    expect(el(p, "goto-thread").hidden).toBe(false);
    const actions = vi.spyOn(p.win.codesense, "act");
    fireEvent.click(el(p, "goto-thread"));
    await waitFor(() => expect(actions).toHaveBeenCalledWith("gotoAgent", { agentId: "reviewer" }));
  });

  it("leaves the registered terminal split uncovered when the human-inbox pane is maximized", async () => {
    const column = document.createElement("div");
    const section = document.createElement("div");
    section.getBoundingClientRect = () =>
      ({
        top: 500,
        bottom: window.innerHeight,
        height: window.innerHeight - 500,
        left: 0,
        right: 800,
        width: 800,
      }) as DOMRect;
    column.append(section);
    document.body.append(column);
    useViewerStore.getState().setLayoutMode("shared");
    const unregister = registerTerminalSection(section);
    try {
      await openInbox(true, PANE, undefined, `workspace:${ORG}:${TEAM}:owner/repo`);
      const maximize = screen.getByLabelText("Maximize panel");
      maximize.focus();
      fireEvent.click(maximize);
      const teamKey = `${ORG}:${TEAM}`;
      expect(useViewerStore.getState().maximizedPanel[teamKey]).toBe(PANEL);
      const shell = screen.getByTestId(`${PANEL}-fullscreen-shell`);
      expect(shell.dataset.maximizeScope).toBe("app");
      expect(shell.dataset.fullscreen).toBe("true");
      expect(shell.style.bottom).toBe(`${window.innerHeight - 500}px`);
      fireEvent.click(screen.getByLabelText("Exit full screen (Esc)"));
      expect(shell.dataset.fullscreen).toBe("false");
      expect(document.activeElement).toBe(maximize);
      const frame = screen.getByTestId("pane-body").querySelector("iframe");
      fireEvent.click(maximize);
      expect(shell.dataset.fullscreen).toBe("true");
      fireEvent.keyDown(document, { key: "Escape" });
      expect(shell.dataset.fullscreen).toBe("false");
      expect(screen.getByTestId("pane-body").querySelector("iframe")).toBe(frame);
    } finally {
      unregister();
      column.remove();
    }
  });
  it("reinitializes a restored role after a partial archive removes the draft recipient's team", async () => {
    rollup = [
      { ...row(1, "human", null), id: 1, team: { org: ORG, team: TEAM }, toRole: "docs" },
      { ...row(1, "human", null), id: 1, team: { org: ORG, team: "web" }, toRole: "docs" },
      { ...row(2, "Mia"), id: 2, team: { org: ORG, team: TEAM }, inReplyTo: 1 },
      { ...row(2, "Ori"), id: 2, team: { org: ORG, team: "web" }, inReplyTo: 1, createdAtMs: 3000 },
    ];
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await openThread(p, "#docs");
    await pick(p, "Ori");
    type(p, "Keep the draft");
    archiveFailTeam = TEAM;
    fireEvent.click(el(p, "archive-thread"));
    await waitFor(() => expect(el(p, "list-notice").textContent).toContain("Store unavailable"));
    expect(el(p, "composer-team").textContent).toBe("in" + TEAM);
    expect(el(p, "recipient").textContent).toBe("#docs");
    expect((el(p, "draft") as HTMLTextAreaElement).value).toBe("Keep the draft");
    fireEvent.submit(el(p, "composer"));
    await waitFor(() => expect(calls("send_inbox_as_human")).toHaveLength(1));
    expect(calls("send_inbox_as_human")[0][1]).toMatchObject({
      team: { org: ORG, team: TEAM },
      to: "#docs",
      inReplyTo: 2,
    });
  });
});

it("counts conversations rather than messages and formats markdown tables", async () => {
  messages.push(
    row(3, "Mia", "human", {
      inReplyTo: 1,
      text: "| Name | State |\n| --- | --- |\n| **Build** | Ready |",
    }),
  );
  messages.push(row(4, "Lee", "human", { inReplyTo: 1 }));
  const p = await openInbox();
  await waitFor(() => expect(usePaneStore.getState().badges[PANEL]?.count).toBe(2));
  await openThread(p);
  expect(el(p, "log").querySelector("table tbody td strong")?.textContent).toBe("Build");
});

it("renders a minted inline HTML child through the real shim, HtmlViewer and message bridge", async () => {
  messages[0].contentType = "html";
  messages[0].text =
    "<button id=\"reply\" onclick=\"codesense.act('replyAsHuman',{messageId:1,text:'From the dashboard'})\">Reply</button>";
  const p = await openInbox(true, PANE, undefined, `workspace:${ORG}:${TEAM}:owner/repo`);
  await openThread(p);
  await waitFor(() => expect(el(p, "log").querySelector("iframe")).not.toBeNull());
  const childFrame = el(p, "log").querySelector("iframe")!;
  const record = [...mintedFrames.values()].find((r) => r.parentFrame)!;
  expect(record.parentFrame).toBe("test");
  expect(childFrame.src).not.toContain(record.token);
  const child = bootPaneDoc(childFrame, messages[0].text, record, true);
  docs.push(child);
  await act(async () => {
    await child.win.codesense.act("replyAsHuman", { messageId: 1, text: "From the dashboard" });
  });
  act(() => useViewerStore.getState().setMaximized(`${ORG}:${TEAM}`, PANEL));
  fireEvent.keyDown(child.doc.body, { key: "Escape", code: "Escape" });
  await waitFor(() =>
    expect(useViewerStore.getState().maximizedPanel[`${ORG}:${TEAM}`]).toBeUndefined(),
  );
  expect(useViewerStore.getState().rightPaneOpen).toBe(true);
  expect(calls("reply_as_human").at(-1)?.[1]).toMatchObject({
    team: { org: ORG, team: TEAM },
    messageId: 1,
    text: "From the dashboard",
  });
  await act(async () =>
    window.dispatchEvent(
      new PaneMessageEvent("message", {
        source: childFrame.contentWindow,
        origin: record.origin,
        data: { type: "codesense:content-size", token: record.token, height: 999 },
      }),
    ),
  );
  await waitFor(() => expect(childFrame.style.height).toBe("720px"));
  const before = calls("reply_as_human").length;
  window.dispatchEvent(
    new PaneMessageEvent("message", {
      source: screen.getByTestId("pane-body").querySelector("iframe")!.contentWindow,
      origin: ORIGIN,
      data: {
        type: "codesense:invoke",
        token: record.token,
        id: 999,
        command: "replyAsHuman",
        args: { messageId: 1, text: "forged" },
      },
    }),
  );
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(calls("reply_as_human")).toHaveLength(before);
  await act(async () => inboxChanged());
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(el(p, "log").querySelector("iframe")).toBe(childFrame);
  expect(calls("mint_frame").filter((c) => (c[1] as any)?.args?.parentFrame)).toHaveLength(1);
  expect(hasLiveFrames((f) => f.frameId === record.frameId && f.panelId === PANEL)).toBe(true);
  let flushed = false;
  let tornDown = false;
  (child.win.codesense as any).onFlush(() => {
    flushed = true;
  });
  (child.win.codesense as any).onTeardown(() => {
    tornDown = true;
  });
  await act(async () => flushFrame(record.frameId));
  expect(flushed).toBe(true);
  await waitFor(() => expect(tornDown).toBe(true));
  await openThread(p, "Ori");
  await waitFor(() =>
    expect(calls("release_frame").some((c) => (c[1] as any).frameId === record.frameId)).toBe(true),
  );
});

it("returns docked cross-team message Escape to the host terminal while actions keep the message team", async () => {
  crossTeam = true;
  teamScopedCrossTeam = true;
  rollup = [
    {
      ...row(1),
      id: 1,
      team: { org: ORG, team: "web" },
      contentType: "html",
      text: "<p>Web mail</p>",
    },
    { ...row(2, "Ori"), id: 2, team: { org: ORG, team: TEAM } },
  ];
  const teamKey = `${ORG}:${TEAM}`;
  const previousWorkspace = useViewerStore.getState().activeWorkspace;
  const terminalVisible = useViewerStore.getState().terminalVisible;
  useViewerStore.setState({ activeWorkspace: { [teamKey]: "inbox-focus" }, terminalVisible: true });
  const cli = document.createElement("div");
  cli.dataset.xtermHost = "";
  cli.innerHTML =
    '<div data-pane-id="inbox-focus"><div class="xterm"><textarea></textarea></div></div>';
  document.body.append(cli);
  const terminal = cli.querySelector("textarea")!;
  try {
    const p = await openInbox(true, PANE, undefined, `workspace:${ORG}:${TEAM}:owner/repo`);
    await openThread(p);
    await waitFor(() => expect(el(p, "log").querySelector("iframe")).not.toBeNull());
    const frame = el(p, "log").querySelector("iframe")!;
    const record = [...mintedFrames.values()].find((r) => r.parentFrame)!;
    expect(record.grant.team).toMatchObject({ org: ORG, team: "web" });
    const child = bootPaneDoc(frame, "<p>Web mail</p>", record, true);
    docs.push(child);
    await act(async () => {
      await child.win.codesense.act("replyAsHuman", { messageId: 1, text: "For Web" });
    });
    expect(calls("reply_as_human").at(-1)?.[1]).toMatchObject({
      team: { org: ORG, team: "web" },
      messageId: 1,
      text: "For Web",
    });
    screen.getByTestId("pane-body").querySelector("iframe")!.focus();
    fireEvent.keyDown(child.doc.body, { key: "Escape", code: "Escape" });
    await waitFor(() => expect(document.activeElement).toBe(terminal));
    expect(useOrgStore.getState().activeTeamName).toBe(TEAM);
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
  } finally {
    cli.remove();
    useViewerStore.setState({ activeWorkspace: previousWorkspace, terminalVisible });
  }
});

it("uses the host sanitizer for human-authored HTML and refuses agent-addressed embeds", async () => {
  messages.push(
    row(3, "human", "Mia", {
      contentType: "html",
      text: "<b>Sent HTML</b><script>bad()</script>",
      inReplyTo: 1,
    }),
  );
  const p = await openInbox();
  await openThread(p);
  // The outgoing row is passed explicitly here; ordinary sent-message projection has its own tests.
  await expect(p.win.codesense.act("embedMessage", { messageId: 3 })).resolves.toMatchObject({
    html: (await import("@/features/weft/components/inbox-message-content")).sanitizeInboxHtml(
      messages[2].text,
    ),
  });
  messages.push(row(4, "Mia", "Ori", { contentType: "html", text: "private" }));
  await expect(p.win.codesense.act("embedMessage", { messageId: 4 })).rejects.toThrow(
    "message unavailable",
  );
  expect(
    calls("mint_frame").filter((c) => (c[1] as any)?.args?.message?.messageId === 4),
  ).toHaveLength(0);
});

it("the default team-scoped Inbox under open lists another team's mail keeps its pane placement", async () => {
  crossTeam = true;
  teamScopedCrossTeam = true;
  rollup = [
    { ...row(1), id: 1, team: { org: ORG, team: TEAM } },
    { ...row(2, "Ori"), id: 2, team: { org: ORG, team: "web" } },
  ];
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(2));
  expect(calls("list_inbox_rollup").length).toBeGreaterThan(0);
  expect(el(p, "team-filter").hidden).toBe(false);
  const profile = { ...profileChoice(), slots: {} };
  act(() => useDashboardProfileStore.setState({ profile }));
  const layout = useViewerStore.getState().rightPaneLayout;
  const section = layout.sections.find((s) => s.group.panelIds.includes(PANEL))!;
  expect(section).toBeDefined();
  const { buildPanelMenuEntries } = await import("../panel-menu");
  expect(buildPanelMenuEntries().filter((p) => p.label === "Inbox")).toHaveLength(1);
  expect(section.group.activePanelId).toBe(PANEL);
});

it("hands focus to the list on reveal, restores it on close, and forwards unhandled Escape", async () => {
  setPreview(true);
  const button = document.createElement("button");
  document.body.append(button);
  button.focus();
  const p = await openInbox(
    true,
    PANE,
    async () => {
      act(() => revealSlot("right-pane", { surface: "inbox", intent: { focusList: true } }));
    },
    `workspace:${ORG}:${TEAM}:owner/repo`,
  );
  await waitFor(() => expect(p.doc.activeElement).toBe(el(p, "conversations")));
  p.win.dispatchEvent(new p.win.Event("focus") as never);
  await waitFor(() => expect(p.sent.some((m) => m.type === "codesense:frame-focus")).toBe(true));
  expect(p.doc.activeElement).toBe(el(p, "conversations"));
  const teamKey = `${ORG}:${TEAM}`;
  act(() => useViewerStore.getState().setMaximized(teamKey, PANEL));
  fireEvent.keyDown(el(p, "conversations"), { key: "Escape", code: "Escape", bubbles: true });
  await waitFor(() => expect(useViewerStore.getState().maximizedPanel[teamKey]).toBeUndefined());
  expect(useViewerStore.getState().rightPaneOpen).toBe(true);
  await openThread(p);
  el(p, "draft").focus();
  const focusMessages = p.sent.filter((m) => m.type === "codesense:frame-focus").length;
  p.win.dispatchEvent(new p.win.Event("focus") as never);
  await waitFor(() =>
    expect(p.sent.filter((m) => m.type === "codesense:frame-focus")).toHaveLength(
      focusMessages + 1,
    ),
  );
  expect(p.doc.activeElement).toBe(el(p, "draft"));
  act(() => revealSlot("right-pane", { surface: "inbox" }));
  await waitFor(() => expect(document.activeElement).toBe(button));
  button.remove();
});

it("forwards both inbox chords from the focused pane and toggles on Shift-Command-I", async () => {
  setPreview(true);
  const p = await openInbox();
  await openThread(p);
  fireEvent.keyDown(el(p, "draft"), { key: "i", code: "KeyI", metaKey: true });
  await waitFor(() =>
    expect(p.sent).toContainEqual(
      expect.objectContaining({
        type: "codesense:shortcut",
        key: "i",
        metaKey: true,
        shiftKey: false,
      }),
    ),
  );
  fireEvent.keyDown(el(p, "draft"), { key: "I", code: "KeyI", metaKey: true, shiftKey: true });
  await waitFor(() => expect(useViewerStore.getState().rightPaneOpen).toBe(false));
  expect(p.sent.filter((m) => m.type === "codesense:shortcut").every((m) => !m.action)).toBe(true);
});

it.each([
  [639, "stack"],
  [640, "split"],
  [960, "wide"],
] as const)(
  "reopens at %ipx with the list focused and visible messages marked read",
  async (width, layout) => {
    crossTeam = true;
    teamScopedCrossTeam = true;
    rollup = messages.map((m) => ({ ...m, id: m.messageId, team: { org: ORG, team: TEAM } }));
    setPreview(true);
    const p = await openInbox();
    p.win.innerWidth = width;
    p.win.dispatchEvent(new p.win.Event("resize") as never);
    expect(el(p, "inbox").dataset.layout).toBe(layout);
    await openThread(p);
    await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(1));
    expect(el(p, "inbox").dataset.view).toBe("thread");
    act(() => revealSlot("right-pane", { surface: "inbox" }));
    await waitFor(() => expect(useViewerStore.getState().rightPaneOpen).toBe(false));
    await new Promise((r) => setTimeout(r, 30));
    rollup.push({ ...row(5), id: 5, team: { org: ORG, team: TEAM } });
    inboxChanged();
    act(() => revealSlot("right-pane", { surface: "inbox" }));
    await waitFor(() => {
      expect(el(p, "inbox").dataset.view).toBe(layout === "stack" ? "list" : "thread");
      expect(p.doc.activeElement).toBe(el(p, "conversations"));
    });
    if (layout !== "stack") {
      await waitFor(() => expect(calls("mark_inbox_messages_read")).toHaveLength(2));
      expect(calls("mark_inbox_messages_read")[1][1]).toEqual({
        team: { org: ORG, team: TEAM },
        messageIds: [5],
      });
    }
  },
);

it("gives hosted message children the pane theme and subsequent theme changes", async () => {
  messages[0].contentType = "html";
  let surface: HTMLElement;
  const p = await openInbox(true, PANE, async () => {
    surface = screen.getByTestId("pane-body").querySelector<HTMLElement>("[data-slot-id]")!;
    surface.style.setProperty("--background", "#ffffff");
    await act(async () => {
      liveHub.publishTheme("codesense", "light", true);
    });
  });
  await openThread(p);
  await waitFor(() => expect(el(p, "log").querySelector("iframe")).not.toBeNull());
  const frame = el(p, "log").querySelector("iframe")!;
  const initialUrl = frame.src;
  expect(JSON.parse(new URL(initialUrl).searchParams.get("theme")!).scheme).toBe("light");
  const paneFrame = screen
    .getByTestId("pane-body")
    .querySelector<HTMLIFrameElement>("[data-pane-current] iframe")!;
  const paneUrl = paneFrame.src;
  expect(JSON.parse(new URL(paneUrl).searchParams.get("theme")!).tokens["--cs-bg"]).toBe("#ffffff");
  const record = [...mintedFrames.values()].find((r) => r.parentFrame)!;
  const child = bootPaneDoc(frame, "<p>Theme</p>", record, true);
  docs.push(child);
  const theme = (await child.win.codesense.theme()) as any;
  expect(theme.tokens["--cs-bg"]).toBe("#ffffff");
  surface!.style.setProperty("--background", "#111111");
  await act(async () => {
    liveHub.publishTheme("codesense", "dark", true);
  });
  await waitFor(() =>
    expect(child.doc.documentElement.style.getPropertyValue("--cs-bg")).toBe("#111111"),
  );
  expect(((await child.win.codesense.theme()) as any).scheme).toBe("dark");
  const darkEmbed = (await p.win.codesense.act("embedMessage", { messageId: 1 })) as {
    url: string;
  };
  expect(JSON.parse(new URL(darkEmbed.url).searchParams.get("theme")!).scheme).toBe("dark");
  expect(JSON.parse(new URL(darkEmbed.url).searchParams.get("theme")!).tokens["--cs-bg"]).toBe(
    "#111111",
  );
  expect(frame.src).toBe(initialUrl);
  surface!.style.setProperty("--background", "#ffffff");
  await act(async () => {
    liveHub.publishTheme("codesense", "light", true);
  });
  await waitFor(() =>
    expect(child.doc.documentElement.style.getPropertyValue("--cs-bg")).toBe("#ffffff"),
  );
  expect(((await child.win.codesense.theme()) as any).scheme).toBe("light");
  expect(frame.src).toBe(initialUrl);
  expect(paneFrame.src).toBe(paneUrl);
});

it("uses a narrowed effective inbox grant even when the policy scope is open", async () => {
  crossTeam = true;
  teamScopedCrossTeam = true;
  narrowedInboxGrant = true;
  rollup = [
    { ...row(1), id: 1, team: { org: ORG, team: TEAM } },
    { ...row(2, "Ori"), id: 2, team: { org: ORG, team: "web" } },
  ];
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  expect(el(p, "inbox").dataset.mode).not.toBe("all");
  expect(calls("list_inbox_rollup")).toHaveLength(0);
  expect(calls("agent_read_inbox").length).toBeGreaterThan(0);
});

it("a late A embed cannot revoke its replacement after navigating A to B to A", async () => {
  messages[0].contentType = "html";
  const p = await openInbox();
  const ipc = tauriInvoke.getMockImplementation()!;
  let first: import("@/tauri/commands").FrameRecord | undefined;
  let finish: (() => void) | undefined;
  tauriInvoke.mockImplementation(async (command: string, args?: Record<string, any>) => {
    const result = await ipc(command, args);
    if (command === "mint_frame" && args?.args.parentFrame && !first) {
      first = result as import("@/tauri/commands").FrameRecord;
      return new Promise((resolve) => {
        finish = () => resolve(result);
      });
    }
    return result;
  });
  await openThread(p);
  await waitFor(() => expect(finish).toBeDefined());
  await openThread(p, "Ori");
  await openThread(p, "Mia");
  await waitFor(() => expect(el(p, "log").querySelector("iframe")).not.toBeNull());
  const replacement = el(p, "log").querySelector("iframe")!;
  const current = [...mintedFrames.values()].find(
    (r) => r.parentFrame && r.frameId !== first!.frameId,
  )!;
  await act(async () => finish!());
  await waitFor(() =>
    expect(calls("release_frame").some((c) => (c[1] as any).frameId === first!.frameId)).toBe(true),
  );
  expect(calls("release_frame").some((c) => (c[1] as any).frameId === current.frameId)).toBe(false);
  expect(mintedFrames.has(current.origin)).toBe(true);
  expect(el(p, "log").querySelector("iframe")).toBe(replacement);
});

it("refreshes delivery receipts on a reused HTML message without rebuilding its body", async () => {
  messages.push(
    row(3, "human", "role:docs", {
      inReplyTo: 1,
      contentType: "html",
      text: "<b>Sent</b>",
      delivery: { state: "pending", instance: null },
    }),
  );
  const p = await openInbox();
  await openThread(p);
  const article = el(p, "message-3");
  await waitFor(() => expect(article.querySelector(".ib-sent-html")).not.toBeNull());
  const body = article.querySelector(".bd");
  expect(article.querySelector(".foot")?.textContent).toContain("Pending");
  messages[2].delivery = { state: "reading", instance: "Ori" };
  await act(async () => inboxChanged());
  await waitFor(() =>
    expect(article.querySelector(".foot")?.textContent).toContain("Reading · Ori"),
  );
  expect(article.querySelector(".bd")).toBe(body);
});

it("reaches a summary-only unread conversation and loads its newest page", async () => {
  messages = [
    row(1, "Old worker"),
    ...Array.from({ length: 500 }, (_, n) => row(n + 2, "Mia", "human", { unread: false })),
  ];
  const p = await openInbox();
  await waitFor(() => expect(usePaneStore.getState().badges[PANEL]?.count).toBe(1));
  const old = [...p.doc.querySelectorAll(".ib-row")].find((r) =>
    r.textContent?.includes("Old worker"),
  )!;
  expect(old.querySelector(".ib-count")?.textContent).toBe("1");
  await openThread(p, "Old worker");
  await waitFor(() => expect(el(p, "log").textContent).toContain("Message 1"));
  expect(
    calls("agent_read_inbox").some(
      ([, args]) => args.query.conversation === "Old worker" && args.query.limit === 50,
    ),
  ).toBe(true);
  expect(el(p, "log").firstElementChild?.classList.contains("ib-older")).toBe(true);
  await waitFor(() => expect(el(p, "log").textContent).toContain("Start of conversation"));
});

it("loads 50 older rows, suppresses auto retry, and retries with one status outside the log", async () => {
  messages = Array.from({ length: 570 }, (_, n) => row(n + 1, "Mia", "human", { unread: false }));
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p);
  const load = () =>
    [...el(p, "log").querySelectorAll<HTMLButtonElement>("button")].find((b) =>
      /Load older|Try again/.test(b.textContent || ""),
    )!;
  expect(load()).toBeDefined();
  const original = tauriInvoke.getMockImplementation()!;
  let fail = true;
  tauriInvoke.mockImplementation(async (cmd, args) => {
    if (cmd === "agent_read_inbox" && args?.query.conversation && fail) throw new Error("offline");
    return original(cmd, args);
  });
  await act(async () => load().click());
  await waitFor(() => expect(el(p, "log").textContent).toContain("Couldn't load older messages."));
  expect(load().textContent).toBe("Try again");
  const before = calls("agent_read_inbox").length;
  await new Promise((r) => setTimeout(r, 80));
  expect(calls("agent_read_inbox")).toHaveLength(before);
  fail = false;
  await act(async () => load().click());
  await waitFor(() => expect(el(p, "older-status").textContent).toBe("Loaded 50 older messages."));
  expect(el(p, "log").contains(el(p, "older-status"))).toBe(false);
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(550);
  expect(p.doc.activeElement).toBe(el(p, "log"));
  await act(async () => load().click());
  await waitFor(() =>
    expect(el(p, "older-status").textContent).toBe(
      "Loaded 20 older messages. Start of conversation.",
    ),
  );
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(570);
});

it("pages cross-team role history team by team until every team ends", async () => {
  crossTeam = true;
  const teams = [
    { org: ORG, team: TEAM },
    { org: ORG, team: "web" },
  ];
  rollup = teams.flatMap((team, index) =>
    Array.from({ length: index ? 620 : 550 }, (_, n) => ({
      id: n + 1,
      team,
      from: "human",
      fromName: "You",
      to: null,
      toRole: "docs",
      text: "Role history " + (n + 1),
      createdAtMs: (n + 1) * 1000,
      unread: false,
      contentType: "markdown",
      inReplyTo: null,
    })),
  );
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p, "#docs");
  const load = () => el(p, "log").querySelector<HTMLButtonElement>(".ib-older button")!;
  await act(async () => load().click());
  await waitFor(() => expect(el(p, "older-status").textContent).toBe("Loaded 100 older messages."));
  expect(calls("agent_read_inbox").map(([, args]) => args.team.team)).toEqual([TEAM, "web"]);
  expect(load().textContent).toBe("Load older messages");
  await act(async () => load().click());
  await waitFor(() => expect(el(p, "older-status").textContent).toBe("Loaded 50 older messages."));
  await act(async () => load().click());
  await waitFor(() =>
    expect(el(p, "older-status").textContent).toBe(
      "Loaded 20 older messages. Start of conversation.",
    ),
  );
  expect(calls("agent_read_inbox").map(([, args]) => args.team.team)).toEqual([
    TEAM,
    "web",
    "web",
    "web",
  ]);
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(1170);
});

it("honors Mark unread while opening is still scanning older unread history", async () => {
  messages = Array.from({ length: 570 }, (_, n) => row(n + 1, "Mia", "human", { unread: n < 70 }));
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  const original = tauriInvoke.getMockImplementation()!;
  let release!: () => void;
  const scanning = new Promise<void>((resolve) => {
    release = resolve;
  });
  tauriInvoke.mockImplementation(async (cmd, args) => {
    const result = await original(cmd, args);
    if (
      cmd === "agent_read_inbox" &&
      args?.query.scope === "to-human" &&
      args.query.conversation === "Mia"
    )
      await scanning;
    return result;
  });
  await openThread(p);
  await waitFor(() =>
    expect(
      calls("agent_read_inbox").some(
        ([, args]) => args.query.scope === "to-human" && args.query.conversation === "Mia",
      ),
    ).toBe(true),
  );
  await menuAction(p, "mark-unread", { kind: "message", messageId: 570 });
  await act(async () => {
    await p.win.codesense.act("markUnread", { messageIds: [570] });
    release();
  });
  const before = calls("agent_read_inbox").length;
  inboxChanged();
  await waitFor(() => expect(calls("agent_read_inbox").length).toBeGreaterThan(before));
  await waitFor(() => expect(el(p, "latest-count").textContent).toBe("Marked unread"));
  expect(calls("mark_inbox_messages_read")).toHaveLength(0);
  expect(messages.filter((m) => m.unread)).toHaveLength(71);
  await waitFor(() => expect(usePaneStore.getState().badges[PANEL]?.count).toBe(1));
});

it("reads an old unread message beyond ten incoming history pages", async () => {
  messages = Array.from({ length: 2101 }, (_, n) =>
    row(n + 1, "Mia", "human", { unread: n === 0 }),
  );
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p);
  await waitFor(() => expect(messages[0].unread).toBe(false));
  expect(
    calls("agent_read_inbox").filter(
      ([, args]) => args.query.scope === "to-human" && args.query.conversation === "Mia",
    ).length,
  ).toBeGreaterThan(10);
  expect(calls("mark_inbox_messages_read").flatMap(([, args]) => args.messageIds)).toContain(1);
  await waitFor(() => expect(usePaneStore.getState().badges[PANEL]).toBeUndefined());
});

it("keeps an older unread conversation reachable behind archived cached rows", async () => {
  messages = [
    row(1),
    ...Array.from({ length: 500 }, (_, n) =>
      row(n + 2, "Mia", "human", { archivedAtMs: Date.now(), unread: false }),
    ),
  ];
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  expect(p.doc.querySelector(".ib-count")?.textContent).toBe("1");
  expect(usePaneStore.getState().badges[PANEL]?.count).toBe(1);
});

it.each(["archived", "non-human"])(
  "advances the raw cursor across a %s-only page",
  async (kind) => {
    messages = [
      ...Array.from({ length: 200 }, (_, n) =>
        row(n + 1, "Mia", n >= 50 && n < 100 && kind === "non-human" ? "Ori" : "human", {
          unread: false,
          ...(n >= 50 && n < 100 && kind === "archived" ? { archivedAtMs: Date.now() } : {}),
        }),
      ),
      ...Array.from({ length: 400 }, (_, n) => row(n + 201, "Ori", "human", { unread: false })),
    ];
    const p = await openInbox();
    await openThread(p);
    const control = () => el(p, "log").querySelector<HTMLButtonElement>(".ib-older button")!;
    const reads = () =>
      calls("agent_read_inbox").filter(([, args]) => args.query.conversation === "Mia");
    await act(async () => control().click());
    await waitFor(() => expect(reads()).toHaveLength(1));
    await waitFor(() => expect(control().getAttribute("aria-disabled")).toBe("false"));
    await act(async () => control().click());
    await waitFor(() => expect(reads()).toHaveLength(2));
    expect(reads().map(([, args]) => args.query.before)).toEqual([101, 51]);
    await waitFor(() => expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(150));
    expect(el(p, "log").textContent).toContain("Start of conversation");
  },
);

it("opening a 570-row thread marks all 70 older unread messages even outside its loaded range", async () => {
  messages = Array.from({ length: 570 }, (_, n) => row(n + 1, "Mia", "human", { unread: n < 70 }));
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p);
  await waitFor(() =>
    expect(
      [...new Set(calls("mark_inbox_messages_read").flatMap(([, args]) => args.messageIds))].sort(
        (a, b) => a - b,
      ),
    ).toEqual(Array.from({ length: 70 }, (_, n) => n + 1)),
  );
  expect(messages.every((m) => !m.unread)).toBe(true);
  await waitFor(() => expect(usePaneStore.getState().badges[PANEL]).toBeUndefined());
});

it("optimistic reading keeps 500 loaded rows and preserves the New marker when paging older", async () => {
  messages = Array.from({ length: 570 }, (_, n) => row(n + 1, "Mia", "human", { unread: n < 100 }));
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p);
  await waitFor(() => expect(messages.every((m) => !m.unread)).toBe(true));
  const refreshed = calls("agent_read_inbox").length;
  inboxChanged();
  await waitFor(() => expect(calls("agent_read_inbox").length).toBeGreaterThan(refreshed));
  // Reading must not turn an unread-only summary change into a 500 → 50 range reset.
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(500);
  const control = () => el(p, "log").querySelector<HTMLButtonElement>(".ib-older button")!;
  for (let page = 0; page < 2; page++) {
    await act(async () => control().click());
    await waitFor(() =>
      expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(page ? 570 : 550),
    );
    expect(el(p, "log").querySelectorAll(".ib-new")).toHaveLength(1);
    expect(
      (el(p, "log").querySelector(".ib-new")?.nextElementSibling as HTMLElement).dataset.messageId,
    ).toBe("71");
  }
});

it.each(["summary-only", "partly loaded"])(
  "archives all pages of a %s conversation",
  async (mode) => {
    const total = mode === "summary-only" ? 120 : 570;
    messages = Array.from({ length: total }, (_, n) =>
      row(n + 1, "Mia", "human", { unread: true }),
    );
    if (mode === "summary-only")
      messages.push(
        ...Array.from({ length: 500 }, (_, n) => row(n + 1000, "Ori", "human", { unread: false })),
      );
    const p = await openInbox(false);
    await waitFor(() =>
      expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(mode === "summary-only" ? 2 : 1),
    );
    const mia = [...p.doc.querySelectorAll<HTMLElement>(".ib-row")].find((r) =>
      r.textContent?.includes("Mia"),
    )!;
    await act(async () => mia.querySelector<HTMLButtonElement>("button")!.click());
    await waitFor(() => expect(calls("mark_inbox_messages_archived")).toHaveLength(1));
    const ids = calls("mark_inbox_messages_archived")[0][1].messageIds;
    expect(new Set(ids).size).toBe(total);
    expect(ids).toContain(1);
    expect(ids).toContain(total);
    expect(messages.filter((m) => m.from === "Mia").every((m) => m.archivedAtMs && !m.unread)).toBe(
      true,
    );
    await waitFor(() =>
      expect(
        [...p.doc.querySelectorAll(".ib-row")].some((r) => r.textContent?.includes("Mia")),
      ).toBe(false),
    );
  },
);

it("archives a summary-only role conversation across every represented team", async () => {
  crossTeam = true;
  rollup = [TEAM, "web"].flatMap((team) => [
    ...Array.from({ length: 120 }, (_, n) => ({
      ...row(n + 1, "human", null, { unread: false }),
      id: n + 1,
      toRole: "docs",
      team: { org: ORG, team },
    })),
    ...Array.from({ length: 500 }, (_, n) => ({
      ...row(n + 1000, "Ori", "human", { unread: false }),
      id: n + 1000,
      team: { org: ORG, team },
    })),
  ]);
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(3));
  const role = [...p.doc.querySelectorAll<HTMLElement>(".ib-row")].find((r) =>
    r.textContent?.includes("#docs"),
  )!;
  await act(async () => role.querySelector<HTMLButtonElement>("button")!.click());
  await waitFor(() => expect(calls("mark_inbox_messages_archived")).toHaveLength(2));
  expect(
    calls("mark_inbox_messages_archived").map(([, args]) => [
      args.team.team,
      new Set(args.messageIds).size,
    ]),
  ).toEqual(
    expect.arrayContaining([
      [TEAM, 120],
      ["web", 120],
    ]),
  );
  await waitFor(() =>
    expect(
      [...p.doc.querySelectorAll(".ib-row")].some((r) => r.textContent?.includes("#docs")),
    ).toBe(false),
  );
});

it("keeps all outgoing role history archived beyond 500 rows after reopening", async () => {
  messages = Array.from({ length: 570 }, (_, n) =>
    row(n + 1, "human", "role:docs", { unread: false }),
  );
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p, "#docs");
  fireEvent.click(el(p, "archive-thread"));
  await waitFor(() => expect(calls("mark_inbox_messages_archived")).toHaveLength(1));
  expect(calls("mark_inbox_messages_archived")[0][1].messageIds).toHaveLength(570);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(0));
  await waitFor(() => expect(appState).toContain("570"));
  cleanup();
  const reopened = await openInbox(false);
  await waitFor(() =>
    expect(el(reopened, "load-state").textContent).toContain("No conversations yet"),
  );
  expect(reopened.doc.querySelectorAll(".ib-row")).toHaveLength(0);
  messages.push(row(571, "human", "role:docs", { unread: false }));
  inboxChanged();
  await waitFor(() => expect(reopened.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(reopened, "#docs");
  await waitFor(() => expect(el(reopened, "log").querySelectorAll(".ib-msg")).toHaveLength(1));
  expect(el(reopened, "log").textContent).toContain("Message 571");
});

it.each(["read", "archive"])(
  "reopens exhausted history for newer mail missed by a bounded refresh before %s",
  async (action) => {
    messages = [row(1, "Mia", "human", { unread: false })];
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await openThread(p);
    expect(el(p, "log").textContent).toContain("Start of conversation");
    // More than one new page arrives, followed by a busier conversation that fills the refresh.
    messages.push(
      ...Array.from({ length: 70 }, (_, n) => row(n + 2)),
      ...Array.from({ length: 500 }, (_, n) => row(n + 72, "Ori", "human", { unread: false })),
    );
    inboxChanged();
    await waitFor(() => expect(el(p, "thread-meta").textContent).toBe("71 messages"));
    if (action === "read") {
      await waitFor(() => expect(el(p, "log").textContent).toContain("Message 71"));
      expect(
        calls("agent_read_inbox").some(
          ([, args]) => args.query.conversation === "Mia" && !args.query.before,
        ),
      ).toBe(true);
      await act(async () =>
        el(p, "log").querySelector<HTMLButtonElement>(".ib-older button")!.click(),
      );
      await waitFor(() => expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(71));
      expect(el(p, "log").textContent).toContain("Start of conversation");
      await waitFor(() =>
        expect(messages.filter((m) => m.from === "Mia").every((m) => !m.unread)).toBe(true),
      );
    } else {
      fireEvent.click(el(p, "archive-thread"));
      await waitFor(() => expect(calls("mark_inbox_messages_archived")).toHaveLength(1));
      expect(calls("mark_inbox_messages_archived")[0][1].messageIds).toEqual(
        Array.from({ length: 71 }, (_, n) => n + 1),
      );
      expect(messages.filter((m) => m.from === "Mia").every((m) => m.archivedAtMs)).toBe(true);
    }
  },
);

it("replaces retained history when 600 new messages have no overlap with the loaded range", async () => {
  messages = Array.from({ length: 600 }, (_, n) => row(n + 1, "Mia", "human", { unread: false }));
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p);
  const older = () => el(p, "log").querySelector<HTMLButtonElement>(".ib-older button")!;
  for (let n = 0; n < 2; n++) {
    await act(async () => older().click());
    await waitFor(() =>
      expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(550 + n * 50),
    );
  }
  await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
  messages.push(
    ...Array.from({ length: 600 }, (_, n) => row(n + 601, "Mia", "human", { unread: false })),
  );
  inboxChanged();
  const before = calls("agent_read_inbox").length;
  await act(async () => useViewerStore.setState({ rightPaneOpen: true }));
  await waitFor(() => expect(el(p, "thread-meta").textContent).toBe("1200 messages"));
  await waitFor(() => expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(50));
  await act(async () => {
    inboxChanged();
    await new Promise((resolve) => setTimeout(resolve, 100));
  });
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(50);
  expect(el(p, "log").querySelector("#message-300")).toBeNull();
  for (let n = 0; n < 11; n++) {
    await act(async () => older().click());
    await waitFor(() =>
      expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(100 + n * 50),
    );
  }
  const ids = [...el(p, "log").querySelectorAll<HTMLElement>(".ib-msg")].map((m) =>
    Number(m.dataset.messageId),
  );
  expect(ids).toEqual(Array.from({ length: 600 }, (_, n) => n + 601));
  expect(
    calls("agent_read_inbox")
      .slice(before)
      .filter(([, args]) => args.query.conversation === "Mia")
      .map(([, args]) => args.query.before),
  ).toEqual([undefined, ...Array.from({ length: 11 }, (_, n) => 1151 - n * 50)]);
}, 15000);

it("drops retained bubbles after another pane archives older mail", async () => {
  messages = Array.from({ length: 71 }, (_, n) => row(n + 1, "Mia", "human", { unread: false }));
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p);
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(71);
  messages.filter((m) => m.messageId < 71).forEach((m) => (m.archivedAtMs = Date.now()));
  inboxChanged();
  await waitFor(() => expect(el(p, "thread-meta").textContent).toBe("1 message"));
  await waitFor(() => expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(1));
  expect(el(p, "log").querySelector<HTMLElement>(".ib-msg")?.dataset.messageId).toBe("71");
  expect(
    calls("agent_read_inbox").some(
      ([, args]) => args.query.conversation === "Mia" && !args.query.before,
    ),
  ).toBe(true);
});

it.each(["incoming", "outgoing"])(
  "keeps the open reader's older range and cursor when one %s message overlaps it",
  async (direction) => {
    messages = Array.from({ length: 600 }, (_, n) => row(n + 1, "Mia", "human", { unread: false }));
    const p = await openInbox(false);
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
    await openThread(p);
    const anchor = el(p, "log").querySelector<HTMLElement>("#message-300")!;
    expect(anchor).not.toBeNull();
    anchor.getBoundingClientRect = () => ({ top: 40, bottom: 140 }) as DOMRect;
    el(p, "log").scrollTop = 3000;
    messages.push(
      row(
        601,
        direction === "incoming" ? "Mia" : "human",
        direction === "incoming" ? "human" : "Mia",
        {
          unread: direction === "incoming",
        },
      ),
    );
    inboxChanged();
    await waitFor(() => expect(el(p, "thread-meta").textContent).toBe("601 messages"));
    if (direction === "incoming")
      await waitFor(() => expect(usePaneStore.getState().badges[PANEL]).toBeUndefined());
    const ids = () =>
      [...el(p, "log").querySelectorAll<HTMLElement>(".ib-msg")].map((m) =>
        Number(m.dataset.messageId),
      );
    expect(ids()).toEqual(Array.from({ length: 501 }, (_, n) => n + 101));
    expect(el(p, "log").querySelector("#message-300")).not.toBeNull();
    await act(async () =>
      el(p, "log").querySelector<HTMLButtonElement>(".ib-older button")!.click(),
    );
    await waitFor(() => expect(ids()).toHaveLength(551));
    expect(
      calls("agent_read_inbox")
        .filter(([, args]) => args.query.conversation === "Mia")
        .map(([, args]) => args.query.before),
    ).toEqual([undefined, 101]);
  },
);

it("replaces open history when archive and growth together are not a verified append", async () => {
  messages = [row(1, "Mia", "human", { unread: false }), row(2, "human", "Mia", { unread: false })];
  const p = await openInbox(false);
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p);
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(2);
  messages[0].archivedAtMs = Date.now();
  messages.push(
    row(3, "Mia", "human", { unread: false }),
    row(4, "Mia", "human", { unread: false }),
    ...Array.from({ length: 500 }, (_, n) => row(n + 5, "Ori", "human", { unread: false })),
  );
  inboxChanged();
  await waitFor(() => expect(el(p, "thread-meta").textContent).toBe("3 messages"));
  expect(
    [...el(p, "log").querySelectorAll<HTMLElement>(".ib-msg")].map((m) =>
      Number(m.dataset.messageId),
    ),
  ).toEqual([2, 3, 4]);
  expect(el(p, "log").querySelector("#message-1")).toBeNull();
});

describe("inbox commands", () => {
  const commandId = (id: string) =>
    dashboardCommandActionId(dashboardRefKey(listing().panes[0].dashboardRef!), id);
  const threadName = (p: PaneDoc) => el(p, "thread-name").textContent;
  async function openWithCommands() {
    setPreview(true);
    const p = await openInbox();
    act(() => syncDashboardCommands());
    return p;
  }
  beforeEach(() => useShortcutsStore.setState({ shortcuts: {}, stateLoaded: true }));
  afterEach(() => {
    // An empty listing changes the synced signature, so the next sync re-registers.
    usePaneStore.setState({ listing: null, knownEntries: {} });
    syncDashboardCommands();
    setDynamicShortcutActions([]);
  });

  it("lists every command under the Inbox in declaration order, with ⌘B waiting on its conflict", async () => {
    await openWithCommands();
    const actions = allShortcutActions().filter((a) => a.command?.dashboardTitle === "Inbox");
    expect(actions.map((a) => [a.label, a.command?.icon])).toEqual([
      ["Next conversation", "chevron-down"],
      ["Previous conversation", "chevron-up"],
      ["Back to list", "arrow-left"],
      ["Reply", "reply"],
      ["Go to session", "square-terminal"],
      ["Archive conversation", "archive"],
      ["New message", "square-pen"],
      ["Filter by team", "layers"],
      ["Toggle conversation list", "panel-left"],
    ]);
    const bindings = resolveBindings({});
    expect(actions.slice(0, 8).every((a) => bindings.get(a.id)?.keystroke === "")).toBe(true);
    expect(bindings.get(commandId("toggle-list"))).toMatchObject({
      keystroke: "",
      unbound: { kind: "conflict", wanted: "CmdOrCtrl+B" },
    });
  });

  it("delivers a rebound chord from the focused pane, never matches a plain j in a terminal", async () => {
    const p = await openWithCommands();
    await openThread(p, "Ori");
    expect(threadName(p)).toBe("Ori");
    expect(
      useShortcutsStore.getState().setShortcut(commandId("next-conversation"), "Alt+J"),
    ).toMatchObject({ ok: true });
    const frame = screen.getByTestId("pane-body").querySelector("iframe")!;
    frame.focus();
    expect(document.activeElement).toBe(frame);
    expect(findShortcutAction(commandId("next-conversation"))!.available!()).toBe(true);
    fireEvent.keyDown(el(p, "log"), { key: "j", code: "KeyJ", altKey: true });
    await waitFor(() => expect(threadName(p)).toBe("Mia"));
    const xterm = document.createElement("div");
    xterm.className = "xterm";
    const pty = document.createElement("textarea");
    xterm.append(pty);
    document.body.append(xterm);
    pty.focus();
    const gate = {
      settingsPanelOpen: false,
      hasActiveWorkspace: true,
      hasTeam: true,
      rowPopupFocused: false,
    };
    for (const key of ["j", "k", "r", "g", "c"]) {
      const e = new KeyboardEvent("keydown", { key, code: `Key${key.toUpperCase()}` });
      Object.defineProperty(e, "target", { value: pty });
      expect(matchShortcut(e, useShortcutsStore.getState().shortcuts, gate)).toBeUndefined();
    }
    xterm.remove();
  });

  it("runs a command picked from ⌘K while the palette holds focus", async () => {
    const p = await openWithCommands();
    const palette = document.createElement("input");
    document.body.append(palette);
    palette.focus();
    act(() => findShortcutAction(commandId("new-message"))!.run());
    await waitFor(() => expect(threadName(p)).toBe("New message"));
    palette.remove();
  });

  it("runs commands queued before the first mount once, in order, after prefs and initial inbox load", async () => {
    let releasePrefs!: () => void;
    let releaseInbox!: () => void;
    const prefsPending = new Promise<void>((resolve) => (releasePrefs = resolve));
    const inboxPending = new Promise<void>((resolve) => (releaseInbox = resolve));
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "load_app_state") await prefsPending;
      if (cmd === "agent_read_inbox") await inboxPending;
      return stubTauri(cmd, args);
    });
    setPreview(true);
    usePaneStore.getState().setListing(`${ORG}:${TEAM}`, listing());
    syncDashboardCommands();
    // Both palette actions and command_run reach the host's real pre-mount queue.
    findShortcutAction(commandId("new-message"))!.run();
    runDashboardCommand(commandId("toggle-list"));
    const p = await openInbox(false);
    const received = vi.fn();
    p.win.codesense.onCommand(received);
    const focusDraft = vi.spyOn(el(p, "draft"), "focus");
    await waitFor(() => expect(received).toHaveBeenCalledTimes(2));
    expect((el(p, "new-message") as HTMLButtonElement).disabled).toBe(true);
    expect(threadName(p)).not.toBe("New message");

    await act(async () => releasePrefs());
    await waitFor(() => expect(calls("agent_read_inbox").length).toBeGreaterThan(0));
    expect(threadName(p)).not.toBe("New message");
    expect(focusDraft).not.toHaveBeenCalled();

    await act(async () => releaseInbox());
    await waitFor(() => expect(threadName(p)).toBe("New message"));
    expect(el(p, "inbox").dataset.list).toBe("hidden");
    expect(focusDraft).toHaveBeenCalledTimes(1);
    messages.push(row(3, "Lee"));
    await act(async () => inboxChanged());
    await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(3));
    expect(focusDraft).toHaveBeenCalledTimes(1);
    expect(el(p, "inbox").dataset.list).toBe("hidden");
  });

  it("answers the plain letters from the page, but never while typing or in a menu", async () => {
    messages.push(row(3, "Mia", "human", { onClick: { type: "goto-agent", agentId: "mia" } }));
    const p = await openWithCommands();
    await openThread(p, "Mia");
    const actions = vi.spyOn(p.win.codesense, "act");
    const press = (target: Element, key: string, init: KeyboardEventInit = {}) =>
      fireEvent.keyDown(target, { key, ...init });

    el(p, "log").focus();
    press(el(p, "log"), "j");
    await waitFor(() => expect(threadName(p)).toBe("Ori"));
    press(el(p, "log"), "k");
    await waitFor(() => expect(threadName(p)).toBe("Mia"));
    press(el(p, "log"), "g");
    await waitFor(() => expect(actions).toHaveBeenCalledWith("gotoAgent", { agentId: "mia" }));
    press(el(p, "log"), "r");
    expect(p.doc.activeElement).toBe(el(p, "draft"));

    // Typing: the composer, the recipient search and an open menu keep their letters.
    for (const key of ["j", "k", "c", "g", "r"]) expect(press(el(p, "draft"), key)).toBe(true);
    fireEvent.click(el(p, "recipient"));
    expect(press(el(p, "recipient-search"), "j")).toBe(true);
    expect(press(el(p, "log"), "c")).toBe(true);
    fireEvent.keyDown(el(p, "recipient-search"), { key: "Escape" });
    expect(press(el(p, "log"), "j", { shiftKey: true })).toBe(true);
    expect(press(el(p, "log"), "j", { altKey: true })).toBe(true);
    expect(threadName(p)).toBe("Mia");

    expect(press(el(p, "log"), "c")).toBe(false);
    expect(threadName(p)).toBe("New message");
    expect(actions.mock.calls.filter(([verb]) => verb === "archive")).toHaveLength(0);
  });

  it("toggles the conversation list beside the thread and goes back to it when stacked", async () => {
    const p = await openWithCommands();
    p.win.innerWidth = 800;
    p.win.dispatchEvent(new p.win.Event("resize") as never);
    await openThread(p, "Mia");
    const list = p.doc.querySelector(".ib-list")!;
    const send = (id: string) => act(() => findShortcutAction(commandId(id))!.run());
    send("toggle-list");
    await waitFor(() => expect(el(p, "inbox").dataset.list).toBe("hidden"));
    expect(list.matches('.ib[data-list="hidden"]:not([data-layout="stack"]) .ib-list')).toBe(true);
    send("toggle-list");
    await waitFor(() => expect(el(p, "inbox").dataset.list).toBeUndefined());
    p.win.innerWidth = 600;
    p.win.dispatchEvent(new p.win.Event("resize") as never);
    send("toggle-list");
    await waitFor(() => expect(el(p, "inbox").dataset.view).toBe("list"));
    send("back-to-list");
    send("reply");
    await waitFor(() => expect(p.doc.activeElement).toBe(el(p, "draft")));
  });

  it("keeps focus and conversation navigation usable with a hidden list", async () => {
    const p = await openWithCommands();
    p.win.innerWidth = 800;
    p.win.dispatchEvent(new p.win.Event("resize") as never);
    await openThread(p, "Mia");
    act(() => findShortcutAction(commandId("toggle-list"))!.run());
    await waitFor(() => expect(el(p, "inbox").dataset.list).toBe("hidden"));
    fireEvent.keyDown(el(p, "draft"), { key: "Escape" });
    expect(p.doc.activeElement).toBe(el(p, "log"));
    act(() => findShortcutAction(commandId("back-to-list"))!.run());
    await waitFor(() => expect(el(p, "inbox").dataset.list).toBeUndefined());
    expect(p.doc.activeElement).toBe(el(p, "conversations"));
    act(() => findShortcutAction(commandId("toggle-list"))!.run());
    act(() => findShortcutAction(commandId("archive-conversation"))!.run());
    await waitFor(() => expect(el(p, "log").hidden).toBe(true));
    expect(el(p, "inbox").dataset.list).toBeUndefined();
    act(() => findShortcutAction(commandId("toggle-list"))!.run());
    expect(el(p, "inbox").dataset.list).toBeUndefined();
  });

  it("reveals and focuses a hidden conversation list when the Inbox is reopened", async () => {
    const p = await openWithCommands();
    p.win.innerWidth = 800;
    p.win.dispatchEvent(new p.win.Event("resize") as never);
    expect(el(p, "inbox").dataset.layout).toBe("split");
    await openThread(p, "Mia");
    act(() => findShortcutAction(commandId("toggle-list"))!.run());
    await waitFor(() => expect(el(p, "inbox").dataset.list).toBe("hidden"));
    const list = p.doc.querySelector(".ib-list")!;
    const hiddenListSelector = '.ib[data-list="hidden"]:not([data-layout="stack"]) .ib-list';
    expect(list.matches(hiddenListSelector)).toBe(true);

    const toggleInbox = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-inbox-rollup")!;
    act(() => toggleInbox.run());
    await waitFor(() => expect(useViewerStore.getState().rightPaneOpen).toBe(false));
    act(() => toggleInbox.run());
    await waitFor(() => expect(useViewerStore.getState().rightPaneOpen).toBe(true));
    await waitFor(() => expect(el(p, "inbox").dataset.list).toBeUndefined());
    expect(el(p, "inbox").dataset.layout).toBe("split");
    expect(list.matches('.ib[data-layout="split"]:not([data-list="hidden"]) .ib-list')).toBe(true);
    await waitFor(() => expect(p.doc.activeElement).toBe(el(p, "conversations")));
  });
});

describe("Jump to latest", () => {
  /** Give the happy-dom log real scroll geometry: 1000px of messages in a 200px view. */
  function scrollable(p: PaneDoc) {
    const log = el(p, "log");
    let top = 800;
    Object.defineProperty(log, "scrollHeight", { configurable: true, get: () => 1000 });
    Object.defineProperty(log, "clientHeight", { configurable: true, get: () => 200 });
    Object.defineProperty(log, "scrollTop", {
      configurable: true,
      get: () => top,
      set: (y: number) => (top = Math.max(0, Math.min(y, 800))),
    });
    const scroll = (y: number) => {
      log.scrollTop = y;
      log.dispatchEvent(new p.win.Event("scroll") as never);
    };
    log.scrollTo = ((o: ScrollToOptions) => scroll(o.top ?? 0)) as never;
    return { scroll, top: () => top };
  }
  const dock = (p: PaneDoc) => el(p, "latest").parentElement!;
  const reads = () => calls("mark_inbox_messages_read").map((c) => c[1].messageIds);
  async function scrolledUp() {
    const p = await openInbox();
    await openThread(p);
    await waitFor(() => expect(reads()).toEqual([[1]]));
    const log = scrollable(p);
    log.scroll(0);
    return { p, log };
  }
  const settle = () => act(() => new Promise((r) => setTimeout(r, 60)));

  it("counts others' mail outside the log, holds it unread, and jumps on activation", async () => {
    const { p, log } = await scrolledUp();
    expect(dock(p).hidden).toBe(true);
    messages.push(row(5), row(6), row(7, "human", "Mia", { inReplyTo: 1, unread: false }));
    inboxChanged();
    await waitFor(() => expect(el(p, "latest").textContent).toContain("2 new"));
    expect(dock(p).hidden).toBe(false);
    expect(el(p, "log").contains(dock(p))).toBe(false);
    expect(el(p, "latest").textContent!.replace(/\s+/g, " ").trim()).toBe("2 new · Jump to latest");
    await settle();
    expect(reads()).toEqual([[1]]);
    expect(messages.find((m) => m.messageId === 5)?.unread).toBe(true);

    el(p, "latest").focus();
    fireEvent.click(el(p, "latest"));
    await waitFor(() => expect(reads()).toEqual([[1], [5, 6]]));
    expect(log.top()).toBe(800);
    expect(dock(p).hidden).toBe(true);
    expect(p.doc.activeElement).toBe(el(p, "log"));
    expect(el(p, "log").querySelectorAll(".ib-new")).toHaveLength(1);
  });

  it("clears and reads when the reader scrolls down by hand, and follows mail at the bottom", async () => {
    const { p, log } = await scrolledUp();
    messages.push(row(5));
    inboxChanged();
    await waitFor(() => expect(el(p, "latest-count").textContent).toBe("1 new"));
    log.scroll(790);
    await waitFor(() => expect(reads()).toEqual([[1], [5]]));
    expect(dock(p).hidden).toBe(true);
    messages.push(row(6));
    inboxChanged();
    await waitFor(() => expect(reads()).toEqual([[1], [5], [6]]));
    expect(dock(p).hidden).toBe(true);
  });

  it("keeps its count while older pages load, resets on another conversation, and caps at 99+", async () => {
    const { p, log } = await scrolledUp();
    messages.push(row(5));
    inboxChanged();
    await waitFor(() => expect(el(p, "latest-count").textContent).toBe("1 new"));
    messages.unshift(row(0.5));
    await act(async () => el(p, "log").querySelector<HTMLElement>(".ib-older button")?.click());
    await settle();
    expect(el(p, "latest-count").textContent).toBe("1 new");

    await openThread(p, "Ori");
    expect(dock(p).hidden).toBe(true);
    await openThread(p, "Mia");
    await waitFor(() => expect(reads().flat()).toContain(5));
    log.scroll(0);
    for (const burst of [0, 1]) {
      messages.push(...Array.from({ length: 50 }, (_, i) => row(100 + burst * 50 + i)));
      inboxChanged();
      await waitFor(() =>
        expect(el(p, "latest-count").textContent).toBe(burst ? "99+ new" : "50 new"),
      );
    }
  });

  it("clears when the reader sends and scrolls to the bottom", async () => {
    const { p, log } = await scrolledUp();
    messages.push(row(5));
    inboxChanged();
    await waitFor(() => expect(dock(p).hidden).toBe(false));
    type(p, "On it");
    fireEvent.keyDown(el(p, "draft"), { key: "Enter", metaKey: true });
    await waitFor(() => expect(dock(p).hidden).toBe(true));
    expect(log.top()).toBe(800);
    await waitFor(() => expect(reads()).toEqual([[1], [5]]));
  });

  it("a send finishing after navigation leaves the current conversation scrolled up and unread", async () => {
    const { p, log } = await scrolledUp();
    let finishSend!: (value: unknown) => void;
    const pending = new Promise((resolve) => (finishSend = resolve));
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, any>) =>
      cmd === "reply_as_human" ? pending : stubTauri(cmd, args),
    );
    type(p, "On it");
    fireEvent.click(el(p, "send"));
    await waitFor(() => expect(calls("reply_as_human")).toHaveLength(1));
    await openThread(p, "Ori");
    log.scroll(0);
    messages.push(row(6, "Ori"));
    inboxChanged();
    await waitFor(() => expect(el(p, "latest-count").textContent).toBe("1 new"));
    await act(async () => finishSend({ status: "sent", messageId: 50 }));
    await waitFor(() => expect(el(p, "send").textContent).not.toContain("Sending"));
    await settle();
    expect(log.top()).toBe(0);
    expect(dock(p).hidden).toBe(false);
    expect(reads().flat()).not.toContain(6);
    expect(messages.find((m) => m.messageId === 6)?.unread).toBe(true);
  });
});

it.each([true, false])(
  "Reload hands back from the built-in reader to the real Inbox (custom badge=%s)",
  async (publishesBadge) => {
    const team = listing();
    const builtin = {
      ...team.panes[0],
      scope: "private" as const,
      team: undefined,
      dashboardRef: { scope: "built-in" as const, org: ORG, id: "inbox" },
      path: "/h/users/42/builtin-panes/panes/inbox.html",
      panesDir: "/h/users/42/builtin-panes/panes",
      panelId: `pane:private:${ORG}/inbox`,
    };
    let mint = 0;
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, any>) => {
      if (cmd === "list_panes" || cmd === "list_builtin_panes")
        return { ...listing(), builtinInbox: builtin };
      if (cmd === "mint_frame" && ++mint > 1) {
        const frame = frameFixture(args!.args, PORT);
        frame.frameId = `recovery-${mint}`;
        frame.origin = `http://f-${frame.frameId}.localhost:${PORT}`;
        frame.token = `secret-${frame.frameId}`;
        frame.grant = {
          ...frame.grant,
          frameId: frame.frameId,
          dashboard: dashboardRefKey(
            args!.args.documentPath === builtin.path
              ? builtin.dashboardRef
              : team.panes[0].dashboardRef!,
          ),
          scope: "team",
          team: { org: ORG, team: TEAM },
        };
        frame.effectiveGrant = ["inbox:read:team"];
        mintedFrames.set(frame.origin, frame);
        return frame;
      }
      return stubTauri(cmd, args);
    });
    await openInbox(false, '<script>throw new Error("broken team Inbox")</script>');
    const strip = await screen.findByTestId("slot-fallback-strip");
    // The strip mounts before it names the built-in fallback it is showing.
    await waitFor(() => expect(strip).toHaveTextContent("showing the built-in Inbox"));
    const fallbackFrame = await waitFor(() => {
      const frame = document.querySelector("iframe");
      expect(frame?.getAttribute("src")).toContain("/pane/");
      return frame!;
    });
    const fallbackRecord = [...mintedFrames.values()].find(
      (record) => record.documentPath === builtin.path,
    )!;
    const fallback = bootPaneDoc(fallbackFrame, PANE, fallbackRecord);
    docs.push(fallback);
    await waitFor(() => expect(fallback.doc.querySelectorAll(".ib-row")).toHaveLength(2));
    await waitFor(() => expect(usePaneStore.getState().badges[PANEL]?.count).toBe(2));
    fallback.win.eval(
      "window.visibilityHistory = []; codesense.onVisibility(shown => window.visibilityHistory.push(shown));",
    );
    fireEvent.click(screen.getByRole("button", { name: "Reload" }));
    await waitFor(() => expect((fallback.win as any).visibilityHistory.at(-1)).toBe(false));
    expect(strip).toBeInTheDocument();
    const retryFrame = await waitFor(() => {
      const frame = [...document.querySelectorAll("iframe")].find((f) => f !== fallbackFrame);
      expect(frame?.getAttribute("src")).toContain("/pane/");
      return frame!;
    });
    const retryRecord = [...mintedFrames.values()].find(
      (record) => record.documentPath === team.panes[0].path,
    )!;
    // A real custom fork may read and send mail without publishing a panel badge.
    const retryHtml = publishesBadge ? PANE : PANE.replace("cs.setBadge(", "void(");
    const retry = bootPaneDoc(retryFrame, retryHtml, retryRecord);
    docs.push(retry);
    await waitFor(() => expect(retry.doc.querySelectorAll(".ib-row")).toHaveLength(2));
    await waitFor(() => expect(screen.queryByTestId("slot-fallback-strip")).toBeNull());
    if (!publishesBadge) {
      await waitFor(() => expect(usePaneStore.getState().badges[PANEL]).toBeUndefined());
      fallback.win.codesense.setBadge({ count: 99 });
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 30));
      });
      expect(usePaneStore.getState().badges[PANEL]).toBeUndefined();
    }
    expect((fallback.win as any).visibilityHistory.at(-1)).toBe(false);
    await openThread(retry);
    type(retry, "Recovered reply");
    fireEvent.click(el(retry, "send"));
    await waitFor(() =>
      expect(calls("send_inbox_as_human").length + calls("reply_as_human").length).toBeGreaterThan(
        0,
      ),
    );
  },
);

it.each([false, true])(
  "binds independent built-in recovery to the supplied current-team root (cold=%s)",
  async (cold) => {
    const recoveryTeam = cold ? TEAM : "web";
    const triage = {
      ...listing().panes[0],
      id: "triage",
      title: "Triage",
      scope: "private" as const,
      team: undefined,
      surface: "human-inbox",
      panelId: `pane:private:${ORG}/triage`,
      path: `${ROOT}/private/panes/triage.html`,
      dashboardRef: { scope: "private" as const, org: ORG, id: "triage" },
    };
    const builtin = {
      ...triage,
      id: "inbox",
      title: "Inbox",
      panelId: PRIVATE_PANEL,
      dashboardRef: { scope: "built-in" as const, org: ORG, id: "inbox" },
      path: "/h/users/42/builtin-panes/panes/inbox.html",
      panesDir: "/h/users/42/builtin-panes/panes",
    };
    const profile = { ...profileChoice(), slots: { "right-pane": [triage.dashboardRef] } };
    useDashboardProfileStore.setState({ org: ORG, profile });
    let recovered = false;
    let mint = 0;
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, any>) => {
      if (cmd === "read_dashboard_profile") return profile;
      if (cmd === "list_panes") {
        if (args?.team === recoveryTeam && !recovered) throw new Error("offline");
        return {
          contentRoot: `${ROOT}/${args?.team}/content`,
          chain: [String(args?.team)],
          panes: [triage],
          builtinInbox: builtin,
        };
      }
      if (cmd === "list_builtin_panes")
        return {
          contentRoot: `${ROOT}/${args?.team}/content`,
          chain: [],
          panes: [builtin],
          builtinInbox: builtin,
        };
      if (cmd === "bridge_doc_context") {
        const location = /^\/h\/users\/42\/orgs\/(.+)\/([^/]+)\/content\/[^/]+$/.exec(args!.page);
        if (!location) throw new Error(`unknown context anchor: ${args!.page}`);
        return {
          org: location[1],
          team: location[2],
          layer: "team",
          contentRoot: `${ROOT}/${location[2]}/content`,
        };
      }
      if (cmd === "mint_frame") {
        const frame = frameFixture(args!.args, PORT);
        frame.frameId = `warm-${++mint}`;
        frame.origin = `http://f-${frame.frameId}.localhost:${PORT}`;
        frame.token = `secret-${frame.frameId}`;
        const location = /^\/h\/users\/42\/orgs\/(.+)\/([^/]+)\/content$/.exec(frame.contentRoot);
        if (!location) throw new Error(`unknown content root: ${frame.contentRoot}`);
        if (args!.args.builtinTeam)
          expect(args!.args.builtinTeam).toEqual({ org: location[1], team: location[2] });
        frame.grant = {
          ...frame.grant,
          frameId: frame.frameId,
          dashboard: dashboardRefKey(
            args!.args.documentPath === builtin.path ? builtin.dashboardRef : triage.dashboardRef,
          ),
          scope: "team",
          team: { org: location[1], team: location[2] },
        };
        frame.effectiveGrant = ["inbox:read:team"];
        mintedFrames.set(frame.origin, frame);
        return frame;
      }
      return stubTauri(cmd, args);
    });
    const saved = {
      sections: [
        {
          id: "notes-first",
          group: { panelIds: ["scratch"], activePanelId: "scratch" },
          size: 0.23,
          collapsed: false,
        },
        {
          id: "mail",
          label: "Triage queue",
          group: {
            panelIds: ["pane:private:acme/api/other", PANEL],
            activePanelId: "pane:private:acme/api/other",
          },
          size: 0.77,
          collapsed: true,
        },
      ],
    };
    useViewerStore.setState({ rightPaneLayout: saved });
    render(
      <TooltipProvider>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </TooltipProvider>,
    );
    if (!cold) {
      await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(`${ORG}:${TEAM}`));
      act(() => useOrgStore.setState({ activeTeamName: recoveryTeam }));
    }
    await waitFor(() => expect(usePaneStore.getState().listError?.message).toBe("offline"));
    act(() => SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-inbox-rollup")!.run());
    const opened = useViewerStore.getState().rightPaneLayout;
    expect(opened.sections[1]).toEqual({
      ...saved.sections[1],
      collapsed: false,
      group: { ...saved.sections[1].group, activePanelId: PANEL },
    });
    const record = await waitFor(() => {
      const value = [...mintedFrames.values()].find((frame) => frame.documentPath === builtin.path);
      expect(value).toBeDefined();
      return value!;
    });
    expect(record.contentRoot).toBe(`${ROOT}/${recoveryTeam}/content`);
    expect(record.grant.team).toEqual({ org: ORG, team: recoveryTeam });
    const fallbackFrame = await waitFor(() => {
      const frame = [...document.querySelectorAll("iframe")].find((node) =>
        node.src.startsWith(record.origin),
      );
      expect(frame).toBeDefined();
      return frame!;
    });
    const fallback = bootPaneDoc(fallbackFrame, PANE, record);
    docs.push(fallback);
    await waitFor(() => expect(fallback.doc.querySelectorAll(".ib-row")).toHaveLength(2));
    await waitFor(() => expect(usePaneStore.getState().pendingIntents[PANEL]).toBeUndefined());
    await waitFor(() => expect(fallback.doc.activeElement).toBe(el(fallback, "conversations")));
    fireEvent.click(el(fallback, "new-message"));
    await pick(fallback, "Mia");
    type(fallback, "Reply in the recovered team");
    fireEvent.submit(el(fallback, "composer"));
    await waitFor(() =>
      expect(calls("send_inbox_as_human").at(-1)?.[1]).toMatchObject({
        team: { org: ORG, team: recoveryTeam },
        to: "@Mia",
        text: "Reply in the recovered team",
      }),
    );
    recovered = true;
    fireEvent.click(screen.getByRole("button", { name: "Reload" }));
    await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(`${ORG}:${recoveryTeam}`));
    expect(useViewerStore.getState().rightPaneLayout).toEqual(opened);
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
  },
);

it("mounts the real bundled reader when its registered attached checkout is unavailable", async () => {
  const temp = mkdtempSync(join(tmpdir(), "sv-inp10-attached-"));
  const attached = join(temp, "checkout-volume");
  const root = join(attached, TEAM, "content");
  const bundled = join(temp, "users", "42", "builtin-panes", "panes");
  mkdirSync(root, { recursive: true });
  mkdirSync(bundled, { recursive: true });
  const registry = join(temp, "users", "42", "context-attachments.yml");
  // JSON is valid YAML and uses the native attachment registry's persisted schema.
  writeFileSync(
    registry,
    JSON.stringify({ attachments: { [ORG]: { slug: ORG, display_name: "Acme", root: attached } } }),
  );
  writeFileSync(join(bundled, "inbox.html"), PANE);
  renameSync(attached, join(temp, "unmounted-volume"));
  expect(existsSync(root)).toBe(false);
  const builtin = {
    ...listing().panes[0],
    scope: "private" as const,
    team: undefined,
    panelId: PRIVATE_PANEL,
    path: join(bundled, "inbox.html"),
    panesDir: bundled,
    dashboardRef: { scope: "built-in" as const, org: ORG, id: "inbox" },
  };
  tauriInvoke.mockImplementation(async (cmd, args: any) => {
    if (cmd === "list_panes") {
      const registered = JSON.parse(readFileSync(registry, "utf8")).attachments[ORG];
      realpathSync(join(registered.root, args.team, "content"));
      throw new Error("the checkout should be unavailable");
    }
    if (cmd === "list_builtin_panes")
      return { contentRoot: bundled, chain: [], panes: [builtin], builtinInbox: builtin };
    if (cmd === "mint_frame") {
      expect(args.args.builtinTeam).toEqual({ org: ORG, team: TEAM });
      realpathSync(args.args.documentPath);
      realpathSync(args.args.contentRoot);
      const frame = frameFixture(args.args, PORT);
      frame.grant.dashboard = dashboardRefKey(builtin.dashboardRef);
      frame.grant.scope = "team";
      frame.effectiveGrant = ["inbox:read:team"];
      mintedFrames.set(frame.origin, frame);
      return frame;
    }
    if (cmd === "bridge_doc_context") return null; // Bundled bytes are outside every checkout.
    return stubTauri(cmd, args);
  });
  try {
    usePaneStore.setState({ teamKey: null, listing: null, listError: null });
    render(
      <TooltipProvider>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </TooltipProvider>,
    );
    await waitFor(() => expect(usePaneStore.getState().listError?.message).toContain("ENOENT"));
    act(() => SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-inbox-rollup")!.run());
    const record = await waitFor(() => {
      const record = [...mintedFrames.values()].find((f) => f.documentPath === builtin.path);
      expect(record).toBeDefined();
      return record!;
    });
    expect(record.grant.team).toEqual({ org: ORG, team: TEAM });
    expect(record.contentRoot).toBe(bundled);
    // The frame is remounted once its mint resolves; boot the minted one.
    const frame = await waitFor(() => {
      const frame = document.querySelector<HTMLIFrameElement>("[data-pane-current] iframe");
      expect(frame?.getAttribute("src")).toContain("/pane/");
      return frame!;
    });
    const page = bootPaneDoc(frame, readFileSync(builtin.path, "utf8"), record);
    docs.push(page);
    await waitFor(() => expect(page.doc.querySelectorAll(".ib-row")).toHaveLength(2));
    expect(screen.queryByText("Your messages are safe.")).toBeNull();
    await waitFor(() => expect(usePaneStore.getState().pendingIntents[PANEL]).toBeUndefined());
  } finally {
    cleanup();
    rmSync(temp, { recursive: true, force: true });
  }
});

it("drains a hidden live badge once preferences become ready, without rendering", async () => {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "load_app_state") await gate;
    return stubTauri(cmd, args);
  });
  const p = await openInbox(false);
  await waitFor(() => expect(calls("load_app_state").length).toBeGreaterThan(0));
  await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
  await new Promise((resolve) => setTimeout(resolve, 80));
  const listBefore = p.doc.querySelector(".ib-rows")!.innerHTML;
  const publish = vi.spyOn(p.win.codesense, "setBadge");
  await act(async () => release());
  await waitFor(() => expect(publish).toHaveBeenCalledWith({ count: 2 }));
  expect(p.doc.querySelector(".ib-rows")!.innerHTML).toBe(listBefore);
});

it("drains dirty badge work after a visible refresh finishes while hidden", async () => {
  const p = await openInbox();
  await new Promise((resolve) => setTimeout(resolve, 80));
  let release!: () => void;
  let started = false;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    const result = stubTauri(cmd, args);
    if (cmd === "agent_read_inbox" && !started) {
      started = true;
      await gate;
    }
    return result;
  });
  await act(async () => inboxChanged());
  await waitFor(() => expect(started).toBe(true));
  await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
  await new Promise((resolve) => setTimeout(resolve, 80));
  const listBefore = p.doc.querySelector(".ib-rows")!.innerHTML;
  const threadBefore = el(p, "log").innerHTML;
  messages.push(row(5, "Lee"));
  await act(async () => release());
  await waitFor(() => expect(usePaneStore.getState().badges[PANEL]?.count).toBe(3));
  expect(p.doc.querySelector(".ib-rows")!.innerHTML).toBe(listBefore);
  expect(el(p, "log").innerHTML).toBe(threadBefore);
});

it("keeps a YAML-only resolved live badge current without a DOM declaration", async () => {
  const html = PANE.replace(/<meta name="codesense:badge" content="live" \/>/, "");
  expect(html).not.toContain('name="codesense:badge"');
  const p = await openInbox(true, html);
  await new Promise((resolve) => setTimeout(resolve, 80));
  await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
  await new Promise((resolve) => setTimeout(resolve, 80));
  messages.push(row(5, "Lee"));
  await act(async () => inboxChanged());
  await waitFor(() => expect(usePaneStore.getState().badges[PANEL]?.count).toBe(3), {
    timeout: 2500,
  });
  expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(2);
});

it("applies the shipped Inbox recipe: fixed team, needs-you summaries, replies and live refresh", async () => {
  messages = [
    row(5, "Mia", "human", { kind: "needs-you", text: "Choose the backend plan" }),
    row(4, "human", "Mia", { kind: "update", text: "Existing reply", unread: false }),
    row(6, "human", "Other", { kind: "update", text: "Sent-only conversation", unread: false }),
    row(2, "Ori", "human", { kind: "update", text: "Update-only conversation" }),
    row(3, "Mia", "human", { kind: "update", text: "Latest update must not be the preview" }),
  ];
  const foreign = row(50, "Foreign", "human", { kind: "needs-you" });
  tauriInvoke.mockImplementation(async (cmd, args: any) => {
    if (cmd === "agent_read_inbox") {
      const bound = args.team.org === ORG && args.team.team === TEAM;
      return inboxView(bound ? messages : [foreign], args.query);
    }
    if (cmd === "reply_as_human" || cmd === "send_inbox_as_human") {
      messages.push(
        row(50, "human", "Mia", {
          kind: "update",
          text: args.text,
          inReplyTo: args.messageId ?? 5,
          unread: false,
        }),
      );
    }
    return stubTauri(cmd, args);
  });
  const p = await openInbox(false, recipePane("inbox"));
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  expect(el(p, "conversations").textContent).toContain("Choose the backend plan");
  expect(el(p, "conversations").textContent).not.toMatch(
    /Update-only|Latest update|Foreign|Sent-only/,
  );
  expect(
    calls("agent_read_inbox").every(([, args]) => args.team.org === ORG && args.team.team === TEAM),
  ).toBe(true);
  const listReads = calls("agent_read_inbox").filter(([, args]) => args.query.scope === "to-human");
  expect(listReads.some(([, args]) => args.query.kind === "needs-you")).toBe(true);
  expect(listReads.some(([, args]) => args.query.kind === undefined)).toBe(true);
  await openThread(p);
  await waitFor(() => expect(el(p, "log").textContent).toContain("Existing reply"));
  expect(el(p, "log").textContent).toContain("Latest update must not be the preview");
  type(p, "Use the first plan");
  fireEvent.click(el(p, "send"));
  await waitFor(() =>
    expect(calls("reply_as_human").length + calls("send_inbox_as_human").length).toBeGreaterThan(0),
  );
  const send = [...calls("reply_as_human"), ...calls("send_inbox_as_human")].at(-1)!;
  expect(send[1].team).toEqual({ org: ORG, team: TEAM });
  messages.push(row(60, "Ori", "human", { kind: "needs-you", text: "A fresh decision" }));
  inboxChanged();
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(2));
  expect(el(p, "conversations").textContent).toContain("A fresh decision");
  await waitFor(() => expect(el(p, "log").textContent).toContain("Use the first plan"));
});

it("pages the Inbox recipe through more than 500 updates to recover a reply outside both initial windows", async () => {
  messages = [
    row(1, "Mia", "human", { kind: "needs-you", text: "Original decision" }),
    row(2, "human", "Mia", { kind: "update", text: "Older retained reply", unread: false }),
    ...Array.from({ length: 500 }, (_, index) =>
      row(index + 3, "Mia", "human", { kind: "update", unread: false }),
    ),
    ...Array.from({ length: 500 }, (_, index) =>
      row(index + 503, "human", "Other", { kind: "update", unread: false }),
    ),
  ];
  const p = await openInbox(false, recipePane("inbox"));
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p);
  const reads = () =>
    calls("agent_read_inbox").filter(
      ([, args]) => args.query.conversation === "Mia" && args.query.scope === "all",
    );
  const control = () => el(p, "log").querySelector<HTMLButtonElement>(".ib-older button")!;
  await waitFor(() => expect(reads()).toHaveLength(1));
  await waitFor(() => expect(control().getAttribute("aria-disabled")).toBe("false"));
  for (let page = 2; page <= 11; page++) {
    await act(async () => control().click());
    await waitFor(() => expect(reads()).toHaveLength(page));
    if (page < 11)
      await waitFor(() => expect(control().getAttribute("aria-disabled")).toBe("false"));
  }
  expect(reads()[0][1].query.before).toBeUndefined();
  expect(
    reads()
      .slice(1)
      .map(([, args]) => args.query.before),
  ).toEqual([453, 403, 353, 303, 253, 203, 153, 103, 53, 3]);
  await waitFor(() => expect(el(p, "log").textContent).toContain("Older retained reply"));
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(502);
  expect(el(p, "log").textContent).toContain("Start of conversation");
  expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1);
});

it("keeps the Inbox recipe thread populated when more than one page of updates arrives", async () => {
  messages = [
    row(1, "Mia", "human", { kind: "needs-you", text: "Decision in the list" }),
    row(2, "human", "Mia", { kind: "update", text: "First reply", unread: false }),
  ];
  const p = await openInbox(false, recipePane("inbox"));
  await waitFor(() => expect(p.doc.querySelectorAll(".ib-row")).toHaveLength(1));
  await openThread(p);
  await waitFor(() => expect(el(p, "log").textContent).toContain("First reply"));
  messages.push(
    ...Array.from({ length: 60 }, (_, index) =>
      row(index + 3, "Mia", "human", {
        kind: "update",
        text: `Incoming update ${index + 3}`,
        unread: false,
      }),
    ),
  );
  inboxChanged();
  await waitFor(() => expect(el(p, "log").textContent).toContain("Incoming update 62"));
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(50);
  expect(el(p, "conversations").textContent).toContain("Decision in the list");
  expect(el(p, "conversations").textContent).not.toContain("Incoming update");
  const control = el(p, "log").querySelector<HTMLButtonElement>(".ib-older button")!;
  await act(async () => control.click());
  await waitFor(() => expect(el(p, "log").textContent).toContain("First reply"));
  expect(el(p, "log").querySelectorAll(".ib-msg")).toHaveLength(62);
});

it("serves the private canonical Inbox fork and falls back to built-in after its file is removed", async () => {
  const temp = mkdtempSync(join(tmpdir(), "sv-doc6-inbox-revert-"));
  const privatePath = join(temp, "private", "panes", "inbox.html");
  const builtinPath = join(temp, "builtin", "panes", "inbox.html");
  mkdirSync(join(temp, "private", "panes"), { recursive: true });
  mkdirSync(join(temp, "builtin", "panes"), { recursive: true });
  writeFileSync(privatePath, recipePane("inbox"));
  writeFileSync(builtinPath, PANE);
  messages.forEach((message) => {
    message.kind = "needs-you";
  });
  const base = listing();
  const privatePane = {
    ...base.panes[0],
    scope: "private" as const,
    team: undefined,
    dashboardRef: { scope: "private" as const, org: ORG, id: "inbox" },
    path: privatePath,
    panesDir: join(temp, "private", "panes"),
    panelId: PRIVATE_PANEL,
  };
  const builtin = {
    ...privatePane,
    dashboardRef: { scope: "built-in" as const, org: ORG, id: "inbox" },
    path: builtinPath,
    panesDir: join(temp, "builtin", "panes"),
  };
  tauriInvoke.mockImplementation(async (cmd, args: any) => {
    if (cmd === "list_panes" || cmd === "list_builtin_panes")
      return {
        ...base,
        panes: [...(existsSync(privatePath) ? [privatePane] : []), builtin],
        builtinInbox: builtin,
      };
    if (cmd === "mint_frame") {
      const record = frameFixture(args.args, PORT);
      record.frameId = existsSync(privatePath) ? "test" : "builtin-restored";
      record.origin = `http://f-${record.frameId}.localhost:${PORT}`;
      record.token = existsSync(privatePath) ? "frame-token" : "builtin-restored";
      record.grant = {
        ...record.grant,
        frameId: record.frameId,
        dashboard: dashboardRefKey(
          args.args.documentPath === privatePath ? privatePane.dashboardRef : builtin.dashboardRef,
        ),
        team: { org: ORG, team: TEAM },
        scope: "team",
      };
      record.effectiveGrant = ["inbox:read:team"];
      mintedFrames.set(record.origin, record);
      return record;
    }
    return stubTauri(cmd, args);
  });
  try {
    const fork = await openInbox(false, readFileSync(privatePath, "utf8"));
    await waitFor(() => expect(fork.doc.querySelectorAll(".ib-row")).toHaveLength(2));
    expect(resolveInboxDashboard({ org: ORG, team: TEAM })?.path).toBe(privatePath);
    expect([...mintedFrames.values()].some((record) => record.documentPath === privatePath)).toBe(
      true,
    );
    rmSync(privatePath);
    await act(async () => {
      await usePaneStore.getState().refresh?.();
    });
    act(() => revealSlot("right-pane", { surface: "inbox" }));
    await waitFor(() =>
      expect(resolveInboxDashboard({ org: ORG, team: TEAM })?.path).toBe(builtinPath),
    );
    const record = await waitFor(() => {
      const current = [...mintedFrames.values()].find(
        (entry) => entry.documentPath === builtinPath,
      );
      expect(current).toBeDefined();
      return current!;
    });
    const frame = [...document.querySelectorAll("iframe")].find((node) =>
      node.src.startsWith(record.origin),
    )!;
    expect(frame).toBeDefined();
    expect(record.documentPath).toBe(builtinPath);
    expect(readFileSync(record.documentPath, "utf8")).toBe(PANE);
  } finally {
    cleanup();
    rmSync(temp, { recursive: true, force: true });
  }
});

it("an inherited Inbox replacement waits for its hosted message's flush", async () => {
  inheritedInbox = true;
  uniquePaneFrames = true;
  messages[0].contentType = "html";
  messages[0].text = "<p>Interactive dashboard</p>";
  const p = await openInbox();
  await openThread(p);
  await waitFor(() => expect(el(p, "log").querySelector("iframe")).not.toBeNull());
  const original = screen.getByTestId("pane-body").querySelector("[data-pane-current] iframe")!;
  const childFrame = el(p, "log").querySelector("iframe")!;
  const record = [...mintedFrames.values()].find((r) => r.parentFrame)!;
  const child = bootPaneDoc(childFrame, messages[0].text, record, true);
  docs.push(child);
  let release!: () => void;
  const pending = new Promise<void>((resolve) => (release = resolve));
  let started = false,
    completed = false;
  child.win.codesense.onFlush(async () => {
    started = true;
    await pending;
    completed = true;
  });
  await act(async () =>
    useDashboardProfileStore.setState({
      profile: {
        ...profileChoice(),
        rightPaneSurfaces: {
          inbox: { scope: "team", org: ORG, team: "platform", id: "alternate" },
        },
      },
    }),
  );
  await act(async () => {
    await new Promise((r) => setTimeout(r, 40));
  });
  try {
    expect(original.isConnected).toBe(true);
    expect(started).toBe(true);
  } finally {
    release();
  }
  await waitFor(() => expect(original.isConnected).toBe(false));
  expect(completed).toBe(true);
});
