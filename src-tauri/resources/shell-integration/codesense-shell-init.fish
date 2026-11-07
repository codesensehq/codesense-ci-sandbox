# SPDX-License-Identifier: MIT
# codesense shell integration for fish
#
# Sourced through fish's --init-command (after the user's config.fish) when
# terminals.shellIntegration is on. The fish counterpart of
# codesense-shell-init.zsh: OSC 7 (cwd) + OSC 133 prompt-start on each prompt,
# command-start/command-end around each command, and Starship unless the user
# picked "plain" (fish's own prompt otherwise). fish already saves history after
# every command.
if not set -q __CODESENSE_SHELL_INIT_DONE
    set -g __CODESENSE_SHELL_INIT_DONE 1

    function __codesense_prompt_start --on-event fish_prompt
        printf '\e]7;file://%s%s\e\\' $hostname (string replace -a '%' '%25' -- $PWD)
        printf '\e]133;A\e\\'
    end

    function __codesense_preexec --on-event fish_preexec
        printf '\e]133;B\e\\'
    end

    # A signal's number on this host: `kill -l NAME` where it answers with one
    # (Linux), else NAME's position in the `kill -l` list (macOS).
    function __codesense_signal_number -a name
        set -l n (command kill -l $name 2>/dev/null)
        if string match -qr '^[0-9]+$' -- "$n"
            echo $n
            return
        end
        set -l names (command kill -l 2>/dev/null | string split -n ' ' | string split -n \t)
        contains -i -- $name $names
    end

    # The statuses a stop signal leaves (Ctrl-Z): 128 + STOP/TSTP/TTIN/TTOU.
    set -g __codesense_stop_statuses
    for __codesense_sig in STOP TSTP TTIN TTOU
        set -l n (__codesense_signal_number $__codesense_sig)
        and set -a __codesense_stop_statuses (math 128 + $n)
    end
    set -e __codesense_sig
    functions -e __codesense_signal_number

    # Whether a job is stopped, by the State field of `jobs` (Job, Group, CPU,
    # State, Command), never the command text.
    function __codesense_has_stopped_job
        for line in (jobs 2>/dev/null)
            set -l fields (string split \t -- $line)
            test "$fields[4]" = stopped; and return 0
        end
        return 1
    end

    # A foreground job that was only suspended (Ctrl-Z) has not ended: no
    # command-end for it (its `fg` reports the end).
    function __codesense_postexec --on-event fish_postexec
        set -l exit $status
        if contains -- $exit $__codesense_stop_statuses; and __codesense_has_stopped_job
            return
        end
        printf '\e]133;D;%s\e\\' $exit
    end

    # Once per machine: when claude or codex is installed but not signed in,
    # say how to sign in. A hint, never a gate. Linux only, where both keep
    # their sign-in in a file; the marker is written only once a hint was shown.
    if not test -e $HOME/.codesense/agent-login-hint-shown; and string match -q Linux -- (uname -s 2>/dev/null)
        set -l agents
        if type -q claude; and not test -s $HOME/.claude/.credentials.json; and not set -q ANTHROPIC_API_KEY; and not set -q CLAUDE_CODE_OAUTH_TOKEN
            set -a agents claude
        end
        set -l codex_home $HOME/.codex
        set -q CODEX_HOME; and set codex_home $CODEX_HOME
        if type -q codex; and not test -s $codex_home/auth.json; and not set -q OPENAI_API_KEY
            set -a agents codex
        end
        if set -q agents[1]
            printf '\e[2mTip: run %s once to sign in on this machine.\e[0m\n' (string join ' or ' $agents)
            mkdir -p $HOME/.codesense 2>/dev/null; and touch $HOME/.codesense/agent-login-hint-shown 2>/dev/null
        end
    end

    # In a CodeSense tab, plain `claude`, `codex` and `opencode` start connected to this
    # tab's CodeSense MCP server (inbox, tools) and report their status through
    # the tab's terminal host, exactly as an agent CodeSense launched. The
    # host's helper lives at a fixed path under `$CODESENSE_ACTIVITY_HOST`, so a
    # tab opened before CodeSense restarted still reports. Outside CodeSense the
    # variables are unset and nothing changes. `command claude` /
    # `command codex` run the plain CLI.

    # Run $argv[1] (claude or codex) with the rest as its command line,
    # observed by the tab's host when it is there.
    function __codesense_run_agent
        set -l agent $argv[1]
        set -e argv[1]
        if test -n "$CODESENSE_ACTIVITY_HOST"; and test -x "$CODESENSE_ACTIVITY_HOST/activity-helper"
            "$CODESENSE_ACTIVITY_HOST/activity-helper" --activity-run $agent -- $agent $argv
        else
            command $agent $argv
        end
    end

    # Whether a `.mcp.json` in this directory or a parent (Claude reads them
    # all) already names a `codesense` server, which is then kept. On a
    # machine the tab's own server always goes in: a folder's entry cannot
    # reach this tab, and the tab's, of the same name, replaces it.
    function __codesense_project_has_codesense
        test -n "$CODESENSE_MCP_SOCK"; and return 1
        set -l dir $PWD
        while true
            if test -f "$dir/.mcp.json"; and command grep -Eq '"codesense"[[:space:]]*:' "$dir/.mcp.json" 2>/dev/null
                return 0
            end
            if test "$dir" = /; or test -z "$dir"
                return 1
            end
            set dir (string replace -r '/[^/]*$' '' -- $dir)
            test -n "$dir"; or set dir /
        end
    end

    # Flags go in their `--flag=value` form: Claude's `--mcp-config` takes
    # several values and would swallow a prompt that followed it. The user's
    # own `--mcp-config`/`--strict-mcp-config` wins. Its status and inbox hooks
    # come with the tab's host, which observes it. Claude's subcommands are
    # left alone.
    if not functions -q claude
        function claude
            set -l extra
            if contains -- "$argv[1]" agents attach auth auto-mode doctor gateway import install logs mcp plugin plugins project respawn rm setup-token stop kill ultrareview update upgrade -v --version -h --help
                command claude $argv
                return
            end
            begin
                set -l own_mcp
                for arg in $argv
                    switch $arg
                        case '--'
                            break
                        case --mcp-config '--mcp-config=*' --strict-mcp-config
                            set own_mcp 1
                    end
                end
                if test -z "$own_mcp"; and test -n "$CODESENSE_MCP_CONFIG"; and test -r "$CODESENSE_MCP_CONFIG"; and not __codesense_project_has_codesense
                    set -a extra "--mcp-config=$CODESENSE_MCP_CONFIG"
                end
            end
            __codesense_run_agent claude $extra $argv
        end
    end

    # Codex takes the server as one `-c` override (it replaces a same-named
    # server, so there is never a duplicate). On a machine, its MCP servers
    # must run inside this tab (the socket admits only the tab's own
    # processes), so it runs without the shared Codex daemon where it can.
    if not functions -q codex
        function codex
            set -l extra
            if contains -- "$argv[1]" login logout mcp mcp-server app-server completion apply sandbox debug features cloud help -V --version -h --help
                command codex $argv
                return
            end
            if test -n "$CODESENSE_CODEX_MCP"
                set -l own_daemon
                for arg in $argv
                    switch $arg
                        case '--'
                            break
                        case --remote '--remote=*'
                            command codex $argv
                            return
                        case --no-daemon
                            set own_daemon 1
                    end
                end
                if test -n "$CODESENSE_MCP_SOCK"; and test -z "$own_daemon"
                    if not set -q __codesense_codex_no_daemon; and command -q codex
                        set -g __codesense_codex_no_daemon no
                        set -l help
                        if command -q timeout
                            set help (timeout 5 codex --help </dev/null 2>/dev/null)
                        else
                            set help (command codex --help </dev/null 2>/dev/null)
                        end
                        if string match -q -- '*--no-daemon*' $help
                            set -g __codesense_codex_no_daemon yes
                        end
                    end
                    test "$__codesense_codex_no_daemon" = yes; and set -a extra --no-daemon
                end
                set -a extra -c "$CODESENSE_CODEX_MCP"
            end
            __codesense_run_agent codex $extra $argv
        end
    end

    # OpenCode reads MCP config from OPENCODE_CONFIG_CONTENT. Keep a user's explicit
    # value; otherwise merge the tab's bridge fragment for this invocation.
    if not functions -q opencode
        function opencode
            set -l codesense_added_opencode_config no
            switch "$argv[1]"
                case auth mcp upgrade version completion help -v --version -h --help
                    command opencode $argv
                    return
            end
            if not set -q OPENCODE_CONFIG_CONTENT; and set -q CODESENSE_OPENCODE_CONFIG; and test -r "$CODESENSE_OPENCODE_CONFIG"
                set -gx OPENCODE_CONFIG_CONTENT (command cat -- "$CODESENSE_OPENCODE_CONFIG" | string collect)
                or return
                set codesense_added_opencode_config yes
            end
            __codesense_run_agent opencode $argv
            set -l opencode_status $status
            if test "$codesense_added_opencode_config" = yes
                set -eg OPENCODE_CONFIG_CONTENT
            end
            return $opencode_status
        end
    end

    if not set -q __CODESENSE_PLAIN; and type -q starship
        starship init fish | source
    end

    # One-shot decision consumed — don't leak it into manually-spawned sub-shells.
    set -e __CODESENSE_PLAIN
end

# Hand-typed Hermes stays unmanaged: tab identity only, no observer or MCP config.
if not functions -q hermes
    function hermes
        set -lx CODESENSE_TAB_SESSION_ID "$CODESENSE_TAB_SESSION_ID"
        command hermes $argv
    end
end
