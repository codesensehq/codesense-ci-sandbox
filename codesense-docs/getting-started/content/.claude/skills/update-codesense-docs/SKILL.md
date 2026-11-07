---
name: update-codesense-docs
description: Refresh the canonical CodeSense docs from this app's offline bundle. Use when the user asks to update or refresh the CodeSense docs.
---

# Update CodeSense docs

1. Call `codesense_overview`. Read `docs.state`, `docs.bundledCommit`, `docs.placedCommit`,
   `docs.appVersion` and `docs.action`. Report an error honestly if the docs cannot be read.
2. Call `update_codesense_docs` for the requested refresh. It resolves the canonical docs root
   for the active profile independently of your working directory. It uses only this app's
   bundled repository: no network, fetch, private remote or credentials.
3. Report the returned versions and state, `root`, `preservationBranch` and `changedPaths`.
   A current clean copy is a no-op. Otherwise the updater preserves local HEAD/history,
   staged content, tracked edits and nonignored untracked files on the named branch before
   selecting the bundled HEAD and index. Ignored machine state stays in place; an ignored
   path that would be overwritten or deleted blocks the refresh. If a later step fails, report the
   preservation branch and error; do not retry with a destructive git command.
4. Call `signal_done` with the result when running as the helper routine. Codex agents must
   pass their own `sessionId` to `signal_done` and `send_message`.

Untouched copies still refresh automatically when a newer app bundle is placed at startup.
Modified copies wait for this explicit refresh. The bundle is the version shipped with the
running app, so updating cannot obtain docs from a future app release.

If a doc and a live tool description disagree, trust the tool description and check
`docs.state`. Do not edit the placed docs as part of this workflow.
