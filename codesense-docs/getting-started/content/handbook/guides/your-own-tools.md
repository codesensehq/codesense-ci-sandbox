# Run your own tools and scripts in a worktree

_Your shell config, your scripts and any agent CLI run in a CodeSense worktree terminal, next to the team context your agent launched with._

Each worktree terminal is a real shell in the worktree's folder, so `make`,
`just`, your own scripts and any agent CLI run there exactly as they do in any
other terminal. The difference is that once an agent has launched there, the
folder holds the team context that agent asked for: `CLAUDE.md`, `AGENTS.md`,
skills and anything else in its `context`, written as real files and hidden
from git.

To run several agents at once, open a worktree per story and split the
terminal area into tiles. Each agent works in its own folder and branch, and
you watch them side by side, with live diffs.

## What the terminal gives you

When CodeSense opens a terminal for a worktree, it starts **your login
shell** with the worktree as its working directory: the shell set in
Settings under **Terminal shell**, else your `$SHELL`. CodeSense supports
zsh, bash and fish; any other shell falls back to zsh.

- **Your config loads.** For zsh, CodeSense points `ZDOTDIR` at a small
  wrapper directory (`~/.codesense/shell/zsh`) whose files source your own
  `.zshenv`, `.zprofile`, `.zshrc` and `.zlogin`. For bash, a wrapper rcfile
  sources `/etc/profile` and then your `~/.bash_profile`, `~/.bash_login`,
  `~/.profile` or `~/.bashrc`. Fish runs its own config as usual. Your
  aliases, functions, `PATH` additions and tools all apply.
- **CodeSense adds a little.** `~/.codesense/bin` goes first on `PATH`, and
  the shell gets a few `CODESENSE_*` variables, such as
  `CODESENSE_TAB_SESSION_ID`.
- **The prompt is your own by default.** **Terminal prompt** in Settings is
  **Plain** ("Your own shell prompt (no Starship)"). Pick **Compact**,
  **Full**, **Minimal** or **Two-line** to use CodeSense's bundled Starship
  prompt instead.

## Run your scripts in a worktree

1. Launch a story's worktree (see [Launch your first agent](../getting-started/first-agent.md)),
   or open the terminal of a worktree you already have.
2. Check that the context is there. CodeSense copies an agent's context in
   when the agent launches in the worktree; a terminal you open on your own
   doesn't trigger that copy:

   ```sh
   ls .claude/skills
   # add-endpoint  release-notes  review-pr  write-migration
   ```

3. Run what you normally run. For example, a worktree for story `LL-214`
   might run:

   ```sh
   npm test                      # the repo's own scripts
   just migrate                  # your task runner
   ./scripts/dev.sh              # or anything you keep in the repo
   ```

Anything you start here, including another agent CLI, reads the same
`CLAUDE.md`, `AGENTS.md` and `.claude/skills/` as every other agent on the
story. Nothing extra shows up in `git status`, because CodeSense lists the
paths it copied in `.git/info/exclude`.

Headless runs, pre-commit hooks and CI-style scripts you start here read the
same context files, because they're plain files in the folder. No
special integration is involved.

> **Coming: the terminal waiting for context to land**
>
>   A command typed in the first second or two can run before `.claude/` is
>   written. Until the terminal waits for the context write, check with `ls`
>   first, as in step 2.

## When the window closes, and when you quit

- **Closing the window** keeps CodeSense running in the background, so its
  terminals and the processes in them keep going.
- **Quitting CodeSense** leaves its terminal sessions running: the terminal
  server keeps them, and the agents in them, going. When you open CodeSense
  again, the terminals reattach where they were.

> **Watch several agents at once**
>
>   Split the terminal area into tiles to watch several worktrees together.
>   See [Terminals](../concepts/terminals.md) for tabs, split tiles and the
>   agents list.

## Related

- [Context in a worktree and reverse sync](../concepts/agent-context.md): how
  context lands in the worktree, and reverse sync (in Beta).
- [Worktrees](../concepts/worktrees.md): how a story becomes a worktree and
  a branch.
