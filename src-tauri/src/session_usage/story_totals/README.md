# `storyTotals`: one batch read of cached story totals

`storyTotals {storyIds: [...]}` answers the `totals` of many stories in one call, from a
per-story index the Mac keeps in its usage store. It is the same read on MCP (`story_totals`),
the in-app bridge (`codesense.storyTotals`) and hosted live dashboards (`storyTotals`). It never
walks git: the index is filled in the background (see "How the index stays current").

## Request

```json
{"storyIds": ["DEMO-140", "DEMO-63", "DEMO-267", "DEMO-287", "DEMO-NEW"]}
```

- **Pages call `CS.storyTotals(ids)` (hosted) or `codesense.kit.storyTotals(ids)` (in the app)** with any
  number of ids: they ask in parts and merge one answer of the shape below (reason indexes renumbered,
  `rest` asked again, `source: "cache"` and the oldest `answered_at` when any part came from the cache).
- One ask: MCP and the in-app bridge take 1 to 500 ids. **A hosted ask holds only the ids that fit R23's
  2 KB `args` (about 120 to 150)**; `CS.storyTotals` sizes its parts by bytes (1900).
- Each id is a non-empty string. More than 500, an empty list or a blank id is
  `invalid_args` (MCP: an invalid-params error). Repeated ids (in any case) are answered once,
  under the first spelling.
- Hosted: `storyTotals.storyIds` is pinned like `storyChanges.storyId` (literals and
  `{epic: ...}` / `{team: ...}` selectors). Every id in the list must pass the pin; viewers need
  the pin. Example manifest block:

  ```yaml
  live:
    queries: [storyTotals, storyChanges, queryContext]
    args:
      storyTotals: {storyIds: [{team: riverside}]}
      storyChanges: {storyId: [{team: riverside}]}
  ```

## Answer (MCP and the in-app bridge)

The fixture below is an invented example for four stories, plus one story that is not indexed yet
(`DEMO-NEW`).

```json
{
  "stories": {
    "DEMO-140": {
      "totals": {"sessions": 3, "commits": 0, "files": 0, "costUsd": 3.5117788999999995, "wallMs": 79782388, "activeMs": 2566991, "idleMs": 11313819, "reviewRounds": 2, "reviewTasks": 2, "lines": null},
      "unavailable": {"commits": 0, "fileLines": 0, "lines": 0},
      "computedAtMs": 1793318400000,
      "mergedAt": null, "mergedAtReason": "no merged PR found locally", "prs": [], "prsTotal": 0,
      "runtimes": ["claude", "codex"]
    },
    "DEMO-63": {
      "totals": {"sessions": 0, "commits": 0, "files": 0, "costUsd": null, "wallMs": 0, "activeMs": 0, "idleMs": 0, "reviewRounds": 0, "reviewTasks": 0, "lines": null},
      "unavailable": {"commits": 0, "costUsd": 1, "fileLines": 0, "lines": 0, "planned": 2},
      "computedAtMs": 1793318400000,
      "mergedAt": null, "mergedAtReason": "no merged PR found locally", "prs": [], "prsTotal": 0,
      "runtimes": []
    },
    "DEMO-267": {
      "totals": {"sessions": 0, "commits": 11, "files": 43, "costUsd": null, "wallMs": 0, "activeMs": 0, "idleMs": 0, "reviewRounds": 0, "reviewTasks": 0, "lines": null},
      "unavailable": {"commits": 0, "costUsd": 1, "lines": 0, "planned": 2},
      "computedAtMs": 1793318400000,
      "mergedAt": 1793254000000,
      "prs": [{"number": 102, "merged": 1793254000000}, {"number": 101, "merged": 1793250000000}],
      "prsTotal": 2,
      "runtimes": []
    },
    "DEMO-287": {
      "totals": {"sessions": 2, "commits": 0, "files": 0, "costUsd": 9.970453200000005, "wallMs": 440489, "activeMs": 619490, "idleMs": 0, "reviewRounds": 0, "reviewTasks": 0, "lines": null},
      "unavailable": {"commits": 0, "fileLines": 3, "lines": 0, "planned": 2},
      "computedAtMs": 1793318400000,
      "refreshing": true,
      "mergedAt": null, "mergedAtReason": "no merged PR found locally", "prs": [], "prsTotal": 0,
      "runtimes": ["claude"]
    },
    "DEMO-NEW": {"pending": true}
  },
  "reasons": [
    "history objects not in local copy",
    "no sessions with priced usage for this story",
    "story has no Touchpoints section",
    "story spans branches with no single final tree"
  ],
  "pending": ["DEMO-NEW"],
  "indexing": {"done": 4, "total": 5}
}
```

