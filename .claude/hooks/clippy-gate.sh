#!/usr/bin/env bash
# Stop hook: when Rust files changed, keep Claude working until clippy is clean.
# Exit 2 sends stderr back to Claude. A second stop in a row is let through so
# a lint Claude cannot fix does not loop.
set -uo pipefail

input=$(cat)
if [ "$(jq -r '.stop_hook_active // false' <<<"$input")" = true ]; then
  exit 0
fi

cd "${CLAUDE_PROJECT_DIR:-$(dirname "$0")/../..}" || exit 0
# Compare with where the branch left main, so changes already committed in
# this turn are still checked. On main itself that is HEAD.
base=$(git merge-base HEAD main 2>/dev/null || echo HEAD)
changed=$({ git diff --name-only "$base" -- '*.rs' 'Cargo.toml' 'Cargo.lock'; git ls-files -o --exclude-standard -- '*.rs'; } 2>/dev/null)
[ -z "$changed" ] && exit 0

if ! out=$(cargo clippy --locked --all-targets --quiet -- -D warnings 2>&1); then
  printf 'cargo clippy failed:\n%s\n' "$(tail -n 80 <<<"$out")" >&2
  exit 2
fi
