# SPDX-License-Identifier: MIT
# codesense shell integration for bash
#
# Sourced by the managed --rcfile wrapper when terminals.shellIntegration is on
# (gated by the `$__CODESENSE_SHELL_INIT` env var set by spawn_pty). The bash
# counterpart of codesense-shell-init.zsh. It:
#   - emits OSC 7 (cwd) + OSC 133 prompt-start on every prompt, command-end
#     after each command, and command-start where bash has PS0 (4.4+; the macOS
#     /bin/bash 3.2 has none, so it gets no command-start mark),
#   - appends history after every command so SIGHUP/SIGKILL at app quit doesn't
#     drop recent commands,
#   - renders the prompt: Starship if on PATH (unless the user picked "plain"),
#     else a dependency-free fallback; "plain" keeps the user's PS1.
#
# Guard against double-sourcing if a nested codesense shell re-enters this file.
[ -n "$__CODESENSE_SHELL_INIT_DONE" ] && return
__CODESENSE_SHELL_INIT_DONE=1

# --- history ------------------------------------------------------------------
# Only when the user's config keeps a history file (an empty/unset HISTFILE
# means they turned it off).
[ -n "$HISTFILE" ] && shopt -s histappend

# --- OSC emitters -------------------------------------------------------------
# OSC 7: cwd as a file URI; literal `%` is percent-encoded so the Rust decoder
# round-trips it.
__codesense_osc7() {
  printf '\033]7;file://%s%s\033\\' "${HOSTNAME}" "${PWD//\%/%25}"
}

# Every prompt after the first ends a command. bash gives a prompt hook no
# reliable way to tell a blank Enter from a command (HISTCMD reads 1 there on
# bash 3.2), so a blank line also reports command-end, with the previous status.
__codesense_prompted=0

# The statuses a stop signal leaves (Ctrl-Z): 128 + STOP/TSTP/TTIN/TTOU, by this
# host's numbering.
__codesense_stop_statuses=" "
for __codesense_sig in STOP TSTP TTIN TTOU; do
  __codesense_stop_statuses="$__codesense_stop_statuses$((128 + $(builtin kill -l "$__codesense_sig"))) "
done
unset __codesense_sig

__codesense_precmd() {
  local exit=$?
  if [ "$__codesense_prompted" = 1 ]; then
    # A foreground job that was only suspended has not ended: no command-end for
    # it (its `fg` reports the end).
    case "$__codesense_stop_statuses" in
      *" $exit "*) [ -n "$(jobs -s)" ] || printf '\033]133;D;%s\033\\' "$exit" ;;
      *) printf '\033]133;D;%s\033\\' "$exit" ;;
    esac
  fi
  __codesense_prompted=1
  [ -n "$HISTFILE" ] && history -a
  __codesense_osc7
  printf '\033]133;A\033\\'
  # Hand the command's status on to any later PROMPT_COMMAND hook (Starship).
  return $exit
}


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
  local agent="$1"
  shift
  if [ -n "${CODESENSE_ACTIVITY_HOST-}" ] && [ -x "$CODESENSE_ACTIVITY_HOST/activity-helper" ]; then
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
  [ -n "${CODESENSE_MCP_SOCK-}" ] && return 1
  local dir="$PWD"
  while :; do
    [ -f "$dir/.mcp.json" ] && command grep -Eq '"codesense"[[:space:]]*:' "$dir/.mcp.json" 2>/dev/null && return 0
    { [ "$dir" = / ] || [ -z "$dir" ]; } && return 1
    dir="${dir%/*}"
    [ -n "$dir" ] || dir=/
  done
}

# Flags go in their `--flag=value` form: Claude's `--mcp-config` takes several
# values and would swallow a prompt that followed it. The user's own
# `--mcp-config`/`--strict-mcp-config` wins. Its status and inbox hooks come
# with the tab's host, which observes it. Claude's subcommands (`claude mcp
# list`) are left alone.
if ! declare -F claude >/dev/null; then
  function claude {
    local arg own_mcp=""
    local -a extra
    extra=()
    case "${1-}" in
      agents|attach|auth|auto-mode|doctor|gateway|import|install|logs|mcp|plugin|plugins|project|respawn|rm|setup-token|stop|kill|ultrareview|update|upgrade|-v|--version|-h|--help)
        command claude "$@"
        return
        ;;
      *)
        for arg in "$@"; do
          case "$arg" in
            --) break ;;
            --mcp-config|--mcp-config=*|--strict-mcp-config) own_mcp=1 ;;
          esac
        done
        if [ -z "$own_mcp" ] && [ -n "${CODESENSE_MCP_CONFIG-}" ] && [ -r "$CODESENSE_MCP_CONFIG" ] && ! __codesense_project_has_codesense; then
          extra+=("--mcp-config=$CODESENSE_MCP_CONFIG")
        fi
        ;;
    esac
    __codesense_run_agent claude ${extra[@]+"${extra[@]}"} "$@"
  }
