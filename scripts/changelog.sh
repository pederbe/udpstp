#!/usr/bin/env bash
# Release-plz postprocessor. Keep one changelog format for all repository paths.
set -euo pipefail
read -r _ version release_date previous < <(sed -n '/^UDPSTP_REPOSITORY_CHANGELOG /p')
version=${version#v}

config=$(mktemp "${TMPDIR:-/tmp}/udpstp-cliff.XXXXXX.toml")
trap 'rm -f "$config"' EXIT
# Release-plz keeps git-cliff's Git options inside [changelog].
sed -n '/^\[changelog\]/,$p' release-plz.toml | sed '/^sort_commits = /i [git]' > "$config"

args=(--offline --config "$config" --unreleased --tag "v$version")
if [ -n "$previous" ]; then
  previous=${previous#v}
  git merge-base --is-ancestor "refs/tags/v$previous" HEAD
  args+=(--tag-pattern "^v(${previous//./\\.}|${version//./\\.})$")
  range="v$previous..HEAD"
else
  args+=(--tag-pattern '^v$')
  range=HEAD
fi
UDPSTP_RELEASE_DATE="$release_date" git-cliff "${args[@]}" -- "$range"
