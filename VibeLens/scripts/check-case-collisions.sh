#!/usr/bin/env bash
# Fails if two source paths differ only by letter case, or only by .ts/.tsx/.js extension with a
# case difference: macOS/Windows file systems are case-insensitive, so `import "./Markdown"` can
# silently resolve to markdown.ts there while CI on Linux is happy.
set -euo pipefail
cd "$(dirname "$0")/.."
dupes=$(git ls-files 2>/dev/null || find . -type f -not -path './node_modules/*' -not -path './target/*')
collisions=$(printf '%s\n' "$dupes" | sed -E 's/\.(tsx|ts|jsx|js|mjs)$//' | awk '{print tolower($0)}' | sort | uniq -d)
if [[ -n "$collisions" ]]; then
  echo "Case-insensitive path collisions (break macOS/Windows):" >&2
  printf '  %s\n' $collisions >&2
  exit 1
fi
echo "no case collisions"
