# SPDX-License-Identifier: MIT
# The rule for a commit or push that cannot be scanned here (no node, or no
# privacy hooks in this checkout). The hooks in this directory source it, and
# install.sh copies it into the dispatcher it installs.
# - is_public: the rule of isPublicRemote in privacy-ci.mjs (the public owner
#   on any host, extra public URLs, or a github.com URL whose owner cannot be read).
# - privacy_required: the rule of privacyRequired in privacy-ci.mjs. A maintainer
#   clone sets git config codesense.privacyRequired=true or CODESENSE_PRIVACY_REQUIRED=1.
# - unscanned: blocks when the clone requires the private rules; otherwise
#   allows with a notice, since CI scans the change.
# - require_inventory: the dispatcher's check before any checkout code runs.
is_public() {
  for extra in ${CODESENSE_PRIVACY_PUBLIC_REMOTES:-}; do
    [ "$extra" = "$1" ] && return 0
  done
  printf "%s" "$1" | grep -qi codesensehq && return 0
  printf "%s" "$1" | grep -qi "github\.com" || return 1
  ! printf "%s" "$1" |
    grep -qiE "^([a-z][a-z0-9+.-]*://)?([^@/]*@)?(www\.)?github\.com(:[0-9]+)?[:/]+[a-z0-9-]+/[^/]+/*$"
}

# Both the env value and git config are read as git booleans: set and not
# readable as false (unparseable included) means required.
privacy_required() {
  if [ -n "${CODESENSE_PRIVACY_REQUIRED:-}" ] &&
    [ "$(git -c codesense.privacyRequired="$CODESENSE_PRIVACY_REQUIRED" \
      config --bool --get codesense.privacyRequired 2>/dev/null)" != false ]; then
    return 0
  fi
  value=$(git config --bool --get codesense.privacyRequired 2>/dev/null)
  [ $? -ne 1 ] && [ "$value" != false ]
}

# True when any remote is public, or when the remotes cannot be listed.
public_remote() {
  remotes=$(git remote -v) || return 0
  tab=$(printf "\t")
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    url=${line#*"$tab"}
    is_public "${url% (*)}" && return 0
  done <<REMOTES
$remotes
REMOTES
  return 1
}

unscanned() {
  if privacy_required; then
    echo "privacy-scan: cannot check $1 (this checkout has no privacy hooks, or node is missing)" >&2
    exit 1
  fi
  echo "privacy hook: skipped the local scan of $1 (no privacy hooks here, or no node); CI scans this" >&2
}

# A clone that requires the private rules needs an inventory from outside the
# tree (findInventory in privacy-ci.mjs). The dispatcher checks it before it
# hands off, so no checked-out branch can decide.
require_inventory() {
  [ -n "${CODESENSE_PRIVACY_TERMS:-}" ] && return 0
  [ -f "${HOME:+$HOME/.config/codesense/privacy-terms.json}" ] &&
    [ -r "$HOME/.config/codesense/privacy-terms.json" ] && return 0
  privacy_required || return 0
  echo "privacy-scan: no private term inventory, and this clone requires one." \
    "Set CODESENSE_PRIVACY_TERMS or place it at ~/.config/codesense/privacy-terms.json." >&2
  exit 1
}
