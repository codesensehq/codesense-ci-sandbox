#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
set -euo pipefail

# The e2e WebDriver plugin (vendored and patched under src-tauri/vendor) runs scripts in any
# frame of the app. Only the e2e-webdriver feature may pull it in: the app's release dependency
# graph (default features, every target) must hold neither the plugin nor its vendored path.
# The e2e graph must hold the vendored copy, which proves the check looks where it should.

cd "$(dirname "$0")/.."

plugin='tauri-plugin-wdio-webdriver'
vendored='src-tauri/vendor/tauri-plugin-wdio-webdriver'

release="$(cargo tree -p codesense -e normal,build --target all)"
if grep -E "(^|[^[:alnum:]_-])${plugin}( |$)" <<<"$release" >/dev/null ||
  grep -F "$vendored" <<<"$release" >/dev/null; then
  echo "${plugin} is in the release dependency graph" >&2
  exit 1
fi

e2e="$(cargo tree -p codesense -e normal,build --target all --features e2e-webdriver)"
if ! grep -F "$vendored" <<<"$e2e" | grep -F "$plugin" >/dev/null; then
  echo "the e2e-webdriver graph does not hold the vendored ${plugin}; this check is blind" >&2
  exit 1
fi

echo "release graph: no ${plugin}; e2e-webdriver graph: the vendored copy"
