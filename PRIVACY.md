# Privacy and network calls

The app has no analytics, telemetry or crash reporting. It makes no AI calls
of its own: the agent CLIs you run (Claude Code, Codex and others) talk to their
providers under their own terms.

This page lists every network call the app makes, built from the source code.
Each entry names where it is in the code, so you can check it.

## The website

The website counts page visits with Cloudflare Web Analytics. It sets no
cookies and stores nothing on your device. Cloudflare sees your IP address, as
it does for any page it serves; we see only totals: pages, referrers, countries,
browsers and page-load speed. It runs only on the public marketing and docs
pages.

## Calls the app makes on its own

### 1. The update check (the app from codesense.ai/download)

- **What:** `GET https://dl.codesense.ai/v1/update/darwin/aarch64/latest.json`,
  a static file that names the latest version.
- **When:** at launch, then about once a day. While a check is failing, or while
  a newer build is waiting to be installed, it checks about once an hour
  instead. It also checks once after the Mac wakes from sleep, and when you
  choose Check now.
- **What it sends:** nothing of its own beyond the address, which names your
  Mac's platform (macOS on Apple Silicon). No app version, account, key,
  install id, device clock, query string or custom header. Like any web
  request, it reveals your IP address to the download host.
- **Code:** `src-tauri/src/license/static_channel.rs` (`fetch_manifest`,
  `run_ticks`), `src-tauri/src/license/scheduler.rs` (`check_due`).

### 2. The update download

- **What:** when the file above names a newer version, the app reads the same
  file through the updater, sends a `HEAD` to the download URL it names to check
  the size, then downloads it with a `GET`. The download must be on the same
  host, with no redirects.
- **What it sends:** no id of its own. The updater's re-read of the file and the
  download carry the updater's `User-Agent: tauri-plugin-updater/<version>`
  header, which names the updater library's version, not the app's. The
  re-read also sends `Accept: application/json`, and the download sends
  `Accept: application/octet-stream`. The size check sends no header of its
  own.
- **What happens to it:** the download is checked against the release signing
  key before it is kept. It is installed when you quit, or when you choose
  Restart to update.
- **Code:** `src-tauri/src/license/updates.rs` (`fetch`, `check_size`).

**Turning it off:** update checks are automatic. There is no setting to turn
them off yet.

### 3. Builds from source: no update check

A build you make yourself uses the `internal` channel instead of calls 1 and 2,
unless you set the `CODESENSE_CHANNEL` build variable.
It makes no update check and sends no request: when it would check, it
reports that it has no update source.

If you set the `CODESENSE_UPDATE_REPO` build variable to a GitHub `owner/repo`,
the build checks that repository's releases instead. Once per launch, after
setup, and when you choose Check for Updates in Settings, it calls:

- `GET https://api.github.com/repos/<owner>/<repo>/releases/latest`;
- `GET https://api.github.com/repos/<owner>/<repo>/releases/assets/<id>`, to
  get the address of the release's update manifest in GitHub's file storage;
- a `GET` of that manifest from GitHub's file storage.

These requests send a `User-Agent: CodeSense` header and no id. The update
itself is downloaded from GitHub's file storage only when you click
Install & Restart.

- **Code:** `src-tauri/build.rs` (`configure_beta_channel`: the channel is
  `internal` unless `CODESENSE_CHANNEL` is set), `src-tauri/src/commands/update.rs` (`UPDATE_REPO`,
  `releases_api_url`); started from `src/app/update-check.ts`
  (`startUpdateCheck`).

### 4. Pictures in the inbox

- **What:** by default, when an agent running in one of this app's terminal
  tabs posts an inbox message with a picture link on `*.giphy.com` or
  `static.klipy.com`, the app downloads the picture straight away. Other
  pictures, including those from other hosts or other senders, are downloaded
  only when you click to load them. A picture written into a message's text is
  never loaded: it shows as a link, which opens in your browser.
