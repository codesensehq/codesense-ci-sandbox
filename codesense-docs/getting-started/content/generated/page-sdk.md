# Page SDK

Generated from the Rust registry and TypeScript declarations. All 107 verbs and every declared Surface member are listed below. Rust checks each call against its frame grant.

[Exact TypeScript declarations](codesense.d.ts) · [Machine reference](page-sdk.json) · [Capabilities](capabilities.md) · [LiveHub](live-hub.md)

SDK version: 4. Features reported to has(): `window`, `firstPaint`, `lifecycle`, `slotContext`, `theme`, `kitTokens`, `virtualList`. A null feature means no separate member-named feature flag is declared. Named types in signatures are defined in the linked .d.ts; unknown stays unknown.

## Surface: version

```ts
readonly version: string;
```

Frozen SDK major version. Additive features are listed for `has()`, not versioned.

## Surface: has

```ts
has(name: string): boolean;
```

Whether this frame may use `name`: an SDK feature this app publishes, or a verb the
frame's effective grant admits. Known once the host answers the shim's ready.
Architecture: informational only; Rust still checks every call.

## Surface: ready

```ts
ready(): void;
```

Tell the host the page rendered. The shim's automatic ready does not count as live.

## Surface: firstPaint

```ts
readonly firstPaint: unknown;
```

Data for the first paint: the page's own `<script type="application/json"
id="codesense-dashboard-data">` JSON when it has one, else the server's first live-hub
snapshot (a `HubSnapshot`), else null. Narrow it before use.

## Surface: textMenuOpen

```ts
readonly textMenuOpen: boolean;
```

Architecture: true only while this frame's automatic text menu is held by the host.

## Surface: invoke

```ts
invoke<V extends keyof VerbMethods>(
      verb: V,
      ...args: Parameters<VerbMethods[V]>
    ): ReturnType<VerbMethods[V]>;
```



## Surface: act

```ts
act<V extends keyof VerbMethods>(
      verb: V,
      ...args: Parameters<VerbMethods[V]>
    ): ReturnType<VerbMethods[V]>;
```



## Surface: onBeforeReload

```ts
onBeforeReload(cb: () => ReloadState | Promise<ReloadState>): Unsubscribe;
```

Capture optional view state when a pane save freezes this document. The latest hook wins.
JSON only, at most 64 KB and 100 ms. Oversize/late results are dropped with frame and host
warnings; they never gate readiness or the swap. Return the unsubscribe to remove the hook.

## Surface: reloadState

```ts
reloadState(): Promise<ReloadState>;
```

The previous pane document's optional snapshot. Resolves exactly once to JSON or null.
A cold load or missing hook resolves null as soon as the host handshake arrives, without
waiting for the capture deadline. Await before restoring your view, then call ready().

## Surface: onFlush

```ts
onFlush(cb: () => void | Promise<void>): Unsubscribe;
```



## Surface: onTeardown

```ts
onTeardown(cb: () => void): Unsubscribe;
```



## Surface: onFocusIn

```ts
onFocusIn(cb: () => void): Unsubscribe;
```



## Surface: onFocusReturn

```ts
onFocusReturn(cb: (event: { anchorId?: string }) => void): Unsubscribe;
```



## Surface: onIntent

```ts
onIntent(cb: (intent: unknown) => void): Unsubscribe;
```



## Surface: onVisibility

```ts
onVisibility(cb: (visible: boolean) => void): Unsubscribe;
```



## Surface: onSlotContext

```ts
onSlotContext(cb: (context: SlotContext) => void): Unsubscribe;
```



## Surface: onMenuAction

```ts
onMenuAction(
      cb: (action: { itemId: string; row: Record<string, unknown> }) => void,
    ): Unsubscribe;
```

Receives each `codesense:command` for a command id this page registered.
Architecture: page-local Open/Reply/Open queue/New message callbacks, including hosted messages.

## Surface: onCommand

```ts
onCommand(cb: (command: CommandEvent) => void): Unsubscribe;
```



## Surface: commands

```ts
readonly commands: {
      /** Declare the command ids this page handles; resolves the ids now active. */
      register(ids: string[]): ReturnType<VerbMethods["commands.register"]>;
      list(): ReturnType<VerbMethods["commands.list"]>;
      run(id: string, args?: unknown): ReturnType<VerbMethods["commands.run"]>;
    };
```



## Surface: commands.register

```ts
register(ids: string[]): ReturnType<VerbMethods["commands.register"]>;
```

Declare the command ids this page handles; resolves the ids now active.

## Surface: commands.list

```ts
list(): ReturnType<VerbMethods["commands.list"]>;
```



## Surface: commands.run

```ts
run(id: string, args?: unknown): ReturnType<VerbMethods["commands.run"]>;
```



## Surface: onTheme

```ts
onTheme(cb: (theme: Theme) => void): Unsubscribe;
```



## Surface: theme

```ts
theme(): Promise<Theme | null>;
```



## Surface: getNavSelection

```ts
getNavSelection(): Promise<NavSelection | null>;
```



## Surface: onNavSelectionChange

```ts
onNavSelectionChange(cb: (selection: NavSelection | null) => void): Unsubscribe;
```



## Surface: focusOut

```ts
focusOut(args?: { direction?: "forward" | "backward" }): Promise<{ focused: boolean }>;
```



## Surface: focusTerminal

```ts
focusTerminal(): Promise<{ focused: boolean }>;
```



## Surface: showMenu

```ts
showMenu(
      args: MenuRequest,
    ): Promise<{ closed: true } | { itemId: string; row: unknown; builtin?: boolean }>;
