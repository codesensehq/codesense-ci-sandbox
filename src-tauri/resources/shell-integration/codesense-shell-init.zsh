# SPDX-License-Identifier: MIT
# codesense shell integration for zsh
#
# Sourced by the managed ZDOTDIR `.zshrc` when terminals.shellIntegration is on
# (gated by the `$__CODESENSE_SHELL_INIT` env var set by spawn_pty). It:
#   - emits OSC 7 (cwd) + OSC 133 (semantic prompt markers) on every prompt,
#   - enables incremental history flush so SIGHUP/SIGKILL at app quit doesn't
#     drop recent commands,
#   - renders the prompt INLINE at shell boot — Starship if it's on PATH (the
#     bundled binary, unless the user picked "plain"), else a dependency-free
#     fallback. No post-spawn write, so there is no init flash.
#
# Guard against double-sourcing if a nested codesense shell re-enters this file.
[[ -n "$__CODESENSE_SHELL_INIT_DONE" ]] && return
__CODESENSE_SHELL_INIT_DONE=1

# --- incremental history flush (survive SIGHUP/SIGKILL at app quit) -----------
# Ensure history is actually persisted: a clean machine with no dotfiles has no
# HISTFILE/SAVEHIST, so INC_APPEND_HISTORY alone would save nothing. Only fill
# in defaults that the user's sourced config left unset (`:=` is no-clobber).
: ${HISTFILE:=$HOME/.zsh_history}
: ${HISTSIZE:=10000}
: ${SAVEHIST:=10000}
# INC_APPEND_HISTORY (append after every command) is what makes history survive
# SIGHUP/SIGKILL at app quit. We deliberately do NOT set SHARE_HISTORY — it
# additionally reorders history across concurrent shells, which would override a
# user who intentionally keeps per-shell history.
setopt INC_APPEND_HISTORY

# --- OSC emitters -------------------------------------------------------------
# OSC 7: report cwd as a file URI so Rust can track it per session. Percent-
# encode literal `%` so the Rust side's %xx decoder round-trips a path that
# actually contains a percent sign (e.g. `/tmp/100%done`).
__codesense_osc7() {
  printf '\e]7;file://%s%s\e\\' "${HOST}" "${PWD//\%/%25}"
}

# Tracks whether a command is in flight, so the very first prompt does not emit
# a spurious command-end (there is no preceding command to report).
__codesense_cmd_active=0

# The statuses a stop signal leaves (Ctrl-Z): 128 + STOP/TSTP/TTIN/TTOU, by this
# host's numbering (`$signals[1]` is EXIT, so a signal's number is its index - 1).
# Computed under zsh's own options, whatever the user set (KSH_ARRAYS shifts
# array indices).
zmodload -i zsh/parameter 2>/dev/null
typeset -ga __codesense_stop_statuses
__codesense_compute_stop_statuses() {
  emulate -L zsh
  local sig
  __codesense_stop_statuses=()
  for sig in STOP TSTP TTIN TTOU; do
    __codesense_stop_statuses+=( $(( ${signals[(i)$sig]} + 127 )) )
  done
}
__codesense_compute_stop_statuses
unfunction __codesense_compute_stop_statuses