- **What it sends:** a `GET` of that link, with `User-Agent: ImageFetcher/1.0`
  and no referrer. GIPHY or KLIPY sees your IP address and which picture was
  chosen.
- **Limits:** public addresses only (no local or private network), `https://`
  only, at most 3 redirects and only to allowed hosts, 8 MiB at most.
- **Turning it off:** each team has its own inbox settings file,
  `config/inbox.yml` inside that team's folder in your context repository
  (`<team>/config/inbox.yml`). Set `remote: off` under `media:` to never
  download pictures, or `remote: ask` to download them only when you click.
  The same file can change the list of hosts.
- **Code:** `src-tauri/src/media/mod.rs` (`fetch`),
  `src-tauri/src/commands/agent_inbox.rs` (`fetch_media`),
  `src/features/weft/components/inbox-message-content.tsx` (`sanitizeInboxHtml`,
  `MARKDOWN_COMPONENTS`).

### 5. git fetch for change history

- **What:** `git fetch` from a repository's `origin`, run by the app with your
  own git credentials, to fill in the history it uses to show who changed what.
- **When:** once in the background after you attach a source checkout; when an
  agent asks to refresh its history; and when you open the changes of a
  finished story whose history is incomplete.
- **What it sends:** a git fetch of branches, without tags, to the remote you
  configured. It may deepen a shallow clone. If `origin/HEAD` is missing, the
  app also asks `origin` for its default branch.
- **Turning it off:** there is no setting to turn this off.
- **Code:** `src-tauri/src/attribution/history.rs` (`HistorySource::fetch`,
  `fetch_after_attach`), `src-tauri/src/session_usage/story_changes.rs`
  (`git_facts`).

## Calls that follow something you set up

### 6. Your remote machines (SSH)

- **What:** CodeSense connects to machines you add, using your system `ssh`,
  your SSH config and your keys. Host keys are pinned, and agent and X11
  forwarding are off.
- **When:** when you connect, test or set up a machine. At launch, it
  reconnects to machines marked to connect automatically or that had open tabs.
  While a machine is connected, the app reads its agents' usage from it every
  60 seconds.
- **What it sends:** your terminal sessions and the files you open on that
  machine. When you prepare a workspace on it, CodeSense also sends the
  committed history of the branch or commit you choose to work from, as a git
  bundle, and the repository's clone URL; the remote machine
  may then fetch from that URL itself. Uncommitted changes are not in the
  bundle. It also installs or updates the CodeSense terminal server on it,
  copied from the app.
- **Turning it off:** disconnect or remove the machine, or turn off its
  automatic connection.
- **Code:** `src-tauri/crates/codesense-session-server/src/ssh.rs`
  (`Connector::ssh`), `src-tauri/src/commands/host_connection_manager.rs`
  (`auto_connect_candidates`), `src-tauri/src/commands/remote_usage_pull.rs`
  (`watch`, `run`).

### 7. git fetch when checking a workspace

- **What:** `git fetch --all --no-tags`, on this Mac or on a remote machine,
  when a workspace move or status check asks to refresh its branches.
- **Code:** `src-tauri/src/mcp/move_workspace.rs` (`local_source_status`),
  `src-tauri/crates/codesense-session-server/src/remote_workspace.rs`
  (`check_worktree_dirty_remote`).

### 8. git-ai notes

- **What:** CodeSense bundles a build of git-ai, which records which lines
  agents wrote. Its notes stay on your Mac unless you set a
  repository to push them, in which case `refs/notes/sense` is pushed to that
  repository's `origin`.
- **After a pull or clone:** when you or an agent run `git pull` or `git clone`
  in a terminal CodeSense started for an agent, git-ai then fetches
  `refs/notes/sense` from the same remote, with your own git credentials. A
  remote that has no such notes sends none.
- **Off by default:** CodeSense turns off git-ai's own telemetry, update
  checks, version checks, log upload and token metrics.