```



## Surface: showPopover

```ts
showPopover(args: MenuRequest & { kind: PopoverKind }): Promise<{ closed: true }>;
```

`agent-quick-send` is the quick view; `agent-inbox-chat` is the agent's inbox chat.

## Surface: setBadge

```ts
setBadge(badge: Badge | null): void;
```



## Surface: snapshot

```ts
snapshot(state?: unknown): Promise<unknown>;
```



## Surface: subscribe

```ts
subscribe(topic: string, options?: SubscribeOptions): Promise<HubSnapshot>;
```



## Surface: unsubscribe

```ts
unsubscribe(
      topic: string,
      callback?: SubscribeOptions["onChange"],
      options?: SubscribeOptions,
    ): Promise<void>;
```



## Surface: page

```ts
page(topic: string, cursor: string, options?: SubscribeOptions): Promise<HubSnapshot>;
```



## Surface: files

```ts
readonly files: {
      [V in keyof VerbMethods as V extends `files.${infer Operation}`
        ? Operation
        : never]: VerbMethods[V];
    };
```

The `records.<operation>` verbs by operation: `records.list(args)` invokes `records.list`.

## Surface: records

```ts
readonly records: {
      [V in keyof VerbMethods as V extends `records.${infer Operation}`
        ? Operation
        : never]: VerbMethods[V];
    };
```



## Surface: setUiState

```ts
setUiState(
      topic: "ui:dock",
      patch: Omit<DockUiState, "selected">,
      requestId?: string,
    ): Promise<unknown>;
```



## Surface: setUiState

```ts
setUiState(topic: string, patch: Record<string, unknown>, requestId?: string): Promise<unknown>;
```



## Surface: openFile

```ts
openFile(
      target: string | { path: string; team?: string; newTab?: boolean; preview?: boolean },
      options?: { newTab?: boolean; preview?: boolean },
    ): Promise<unknown>;
```



## Surface: subscribeContext

```ts
subscribeContext(
      query: Record<string, unknown>,
      cb: (result: unknown, error: unknown) => void,
    ): Unsubscribe;
```



## Surface: onSessions

```ts
onSessions(cb: (rows: Record<string, unknown>[] | null, error: unknown) => void): Unsubscribe;
```



## Surface: onAgentsChanged

```ts
onAgentsChanged(cb: () => void): Unsubscribe;
```



## Surface: onInboxChanged

```ts
onInboxChanged(
      cb: (change?: {
        changes?: Array<{
          team: { org: string; team: string };
          messageIds: number[];
        }>;
      }) => void,
    ): Unsubscribe;
```

The change, when the host knows it: `changes` names, per team, the messages a committed
inbox update re-read (stored, read, archived, reacted to). It names only the frame's own
team when the frame is team-scoped. Absent for session refreshes.

## Surface: onRoutineResult

```ts
onRoutineResult(cb: () => void): Unsubscribe;
```



## Surface: onRoutinesChanged

```ts
onRoutinesChanged(cb: () => void): Unsubscribe;
```



## Surface: onMachinesChanged

```ts
onMachinesChanged(cb: () => void): Unsubscribe;
```



## Surface: onWorktreesChanged

```ts
onWorktreesChanged(cb: () => void): Unsubscribe;
```



## Surface: onPasteTargetChanged

```ts
onPasteTargetChanged(cb: () => void): Unsubscribe;
```

Runs `cb` when the answer to canPasteToTerminal() flips between true and false.
Re-read then.

## Surface: sprints

```ts
sprints(): Promise<unknown>;
```



## Surface: taxonomy

```ts
taxonomy(): Promise<unknown>;
```



## Surface: kit

```ts
kit?: Kit;
```

`/kit/v1/kit.js`, when the page loads the kit.

## VerbMethods: agent

```ts
agent(args?: Record<string, unknown>): Promise<unknown>;
```

read · sessions:read · MCP twin agent_get

Class: read; default grant: sessions:read; MCP twin: agent_get.

## VerbMethods: agents

```ts
agents(args?: Record<string, unknown>): Promise<Array<Record<string, unknown>>>;
```

read · sessions:read · MCP twin agent_list

Class: read; default grant: sessions:read; MCP twin: agent_list.

## VerbMethods: analyze

```ts
analyze(args?: Record<string, unknown>): Promise<unknown>;
```

read · context:read

Class: read; default grant: context:read; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: archive

```ts
archive(args?: Record<string, unknown>): Promise<unknown>;
```

metadata · inbox:write

Class: metadata; default grant: inbox:write; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: canPasteToTerminal

```ts
canPasteToTerminal(args?: Record<string, unknown>): Promise<boolean>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Pages ask it to enable a paste control; agents address sessions by handle with send_to_session..

## VerbMethods: closeTerminalTab

```ts
closeTerminalTab(args: {
      handle: string;
      [key: string]: unknown;
    }): Promise<{
      closed: boolean;
      [key: string]: unknown;
    }>;
```

manage · sessions:stop · confirmable · MCP twin close_terminal_tab

Class: manage; default grant: sessions:stop; MCP twin: close_terminal_tab.

## VerbMethods: closeWorkspace