# Whether status $1 is a stop signal's and a job is suspended (under zsh's own
# options, whatever the user set).
__codesense_suspended() {
  emulate -L zsh
  (( ${__codesense_stop_statuses[(Ie)$1]} )) && [[ -n ${(M)${(v)jobstates}:#suspended*} ]]
}

# precmd: command-end for the just-finished command (capture $? FIRST), then
# cwd + prompt-start for the prompt about to be drawn. A foreground job that was
# only suspended has not ended: no command-end for it (its `fg` reports the end).
__codesense_precmd() {
  local exit=$?
  if (( __codesense_cmd_active )); then
    if ! __codesense_suspended "$exit"; then
      printf '\e]133;D;%s\e\\' "$exit"
    fi
    __codesense_cmd_active=0
  fi
  __codesense_osc7
  printf '\e]133;A\e\\'
}

# preexec: command-start, right after the user submits a command line.
__codesense_preexec() {
  printf '\e]133;B\e\\'
  __codesense_cmd_active=1
}

# Append to the hook arrays — never define bare precmd()/preexec(), which would
# clobber Starship's own hooks.
autoload -Uz add-zsh-hook
add-zsh-hook precmd  __codesense_precmd
add-zsh-hook preexec __codesense_preexec


# --- agent sign-in hint -------------------------------------------------------
# Once per machine: when claude or codex is installed but not signed in, say how
# to sign in. A hint, never a gate. Linux only, where both keep their sign-in in
# a file; the marker is written only once a hint was shown.
__codesense_agent_hint() {
  [ -e "$HOME/.codesense/agent-login-hint-shown" ] && return
  case "$OSTYPE" in
    linux*) ;;
    "") [ "$(uname -s 2>/dev/null)" = Linux ] || return ;;
    *) return ;;
  esac
  local agents=""
  if command -v claude >/dev/null 2>&1 && [ ! -s "$HOME/.claude/.credentials.json" ] && [ -z "$ANTHROPIC_API_KEY" ] && [ -z "$CLAUDE_CODE_OAUTH_TOKEN" ]; then
    agents="claude"
  fi
  if command -v codex >/dev/null 2>&1 && [ ! -s "${CODEX_HOME:-$HOME/.codex}/auth.json" ] && [ -z "$OPENAI_API_KEY" ]; then
    agents="${agents:+$agents or }codex"
  fi
  [ -n "$agents" ] || return
  printf '\033[2mTip: run %s once to sign in on this machine.\033[0m\n' "$agents"
  mkdir -p "$HOME/.codesense" 2>/dev/null && : >"$HOME/.codesense/agent-login-hint-shown" 2>/dev/null
}
__codesense_agent_hint
unset -f __codesense_agent_hint

# --- agents connected to this tab ---------------------------------------------
# In a CodeSense tab, plain `claude`, `codex` and `opencode` start connected to this tab's
# CodeSense MCP server (inbox, tools) and report their status through the tab's
# terminal host, exactly as an agent CodeSense launched. The host's helper
# lives at a fixed path under `$CODESENSE_ACTIVITY_HOST`, so a tab opened
# before CodeSense restarted still reports. Outside CodeSense the variables are
# unset and nothing changes. `command <cli>` runs the plain CLI.

# Run `$1` (claude, codex or opencode) with the rest as its command line, observed by the
# tab's host when it is there; the helper starts the CLI unobserved otherwise.
__codesense_run_agent() {
  emulate -L zsh
  local agent=$1
  shift
  if [[ -n $CODESENSE_ACTIVITY_HOST && -x $CODESENSE_ACTIVITY_HOST/activity-helper ]]; then
    "$CODESENSE_ACTIVITY_HOST/activity-helper" --activity-run "$agent" -- "$agent" "$@"
  else
    command "$agent" "$@"
  fi
}

# Whether a `.mcp.json` in this directory or a parent (Claude reads them all)
# already names a `codesense` server, which is then kept. On a machine the
# tab's own server always goes in: a folder's entry cannot reach this tab, and
# the tab's, of the same name, replaces it.
__codesense_project_has_codesense() {
  emulate -L zsh
  [[ -n $CODESENSE_MCP_SOCK ]] && return 1
  local dir=$PWD
  while true; do
    [[ -f $dir/.mcp.json ]] && command grep -Eq '"codesense"[[:space:]]*:' "$dir/.mcp.json" 2>/dev/null && return 0
    [[ $dir == / || -z $dir ]] && return 1
    dir=${dir:h}
  done
}

# Flags go in their `--flag=value` form: Claude's `--mcp-config` takes several
# values and would swallow a prompt that followed it. The user's own
# `--mcp-config`/`--strict-mcp-config` wins. Its status and inbox hooks come
# with the tab's host, which observes it. Claude's subcommands (`claude mcp
# list`) are left alone.
if ! (( $+functions[claude] )); then
  function claude {
    emulate -L zsh
    local -a extra
    local arg own_mcp=
    case $1 in
      agents|attach|auth|auto-mode|doctor|gateway|import|install|logs|mcp|plugin|plugins|project|respawn|rm|setup-token|stop|kill|ultrareview|update|upgrade|-v|--version|-h|--help)
        command claude "$@"
        return
        ;;
      *)
        for arg in "$@"; do
          case $arg in
            --) break ;;
            --mcp-config|--mcp-config=*|--strict-mcp-config) own_mcp=1 ;;
          esac
        done
        if [[ -z $own_mcp && -n $CODESENSE_MCP_CONFIG && -r $CODESENSE_MCP_CONFIG ]] && ! __codesense_project_has_codesense; then
          extra+=("--mcp-config=$CODESENSE_MCP_CONFIG")
        fi
        ;;
    esac
    __codesense_run_agent claude "${extra[@]}" "$@"
  }