- **Code:** `src-tauri/src/attribution/managed.rs` (`CONFIG`,
  `LOCAL_NOTES_CONFIG`), `src-tauri/src/attribution/prefs.rs`
  (`push_destinations`). The fetch after a pull or clone is in the git-ai build
  CodeSense bundles (tag `v1.7.5-cs.4`): `src/daemon.rs`
  (`apply_pull_notes_sync_side_effect`, `apply_clone_notes_sync_side_effect`).

### 9. MCP servers you configure

- **What:** when an Agent you configured with a remote (HTTP) MCP server runs
  on one of your remote machines, this Mac makes the MCP calls to that server
  for it, with the headers you configured. No redirects.
- **Turning it off:** remove the server from the Agent's MCP configuration.
- **Code:** `src-tauri/src/mcp/local_mcp_relay/`.

### 10. Agent accounts you add

- **What:** when you add a Claude Code or Codex account in CodeSense, the app
  runs that agent's own CLI to sign you in and, when it lists your accounts, to
  check that the sign-in still holds. For each Codex account, it also runs the
  Codex CLI about every 5 minutes (every minute while your usage is on screen)
  to read your usage limits. While that read is failing, it retries sooner:
  after 1 second, then 2, 4 and so on, doubling up to 5 minutes.
- **What it sends:** CodeSense sends nothing itself. The CLI may talk to its own
  service, Anthropic or OpenAI, under that account, as it would if you ran it.
- **Turning it off:** remove the account from CodeSense.
- **Code:** `src-tauri/src/commands/agent_accounts.rs` (`claude_login_in`,
  `codex_login_in`, `verify_login`), `src-tauri/src/commands/agent_usage/mod.rs`
  (`run_scheduler`, `refresh_polled`), `src-tauri/src/commands/agent_usage/scheduler.rs`
  (`refresh_interval`, `RetryBackoff::fail`).

### 11. Publishing a dashboard (`share.codesense.ai`)

Dashboard publishing is a build option. The DMG from codesense.ai/download
includes it; a build from source leaves it out unless you turn it on. In a build that
includes it, these calls are made only after you save a publisher key. Without
a key, none of them is made.

- **What:** `https://share.codesense.ai/api/dash/v1/...`, authenticated with your
  publisher key. No other host, and no redirects.
- **Who can see it:** a published dashboard is not public. Only the email
  addresses you share it with can open it, after they sign in with a one-time
  link or code sent to that address. This is the same for every `visibility`
  setting: `team` gives no one else access. Removing a viewer, or unpublishing,
  ends their access.
