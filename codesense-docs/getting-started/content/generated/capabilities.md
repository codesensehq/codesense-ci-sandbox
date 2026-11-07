# Capabilities

30 grantable capabilities. Under team-safe, declarations are intersected with the slot and scope ceilings. The default open policy grants full reach. Registry grants are defaults. Conditional grants override them when the topic, caller scope or bound collection scope matches; all admitted verbs below include those conditional paths. Frame ceilings can further restrict reach.

| Capability | Team-safe team | Team-safe message | Admitted verbs | Conditional admission |
| --- | --- | --- | --- | --- |
| sessions:read | true | true | agent, agents, commands.register, dismiss_agent_mark, focusOut, focusTerminal, gotoAgent, launchUnder, modelRates, moveWorkspace, openLauncher, openRoutineDraft, page, prefs, sessionRead, sessionUsage, sessions, setPrefs, setUiState, showMenu, showPopover, storyChanges, storyTotals, subscribe, unsubscribe, whyLine |  |
| sessions:send | true | false | runPrompt, sendPrompt, sendToSession, sendToWorkspaceTab |  |
| sessions:stop | true | false | closeTerminalTab, closeWorkspace, stopSession |  |
| launch | true | false | openTab, openWorkspace, runSkill |  |
| inbox:read:own | true | true | none in page registry |  |
| inbox:read:team | true | true | embedMessage, humanStatus, inbox, openAgentInbox, openMessage, page, roles, subscribe, unsubscribe, updates, watchAgentActivity, watchAgentInbox, watchInbox | page: topic=inbox; caller scope=team → inbox:read:team; subscribe: topic=inbox; caller scope=team → inbox:read:team; unsubscribe: topic=inbox; caller scope=team → inbox:read:team |
| inbox:read:all-teams | false | false | inboxRollup, page, subscribe, unsubscribe | page: topic=inbox; caller scope=other → inbox:read:all-teams; subscribe: topic=inbox; caller scope=other → inbox:read:all-teams; unsubscribe: topic=inbox; caller scope=other → inbox:read:all-teams |
| inbox:write | true | true | archive, loadMedia, markRead, markUnread, postMessage, react, replyAsHuman, sendAsHuman, setHumanStatus |  |
| records:read:own | false | false | page, records.collections, records.history, records.list, records.read, records.readAt, records.search, subscribe, unsubscribe | page: topic=records; bound collection scope=private → records:read:own; records.collections: bound collection scope=private → records:read:own; records.history: bound collection scope=private → records:read:own; records.list: bound collection scope=private → records:read:own; records.read: bound collection scope=private → records:read:own; records.readAt: bound collection scope=private → records:read:own; records.search: bound collection scope=private → records:read:own; subscribe: topic=records; bound collection scope=private → records:read:own; unsubscribe: topic=records; bound collection scope=private → records:read:own |
| records:write:own | false | false | records.create, records.delete, records.restore, records.setFields, records.write | records.create: bound collection scope=private → records:write:own; records.delete: bound collection scope=private → records:write:own; records.restore: bound collection scope=private → records:write:own; records.setFields: bound collection scope=private → records:write:own; records.write: bound collection scope=private → records:write:own |
| records:read:team | true | true | page, records.collections, records.history, records.list, records.read, records.readAt, records.search, subscribe, unsubscribe | page: topic=records; bound collection scope=team → records:read:team; page: topic=records; bound collection scope=story → records:read:team; records.collections: bound collection scope=team → records:read:team; records.collections: bound collection scope=story → records:read:team; records.history: bound collection scope=team → records:read:team; records.history: bound collection scope=story → records:read:team; records.list: bound collection scope=team → records:read:team; records.list: bound collection scope=story → records:read:team; records.read: bound collection scope=team → records:read:team; records.read: bound collection scope=story → records:read:team; records.readAt: bound collection scope=team → records:read:team; records.readAt: bound collection scope=story → records:read:team; records.search: bound collection scope=team → records:read:team; records.search: bound collection scope=story → records:read:team; subscribe: topic=records; bound collection scope=team → records:read:team; subscribe: topic=records; bound collection scope=story → records:read:team; unsubscribe: topic=records; bound collection scope=team → records:read:team; unsubscribe: topic=records; bound collection scope=story → records:read:team |
| records:write:team | true | false | records.create, records.delete, records.restore, records.setFields, records.write | records.create: bound collection scope=team → records:write:team; records.create: bound collection scope=story → records:write:team; records.delete: bound collection scope=team → records:write:team; records.delete: bound collection scope=story → records:write:team; records.restore: bound collection scope=team → records:write:team; records.restore: bound collection scope=story → records:write:team; records.setFields: bound collection scope=team → records:write:team; records.setFields: bound collection scope=story → records:write:team; records.write: bound collection scope=team → records:write:team; records.write: bound collection scope=story → records:write:team |
| context:read | true | true | analyze, context, getDocDir, getEpicHandle, openFile, queryContext, validateYaml |  |
| context:write | true | false | draft, routeToContext, setField, setStatus |  |
| files:read | true | true | files.list, files.read, page, readWorkspace, subscribe, unsubscribe | page: topic=files → files:read; subscribe: topic=files → files:read; unsubscribe: topic=files → files:read |
| files:write | true | false | files.create, files.delete, files.move |  |
| routines:read | true | true | routine, routineResult, routineRun, routineRuns, routineStatus, routines, run, runEvidence, runWorkProduct, runs |  |
| routines:run | true | false | routineRefresh |  |
| routines:manage | true | false | routineCancel |  |
| repo:read | true | true | getWorkspaceStatus, openDiff, setup, worktrees |  |
| repo:write | true | false | none in page registry |  |
| machines:read | true | true | getDependencyStatus, machines |  |
| machines:manage | false | false | none in page registry |  |
| accounts:manage | false | false | setModelRate |  |
| dashboards:install:private | false | false | none in page registry |  |
| dashboards:publish:team | true | true | none in page registry |  |
| terminal:view | true | true | none in page registry |  |
| terminal:control | false | false | none in page registry |  |
| commands:run | true | true | commands.list, commands.run |  |
| admin | false | false | none in page registry |  |

## Slot ceilings

| Slot | Capabilities |
| --- | --- |
| right-pane | sessions:read, sessions:send, sessions:stop, launch, inbox:read:own, inbox:read:team, inbox:read:all-teams, inbox:write, records:read:own, records:write:own, records:read:team, records:write:team, context:read, context:write, files:read, files:write, routines:read, routines:run, routines:manage, repo:read, repo:write, machines:read, machines:manage, accounts:manage, dashboards:install:private, dashboards:publish:team, terminal:view, terminal:control, commands:run, admin |
| dock | sessions:read, sessions:send, sessions:stop, launch, inbox:read:own, inbox:read:team, inbox:write, records:read:team, records:write:team, context:read, context:write, files:read, files:write, routines:read, routines:run, routines:manage, repo:read, repo:write, machines:read, dashboards:publish:team, terminal:view, commands:run |
| file-nav | sessions:read, sessions:send, sessions:stop, launch, inbox:read:own, inbox:read:team, inbox:read:all-teams, inbox:write, records:read:own, records:write:own, records:read:team, records:write:team, context:read, context:write, files:read, files:write, routines:read, routines:run, routines:manage, repo:read, repo:write, machines:read, machines:manage, accounts:manage, dashboards:install:private, dashboards:publish:team, terminal:view, terminal:control, commands:run, admin |

Private-record capabilities records:read:own, records:write:own are excluded from slots centre, dock, message and scopes team, message under team-safe. Reads and subscriptions also resolve own/team reach from the bound collection.
