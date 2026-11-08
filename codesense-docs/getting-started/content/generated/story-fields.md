# Story fields

A story is a Markdown file with YAML frontmatter, or an HTML file with `<meta name="cs:…">` tags. Both formats carry the same fields; in HTML a list is one comma-separated value. These are the registered fields, the same list `get_story_format` returns. Dashboards read them by their Markdown name.

`validate_story` gives any other key the advisory `UNREGISTERED_FIELD`. A team that uses its own field declares it in its `content/field-schema.yml` (or the org in its `tag_schema`), and the advisory stops.

| Markdown key | HTML meta name | Kind | Required | Values | What it's for |
| --- | --- | --- | --- | --- | --- |
| status | cs:status | text |  |  | The story's pipeline stage, which is also its board column: one of the team's statuses (status-taxonomy.yml). Free text; an unusual value is advisory. |
| type | cs:type | text | yes | story, bug, spike | Document type. One of the listed values makes the file a pipeline story; any other value, or none, keeps it a plain document. |
| depends_on | cs:depends-on | list |  |  | Story ids this story depends on. Each must resolve; drives the dependency graph. |
| tags | cs:tags | list |  |  | Free-form tags, for example [infra, urgent]. |
| supporting_docs | cs:supporting-docs | list |  |  | Relative paths to supporting documents. A path that is not found is advisory. |
| story_id | cs:story-id | text |  |  | The story's display id, for example W-5. Unique within an org. |
| source_repo | cs:source-repo | text | yes |  | The repo the story's code lives in, as owner/repo. Must be a configured repo: resolve it with list_source_repos, never invent it. |
| work_repo | cs:work-repo | text |  |  | Optional. The repo whose history holds the story's work when that is not source_repo, as owner/repo (for example the context repo); story_changes reads it. |
| dev_branch | cs:dev-branch | text |  |  | Legacy. Read only as a fallback on older documents; use branch. |
| branch_name | cs:branch-name | text |  |  | Legacy. Read only as a fallback on older documents; use branch. |
| team | cs:team | text |  |  | The team the document belongs to. If set, it must be a configured team. |
| repos | cs:repos | list |  |  | Epics: the repos the epic spans, as owner/repo. |
| branch | cs:branch | text |  |  | Stories: the story's branch, used to create its worktree. Epics: the integration branch (by default integration/ followed by the epic id). |
| sprint | cs:sprint | text |  |  | The sprint the story belongs to, for example 2026-08-A. |
| points | cs:points | number |  |  | Estimate in points, on the Fibonacci scale up to 13. A non-numeric value is ignored; an off-scale value is advisory. |
| assigned_to | cs:assigned-to | text |  |  | Who the story is assigned to: a person's login or an agent id. A login missing from the org roster is advisory. |
| rank | cs:rank | text |  |  | Order within a board column. The board writes it; you rarely set it by hand. |
| subject_id | cs:subject-id | text |  |  | A standalone document's automation subject id, which makes a plain document an addressable automation subject. A story uses story_id instead. |
| title | cs:title | text |  |  | The story's title. Without it, Markdown uses the first # heading and HTML uses the title tag or the first h1. |
| parent_epic | cs:parent-epic | text |  |  | The id of the epic the story belongs to. Without it, the epic folder on the story's path counts. |
| epic | cs:epic | text |  |  | Alias of parent_epic, read when parent_epic is absent. |
| priority | cs:priority | text |  | p0, p1, p2, p3, p4 | Priority, where p0 is the highest. Sorting also ranks words such as critical, high, medium and low. |
| blocked_reason | cs:blocked-reason | text |  |  | Why the story is blocked, for a board to show on its card. Set it while the status is a blocked one; with any other status, it is advisory. |
| wave | cs:wave | number |  |  | Optional. The story's board wave, for a board to use instead of the wave it would compute from depends_on. A value that is not a positive whole number is advisory. |
| stream | cs:stream | text |  |  | The work stream the story is in, for example frontend or backend, for a board to group by. |
| sub_epic | cs:sub-epic | text |  |  | The sub-epic the story belongs to within its parent epic, for a board to group by. |
