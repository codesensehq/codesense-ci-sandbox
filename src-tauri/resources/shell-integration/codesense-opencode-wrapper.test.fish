#!/usr/bin/env fish
# SPDX-License-Identifier: MIT

# Exercise the hand-typed OpenCode wrapper with a fake CLI. Pass the path to
# codesense-shell-init.fish as argv[1]. The temporary executable prints the
# environment it receives, then the wrapper must remove the injected value.
set -l root (mktemp -d)
mkdir -p "$root/bin"
printf '{"mcp":{"codesense":{"enabled":true}}}\n' > "$root/opencode-mcp.json"
printf '#!/bin/sh\nprintf "%%s\\n" "$OPENCODE_CONFIG_CONTENT"\n' > "$root/bin/opencode"
chmod +x "$root/bin/opencode"
set -gx PATH "$root/bin" $PATH
set -gx CODESENSE_OPENCODE_CONFIG "$root/opencode-mcp.json"
set -e OPENCODE_CONFIG_CONTENT
source $argv[1]

set -l result (opencode)
if test "$result" != '{"mcp":{"codesense":{"enabled":true}}}'
    echo "OpenCode did not receive the tab MCP config: $result" >&2
    exit 1
end
if set -q OPENCODE_CONFIG_CONTENT
    echo "OpenCode MCP config leaked after the command" >&2
    exit 1
end
