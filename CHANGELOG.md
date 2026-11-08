# Changelog

All notable changes to CodeSense are written here, for the people who use it.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and CodeSense uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until 1.0, a minor release (0.x.0) may change or remove things you rely on. When one does, its entry says so under "Changed" or "Removed".

## [Unreleased]

### Privacy

- Clarified: the app has no sign-in and no usage tracking; sharing a dashboard uses owner and viewer accounts on share.codesense.ai.

## [0.1.0] - 2026-11-06

The first public release of CodeSense. CodeSense is a Mac app that runs your team of Claude Code, Codex and OpenCode agents across every repo, all in your own git. It needs macOS on Apple Silicon. You run Claude Code, Codex or OpenCode with your own subscriptions and settings; CodeSense never calls a model itself.

### Added

- **A worktree for every story.** Open a story and CodeSense makes its worktree in the story's own repo, on its own branch, and starts the agent there. Open it again and you're back where you left off.
- **Your team's context in every worktree.** Skills, CLAUDE.md, AGENTS.md and any other files your context names land in each agent's worktree when it launches, as real files hidden from git.
- **Skills that get better.** Improve a skill in any worktree and have an agent sync it back to your team's context. The next agent you launch starts with the better version.
- **Plans are plain files.** Stories are Markdown with frontmatter, or HTML pages. Status, sprint and dependencies are fields in the file, and the sidebar marks a story ready once everything it depends on is done.
- **An inbox for every agent.** Your agents message each other and you. Only what waits on you lands in Needs you; progress goes to Updates. When an agent needs a decision, it asks you and waits.
- **One list of every agent.** See every running agent, who launched it, where it runs and which ones need you. Press ⌘\ from anywhere to bring it back.
- **Live status.** Claude Code, Codex and OpenCode sessions show whether they're running, waiting on you or done, whether CodeSense launched them or you typed them by hand.
- **Saved agents.** Save Claude Code, Codex or OpenCode with your flags, MCP servers, permissions, an avatar and a starting prompt. Launch it in a terminal or headless.
- **Review over the terminal.** The diff and a code editor float over the agent's terminal while it keeps working. ⌘⇧D shows the change; ⌘⇧C opens the file so you can fix a line yourself.
- **Agents keep running when you quit.** Open CodeSense again and every terminal is where it was.
- **Remote machines.** Run Claude Code and Codex agents on a Linux machine over SSH, with the same tools, the same inbox and your team's context.
- **Routines.** Run an agent on a schedule or when mail arrives. A routine wakes the agent that's already running instead of starting a second one.
- **Live pages and dashboards.** Any HTML file in your context is a live page beside your plans. Ask an agent for a dashboard and it writes one over your stories, agents and usage.
- **The whole app over MCP.** Your agents can open workspaces, run and read terminals, message each other and you, and query your context.
- **Cost per story.** Every Claude Code, Codex and OpenCode session CodeSense launches records its tokens and spend, and the story, agent and model it was spent on.
- **Who wrote each line.** When a Claude Code or Codex agent commits from a CodeSense worktree on your Mac, each line it added links back to the session that wrote it.
- **Your git, your rules.** CodeSense never commits, pushes or merges. Your agents do the git, with your own keys, hooks and branch protections.

### Privacy

- CodeSense checks for updates automatically. The check names only your Mac's platform; it sends no account, version or usage data. There's no sign-in and no usage tracking.
- When you share a dashboard, the CodeSense operator sees the email addresses of the people you invite before the invites are sent. See [Privacy and network calls](https://github.com/codesensehq/codesense/blob/main/PRIVACY.md#11-publishing-a-dashboard-sharecodesenseai).

### Security

- CodeSense's background terminal service accepts connections from programs running under your Mac user account, and from no other user. CodeSense trusts what runs as you; see [How CodeSense trusts agents](https://github.com/codesensehq/codesense/blob/main/SECURITY.md#how-codesense-trusts-agents).

### Known limitations

- CodeSense runs only on Macs with Apple Silicon. There is no Intel Mac, Windows or Linux desktop build.
- CodeSense is tested on macOS 26 (Tahoe); older versions may work.
- Routines you set up on your Mac run only while CodeSense is open.
- Live status and inbox delivery work with Claude Code, Codex and OpenCode. Other command-line agents run in a worktree terminal and read your context; full support for more agent CLIs is coming.
- OpenCode (2.1.4 or a newer 2.x release) runs on your Mac only. Remote machines and line attribution work with Claude Code and Codex. CodeSense starts OpenCode with its own config folder, so your `opencode auth login` logins and a repo's `opencode.json` apply, but `~/.config/opencode` doesn't.
- There's no button to open a pull request. Ask your agent to do it.
- There's no company sign-in, roles or central audit log.
- A running agent sees changes to your team's context only when it asks for them. New agents get the latest when they launch.
- A story changes one repo. To change two repos, write one story for each.

### Known issues

- When you edit the label of a private remote machine and save, CodeSense says it saved, but the old label stays.
- If you build CodeSense from source in a folder whose path contains the word "resolve", one of the project's own checks fails. Build from a different folder.

[Unreleased]: https://github.com/codesensehq/codesense/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/codesensehq/codesense/releases/tag/v0.1.0