- **What it keeps:** the files of up to the five most recent published versions,
  until you unpublish. Live dashboards also keep the last answer to each live
  query, up to 64 per dashboard. Terminal streams and typing are relayed, not
  stored. Unpublishing deletes the files and the stored live answers. The
  dashboard's name and settings, the invited email addresses, and records of
  sign-ins and access stay after you unpublish. A viewer you remove stays in
  the share records, with their email address. There is no way yet to delete
  them, or your account, yourself: ask us through the contact on
  [codesense.ai/privacy](https://codesense.ai/privacy).
- **What it sends:**
  - When a dashboard is published (by you, or by an agent through its tools):
    the files the dashboard lists (their contents and hashes), its title and
    settings, and the names of any live queries it uses. A live dashboard also
    sends a SHA-256 hash of the organisation and team names.
  - When you or an agent share it: the email addresses of the people invited.
  - When you invite someone to a shared dashboard, their email address is seen by
    the CodeSense operator before the invite is sent.
  - When you unpublish it, move it or sign its viewers out: that request.
  - While it stays published: edited files are sent again 60 seconds after the
    last change, and the app checks in every 15 minutes.
  - Opening the Dashboards manager or the key's status in Settings asks the
    server for your dashboards or checks the key.
- **Live dashboards:** a dashboard with live data keeps a connection open to
  `wss://share.codesense.ai/api/dash/v1/live` while it is published. It answers
  viewers' questions with redacted app data, and it can stream redacted terminal
  output and accept typing from the owner. A person must confirm live data,
  terminal streams and remote typing in the app. An agent can't turn them on or
  widen them.
- **Turning it off:** unpublish the dashboard, turn off its live data, or remove
  the publisher key.
- **Code:** `src-tauri/src/dashboard_publish/` (`client.rs`, `watch.rs`,
  `live.rs`, `approval.rs`, `confirm.rs`, `keys.rs`).

## Content you open

### 12. Dashboards, documents and stories in the viewer

What you or your agents write can load what it references when you open it:

- **HTML dashboards and documents** load anything they reference. So does an
  HTML message to you in the inbox, which shows as a dashboard. A dashboard can
  declare that it uses no outside network apart from the hosts it lists; then
  only those hosts are allowed.
- **Markdown documents and stories** load the pictures they name
  (`![](https://...)`) from those hosts: in the document viewer, the story view,
  the before-and-after view of a change, and the preview pane.
- **A person's picture** that a team's people list names (`avatar_url`) loads
  when that person is shown.
- **Drawing canvases** (`.excalidraw`) load nothing from the network. A link
  in a canvas stays a link, so opening one loads no YouTube, Figma or other
  outside frame. Generated HTML written into a canvas element is not shown, and
  a picture is shown only when the file holds it as a `data:` image. The
  canvas's library menu has no Publish item, so nothing in it is sent to
  Excalidraw.

Dashboards and documents are files in your own context repos, and an HTML message
in the inbox shows as a dashboard. They run with the app's page APIs, so open only
ones you trust; see
[How CodeSense trusts agents](https://github.com/codesensehq/codesense/blob/main/SECURITY.md#how-codesense-trusts-agents).

- **Code:** `src-tauri/src/http/frame_boundary.rs` (`request_frame`),
  `src-tauri/tauri.conf.json` (`app.security`),
  `src/features/weft/components/inbox-message-dashboard.tsx`
  (`InboxMessageDashboard`), `src/shared/lib/markdown-worker/parse.worker.ts`
  (`processor`), `src/features/dashboard/components/markdown-viewer.tsx`
  (`MarkdownViewer`), `src/features/viewer/components/story-hud.tsx`
  (`StoryHudExpanded`), `src/features/git-sync/components/redline-prose.tsx`
  (`RedlineProse`), `src/features/viewer/components/markdown-preview-pane.tsx`
  (`MarkdownPreviewBody`), `src/shared/components/person-avatar.tsx`
  (`PersonAvatar`), `src/features/dashboard/components/excalidraw-canvas.tsx`
  (`rejectEmbeddable`, `LIBRARY_PUBLISH_ITEM_SELECTOR`),
  `src/features/dashboard/lib/excalidraw-scene.ts` (`sceneForLoad`, also used
  by `renderBase64` in `src/features/dashboard/lib/export-canvas.ts`).

## Links that open in your browser

These open in your default browser only when you click them. The browser, not
CodeSense, makes the request.

- Any `http(s)` link in the app, in documents, notes and agent results, and in
  the terminal.
- Download and help links on `codesense.ai`, the Git download page, GitHub
  profile and pull-request links, your organisation's repository link, and links
  to your published dashboards.
- Codex sign-in opens the sign-in page that the Codex CLI returns.
- Installing Apple's Command Line Tools (for git) starts Apple's installer,
  which you confirm in Apple's dialog.

## Not network calls

- The app runs a small web server for its own windows and agents. It listens on
  `127.0.0.1` and `::1` only. Agent CLIs report their usage to it locally.
- The bundled git-ai binary, the product documentation, the terminal fonts and
  the drawing canvas's fonts ship inside the app. Nothing is downloaded at
  runtime to install them.

## What runs in your terminals and agents

CodeSense starts the agent CLIs, shells and commands you choose. Whatever they
do on the network is theirs: for example, Claude Code and Codex talk to their
own services under your accounts. The agent CLI runs CodeSense starts on its
own, for accounts you add, are in section 10.
