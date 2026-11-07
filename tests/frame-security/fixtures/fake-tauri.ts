import { recordsHandlers } from "../../../src/app/mock/handlers/records";
import type { MockEventBus } from "../../../src/app/mock/mock-events";
/**
 * The fake Tauri backend behind the real host components in a browser test: frame minting,
 * grants, verb admission, the live hub (including windowed subscriptions) and events.
 * Shared by the frame-security lifecycle host and the pane harness host.
 */
import { inboxFixtureRollup, type InboxFixtureRow } from "../../../src/app/mock/inbox-fixtures";
import { useAgentListProjectionStore } from "../../../src/features/viewer/lib/agent-list-projection";
import { liveHub, orderedRows } from "../../../src/features/dashboard/lib/live-hub";
import { VERB_REGISTRY } from "../../../src/features/dashboard/lib/verb-registry.generated";
import { terminalManager } from "../../../src/shared/terminal/terminal-manager";
import { useViewerStore } from "../../../src/shared/stores/viewer-store";
import { DEFAULT_NAV_LENS } from "../../../src/shared/lib/nav-lens";
import { useOrgStore } from "../../../src/shared/stores/org-store";
import { pathToTeamContentDirSync } from "../../../src/shared/lib/team-content-path";

type Row = Record<string, unknown> & { id: string };

export type FakeTauriOptions = {
  runs?: import("../../../src/shared/lib/run-outcome").RunOutcomeRead[];
  runReads?: Record<string, import("../../../src/tauri/commands").RunRead>;
  records?: Array<{ id: string; title: string; body: string; pinned?: boolean }>;
  /** The caller kind minted frames get. */
  kind?: "private" | "team" | "builtin" | "message";
  /** The frame record's effective grant; omitted, the frame record has none. */
  effectiveGrant?: string[];
  /** Session rows; omitted, one fixture session. */
  sessions?: Row[];
  /** Production live-hub inbox message envelopes and projection count rows. */
  inbox?: Row[];
  /** Further live-hub topics, served whole or by window. */
  topics?: Record<string, Row[]>;
  /** Explicit projection fixtures, keyed by [topic, viewKey]. */
  views?: Record<string, Row[]>;
  /** Held before query_context answers. */
  queryGate?: Promise<void>;
  /** The active tab per team key. */
  activeTab?: Record<string, string>;
  /** Verb classes whose every call is denied with `denied: <class> budget`. */
  budgetDenied?: string[];
  /** Verbs that need a confirmation, as a per-dashboard confirm override would make them.
   *  Policy: test configuration only; the default stays "no new prompts". */
  confirm?: string[];
  /** The rollup carries only this many newest messages, as the real window does; history
   *  reads then filter by conversation and `before`, newest first. Omitted: every message. */
  rollupWindow?: number;
};

const session = {
  id: "fixture-session",
  sessionId: "fixture-session",
  handle: "fixture-agent",
  kind: "cli",
  alive: true,
  backgrounded: false,
  startedWith: null,
  startedAtMs: null,
  launcher: null,
  launchTeamKey: "fixture:dev",
  team: { org: "fixture", team: "dev" },
  rowNames: { primary: "Fixture", secondary: null, full: "Fixture" },
};
const reference = {
  id: "fixture-agent",
  sessionRef: "fixture-session",
  inboxRef: "fixture-agent",
  teamKey: "fixture:dev",
  parentId: null,
  parentSource: null,
  order: 0,
  workspaceRef: null,
  storyRef: null,
  workspaceRank: 0,
};

