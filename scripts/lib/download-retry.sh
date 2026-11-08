# SPDX-License-Identifier: MIT
# Shared by the scripts that download pinned CI tools (install-scanners.sh, fetch-git-ai.sh, build-emulator.sh).
# Source it, then: download_with_retry <out> <url> [extra curl args...]
#
# GitHub release downloads can answer 5xx for minutes at a time, and curl's own --retry misses some of
# those (macOS curl reports a 504 over HTTP/2 as exit 56, which --retry does not retry). So the retry
# lives here: any failure is tried again after a growing pause (5s, 10s, 20s, 40s, then 60s), except
# an HTTP 4xx other than 408 or 429, which is permanent and returns at once.
#
# Bounds: no pause or attempt starts that would reach the deadline, and one attempt runs at most 120s,
# so a call ends within DOWNLOAD_RETRY_BUDGET (the one budget setting, default 360s) plus 120s.
# A caller that downloads several files sets DOWNLOAD_DEADLINE=$((SECONDS + DOWNLOAD_RETRY_BUDGET))
# once, so all its downloads share one budget.
# DOWNLOAD_RETRY_DELAYS overrides the pauses (tests use short ones).
#
# Each attempt starts from an empty <out>. This only fetches bytes: the caller still checks the sha256
# before using anything, whether the download needed retries or not.
# Returns 0 on success, else the last curl exit status; DOWNLOAD_HTTP_CODE holds the last HTTP code.

DOWNLOAD_RETRY_BUDGET=${DOWNLOAD_RETRY_BUDGET:-360}

download_with_retry() {
  local out=$1 url=$2 delay left status
  shift 2
  local deadline=${DOWNLOAD_DEADLINE:-$((SECONDS + DOWNLOAD_RETRY_BUDGET))}
  local delays
  read -r -a delays <<<"${DOWNLOAD_RETRY_DELAYS:-5 10 20 40 60 60 60 60}"
  delays+=(last)
  DOWNLOAD_HTTP_CODE=""
  for delay in "${delays[@]}"; do
    if (( SECONDS >= deadline )); then
      echo "download: retry budget spent: $url" >&2
      return "${status:-1}"
    fi
    rm -f "$out"
    status=0
    # -q (first) ignores ~/.curlrc.
    DOWNLOAD_HTTP_CODE=$(curl -q --fail --silent --show-error --location \
      --connect-timeout 20 --max-time 120 "$@" \
      --write-out '%{http_code}' --output "$out" "$url") || status=$?
    (( status != 0 )) || return 0
    case "$DOWNLOAD_HTTP_CODE" in
      408|429) ;;
      4??) return "$status" ;;
    esac
    [ "$delay" != last ] || return "$status"
    left=$((deadline - SECONDS))
    if (( delay >= left )); then
      echo "download: retry budget spent: $url" >&2
      return "$status"
    fi
    echo "download: curl exit $status, HTTP ${DOWNLOAD_HTTP_CODE:-none}; retrying in ${delay}s: $url" >&2
    sleep "$delay"
  done
}