fi

# Codex takes the server as one `-c` override (it replaces a same-named server,
# so there is never a duplicate). On a machine, its MCP servers must run inside
# this tab (the socket admits only the tab's own processes), so it runs without
# the shared Codex daemon where it supports that.
if ! (( $+functions[codex] )); then
  function codex {
    emulate -L zsh
    local -a extra
    local arg own_daemon=
    case $1 in
      login|logout|mcp|mcp-server|app-server|completion|apply|sandbox|debug|features|cloud|help|-V|--version|-h|--help)
        command codex "$@"
        return
        ;;
    esac
    if [[ -n $CODESENSE_CODEX_MCP ]]; then
      for arg in "$@"; do
        case $arg in
          --) break ;;
          --remote|--remote=*) command codex "$@"; return ;;
          --no-daemon) own_daemon=1 ;;
        esac
      done
      if [[ -n $CODESENSE_MCP_SOCK && -z $own_daemon ]]; then
        if [[ -z $__codesense_codex_no_daemon ]] && command -v codex >/dev/null 2>&1; then
          __codesense_codex_no_daemon=no
          {
            if command -v timeout >/dev/null 2>&1; then timeout 5 codex --help; else command codex --help; fi
          } </dev/null 2>/dev/null | command grep -q -- '--no-daemon' && __codesense_codex_no_daemon=yes
        fi
        [[ $__codesense_codex_no_daemon == yes ]] && extra+=(--no-daemon)
      fi
      extra+=(-c "$CODESENSE_CODEX_MCP")
    fi
    __codesense_run_agent codex "${extra[@]}" "$@"
  }
fi

# OpenCode reads MCP config from OPENCODE_CONFIG_CONTENT. Keep a user's explicit
# value; otherwise merge the tab's bridge fragment while this invocation runs.
if ! (( $+functions[opencode] )); then
  function opencode {
    emulate -L zsh
    case $1 in
      auth|mcp|upgrade|version|completion|help|-v|--version|-h|--help)
        command opencode "$@"
        return
        ;;
    esac
    if [[ -z ${OPENCODE_CONFIG_CONTENT+x} && -n $CODESENSE_OPENCODE_CONFIG && -r $CODESENSE_OPENCODE_CONFIG ]]; then
      local -x OPENCODE_CONFIG_CONTENT
      OPENCODE_CONFIG_CONTENT=$(command cat -- "$CODESENSE_OPENCODE_CONFIG") || return
      __codesense_run_agent opencode "$@"
    else
      __codesense_run_agent opencode "$@"
    fi
  }
fi

# --- prompt -------------------------------------------------------------------
# plain → keep the user's own prompt untouched (the OSC hooks above still run, so
#         cwd/marker capture is unaffected — integration ≠ prompt takeover).
# else  → Starship if on PATH (bundled, or the user's own), otherwise the
#         dependency-free fallback.
if [[ -n "$__CODESENSE_PLAIN" ]]; then
  : # leave $PROMPT as the user's config set it
elif command -v starship >/dev/null 2>&1; then
  eval "$(starship init zsh)"
else
  # Dependency-free fallback: cwd + git branch. No oh-my-zsh, no dotfiles.
  setopt PROMPT_SUBST
  __codesense_git_branch() {
    local branch
    branch=$(git symbolic-ref --short HEAD 2>/dev/null) || return
    print -n " %F{magenta}${branch}%f"
  }
  PROMPT='%F{cyan}%~%f$(__codesense_git_branch) %F{green}❯%f '
fi

# One-shot decision consumed — don't leak it into manually-spawned sub-shells.
unset __CODESENSE_PLAIN

# Hand-typed Hermes stays unmanaged: tab identity only, no observer or MCP config.
if ! (( $+functions[hermes] )); then
  function hermes {
    ( export CODESENSE_TAB_SESSION_ID="${CODESENSE_TAB_SESSION_ID-}"; command hermes "$@" )
  }
fi
