# Terminals

_A terminal per worktree with your login shell and dotfiles: tabs, tiles, the overlays over them, and sessions that outlive the app._

Every workspace in CodeSense has real terminals: a full PTY with xterm.js, clickable links and WebGL rendering. It isn't a simulated console. Anything you run in Terminal.app or iTerm runs here, including `make`, your scripts and any agent CLI.

## Your shell, your config

A CodeSense terminal starts your **login shell** in the workspace's folder, which for a story is its worktree. zsh, bash and fish are supported. CodeSense uses `$SHELL`, then your account's login shell, and falls back to zsh. To pick another, enter its path in **Terminal shell** in **Settings → General**; leave it blank to keep the detected one.

Your own startup files load, so your `PATH`, aliases, functions and prompt are all there. zsh reads `.zshenv`, `.zprofile`, `.zshrc` and `.zlogin`. bash reads what a login bash reads (`/etc/profile`, then the first of `~/.bash_profile`, `~/.bash_login` or `~/.profile`), or `~/.bashrc` if you keep everything there. fish starts as a login shell.

CodeSense adds only a little on top:

- `~/.codesense/bin` goes first on `PATH`. It holds `codesense-bridge`, the MCP bridge agents use.
- A few variables such as `CODESENSE_TAB_SESSION_ID` and `CODESENSE_API_PORT` identify the tab to CodeSense.
- `claude` and `codex` start connected to CodeSense: its MCP tools, its status hooks and, for Claude, its new-mail hooks. On this Mac, `opencode` does too, through CodeSense's plugin. `command claude` runs the plain CLI. See [MCP tools for agents](mcp-tools.md#claude-and-codex-typed-in-any-codesense-tab).
- `TERM=xterm-256color`.

> **Commands CodeSense runs use a POSIX shell**
>
> Commands CodeSense runs for you, such as worktree setup, run in your shell when it's zsh or bash. If your shell is fish, they run in zsh, because they're written for a POSIX shell.

> **Beta**
>
> A tab's status dot follows Claude Code and Codex, whether CodeSense launched them or you typed them, on this Mac or on a machine, and OpenCode on this Mac.

See [Run your own tools and scripts in a worktree](../guides/your-own-tools.md) for a walkthrough.

## Tabs and tiles

A workspace can hold several terminal tabs. Split them into tiles to watch several agents or shells at once, drag to split, and arrange tiles as columns, rows or a grid.

