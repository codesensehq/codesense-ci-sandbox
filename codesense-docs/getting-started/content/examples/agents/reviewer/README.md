# While you were away

A reviewer agent and team routine turn existing records into a short, cited
review. The custom right pane shows its latest successful result, earlier
reviews, and opened/acted receipts. Nothing is installed by browsing this
example.

## Install in your team

Resolve the attached context repo with `context_repo_status` for org
`acme/team-context`. Its `repoPath` is the repository root. Copy
these files relative to that root (preserve any existing team files):

| From this example | Destination |
| --- | --- |
| `agent.yaml` | `web/config/agents/reviewer/agent.yaml` |
| `routines/while-you-were-away/routine.yml` | `web/config/agents/reviewer/routines/while-you-were-away/routine.yml` |
| `skills/cited-retrieval/` | `web/content/.claude/skills/cited-retrieval/` |
| `panes/while-you-were-away.html` | `web/panes/while-you-were-away.html` |

The routine belongs to the agent through its path; do not add `agent:` to
the YAML. Like the manager example, it uses your configured Claude runtime;
the review format and retrieval contract do not depend on that runtime.
For a Codex agent, choose its runtime and project the same skill to
`content/.agents/skills/cited-retrieval/`, updating the agent's startup path.

The default person is `sam`, the schedule is weekdays at 07:00 in
`America/Los_Angeles`, and `runs_on: one_mac` prevents one review per Mac.
Set `owner` to the local principal you want displayed. If changing person
or time zone, change the routine default/schedule and the pane's `PERSON`
or `TIME_ZONE` constant together. Use a separate routine ID and matching
pane key for a second person: one routine key holds one latest result.

After copying, use `routine_list` / `routine_get` to confirm discovery and
validation. Arm it on this Mac with `routine_arm_on` and enable it with
`routine_set_enabled`:

```json
{"id":"acme/team-context/web/while-you-were-away","machine":"this_mac"}
```

```json
{"id":"acme/team-context/web/while-you-were-away","enabled":true}
```

Run on demand with `routine_run_now` using that same `id` (optionally
`variables: {"person":"sam"}`). Open **Panels → While you were away**
to place the custom pane on the right. **Generate review** / **Regenerate**
uses the existing `codesense.routineRefresh` bridge verb, which calls the
same run-now policy. Loading the pane never launches an agent.

The routine writes `web/content/reviews/sam/YYYY-MM-DD.md`, replaces
that file on same-day reruns, and publishes the same review with
`routine_publish_result` before signaling success. It commits and pushes
the team repo itself, then calls `report_context_sync`. Receipt writes
change only frontmatter in the working tree; the next run commits this
person's accumulated receipt edits with the new review. No `type` or
`cs:*` metadata is added to reviews.

The pane uses `queryContext` deliberately over this person's reviews.
Agents' MCP `query_context` hides top-level `reviews/` by default; opt in
with a `where.pathPrefix` or `where.pathGlob` starting with `reviews/`.
Default agent hydration remains unchanged. Freshness excludes review and
receipt writes, so reading a review does not make it stale.

Commit chips open the repository commit; story chips open the file at its
cited line. Run, update and inbox chips show the existing read result in
the pane. An old message may have aged out of retained history; the pane
says so instead of substituting another record with the same ID.

The **Last 14 days** strip reads receipts back from review frontmatter.
This example's checks exercise the model, discovery, schema, HTML contract and
pane behavior without building the app.