- **`stories`** is keyed by the ids as asked. Each story is exactly one of:
  - **ready**: `totals` is `story_changes {storyId}.totals`, field for field (same nulls). `unavailable`
    is `story_changes.unavailable` with each reason replaced by its index in `reasons`
    (`reasons[i]` is the text). `computedAtMs` is when the Mac computed it (epoch ms).
    `reviewRounds` counts the story's complete review-round markers and is
    null with `unavailable.reviewRounds` when they cannot be read; `reviewTasks` counts its review
    tasks, or the tasks its markers name when none is typed. Read `reviewRounds` and `reviewTasks` as nullable.
    `refreshing: true` (absent otherwise) means something changed since (a commit, merge, session,
    task or review marker) and a recompute is queued; the values shown are the last computed ones.
    Beside the totals, from the same computation:
    - `mergedAt`: the latest merge time of the story's merged PRs (epoch ms, the committer time of
      the merge), or null with `mergedAtReason` (`no merged PR found locally`, or `no merge time for
      the merged PRs`);
    - `prs`: its newest merged PRs, at most 5, as `{number, merged}` (`merged`: that PR's merge
      time, or null), with `prsTotal` counting them all (`storyChanges.prs` has every one);
    - `runtimes`: the sorted ids of the CLIs that reported its sessions' usage (exact, not a
      model-name guess; the ids `lines.ai.byRuntime` uses).
    - `firstActivityAt`, `lastActivityAt`: the earliest and latest time (epoch ms) among the story's
      sessions (the usage times the report rolls up; a session's timing counts only where it has no
      usage), commits (authored), merges, tasks (created and closed) and review rounds
      (`reportedAt`), or null with `activityReason` (`no session, commit, merge, task or review
      round is recorded for this story`) when none is recorded. A row computed before these
      existed answers them null with `activityReason: "refreshing"` (and `refreshing: true`) while
      the throttled recompute reaches it: its totals are shown at once, never `pending`, and a
      read never computes;
    - `openSessions`: how many of its sessions have observed timing, no end, and were active (by
      usage or turn) in the 15 minutes before `computedAtMs`; a session that was killed, so has no
      end, stops counting once it has been quiet that long. A session with no observed timing is
      not counted. Null while the row is refreshing for these fields.
    - `linesInProgress`: the lines on the story's branches that have not merged, shaped like
      `totals.lines` (null with `linesInProgressReason` when a commit cannot be read or is still
      being counted). `totals.lines` keeps its rule (what landed, plus an unmerged branch when
      nothing has landed); when a branch merges, its lines move from `linesInProgress` into
      `lines` without a recount. Limit: after a squash or rebase merge, lines stay in `linesInProgress`
      while the story's branch ref still exists (local or on origin) until it is deleted.
    While a story's commits are still being counted (a backfill, or new commits) its `lines` and
    `linesInProgress` are the earlier ones with `refreshing: true`; a story that had none answers
    `lines: null` with the reason `counting in the background`. Never 0.
  - **pending** (`{"pending": true}`): not indexed yet. The Mac has queued it. **Never read it as 0:**
    show "Loading…" and ask again.
  - **error** (`{"error": "<why>"}`, hosted: `{"error": "failed"}`): the last compute failed. It is
    retried in the background.
