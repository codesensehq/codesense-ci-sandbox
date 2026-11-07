# LiveHub topics

15 built-in topics and 1 dynamic topic family. Row shapes are read from actual producers; Value expressions and dynamic roster/collection state remain explicitly untyped. Topics with untyped fields: sessions, inbox, agentList, records, paneSources, machineStats, runEndorsements, runEvidence, files, stories, agentDefinitions, profile, workspaceConfig, prefs, storyTotals, ui:<slotId>.

View keys: `all` for non-team grants; `team:{org}/{team}` for a pinned team; `undefined` is the agentList status ordering suffix. For sessions, inbox and agentList, team grants cannot read other teams. Records instead use `records:{scope}:{owner}:{collection}`: private owner is the org; team owner is org/team. Global UI topics `ui:dock`, `theme` always use `all`, including in a team-pinned frame. Other UI topics follow the frame view.

[SDK subscribe/page signatures and envelopes](codesense.d.ts) · [Machine reference](live-hub.json)

## sessions

Row shape 1: untyped (Value): runtime and roster objects are merged; these are the source-declared core fields, not a closed row type.

| Field | Type / source expression |
| --- | --- |
| id | string |
| handle | string |
| displayName | string |
| state | "running" \| "idle" \| "needs-you" \| "exited" \| "offline" \| "unknown" |
| machine | string \| null |
| machineId | string \| null |
| workspace | string \| null |
| team | untyped (Value): crate::commands::dashboard_launch::LaunchTeam \| null |
| launchedBy | string \| null |
| story | string \| null |
| chatWorkspaceId | string \| null |
| workspaceHandle | string \| null |
| deadlineAtMs | integer \| null |
| expiring | boolean |
| outputRunId | untyped (Value) |
| kind | untyped (Value) |
| sessionId | string \| null |
| alive | untyped (Value) |
| status | untyped (Value) |
| failed | untyped (Value) |
| backgrounded | untyped (Value) |
| startedWith | untyped (Value) |
| startedAtMs | untyped (Value) |
| launcher | untyped (Value) |
| lastOutputMs | untyped (Value) |
| provenance | untyped (Value) |
| accountProvenance | untyped (Value) |
| agentRevision | untyped (Value) |
| launchTeamKey | untyped (Value) |
| detected | untyped (Value) |
| names | untyped (Value) |
| rowNames | untyped (Value) |
| model | untyped (Value) |
| pendingAsk | untyped (Value) |
| agent | string \| null |
| runtime | untyped (Value) |
| usage | untyped (Value) |
| role | string \| null |
| queueRole | string \| null |
| roleJoinError | string \| null |
| avatar | string \| null |
| progress | untyped (Value): crate::dashboard_stream::hub::SessionProgress \| null |
| launchTelemetry | untyped (Value): crate::commands::launch_telemetry::LaunchTelemetry \| null |
| releaseQualified | boolean \| null |
| releaseNote | string \| null |
| inboxWaiter | untyped (Value): InboxWaiter has a custom Serialize implementation; variant names do not define its wire values. Whether new mail reaches its agent at once (an idle agent starts a turn; a running one hears at its next step), through its CLI's inbox port: a waiter, or its host submitting the notice. False for an agent CodeSense has no way to tell (its CLI has no wake path, its host cannot name its process, or its waiter has gone): nothing tells it about its mail. `"unknown"` for a tab on a machine that is not connected now. |
| subagents | integer |
| sessionRef | untyped (Value) |
| inboxRef | untyped (Value) |
| teamKey | untyped (Value) |
| placementTeamRef | untyped (Value) |
| parentId | untyped (Value) |
| parentSource | untyped (Value) |
| order | untyped (Value) |
| workspaceRef | untyped (Value) |
| storyRef | untyped (Value) |
| workspaceRank | untyped (Value) |
| crewParentId | untyped (Value) |
| crewOrder | untyped (Value) |
| crewLead | untyped (Value) |
| hasAgent | untyped (Value) |
| effectiveStatus | untyped (Value) |
| needsYouKind | untyped (Value) |
| managedState | untyped (Value) |
| activityAtMs | untyped (Value) |
| crashId | untyped (Value) |
| endReason | untyped (Value) |
| statusSinceMs | untyped (Value) |

## inbox

Row shape 1

| Field | Type / source expression |
| --- | --- |
| id | untyped (Value) |
| rowType | "counts" |
| team | untyped (Value) |
| humanUnread | untyped (Value) |
| recipients | untyped (Value) |
| needsYouBySender | untyped (Value) |
| sessions | untyped (Value) |

Row shape 2

| Field | Type / source expression |
| --- | --- |
| id | untyped (Value) |
| rowType | "message" |
| team | untyped (Value) |
| messageId | untyped (Value) |
| createdAt | untyped (Value) |
| message | untyped (Value) |
| conversation | untyped (Value) |

## agentList

Row shape 1

| Field | Type / source expression |
| --- | --- |
| id | string |
| sessionRef | string |
| inboxRef | string |
| teamKey | string |
| placementTeamRef | string \| null |
| parentId | string \| null |
| parentSource | string \| null |
| order | number |
| workspaceRef | string \| null |
| workspace | untyped (Value): Value |
| storyRef | string \| null |
| workspaceRank | integer |
| crewParentId | string \| null |
| crewOrder | integer \| null |
| crewLead | boolean |
| statusGroup | integer \| null |

Row shape 2

| Field | Type / source expression |
| --- | --- |
| spendRows | untyped (Value) |
| ready | untyped (Value) |
| homeTeam | untyped (Value) |
| teamOrder | untyped (Value) |
| teamNames | untyped (Value) |
| pane | untyped (Value) |
| workspaceRollups | untyped (Value) |
| id | untyped (Value) |
| rowType | untyped (Value) |
| order | untyped (Value) |

## records

Row shape 1

| Field | Type / source expression |
| --- | --- |
| id | untyped (Value) |
| fields | untyped (Value) |
| preview | untyped (Value) |
| refs | untyped (Value) |
| updatedAt | untyped (Value) |
| updatedBy | untyped (Value) |
| title | untyped (Value) |
| headCommit | untyped (Value) |
| order | untyped (Value) |

## paneSources

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## machineStats

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## runEndorsements

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## runEvidence

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## files

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## stories

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## agentDefinitions

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## profile

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## workspaceConfig

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## prefs

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## storyTotals

Row shape 1: untyped (Value)

| Field | Type / source expression |
| --- | --- |


## ui:<slotId>

Row shape 1: untyped (Value): persisted shared slot state spread into a row with id set to the topic.

| Field | Type / source expression |
| --- | --- |
| id | string |

## Envelopes

### Snapshot

| Field | Type |
| --- | --- |
| topic | string |
| viewKey | string |
| rev | integer |
| snapshot | Array<untyped (Value): Value> |
| total | integer \| null |
| cursor | string \| null |

### Delta

| Field | Type |
| --- | --- |
| actor | untyped (Value): Value \| null |
| cursor | string \| null \| null |
| kind | string \| null |
| topic | string |
| viewKey | string |
| rev | integer |
| upserts | Array<untyped (Value): Value> |
| removes | Array<string> |
| requestId | string \| null |
| total | integer \| null |

### SubscriptionReply

| Field | Type |
| --- | --- |
| topic | string |
| viewKey | string |
| rev | integer |
| snapshot | Array<untyped (Value): Value> \| null |
| deltas | Array<object> \| null |
| total | integer \| null |
| cursor | string \| null |
