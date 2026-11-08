# Security and privacy

_What leaves your Mac, and how each safety claim works._

## What leaves your Mac

The app has no account and no telemetry: CodeSense sends no usage data or crash reports. Sharing a dashboard is the exception to the account part: owners and viewers sign in on share.codesense.ai. Code and context stay on your machines and your git host.

| What | Where it goes | When |
|---|---|---|
| Your code and context | Your git host, when you or your agents push | Only when you push |
| Agent traffic | Each CLI talks to its own vendor on your login | When an agent runs; CodeSense isn’t in the path |
| A published dashboard | share.codesense.ai, to the viewers you invite. Each invitee's email address goes there too, and the CodeSense operator reviews it before the invite is sent | Only when you publish or invite |
| The daily update check | dl.codesense.ai | About once a day: it reads a static file that names the latest version, and sends no key, install ID, app version or anything from your repos. [What leaves your Mac](https://codesense.ai/open-source#what-leaves) has the summary |
| Attribution notes | Nowhere | Unless you choose to share them |

## No model calls, no tokens

CodeSense never calls a model. Agents run the unmodified official CLIs on each person’s own login, and CodeSense never reads, stores or forwards their credentials.

When you launch an agent, CodeSense starts the vendor’s CLI in a terminal with the files you assigned, the way you would by hand. The CLI signs in with your account and talks to its vendor directly. CodeSense watches its status and usage from your Mac.

## Your keys stay home

Your remote boxes never get your git keys: CodeSense seeds each clone from your Mac over SSH, and commits come back the same way.

A box holds the clone, the files its agents were given, and the CLIs signed in with your logins there. It never holds your GitHub credentials. See [Remote machines](remote-machines.md#your-keys-stay-home).

## Panes are fenced in

Each page runs in an isolated frame with its own capability token. Dashboards are your own files, so open only ones you trust; see [How CodeSense trusts agents](https://github.com/codesensehq/codesense/blob/main/SECURITY.md#how-codesense-trusts-agents). A guard keeps credential files out of managed Claude sessions’ ordinary reads; it is not a sandbox.

A page can only call the verbs `window.codesense` exposes, and every action goes through the same gate: a page can open a workspace, message an agent or type into one, and create, move or delete team content files. Moves and deletes run without a confirm sheet unless you set `files.move` or `files.delete` to prompt for that dashboard. A dashboard published to share.codesense.ai is checked before upload: an allowlist of files, size limits, and no app calls in its pages.

## Updates are verified

Every update carries a signature the app checks before it installs.