```ts
closeWorkspace(args: {
      discardUnsaved?: boolean;
      removeWorktree?: boolean;
      target: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

manage · sessions:stop · confirmable · MCP twin close_workspace

Class: manage; default grant: sessions:stop; MCP twin: close_workspace.

## VerbMethods: commands.list

```ts
"commands.list"(args?: Record<string, unknown>): Promise<Array<{
      active: boolean;
      binding: string;
      dashboard: string;
      humanOnly: boolean;
      id: string;
      label: string;
      scope: "global" | "slot-visible" | "slot-focused";
      [key: string]: unknown;
    }>>;
```

read · commands:run · MCP twin command_list

Class: read; default grant: commands:run; MCP twin: command_list.

## VerbMethods: commands.register

```ts
"commands.register"(args: {
      ids: string[];
      [key: string]: unknown;
    }): Promise<{
      active: string[];
      [key: string]: unknown;
    }>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Frame-scoped declaration: the calling dashboard names which of its own declared commands are active.

## VerbMethods: commands.run

```ts
"commands.run"(args: {
      args?: unknown;
      id: string;
      [key: string]: unknown;
    }): Promise<{
      requested: true;
      [key: string]: unknown;
    }>;
```

read · commands:run · MCP twin command_run

Class: read; default grant: commands:run; MCP twin: command_run.

## VerbMethods: context

```ts
context(args?: Record<string, unknown>): Promise<unknown>;
```

read · context:read

Class: read; default grant: context:read; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: dismiss_agent_mark

```ts
dismiss_agent_mark(args: {
      crashId: string;
      requestId?: string;
      sessionId: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

metadata · sessions:read · MCP twin dismiss_agent_mark

Class: metadata; default grant: sessions:read; MCP twin: dismiss_agent_mark.

## VerbMethods: draft

```ts
draft(args?: Record<string, unknown>): Promise<unknown>;
```

metadata · context:write

Class: metadata; default grant: context:write; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: embedMessage

```ts
embedMessage(args?: Record<string, unknown>): Promise<unknown>;
```

spawn · inbox:read:team

Class: spawn; default grant: inbox:read:team; MCP twin: Host-minted inline inbox message frame.

## VerbMethods: files.create

```ts
"files.create"(args: {
      content?: string;
      onCollision: "keep-both" | "fail";
      path: string;
      [key: string]: unknown;
    }): Promise<{
      path: string;
      [key: string]: unknown;
    }>;
```

metadata · files:write · MCP twin create_content_file

Class: metadata; default grant: files:write; MCP twin: create_content_file.

## VerbMethods: files.delete

```ts
"files.delete"(args: {
      path: string;
      [key: string]: unknown;
    }): Promise<{
      deletedPath: string;
      [key: string]: unknown;
    }>;
```

metadata · files:write · confirmable · MCP twin delete_content_file

Class: metadata; default grant: files:write; MCP twin: delete_content_file.

## VerbMethods: files.list

```ts
"files.list"(args?: {
      agentId?: string;
      dir?: string;
      scope?: "team" | "private" | "story" | "agent";
      storyId?: string;
      team?: string;
      [key: string]: unknown;
    }): Promise<{
      rows: Array<{
        gitState?: string;
        id: string;
        inheritedFrom?: string;
        kind: "dir" | "file" | "story" | "epic";
        mtime?: number | null;
        name: string;
        path: string;
        size?: number | null;
        [key: string]: unknown;
      }>;
      [key: string]: unknown;
    }>;
```

read · files:read · MCP twin list_context_content

Class: read; default grant: files:read; MCP twin: list_context_content.

## VerbMethods: files.move

```ts
"files.move"(args: {
      from: string;
      to: string;
      [key: string]: unknown;
    }): Promise<{
      fromPath: string;
      toPath: string;
      [key: string]: unknown;
    }>;
```

metadata · files:write · confirmable · MCP twin move_content_file

Class: metadata; default grant: files:write; MCP twin: move_content_file.

## VerbMethods: files.read

```ts
"files.read"(args: {
      maxBytes?: number;
      path: string;
      [key: string]: unknown;
    }): Promise<{
      bytesUrl?: string;
      sha256: string;
      size: number;
      text?: string;
      truncated: boolean;
      [key: string]: unknown;
    }>;
```

read · files:read · MCP twin read_content_file

Class: read; default grant: files:read; MCP twin: read_content_file.

## VerbMethods: focusOut

```ts
focusOut(args?: {
      direction?: "forward" | "backward";
      [key: string]: unknown;
    }): Promise<{
      focused: boolean;
      [key: string]: unknown;
    }>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Host-only frame menus, popovers and focus; requires a rendered frame.

## VerbMethods: focusTerminal