export function installFakeTauri(options: FakeTauriOptions = {}) {
  const api = window as any;
  api.events = [];
  api.calls = [];
  api.mints = 0;
  api.windows = [];
  /** Every verb admission: `{verb, class, at, status}` with `at` from performance.now(). */
  api.dispatches = [];
  api.answers = [];
  const pending = new Map<string, { verb: string; args: unknown }>();
  const grants = new Map<string, unknown>();
  const frameRecords = new Map<string, any>();
  let rebound = 0;
  const sessions = options.sessions ?? [session];
  const topics: Record<string, Row[]> = {
    sessions: sessions as Row[],
    agentList: options.sessions ? [] : [reference],
    inbox: options.inbox ?? [],
    ...options.topics,
  };
  let appState = JSON.stringify({
    liveHubUi: Object.fromEntries(
      Object.entries(topics)
        .filter(([topic]) => topic.startsWith("ui:"))
        .map(([topic, rows]) => [
          JSON.stringify([topic, topic === "ui:file-nav" ? "team:fixture/dev" : "all"]),
          { data: rows[0] ?? {}, at: Date.now() },
        ]),
    ),
  });
  const revs = new Map<string, number>();
  const subscriptions = new Map<
    string,
    { topic: string; window?: [number, number]; rev: number; viewKey?: string }
  >();
  const callbacks = new Map<number, (event: unknown) => void>();
  const listeners = new Map<string, number[]>();
  let callbackId = 0;
  const rev = (topic: string) => revs.get(topic) ?? 1;
  const fileViews = new Map<string, Record<string, string>>();
  const rowsIn = (topic: string, window?: [number, number], viewKey?: string) => {
    let rows = options.views?.[JSON.stringify([topic, viewKey])] ?? topics[topic] ?? [];
    const selector = viewKey && fileViews.get(viewKey);
    if (topic === "files" && selector)
      rows = rows.filter((row) => {
        const dir =
          row.parentPath ??
          (typeof row.path === "string" ? row.path.slice(0, row.path.lastIndexOf("/")) : "");
        return (
          dir === (selector.dir || row.root || "/fixture") &&
          (!row.scope || row.scope === selector.scope)
        );
      });
    return window ? rows.slice(window[0], window[1]) : rows;
  };
  const inboxMessages = () =>
    topics.inbox.filter((row): row is InboxFixtureRow => row.rowType === "message");
  const emit = (event: string, payload: unknown) => {
    for (const id of listeners.get(event) ?? []) callbacks.get(id)?.({ event, id, payload });
  };
  /** Replace or add a row, in the hub's own row order, and push the delta to the shared view and to every subscription;
   *  a windowed one gets the rows that entered and left its window, and the new total. */
  const upsert = (topic: string, row: Row) => {
    const rows = (topics[topic] ??= []);
    const shown = new Map(
      [...subscriptions].map(([id, s]) => [
        id,
        rowsIn(s.topic, s.window, s.viewKey).map((r) => r.id),
      ]),
    );
    const index = rows.findIndex((r) => r.id === row.id);
    if (index >= 0) rows[index] = row;
    else rows.push(row);
    orderedRows(topic, rows);
    const next = rev(topic) + 1;
    revs.set(topic, next);
    const delta = {
      topic,
      viewKey: "all",
      rev: next,
      upserts: [row],
      removes: [] as string[],
      total: rows.length,
    };
    emit("codesense:delta", delta);
    for (const [subscriptionId, s] of subscriptions) {
      if (s.topic !== topic) continue;
      const was = new Set(shown.get(subscriptionId));
      const now = rowsIn(topic, s.window, s.viewKey);
      const ids = new Set(now.map((r) => r.id));
      emit("codesense:subscription-delta", {
        subscriptionId,
        baseRev: s.rev,
        delta: {
          ...delta,
          viewKey: s.viewKey ?? "all",
          upserts: now.filter((r) => r === row || !was.has(r.id)),
          removes: [...was].filter((id) => !ids.has(id)),
        },
      });
      s.rev = next;
    }
  };
  api.emit = emit;
  /** The committed inbox change, naming the rows the projection pass touched (none: a
   *  change that names no rows). */
  const inboxChanged = (team: unknown, messageIds: number[]) =>
    emit("agent-inbox-changed", messageIds.length ? { changes: [{ team, messageIds }] } : null);
  api.inboxChanged = inboxChanged;
  api.upsert = upsert;
  api.writeUi = (patch: Record<string, unknown>) =>
    (options as { slotId?: string }).slotId === "file-nav"
      ? liveHub.writeUi("ui:file-nav", "team:fixture/dev", patch)
      : liveHub.writeUi("ui:dock", "all", patch);
  api.setSession = (patch: object) => {
    Object.assign(session, patch);
    useAgentListProjectionStore.setState({ sessions: [{ ...session }] as never });
  };
  api.viewer = useViewerStore;
  terminalManager.holdsVisible = () => true;
  terminalManager.watchShown = () => () => {};

  api.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  api.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
    transformCallback: (callback: (event: unknown) => void) => {
      callbacks.set(++callbackId, callback);
      return callbackId;
    },
    unregisterCallback: (id: number) => callbacks.delete(id),
    invoke: async (command: string, args: any) => {
      api.calls.push([command, args]);
      if (command === "check_path_exists")
        return useOrgStore
          .getState()
          .orgs.some((org) =>
            org.teams.some(
              (team) => pathToTeamContentDirSync(org.org_root, team.name) === args.path,
            ),
          );
      if (command === "plugin:event|listen") {
        listeners.set(args.event, [...(listeners.get(args.event) ?? []), args.handler]);
        return args.handler;
      }
      if (command === "get_http_api_port") return api.framePort;
      if (command === "mint_frame") {
        const id = args.args.documentPath.split("/").pop();
        const frameId = `${id}:${++api.mints}`;
        const record = {
          frameId,
          token: id,
          origin: `http://127.0.0.1:${api.framePort}`,
          kind: options.kind ?? "private",
          documentPath: args.args.documentPath,
          ...(options.effectiveGrant ? { effectiveGrant: options.effectiveGrant } : {}),
          grant: {
            frameId,
            principal: "fixture",
            dashboard: id,
            slot: args.args.slot,
            team: { org: "fixture", team: "dev" },
            scope: "open",
          },
        };
        grants.set(id, record.grant);
        frameRecords.set(id, record);
        return record;
      }
      if (command === "rebind_frame_team") {
        const previous = frameRecords.get(args.token);
        if (!previous) throw new Error("denied: frame identity");
        const token = `${args.frameId}#${++rebound}`;
        const record = {
          ...previous,
          token,
          grant: { ...previous.grant, team: { org: args.org, team: args.team } },
        };
        grants.delete(args.token);
        frameRecords.delete(args.token);
        grants.set(token, record.grant);
        frameRecords.set(token, record);
        return record;
      }
      if (command === "resolve_frame_grant")
        return (
          grants.get(args.token) ?? {
            frameId: args.token,
            principal: "fixture",
            dashboard: args.token,
            slot: "right-pane",
            team: { org: "fixture", team: "dev" },
            scope: "open",
          }
        );
      if (command === "verb_dispatch") {
        const verbClass = VERB_REGISTRY.find((spec) => spec.name === args.verb)?.class ?? "unknown";
        const record = {
          verb: args.verb,
          args: args.args,
          class: verbClass,
          at: performance.now(),
          status: "admit",
        };
        api.dispatches.push(record);
        if (options.budgetDenied?.includes(verbClass)) {
          record.status = "deny";
          return { status: "deny", reason: `denied: ${verbClass} budget` };
        }
        if (options.confirm?.includes(args.verb)) {
          const confirmId = crypto.randomUUID();
          pending.set(confirmId, { verb: args.verb, args: args.args });
          record.status = "pending";
          return { status: "pending", confirmId, allowAlways: false };
        }
        return { status: "admit", callId: crypto.randomUUID() };
      }
      if (command === "verb_answer") {
        api.answers.push({ confirmId: args.confirmId, allow: args.allow });
        const call = pending.get(args.confirmId);
        pending.delete(args.confirmId);
        return { ...call, callId: crypto.randomUUID(), allowAlways: false };
      }
      if (command === "verb_settle") return null;
      if (command === "run_read_list") return options.runs ?? [];
      if (command === "run_read") return options.runReads?.[args.runId] ?? null;
      if (command === "agent_read_roles") return { roles: [] };
      if (command === "avatar_packs_list_for_team")
        return { packs: [], warnings: [], default_pack: null };
      if (command === "agent_definitions_prepare")
        return {
          topic: "agentDefinitions",
          viewKey: "all",
          rev: rev("agentDefinitions"),
          snapshot: rowsIn("agentDefinitions"),
        };
      if (command === "commands_register") return { dashboard: "files-tree", active: args.ids };
      if (command === "file_surface_prepare_view") {
        const key = `team:fixture/dev|files:${JSON.stringify([args.args.scope ?? "team", args.args.team, args.args.storyId, args.args.agentId, args.args.dir])}`;
        fileViews.set(key, args.args);
        return key;
      }
      if (command === "live_hub_page" && !options.records) {
        const sub = subscriptions.get(args.subscriptionId)!;
        const offset = Number(args.cursor);
        const rows = rowsIn(sub.topic, undefined, sub.viewKey);
        if (sub.window) sub.window = [sub.window[0], Math.min(offset + 200, rows.length)];
        return {
          topic: sub.topic,
          viewKey: sub.viewKey,
          rev: sub.rev,
          snapshot: rows.slice(offset, offset + 200),
          total: rows.length,
          cursor: offset + 200 < rows.length ? String(offset + 200) : null,
        };
      }
      if (command === "live_hub_subscribe") {
        if (args.subscriptionId) {
          subscriptions.set(args.subscriptionId, {
            topic: args.topic,
            viewKey: args.viewKey,
            window: args.window,
            rev: rev(args.topic),
          });
          if (args.window) api.windows.push([args.topic, args.window]);
        }
        return {
          topic: args.topic,
          viewKey: args.viewKey,
          rev: rev(args.topic),
          snapshot: rowsIn(
            args.topic,
            args.window ?? (args.topic === "files" ? [0, 200] : undefined),
            args.viewKey,
          ),
          total: rowsIn(args.topic, undefined, args.viewKey).length,
          cursor:
            args.topic === "files" && rowsIn(args.topic, undefined, args.viewKey).length > 200
              ? "200"
              : null,
        };
      }
      if (command === "live_hub_window") {
        const s = subscriptions.get(args.subscriptionId)!;
        const before = rowsIn(s.topic, s.window, s.viewKey).map((row) => row.id);
        s.window = args.window;
        s.rev = rev(s.topic);
        api.windows.push([s.topic, args.window]);
        const upserts = rowsIn(s.topic, args.window);
        const kept = new Set(upserts.map((row) => row.id));
        return {
          topic: s.topic,
          viewKey: "all",
          rev: rev(s.topic),
          upserts,
          removes: before.filter((id) => !kept.has(id)),
          total: topics[s.topic].length,
        };
      }
      if (command === "live_hub_unsubscribe") {
        subscriptions.delete(args.subscriptionId);
        return null;
      }
      if (command === "agent_read_sessions") return topics.sessions;
      if (command === "file_surface_call" && args.operation === "resolve-open")
        return { path: args.args.path, org: "fixture", team: "dev" };
      if (command === "bridge_resolve_open_target")
        return { path: args.path, org: "fixture", team: "dev" };
      if (command === "send_prompt_to_agent") {
        await api.promptGate;
        return api.promptResult ?? { status: "submitted", submitted: true, message: null };
      }
      if (command === "list_inbox_rollup") {
        // Held only while a spec sets the gate, so other specs keep their timing.
        if (api.rollupGate) await api.rollupGate;
        const rollup = inboxFixtureRollup(inboxMessages());
        if (options.rollupWindow)
          rollup.messages = [...rollup.messages]
            .sort((a, b) => b.createdAtMs - a.createdAtMs)
            .slice(0, options.rollupWindow);
        return rollup;
      }
      if (command === "get_human_status")
        return { status: "online", note: "", updatedAtMs: Date.now() };
      if (command === "agent_read_inbox") {
        const query = args.query ?? {};
        const rows = options.rollupWindow
          ? inboxMessages()
              .filter(
                (row) =>
                  (!query.conversation || row.message.conversation === query.conversation) &&
                  (!query.before || row.messageId < query.before),
              )
              .sort((a, b) => b.messageId - a.messageId)
          : inboxMessages();
        const messages = rows.slice(0, query.limit ?? rows.length).map((row) => row.message);
        const rollup = inboxFixtureRollup(rows);
        return {
          messages,
          conversations: rollup.conversations,
          humanUnread: rollup.humanUnread,
          hasMore: messages.length < rows.length,
        };
      }
      // The engine's toggle: one reaction per (message, reactor, emoji). `remove` omitted
      // toggles, true removes and false adds; a change re-publishes the row.
      if (command === "react_as_human") {
        await api.reactGate;
        if (api.reactError) throw new Error(api.reactError);
        const row = inboxMessages().find(
          (r) =>
            r.messageId === args.messageId &&
            r.team.org === args.team?.org &&
            r.team.team === args.team?.team,
        );
        if (!row) return { status: "invalid", message: `Message ${args.messageId} is not here.` };
        const groups = (row.message.reactions ?? []).map((g) => ({
          ...g,
          reactors: [...g.reactors],
        }));
        const group = groups.find((g) => g.emoji === args.emoji);
        const had = !!group?.reactors.includes("human");
        const remove = args.remove ?? had;
        if (remove === !had)
          return {
            status: "reacted",
            messageId: args.messageId,
            emoji: args.emoji,
            outcome: "unchanged",
          };
        if (remove) group!.reactors = group!.reactors.filter((r) => r !== "human");
        else if (group) group.reactors.push("human");
        else groups.push({ emoji: args.emoji, count: 0, reactors: ["human"] });
        const reactions = groups
          .map((g) => ({ ...g, count: g.reactors.length }))
          .filter((g) => g.count);
        upsert("inbox", {
          ...row,
          message: { ...row.message, reactions },
          conversation: { ...row.conversation, reactions },
        });
        inboxChanged(row.team, [args.messageId]);
        return {
          status: "reacted",
          messageId: args.messageId,
          emoji: args.emoji,
          outcome: remove ? "removed" : "added",
        };
      }
      if (command === "local_profile_status") return { active: { principal: "fixture" } };
      if (command === "live_hub_view_key")
        return (options as { slotId?: string }).slotId === "file-nav" ? "team:fixture/dev" : "all";
      if (command === "bridge_doc_context")
        return { org: "fixture", team: "dev", layer: "team", contentRoot: "/fixture" };
      if (command === "query_context") {
        await options.queryGate;
        return { rows: [], total: 0 };
      }
      if (command === "load_app_state") return appState;
      if (command === "save_app_state") {
        appState = args.state;
        return null;
      }
      return null;
    },
  };
  useOrgStore.setState({
    activeOrgSlug: "fixture",
    activeTeamName: "dev",
    orgs: [
      {
        slug: "fixture",
        display_name: "Fixture",
        created_at: "2026-01-01T00:00:00Z",
        remote_url: null,
        config_flags: {},
        people: [],
        org_root: "/fixture",
        teams: [{ name: "dev", source_repos: [] }],
      },
    ],
  });
  useViewerStore.setState({
    navLens: {
      "fixture:dev": {
        ...DEFAULT_NAV_LENS,
        source: { kind: topics["ui:file-nav"]?.[0]?.scope === "private" ? "private" : "clone" },
      },
    },
    activeWorkspace: { "fixture:dev": "terminal" },
    terminalVisible: true,
    activeTab: options.activeTab ?? {},
    ...(topics["ui:file-nav"]?.[0]?.expanded
      ? { statusNavExpandedPaths: { "fixture:dev": topics["ui:file-nav"][0].expanded as string[] } }
      : {}),
  });
  useAgentListProjectionStore.setState({
    sessions: sessions as never,
    references: topics.agentList as never,
  });
  for (const topic of Object.keys(topics))
    liveHub.seed({ topic, viewKey: "all", rev: 1, snapshot: [...topics[topic]] });
  if (options.records) {
    const invoke = api.__TAURI_INTERNALS__.invoke;
    const commands = recordsHandlers(
      { emit } as MockEventBus,
      Object.fromEntries(
        [
          "mint_frame",
          "release_frame",
          "live_hub_subscribe",
          "live_hub_unsubscribe",
          "live_hub_page",
          "live_hub_window",
        ].map((name) => [name, (args: Record<string, unknown>) => invoke(name, args)]),
      ),
    );
    let seedFrame: any;
    let failWrites = false;
    const createNote = (note: NonNullable<FakeTauriOptions["records"]>[number]) =>
      commands.records_call({
        frameId: seedFrame.frameId,
        operation: "create",
        args: {
          collection: "notes",
          id: note.id,
          fields: { title: note.title, pinned: note.pinned ?? false },
          body: note.body,
        },
      });
    const ready = (async () => {
      seedFrame = await commands.mint_frame({
        args: {
          documentPath: "/fixture/notes-seed.html",
          contentRoot: "/fixture",
          slot: "right-pane",
        },
      });
      for (const note of options.records!) await createNote(note);
    })();
    let releasePrompt: (() => void) | undefined;
    api.noteFixture = {
      create: async (note: NonNullable<FakeTauriOptions["records"]>[number]) => {
        await ready;
        return createNote(note);
      },
      holdPrompt: () => {
        api.promptGate = new Promise<void>((resolve) => {
          releasePrompt = resolve;
        });
      },
      releasePrompt: () => {
        releasePrompt?.();
        api.promptGate = undefined;
      },
      promptResult: (result: unknown) => {
        api.promptResult = result;
      },
      fail: (value: boolean) => {
        failWrites = value;
      },
      read: async (id: string) => {
        await ready;
        return commands.records_call({
          frameId: seedFrame.frameId,
          operation: "read",
          args: { collection: "notes", id },
        });
      },
      write: async (id: string, body: string) => {
        await ready;
        return commands.records_call({
          frameId: seedFrame.frameId,
          operation: "write",
          args: { collection: "notes", id, body },
        });
      },
      prefs: () => appState,
    };
    api.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
      await ready;
      if (command === "load_app_state") return appState;
      if (command === "save_app_state") {
        appState = args.state;
        return null;
      }
      if (command === "records_call" && args.operation === "write" && failWrites)
        throw new Error("Store unavailable");
      if (commands[command]) {
        api.calls.push([command, args]);
        return commands[command](args);
      }
      return invoke(command, args);
    };
  }
  return api;
}