- **`reasons`** holds each distinct reason text once, so 500 stories fit in one answer. A story with
  no sessions has `unavailable.costUsd` `no sessions with priced usage for this story`, or, while
  this Mac's managed Claude settings send session telemetry elsewhere, `usage not recorded: this
  Mac's managed Claude settings send session telemetry to your organization's collector`. A read
  gives the one that holds now, so a kept row follows a managed-settings change without a recompute.
- **`pending`** lists the asked ids that are pending (same as `stories[id].pending`).
- **`indexing`** is the index's progress for the caller's scope: `done` stories have values (or an
  error), `total` stories are known to the index. `done < total` while the backfill runs.
- **`truncated` / `rest`** (only when cut): the stories and reasons are kept under 240 KB so the
  answer always fits a hosted 256 KB answer. Ids that did not fit are listed in `rest`, in the
  order asked: ask again for them (`CS.storyTotals` and `kit.storyTotals` do). Typical stories are about
  400 bytes, so 500 ids fit.

## Hosted projection

Hosted answers have the same shape. `totals` pass whole (counts only, as for `storyChanges`).
`unavailable` keeps only the reasons `storyChanges` publishes (`actual`, `attribution`, `commits`,
`costUsd`, `lines`, `planned`): `fileLines` and `time` are dropped, and `reasons` only lists texts
still referenced. For the fixture above, hosted `DEMO-140` is
`{"totals": {...}, "unavailable": {"commits": 0, "lines": 0}, "computedAtMs": 1793318400000}`.

## `busy`

A hosted live query that waits for one of the Mac's 8 query slots no longer spends its 10 s answer
time waiting. It waits at most 3 s for a slot; past that the Mac answers `busy` and the relay
answers **HTTP 503 `{"error": "mac_busy"}`** (it serves a cached answer instead when it may, as
for `mac_offline`). It is never `failed`/`answer_failed` for a saturated link.

The toolkit retries it: `CS.query` (cs-live.js) and the in-app kit retry `busy` and `mac_busy` up
to 5 times, waiting 1 s, 2 s, 4 s, 8 s, 8 s (plus up to 250 ms of jitter), then reject with
`Error("mac_busy")` (or `busy`). `CS.query` and `CS.storyTotals` also retry the relay's own
`rate_limited` (every relay slot, or this viewer's 4, taken) the same way; `CS.queryAll` keeps its
own `rate_limited` backoff. Pages show the row as still loading while it retries.

## How the index stays current

- Computing one story's totals is the `story_changes` computation itself (one function), in two
  halves: the session half (sessions, cost, time, review rounds; no git) and the git half (commits,
  files, lines). Both are cached in the usage store (`story_totals` table), the git half with what it
  was read from.
- A story is queued the first time it is asked for (`pending`), then kept current:
  - **Sessions:** every write to a session's usage, timing, record, story link or attribution queues
    the session; every 5 s the worker resolves queued sessions to their stories with the same
    resolution `story_changes` uses (a stored link, a story worktree holding the session's folder,
    the launch record, another machine's usage) and marks those stories. A price change marks every
    story. Tasks and task links mark their story directly.
  - **Git:** the Mac reads the refs of the repositories the stories were read from every 15 s (git2
    in-process, no fetch): a moved branch a story followed or whose name holds its id, and a merge or
    squash landing on the default branch that names the story or whose second parent is one of its
    branch tips, mark it. Refs that moved while the app was closed count (the last tips are kept).
  - **Context:** a review record (`reviews/rounds|findings|dispositions`) marks the story its
    `story:` names; another changed path that names a story (its file) marks it; a capped change (and
    the index's first build at every start) marks only the stories whose context inputs changed: the
    indexed files naming a story, each with its home-relative path, mtime and content hash, are kept
    per story (`story_totals_ctx`) and compared with the live index (an mtime that differs is
    confirmed by one read of the content, so a pull or a moved home marks nothing; a store whose files table
    was rebuilt from an older form records its computed stories' files without marking them); those stories also rebuild their session
    rows.
- A session-only change recomputes the session half and keeps the git half (no git read); a git or
  context change, or a change to the sessions' branches or worktrees, reads git again.
- The worker computes one story at a time with a 1 s rest after each (a never-computed story, and
  every other turn a changed one), so it takes at most one core, off the read path (a read only queues a row and answers
  `pending` at once). A story's lines are not counted by its compute: they are the fold of its commit rows
  (below), so a compute of any size answers them. A whole compute past 90 s becomes an error row, retried after 5 minutes, never a stall.
- A merge commit adds no files to a story (a merge of main into the branch would list everything
  main changed).
- A changed story is recomputed after 3 s of quiet, at most once per 30 s, one at a time, alternating with the fill. A moved notes ref marks only stories with commits that are not merged, each at most once per 5 minutes (a move inside that waits and is honoured when it ends); the first read of a repository's refs is a baseline and marks nothing, and a default-branch move that cannot be read marks that repository's open stories once. A new version, a price change and a capped change recompute stories without counting as activity. Nothing starts while a compute (a timed-out one included) is still running; a result that arrives after the timeout is stored unless the story changed meanwhile. Rows
  computed by an earlier version of the computation read as `refreshing` and are recomputed.
- After a restart the persisted values are served at once.

## Session rows (contribution store)

A story's session half is the fold of one `contribution` row per session (`session_usage/contribution.rs`,
`story_changes/rows.rs`), not a re-read of every session. A row holds what the session adds: its cost (the one
calculation `report_with`, `session_read` and the row share, `cost_rollup.rs`), its observed timing, the span of its
usage and where it ran. A usage, turn, end, record, link or attribution write queues the session in `source_event`;
the sweep asks the story adapter for that session's rows and rewrites only what changed, in one transaction with
the queue row, so a replay or a restart changes nothing and a reattributed session moves its row to the other story.
A story whose rows are not built (new, or a new row version) is built whole by its first compute. The fold's
reducers are a const table (`sum`, `min`, `max`, `count`, `union`); a number a row cannot know makes a sum null
with that row's reason, never partial. The git half, the tasks and the review rounds are unchanged and still
computed per story. Reads never touch the rows.

Recomputed stories are published on the live hub's `storyTotals` topic as rows `{id, story, computedAtMs}` (no totals),
one per recompute with no extra debounce (the worker computes one story at a time with a rest between); a page that
shows the story reads its totals again. Each row belongs to the view of the scope it was computed in (`id` is
`<view>|<story>`): a team page receives its own team's stories only, and unrestricted views receive all.

A session whose usage is recorded under more than one id (its own and its CLI's native one) is one row that folds
every usage entry: its first time is the earliest, its last the latest, and its recent activity (for `openSessions`)
the later of its last usage and the end of its observed timing.

A usage row that changes (a repricing, a late cost, an upsert that moves it to another session) queues both the
session it was and the one it is. A change the queue cannot see (a story file or worktree link that moves a session
between stories) makes the stories it names forget their built rows, so their next compute rebuilds them whole;
that compute is the throttled worker's, never a read's.


## Commit rows (story lines)

A story's `lines` are the fold of one row per commit (kind `story_commits`, `story_changes/commit_rows.rs`, on the
same `contribution` tables as the session rows). The story walk (`git::collect_refs`) alone decides which commits a
story has and in what state: `landed` (in a merged PR's range), `branch` (on a story branch) or `side` (only on a
branch proven never to merge while other PRs landed). Every walk rewrites the story's rows in one transaction. A
commit already counted under the same notes (its `noteKey`: the note blob of each git-ai notes ref, never the ref's
tip, which moves with every commit in the repository) keeps its count, whichever story or scope counted it; every
other commit is uncounted and its sha is queued in `source_event`.

The commit sweep, in the worker loop beside the session sweep, takes due `commit` sources story by story (the story with the fewest
commits left first, oldest queued among equals, so stories complete) and counts
each with the same per-commit function the whole-story count (`line_share::count`) is the sum of
(`Counter::commit_lines_of`; it blames the file but reads only the commit's own git-ai note, once per commit, since
only the commit's own lines are counted), so the split between AI, human and unattributed lines agrees with `storyChanges`
by construction. It counts for as long as the last compute ran (at least 2 s), then rests twice as long (a third of a core), never holds the
`UsageStore` lock during a blame, and bounds one commit by the 90 s compute ceiling. Its write is `contribution::fill`:
it updates the sha's existing rows (counts and `noteKey`) and never inserts or deletes, so a walk that dropped the
sha meanwhile is not undone, and it marks a story whose last commit it filled (a quiet mark; no walk) so its lines are folded
once. A reading (`repo not found`, `revision not local`, ...) is a result and fills `lines: null` with its reason. A
failure to read (git-ai crashed, a timeout) counts an attempt and waits 5 minutes doubled per attempt (cap 6 h);
the fifth gives up with `attribution unavailable for one or more commits`. A new walk queues the sha again with
no attempts.

The backfill is the `TOTALS_STEPS` version 9 mark: every computed story walks once (stories that stored the old
`too large to compute in the background` reason first among the quiet ones; a really changed story always goes
before them), writes its rows and queues its commits, and the sweep counts
them at the pace above. Everything is persisted (the marks, the queue, each counted row), so a restart resumes and
no sha is counted twice. Until a story's last commit is counted its earlier values show with `refreshing: true`.