```ts
focusTerminal(args?: Record<string, unknown>): Promise<{
      focused: boolean;
      [key: string]: unknown;
    }>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Host-only frame menus, popovers and focus; requires a rendered frame.

## VerbMethods: getDependencyStatus

```ts
getDependencyStatus(args?: Record<string, unknown>): Promise<unknown>;
```

read · machines:read

Class: read; default grant: machines:read; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: getDocDir

```ts
getDocDir(args?: Record<string, unknown>): Promise<string | null>;
```

read · context:read

Class: read; default grant: context:read; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: getEpicHandle

```ts
getEpicHandle(args?: Record<string, unknown>): Promise<string | null>;
```

read · context:read

Class: read; default grant: context:read; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: getWorkspaceStatus

```ts
getWorkspaceStatus(args?: {
      storyId?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

read · repo:read

Class: read; default grant: repo:read; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: gotoAgent

```ts
gotoAgent(args: {
      agentId: string;
      preserveFocus?: boolean;
      [key: string]: unknown;
    }): Promise<unknown>;
```

spawn · sessions:read

Class: spawn; default grant: sessions:read; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: humanStatus

```ts
humanStatus(args?: Record<string, unknown>): Promise<unknown>;
```

read · inbox:read:team · MCP twin get_human_status

Class: read; default grant: inbox:read:team; MCP twin: get_human_status.

## VerbMethods: inbox

```ts
inbox(args?: {
      before?: number;
      conversation?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

read · inbox:read:team · MCP twin inbox_list

Class: read; default grant: inbox:read:team; MCP twin: inbox_list.

## VerbMethods: inboxRollup

```ts
inboxRollup(args?: Record<string, unknown>): Promise<unknown>;
```

read · inbox:read:all-teams

Class: read; default grant: inbox:read:all-teams; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: launchUnder

```ts
launchUnder(args: {
      handle: string;
      task?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

spawn · sessions:read

Class: spawn; default grant: sessions:read; MCP twin: Opens the native launch sheet; agents launch with open_workspace.

## VerbMethods: loadMedia

```ts
loadMedia(args: {
      messageId: number;
      org?: string;
      team?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

metadata · inbox:write · confirmable

Class: metadata; default grant: inbox:write; MCP twin: Fetch one inbox image after the user requests it.

## VerbMethods: machines

```ts
machines(args?: Record<string, unknown>): Promise<Array<Record<string, unknown>>>;
```

read · machines:read · MCP twin list_machines

Class: read; default grant: machines:read; MCP twin: list_machines.

## VerbMethods: markRead

```ts
markRead(args?: Record<string, unknown>): Promise<unknown>;
```

metadata · inbox:write

Class: metadata; default grant: inbox:write; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: markUnread

```ts
markUnread(args: {
      messageIds: number[];
      org?: string;
      team?: string;
      [key: string]: unknown;
    }): Promise<{
      marked: number;
      [key: string]: unknown;
    }>;
```

metadata · inbox:write · MCP twin mark_unread

Class: metadata; default grant: inbox:write; MCP twin: mark_unread.

## VerbMethods: modelRates

```ts
modelRates(args?: Record<string, unknown>): Promise<unknown>;
```

read · sessions:read · MCP twin model_rates

Class: read; default grant: sessions:read; MCP twin: model_rates.

## VerbMethods: moveWorkspace

```ts
moveWorkspace(args: {
      after?: string;
      before?: string;
      teamKey: string;
      workspaceId: string;
      [key: string]: unknown;
    }): Promise<{
      moved: boolean;
      [key: string]: unknown;
    }>;
```

prefs · sessions:read · MCP twin reorder_workspace

Class: prefs; default grant: sessions:read; MCP twin: reorder_workspace.

## VerbMethods: openAgentInbox

```ts
openAgentInbox(args: {
      handle: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

spawn · inbox:read:team

Class: spawn; default grant: inbox:read:team; MCP twin: Reveals the agent and opens its native inbox rail.

## VerbMethods: openDiff

```ts
openDiff(args: {
      path: string;
      range?: {
        end: number;
        side?: "after";
        start: number;
      };
      repo: string;
      rev: string;
      storyId?: string;
    }): Promise<{
      status: "opened";
      [key: string]: unknown;
    } | {
      fetchCommand: string;
      reason: string;
      status: "commitNotLocal";
      [key: string]: unknown;
    } | {
      reason: string;
      status: "worktreeRemoved" | "refused" | "unavailable";
      [key: string]: unknown;
    }>;
```

spawn · repo:read

Class: spawn; default grant: repo:read; MCP twin: Opens a read-only source diff tab in the Mac viewer..

## VerbMethods: openFile

```ts
openFile(args: {
      newTab?: boolean;
      path: string;
      preview?: boolean;
      team?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

spawn · context:read

Class: spawn; default grant: context:read; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: openLauncher

```ts
openLauncher(args: {
      org?: string;
      task: string;
      team?: string;
      [key: string]: unknown;
    }): Promise<{
      opened: boolean;
      [key: string]: unknown;
    }>;
```

spawn · sessions:read

Class: spawn; default grant: sessions:read; MCP twin: Opens a filled native launch sheet; agents launch with open_workspace.

## VerbMethods: openMessage

```ts
openMessage(args?: Record<string, unknown>): Promise<unknown>;
```

spawn · inbox:read:team

Class: spawn; default grant: inbox:read:team; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: openRoutineDraft

```ts
openRoutineDraft(args: {
      agent?: string;
      name: string;
      task: string;
      when?: string;
      [key: string]: unknown;
    }): Promise<{
      opened: boolean;
      [key: string]: unknown;
    }>;
```

spawn · sessions:read

Class: spawn; default grant: sessions:read; MCP twin: Opens a filled private routine draft; agents create with routine_create.

## VerbMethods: openTab

```ts
openTab(args?: {
      agent?: boolean;
      workspaceId?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

spawn · launch · MCP twin open_terminal_tab

Class: spawn; default grant: launch; MCP twin: open_terminal_tab.

## VerbMethods: openWorkspace

```ts
openWorkspace(args?: {
      headed?: boolean;
      initialPrompt?: string;
      sessionMode?: "new" | "focus";
      storyId?: string;
      target?: {
        kind?: "story" | "epic" | "branch" | "dir";
        path?: string;
        storyId?: string;
        [key: string]: unknown;
      };
      [key: string]: unknown;
    }): Promise<unknown>;
```

spawn · launch · confirmable · MCP twin open_workspace

Class: spawn; default grant: launch; MCP twin: open_workspace.

## VerbMethods: page

```ts
page(args: {
      collection?: string;
      cursor: string;
      topic: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Host projection subscription, inbox cursor paging or shared slot UI topics.

Conditional grants (override the default when their conditions match):

- topic=files → files:read
- topic=records; bound collection scope=private → records:read:own
- topic=records; bound collection scope=team → records:read:team
- topic=records; bound collection scope=story → records:read:team
- topic=inbox; caller scope=team → inbox:read:team
- topic=inbox; caller scope=other → inbox:read:all-teams

## VerbMethods: postMessage

```ts
postMessage(args?: {
      team?: Record<string, unknown>;
      text?: string;
      to?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

send · inbox:write · confirmable · MCP twin send_message

Class: send; default grant: inbox:write; MCP twin: send_message.

## VerbMethods: prefs

```ts
prefs(args?: Record<string, unknown>): Promise<unknown>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: queryContext

```ts
queryContext(args?: Record<string, unknown>): Promise<unknown>;
```

read · context:read · MCP twin query_context

Class: read; default grant: context:read; MCP twin: query_context.

## VerbMethods: react

```ts
react(args?: {
      emoji?: string;
      messageId?: number;
      remove?: boolean;
      [key: string]: unknown;
    }): Promise<unknown>;
```

metadata · inbox:write · MCP twin react

Class: metadata; default grant: inbox:write; MCP twin: react.

## VerbMethods: readWorkspace

```ts
readWorkspace(args: {
      path: string;
      team?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

read · files:read

Class: read; default grant: files:read; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: records.collections

```ts
"records.collections"(args?: {
      append?: string;
      base?: string;
      body?: string;
      collection?: string;
      commit?: string;
      fields?: Record<string, unknown>;
      id?: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

read · records:read:own · MCP twin records_collections

Class: read; default grant: records:read:own; MCP twin: records_collections.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:read:own
- bound collection scope=team → records:read:team
- bound collection scope=story → records:read:team

## VerbMethods: records.create

```ts
"records.create"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit?: string;
      fields?: Record<string, unknown>;
      id?: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

metadata · records:write:own · MCP twin records_create

Class: metadata; default grant: records:write:own; MCP twin: records_create.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:write:own
- bound collection scope=team → records:write:team
- bound collection scope=story → records:write:team

## VerbMethods: records.delete

```ts
"records.delete"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit?: string;
      fields?: Record<string, unknown>;
      id: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

metadata · records:write:own · confirmable · MCP twin records_delete

Class: metadata; default grant: records:write:own; MCP twin: records_delete.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:write:own
- bound collection scope=team → records:write:team
- bound collection scope=story → records:write:team

## VerbMethods: records.history

```ts
"records.history"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit?: string;
      fields?: Record<string, unknown>;
      id: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

read · records:read:own · MCP twin records_history

Class: read; default grant: records:read:own; MCP twin: records_history.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:read:own
- bound collection scope=team → records:read:team
- bound collection scope=story → records:read:team

## VerbMethods: records.list

```ts
"records.list"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit?: string;
      fields?: Record<string, unknown>;
      id?: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

read · records:read:own · MCP twin records_list

Class: read; default grant: records:read:own; MCP twin: records_list.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:read:own
- bound collection scope=team → records:read:team
- bound collection scope=story → records:read:team

## VerbMethods: records.read

```ts
"records.read"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit?: string;
      fields?: Record<string, unknown>;
      id: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

read · records:read:own · MCP twin records_read

Class: read; default grant: records:read:own; MCP twin: records_read.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:read:own
- bound collection scope=team → records:read:team
- bound collection scope=story → records:read:team

## VerbMethods: records.readAt

```ts
"records.readAt"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit: string;
      fields?: Record<string, unknown>;
      id: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

read · records:read:own · MCP twin records_read_at

Class: read; default grant: records:read:own; MCP twin: records_read_at.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:read:own
- bound collection scope=team → records:read:team
- bound collection scope=story → records:read:team

## VerbMethods: records.restore

```ts
"records.restore"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit: string;
      fields?: Record<string, unknown>;
      id: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

metadata · records:write:own · MCP twin records_restore

Class: metadata; default grant: records:write:own; MCP twin: records_restore.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:write:own
- bound collection scope=team → records:write:team
- bound collection scope=story → records:write:team

## VerbMethods: records.search

```ts
"records.search"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit?: string;
      fields?: Record<string, unknown>;
      id?: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

read · records:read:own · MCP twin records_search

Class: read; default grant: records:read:own; MCP twin: records_search.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:read:own
- bound collection scope=team → records:read:team
- bound collection scope=story → records:read:team

## VerbMethods: records.setFields

```ts
"records.setFields"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit?: string;
      fields: Record<string, unknown>;
      id: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

metadata · records:write:own · MCP twin records_set_fields

Class: metadata; default grant: records:write:own; MCP twin: records_set_fields.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:write:own
- bound collection scope=team → records:write:team
- bound collection scope=story → records:write:team

## VerbMethods: records.write

```ts
"records.write"(args: {
      append?: string;
      base?: string;
      body?: string;
      collection: string;
      commit?: string;
      fields?: Record<string, unknown>;
      id: string;
      limit?: number;
      patch?: {
        find: string;
        replace: string;
        [key: string]: unknown;
      };
      query?: string;
      requestId?: string;
      undo?: boolean;
      window?: [number, number];
    }): Promise<unknown>;
```

metadata · records:write:own · MCP twin records_write

Class: metadata; default grant: records:write:own; MCP twin: records_write.

Conditional grants (override the default when their conditions match):

- bound collection scope=private → records:write:own
- bound collection scope=team → records:write:team
- bound collection scope=story → records:write:team

## VerbMethods: replyAsHuman

```ts
replyAsHuman(args?: {
      messageId?: number;
      text?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

send · inbox:write · confirmable · MCP twin reply

Class: send; default grant: inbox:write; MCP twin: reply.

## VerbMethods: roles

```ts
roles(args?: Record<string, unknown>): Promise<unknown>;
```

read · inbox:read:team · MCP twin role_state

Class: read; default grant: inbox:read:team; MCP twin: role_state.

## VerbMethods: routeToContext

```ts
routeToContext(args?: Record<string, unknown>): Promise<unknown>;
```

metadata · context:write

Class: metadata; default grant: context:write; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: routine

```ts
routine(args?: {
      key?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

read · routines:read · MCP twin routine_get

Class: read; default grant: routines:read; MCP twin: routine_get.

## VerbMethods: routineCancel

```ts
routineCancel(args?: {
      key?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

manage · routines:manage · confirmable

Class: manage; default grant: routines:manage; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: routineRefresh

```ts
routineRefresh(args?: {
      key?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

spawn · routines:run

Class: spawn; default grant: routines:run; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: routineResult

```ts
routineResult(args?: {
      key?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

read · routines:read · MCP twin routine_result

Class: read; default grant: routines:read; MCP twin: routine_result.

## VerbMethods: routineRun

```ts
routineRun(args?: Record<string, unknown>): Promise<unknown>;
```

read · routines:read · MCP twin routine_run

Class: read; default grant: routines:read; MCP twin: routine_run.

## VerbMethods: routineRuns

```ts
routineRuns(args?: Record<string, unknown>): Promise<unknown>;
```

read · routines:read · MCP twin routine_runs

Class: read; default grant: routines:read; MCP twin: routine_runs.

## VerbMethods: routineStatus

```ts
routineStatus(args?: {
      key?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

read · routines:read

Class: read; default grant: routines:read; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: routines

```ts
routines(args?: Record<string, unknown>): Promise<Array<Record<string, unknown>>>;
```

read · routines:read · MCP twin routine_list

Class: read; default grant: routines:read; MCP twin: routine_list.

## VerbMethods: run

```ts
run(args?: Record<string, unknown>): Promise<unknown>;
```

read · routines:read · MCP twin read_run

Class: read; default grant: routines:read; MCP twin: read_run.

## VerbMethods: runEvidence

```ts
runEvidence(args?: Record<string, unknown>): Promise<unknown>;
```

read · routines:read · MCP twin read_run_evidence

Class: read; default grant: routines:read; MCP twin: read_run_evidence.

## VerbMethods: runPrompt

```ts
runPrompt(args: {
      prompt: string;
      target: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

send · sessions:send · confirmable

Class: send; default grant: sessions:send; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: runSkill

```ts
runSkill(args: {
      args?: string[];
      skill: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

spawn · launch

Class: spawn; default grant: launch; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: runWorkProduct

```ts
runWorkProduct(args?: Record<string, unknown>): Promise<unknown>;
```

read · routines:read · MCP twin read_run_work_product

Class: read; default grant: routines:read; MCP twin: read_run_work_product.

## VerbMethods: runs

```ts
runs(args?: Record<string, unknown>): Promise<unknown>;
```

read · routines:read · MCP twin list_runs

Class: read; default grant: routines:read; MCP twin: list_runs.

## VerbMethods: sendAsHuman

```ts
sendAsHuman(args?: {
      team?: Record<string, unknown>;
      text?: string;
      to?: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

send · inbox:write · confirmable · MCP twin send_message

Class: send; default grant: inbox:write; MCP twin: send_message.

## VerbMethods: sendPrompt

```ts
sendPrompt(args: {
      handle: string;
      submit?: boolean;
      text: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

send · sessions:send · confirmable

Class: send; default grant: sessions:send; MCP twin: Agents prompt running agents with send_to_session; this is the person's typed send from a page..

## VerbMethods: sendToSession

```ts
sendToSession(args: {
      target: string;
      text: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

send · sessions:send · confirmable · MCP twin send_to_session

Class: send; default grant: sessions:send; MCP twin: send_to_session.

## VerbMethods: sendToWorkspaceTab

```ts
sendToWorkspaceTab(args: {
      mode?: "prompt" | "command";
      sessionId: string;
      text: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

send · sessions:send · confirmable

Class: send; default grant: sessions:send; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: sessionRead

```ts
sessionRead(args: {
      cursor?: number | null;
      include?: unknown[];
      limit?: number | null;
      sessionId: string;
      toolUseId?: string | null;
      turn?: number | null;
    }): Promise<unknown>;
```

read · sessions:read · MCP twin session_read

Class: read; default grant: sessions:read; MCP twin: session_read.

## VerbMethods: sessionUsage

```ts
sessionUsage(args?: Record<string, unknown>): Promise<unknown>;
```

read · sessions:read · MCP twin session_usage

Class: read; default grant: sessions:read; MCP twin: session_usage.

## VerbMethods: sessions

```ts
sessions(args?: Record<string, unknown>): Promise<Array<Record<string, unknown>>>;
```

read · sessions:read · MCP twin list_sessions

Class: read; default grant: sessions:read; MCP twin: list_sessions.

## VerbMethods: setField

```ts
setField(args: {
      field?: string;
      fields?: Record<string, unknown>;
      path: string;
      value?: unknown;
      [key: string]: unknown;
    }): Promise<unknown>;
```

metadata · context:write

Class: metadata; default grant: context:write; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: setHumanStatus

```ts
setHumanStatus(args?: Record<string, unknown>): Promise<unknown>;
```

metadata · inbox:write

Class: metadata; default grant: inbox:write; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: setModelRate

```ts
setModelRate(args?: Record<string, unknown>): Promise<unknown>;
```

metadata · accounts:manage · MCP twin set_model_rate

Class: metadata; default grant: accounts:manage; MCP twin: set_model_rate.

## VerbMethods: setPrefs

```ts
setPrefs(args?: Record<string, unknown>): Promise<unknown>;
```

prefs · sessions:read

Class: prefs; default grant: sessions:read; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: setStatus

```ts
setStatus(args: {
      path: string | string[];
      status: string;
      [key: string]: unknown;
    }): Promise<Array<{
      ok?: boolean;
      path?: string;
      [key: string]: unknown;
    }>>;
```

metadata · context:write

Class: metadata; default grant: context:write; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: setUiState

```ts
setUiState(args: {
      patch: Record<string, unknown>;
      requestId?: string;
      topic: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

prefs · sessions:read

Class: prefs; default grant: sessions:read; MCP twin: Host-owned shared slot UI state.

## VerbMethods: setup

```ts
setup(args?: Record<string, unknown>): Promise<unknown>;
```

read · repo:read · MCP twin describe_setup

Class: read; default grant: repo:read; MCP twin: describe_setup.

## VerbMethods: showMenu

```ts
showMenu(args: {
      anchorId?: string;
      anchorRect: {
        height: number;
        width: number;
        x: number;
        y: number;
        [key: string]: unknown;
      };
      items?: unknown[];
      kind?: string;
      row?: Record<string, unknown>;
      sessionId?: string;
      target?: "row" | "text" | "frame";
      text?: {
        editable?: boolean;
        hasSelection?: boolean;
        hasText?: boolean;
        password?: boolean;
        [key: string]: unknown;
      };
      [key: string]: unknown;
    }): Promise<{
      closed: true;
      [key: string]: unknown;
    } | {
      builtin?: boolean;
      itemId: string;
      row: unknown;
      [key: string]: unknown;
    }>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Host-only frame menus, popovers and focus; requires a rendered frame.

## VerbMethods: showPopover

```ts
showPopover(args: {
      anchorId?: string;
      anchorRect: {
        height: number;
        width: number;
        x: number;
        y: number;
        [key: string]: unknown;
      };
      items?: unknown[];
      kind: string;
      row?: Record<string, unknown>;
      sessionId?: string;
      target?: "row" | "text" | "frame";
      text?: {
        editable?: boolean;
        hasSelection?: boolean;
        hasText?: boolean;
        password?: boolean;
        [key: string]: unknown;
      };
      [key: string]: unknown;
    }): Promise<{
      closed: true;
      [key: string]: unknown;
    }>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Host-only frame menus, popovers and focus; requires a rendered frame.

## VerbMethods: stopSession

```ts
stopSession(args: {
      handle: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

manage · sessions:stop · confirmable

Class: manage; default grant: sessions:stop; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: storyChanges

```ts
storyChanges(args: {
      cursor?: string | null;
      include?: "ranges"[];
      limit?: number | null;
      only?: Array<"tasks" | "sessions" | "commits" | "prs" | "branches" | "worktrees" | "actual" | "reviews"> | null;
      storyId: string;
    }): Promise<unknown>;
```

read · sessions:read · MCP twin story_changes

Class: read; default grant: sessions:read; MCP twin: story_changes.

## VerbMethods: storyTotals

```ts
storyTotals(args: {
      storyIds: string[];
    }): Promise<unknown>;
```

read · sessions:read · MCP twin story_totals

Class: read; default grant: sessions:read; MCP twin: story_totals.

## VerbMethods: subscribe

```ts
subscribe(args: {
      agentId?: string;
      collection?: string;
      dir?: string;
      scope?: string;
      sinceRev?: number;
      storyId?: string;
      team?: string;
      topic: string;
      window?: [number, number];
      [key: string]: unknown;
    }): Promise<unknown>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Host projection subscription, inbox cursor paging or shared slot UI topics.

Conditional grants (override the default when their conditions match):

- topic=files → files:read
- topic=records; bound collection scope=private → records:read:own
- topic=records; bound collection scope=team → records:read:team
- topic=records; bound collection scope=story → records:read:team
- topic=inbox; caller scope=team → inbox:read:team
- topic=inbox; caller scope=other → inbox:read:all-teams

## VerbMethods: unarchive

```ts
unarchive(args: {
      messageIds: number[];
      org?: string;
      restoreUnread?: number[];
      team?: string;
      [key: string]: unknown;
    }): Promise<{
      unarchived: number;
      [key: string]: unknown;
    }>;
```

metadata · inbox:write

Class: metadata; default grant: inbox:write; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: unsubscribe

```ts
unsubscribe(args: {
      collection?: string;
      topic: string;
      [key: string]: unknown;
    }): Promise<unknown>;
```

read · sessions:read

Class: read; default grant: sessions:read; MCP twin: Host projection subscription, inbox cursor paging or shared slot UI topics.

Conditional grants (override the default when their conditions match):

- topic=files → files:read
- topic=records; bound collection scope=private → records:read:own
- topic=records; bound collection scope=team → records:read:team
- topic=records; bound collection scope=story → records:read:team
- topic=inbox; caller scope=team → inbox:read:team
- topic=inbox; caller scope=other → inbox:read:all-teams

## VerbMethods: updates

```ts
updates(args?: Record<string, unknown>): Promise<unknown>;
```

read · inbox:read:team · MCP twin list_agent_updates

Class: read; default grant: inbox:read:team; MCP twin: list_agent_updates.

## VerbMethods: validateYaml

```ts
validateYaml(args?: Record<string, unknown>): Promise<unknown>;
```

read · context:read

Class: read; default grant: context:read; MCP twin: Host operation bound to the rendered dashboard; no identical MCP binding.

## VerbMethods: watchAgentActivity

```ts
watchAgentActivity(args?: Record<string, unknown>): Promise<unknown>;
```

read · inbox:read:team

Class: read; default grant: inbox:read:team; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: watchAgentInbox

```ts
watchAgentInbox(args?: Record<string, unknown>): Promise<unknown>;
```

read · inbox:read:team

Class: read; default grant: inbox:read:team; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: watchInbox

```ts
watchInbox(args?: Record<string, unknown>): Promise<unknown>;
```

read · inbox:read:team

Class: read; default grant: inbox:read:team; MCP twin: Host presentation or dashboard-local operation.

## VerbMethods: whyLine

```ts
whyLine(args: {
      end: number;
      path: string;
      repo: string;
      rev?: string | null;
      start: number;
      story?: string | null;
    }): Promise<unknown>;
```

read · sessions:read · MCP twin why_line

Class: read; default grant: sessions:read; MCP twin: why_line.

## VerbMethods: worktrees

```ts
worktrees(args?: Record<string, unknown>): Promise<Array<Record<string, unknown>>>;
```

read · repo:read · MCP twin list_worktrees

Class: read; default grant: repo:read; MCP twin: list_worktrees.

## Kit: kit.stageRail

```ts
stageRail(role: "not_started" | "active" | "review" | "blocked" | "done" | null | undefined, ready?: boolean): HTMLSpanElement;
```



## Kit: kit.statusMark

```ts
statusMark(mark: string, options?: { size?: 6 | 7 | 8; still?: boolean }): HTMLSpanElement;
```

`size` is 6, 7 (default) or 8; `still` draws the mark without its motion.

## Kit: kit.fileKindIcon

```ts
fileKindIcon(kind: string): SVGSVGElement;
```



## Kit: kit.navGlyph

```ts
navGlyph(name: "changes" | "history" | "grouped"): Element;
```

The nav's own glyphs for the Changes and History views and the by-stage arrangement.

## Kit: kit.fileKindOf

```ts
fileKindOf(path: string): string;
```

The nav's file kind for a path, the name `fileKindIcon` takes.

## Kit: kit.stripViewerExtension

```ts
stripViewerExtension(name: string): string;
```

The name without its viewer extension, as the nav shows it.

## Kit: kit.stripPipelineExtension

```ts
stripPipelineExtension(name: string): string;
```

The name without its pipeline extension, as the nav shows it.

## Kit: kit.inLens

```ts
inLens(lens: NavLens, story: NavStory): boolean;
```

Whether a story belongs to a lens.

## Kit: kit.countLenses

```ts
countLenses(stories: Iterable<NavStory>): Record<NavLens, number>;
```

How many stories each lens holds.

## Kit: kit.stageGroupOf

```ts
stageGroupOf(story: NavStory): string;
```

The stage group a story is listed under: `needs`, a role, or `off`.

## Kit: kit.compareNavNames

```ts
compareNavNames(a: string, b: string): number;
```

The nav's name order (numeric, case-insensitive); a sort comparator.

## Kit: kit.createEditor

```ts
createEditor?: (element: HTMLElement, options?: KitEditorOptions) => KitEditor;
```

The createEditor export of /kit/v1/editor.js (loaded separately as an ES module).

## Kit: kit.version

```ts
readonly version: string;
```



## Kit: kit.query

```ts
query(name: string, args?: Record<string, unknown>): Promise<{ ok: boolean; answer?: unknown }>;
```

The same read in both hosts: resolves the answer message.

## Kit: kit.has

```ts
has(name: string): boolean;
```

Delegates to `codesense.has`.

## Kit: kit.call

```ts
call(verb: string, args?: unknown): Promise<unknown>;
```

The raw bridge verb (the app only): not budgeted, for actions. Reads go through `query`, `queryAll`, `storyTotals` and the patterns, which share the budget.

## Kit: kit.keyed

```ts
keyed<Item>(
      container: HTMLElement,
      items: readonly Item[],
      options: {
        key: (item: Item) => string | number;
        render: (item: Item) => HTMLElement;
        version?: (item: Item) => string | number;
        update?: (element: HTMLElement, item: Item) => void;
      },
    ): void;
```

Reuse nodes by key and version (default JSON.stringify(item)). Patch every reused
node with update; a changed version calls render. New nodes are initialized by render.

## Kit: kit.virtualList

```ts
virtualList?: <Row extends HubRow = HubRow>(
      options: VirtualListOptions<Row>,
    ) => VirtualList<Row>;
```

`/kit/v1/virtual-list.js`, when the page loads it.
Rows keep their node by id and layoutKey; update patches reused rows in place.

## Kit: kit.[helper: string]

```ts
[helper: string]: unknown;
```

Undeclared kit helpers have unknown type.