| Shortcut | Does |
|---|---|
| <kbd>⌘\\</kbd> | Toggle the terminal |
| <kbd>⌘⇧L</kbd> | Cycle the tile layout (columns, rows, grid) |
| <kbd>⌘⇧M</kbd> | Expand in and out: the focused tile fills the area, then shows all its tabs at once, then steps back out to one tab and to all your workspaces, turning round at each end. With one tab it's a plain expand and restore. A brief note names where you are. In all tabs, drag a divider to resize, double-click it to even the pair |
| <kbd>⌘⇧]</kbd> / <kbd>⌘⇧[</kbd> | Next or previous terminal tab |
| <kbd>⌘⌥</kbd> + arrow | Focus the pane in that direction |
| <kbd>⌘T</kbd> | New terminal tab in the focused tile |
| <kbd>⌘D</kbd> | Split: a new terminal beside the focused one |
| <kbd>⌘⇧N</kbd> | New terminal on another machine |
| <kbd>⌘W</kbd> | Close the tab: Hide if something is running, End if it's idle |
| <kbd>⌘⇧T</kbd> | Reopen the last closed tab (a Hidden tab comes back running; an Ended one opens a fresh shell in the same place) |
| <kbd>F2</kbd> | Rename the tab whose pill has keyboard focus (in the terminal itself, F2 goes to the program) |
| <kbd>⌘F</kbd> | Find in the terminal: <kbd>⏎</kbd> / <kbd>⇧⏎</kbd> next or previous, **Aa** match case, <kbd>Esc</kbd> closes |
| <kbd>⌘⇧K</kbd> | Clear the terminal (skipped while a full-screen program is running) |
| <kbd>⌘⌥1</kbd>…<kbd>⌘⌥8</kbd> | Jump to tab 1–8 in the focused tile; <kbd>⌘⌥9</kbd> jumps to the last tab |
| <kbd>⌘⇧J</kbd> | Jump to what needs you: a waiting question, a permission prompt or a crashed tab, across all workspaces |

Each team remembers whether its terminal tray is showing. The shortcuts are listed and can be changed in **Settings → Shortcuts**.

On a Mac the terminal shortcuts use <kbd>⌘</kbd>, never <kbd>⌃</kbd>: Control chords like <kbd>⌃T</kbd>, <kbd>⌃F</kbd> and <kbd>⌃K</kbd> always go to the shell. <kbd>⌘1</kbd>…<kbd>⌘9</kbd> jump between workspaces.

## Over the terminal

Four tools float over the focused terminal tile, so you can look at an agent's work without leaving its terminal. Each one belongs to the tile's active tab. Turn them on with the shortcut or from the tile's **View options** menu.

| Shortcut | Tool | What it's for |
|---|---|---|
| <kbd>⌘⇧D</kbd> | **Source diff** | Review the worktree's changes: the whole branch or only what's uncommitted, file by file, as a tree or a flat list. It's for reading only. There's no commit, publish or sync here, and nothing you do in it is sent to the agent. Press <kbd>e</kbd> to open the file in the code editor. |
| <kbd>⌘⇧C</kbd> | **Code editor** | Edit the worktree's files: a file tree, tabs, find and replace, multiple cursors, a change gutter against `HEAD`, <kbd>⌘P</kbd> to open a file and <kbd>⌃G</kbd> to go to a line. A save never overwrites a change that landed underneath you; you get a merge view instead. |
| <kbd>⌘⇧H</kbd> | **Story summary** | The story's doc over its terminal (on an epic tile, the epic's README). <kbd>⌘⇧U</kbd> expands or collapses it. |
| <kbd>⌘I</kbd> | **Inbox** | This tab's inbox, docked beside its terminal: everything the session sent and received, what was typed into it, and what it holds through a role. Reading here doesn't mark anything read. <kbd>⌘⇧I</kbd> opens your Inbox, for every session, in the right pane. |

Source diff and the code editor work the same on a worktree on a remote machine.

## Output is never lost to the UI

The terminal process, not the view, owns the session. Switching teams or tabs, or closing a tile, doesn't lose a running agent's output. If the app's window reloads or crashes, CodeSense re-attaches to the terminals that are still running.

Each terminal keeps 5,000 lines of history by default. Change it with **Terminal scrollback** in **Settings → General** (1,000 to 50,000, or Maximum).

New tabs open instantly because CodeSense keeps a few shells warm in the background. Set the count with **Warm terminal pool size** in **Settings → General** (0 to 5, default 2).

## Closing the window vs quitting

By default, closing the window keeps CodeSense running in the background. The MCP host, sync and any running agents keep going, and clicking the dock icon brings the window back. To make closing quit instead, turn on **Quit on window close** in **Settings → General**.

Quitting CodeSense (<kbd>⌘Q</kbd>) closes the app but not your terminals. They run in a separate process, the terminal server (`codesense-session-server`), which keeps running after you quit, so agents carry on working. The next time you open CodeSense it reconnects, and your tabs come back with everything that ran while it was closed. Restarting your Mac ends them. On a remote machine the terminal server runs in your account there, so its sessions keep going while your Mac sleeps or restarts.

If the terminal server can't start, terminals still open but run inside the app and close when you quit. If one left by another version of CodeSense is still running, CodeSense asks to restart it, which ends the terminals it runs. To stop it yourself, quit CodeSense, then run this in Terminal. It ends every terminal session and agent still running in it, and matches only the server itself:

```sh
pkill -f '/codesense-session-server[^ ]* --(resources|emulator|root) '
```

## Background agents

An agent launched headless, or from a page, runs with no visible tab. The **Agents** panel lists live agent sessions with their status. Each agent's row has a **Stop** that stops that agent.

To stop a page or routine from flooding your machine, background agents are capped. The **Background agent limit** in **Settings → General** is Auto (6), 2, 4, 6 or 8. Tabs you have open don't count toward it.

## Agents can use terminals too

Agents launched from CodeSense can open a terminal tab in an open workspace, run a command in it, and read the output back, for example to start a dev server and watch its logs. See [MCP tools for agents](mcp-tools.md).

The full guide ships in the app's docs: **Terminals & machines → Terminal tabs**.
