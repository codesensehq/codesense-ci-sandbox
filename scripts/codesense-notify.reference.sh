#!/bin/bash
# SPDX-License-Identifier: MIT
# Source of Claude's status hook. CodeSense embeds it (codesense_pty_core::notify_hook)
# and each host installs it as codesense-notify.sh beside its activity helper; Claude's
# status hooks run it as "$CODESENSE_ACTIVITY_HOST/codesense-notify.sh". No workspace or
# team content carries a copy. The seam test (scripts/codesense-notify-hook.test.sh)
# runs this file.
# Codesense terminal status hook — sends Claude Code lifecycle events to the local HTTP API.
#
# SS-FU-17 AC-4 (graceful degradation): when the codesense app isn't running
# (no port to reach), exit 0 with one legible line on stderr rather than
# failing opaquely or blocking the lifecycle event. Presence of the hook
# means "installed"; this message means "installed but inactive."
#
# Diagnosability: when a CodeSense tab's hook fails to reach the app (no port,
# curl error, non-200, or a post the app dropped as an unknown session) it
# appends one timestamped line to ~/.codesense/logs/notify-hook.log. A hook
# running outside a CodeSense tab is expected and is not logged.
# Logging is best-effort and never blocks or fails the lifecycle event.
#
# Endpoint: CODESENSE_STATUS_URL, when set, is the full status endpoint (an
# injected value for an agent whose PTY is not local). Unset, the hook posts to
# the local app exactly as before.

LOG_DIR="$HOME/.codesense/logs"
LOG="$LOG_DIR/notify-hook.log"
log_fail() {
  {
    mkdir -p "$LOG_DIR"
    # Cap the log so a hook firing outside CodeSense can't grow it without bound.
    if [ -f "$LOG" ] && [ "$(wc -c <"$LOG")" -gt 524288 ]; then
      mv -f "$LOG" "$LOG.1"
    fi
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG"
  } 2>/dev/null
}

# Prefer per-instance env var (set by PTY); fall back to legacy port file.
PORT=${CODESENSE_API_PORT:-$(cat ~/.codesense/http.port 2>/dev/null)}
if [ -z "$PORT" ] && [ -z "$CODESENSE_STATUS_URL" ]; then
  echo "codesense not running — skipping (start the app to re-enable)" >&2
  # Only a CodeSense tab expects a post; elsewhere this is the normal case.
  [ -n "$CODESENSE_TAB_SESSION_ID" ] && log_fail "no-port: CODESENSE_API_PORT unset and no ~/.codesense/http.port"
  exit 0
fi
# Not a CodeSense terminal tab — the normal case outside the app; not logged.
[ -z "$CODESENSE_TAB_SESSION_ID" ] && exit 0

INPUT=$(cat)

post() {
  RESP=$(curl -s --max-time 3 -w '\n%{http_code}' -X POST "$URL" \
    -H "Content-Type: application/json" \
    -H "X-Codesense-Tab-Session-Id: $CODESENSE_TAB_SESSION_ID" \
    -d "$INPUT" 2>/dev/null)
  RC=$?
}

URL=${CODESENSE_STATUS_URL:-http://127.0.0.1:$PORT/api/v1/terminal/status}
post
# The session outlived the app that spawned it (an app restart or update keeps the
# terminal sessions): CODESENSE_API_PORT is that old instance's port. The running app
# rewrites ~/.codesense/http.port at every start, so retry there once.
if [ "$RC" -eq 7 ] && [ -z "$CODESENSE_STATUS_URL" ]; then
  CURRENT=$(cat ~/.codesense/http.port 2>/dev/null)
  if [ -n "$CURRENT" ] && [ "$CURRENT" != "$PORT" ]; then
    URL=http://127.0.0.1:$CURRENT/api/v1/terminal/status
    post
  fi
fi

CODE=${RESP##*$'\n'}
if [ "$RC" -ne 0 ] || [ "$CODE" != "200" ] || [[ "$RESP" == *'"unknown-session"'* ]]; then
  EVENT=$(printf '%s' "$INPUT" | sed -n 's/.*"hook_event_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
  BODY=${RESP%$'\n'*}
  log_fail "post-failed: event=${EVENT:-?} session=$CODESENSE_TAB_SESSION_ID url=$URL curl_rc=$RC http=${CODE:-none} body=${BODY:0:200}"
fi

exit 0