fi

# Codex takes the server as one `-c` override (it replaces a same-named server,
# so there is never a duplicate). On a machine, its MCP servers must run inside
# this tab (the socket admits only the tab's own processes), so it runs without
# the shared Codex daemon where it supports that.
if ! declare -F codex >/dev/null; then
  function codex {
    local arg own_daemon=""
    local -a extra
    extra=()
    case "${1-}" in
      login|logout|mcp|mcp-server|app-server|completion|apply|sandbox|debug|features|cloud|help|-V|--version|-h|--help)
        command codex "$@"
        return
        ;;
    esac
    if [ -n "${CODESENSE_CODEX_MCP-}" ]; then
      for arg in "$@"; do
        case "$arg" in
          --) break ;;
          --remote|--remote=*) command codex "$@"; return ;;
          --no-daemon) own_daemon=1 ;;
        esac
      done
      if [ -n "${CODESENSE_MCP_SOCK-}" ] && [ -z "$own_daemon" ]; then
        if [ -z "${__codesense_codex_no_daemon-}" ] && command -v codex >/dev/null 2>&1; then
          __codesense_codex_no_daemon=no
          {
            if command -v timeout >/dev/null 2>&1; then timeout 5 codex --help; else command codex --help; fi
          } </dev/null 2>/dev/null | command grep -q -- '--no-daemon' && __codesense_codex_no_daemon=yes
        fi
        [ "$__codesense_codex_no_daemon" = yes ] && extra+=(--no-daemon)
      fi
      extra+=(-c "$CODESENSE_CODEX_MCP")
    fi
    __codesense_run_agent codex ${extra[@]+"${extra[@]}"} "$@"
  }
fi

# OpenCode reads MCP config from OPENCODE_CONFIG_CONTENT. Keep a user's explicit
# value; otherwise merge the tab's bridge fragment while this invocation runs.
if ! declare -F opencode >/dev/null; then
  function opencode {
    local had_config=${OPENCODE_CONFIG_CONTENT+x}
    local -x OPENCODE_CONFIG_CONTENT
    case "${1-}" in
      auth|mcp|upgrade|version|completion|help|-v|--version|-h|--help)
        command opencode "$@"
        return
        ;;
    esac
    if [ -z "$had_config" ] && [ -n "${CODESENSE_OPENCODE_CONFIG-}" ] && [ -r "$CODESENSE_OPENCODE_CONFIG" ]; then
      OPENCODE_CONFIG_CONTENT=$(command cat -- "$CODESENSE_OPENCODE_CONFIG") || return
    fi
    __codesense_run_agent opencode "$@"
  }
fi

# --- prompt -------------------------------------------------------------------
if [ -n "$__CODESENSE_PLAIN" ]; then
  : # leave PS1 as the user's config set it
elif command -v starship >/dev/null 2>&1; then
  eval "$(starship init bash)"
else
  __codesense_git_branch() {
    local branch
    branch=$(git symbolic-ref --short HEAD 2>/dev/null) || return
    # \001/\002 mark the color codes as zero-width so readline measures the prompt right.
    printf ' \001\033[35m\002%s\001\033[0m\002' "$branch"
  }
  PS1='\[\033[36m\]\w\[\033[0m\]$(__codesense_git_branch) \[\033[32m\]❯\[\033[0m\] '
fi

# Put our hook at the FRONT of PROMPT_COMMAND (this runs after Starship's init
# added its own), so it reads the command's real status before any other hook.
if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
  PROMPT_COMMAND=(__codesense_precmd "${PROMPT_COMMAND[@]}")
else
  PROMPT_COMMAND="__codesense_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi

# Command-start mark, where bash supports PS0.
if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; then
  PS0="${PS0}"'\033]133;B\033\\'
fi

# One-shot decision consumed — don't leak it into manually-spawned sub-shells.
unset __CODESENSE_PLAIN

# Hand-typed Hermes stays unmanaged: tab identity only, no observer or MCP config.
if ! declare -F hermes >/dev/null; then
  function hermes {
    ( export CODESENSE_TAB_SESSION_ID="${CODESENSE_TAB_SESSION_ID-}"; command hermes "$@" )
  }
fi
