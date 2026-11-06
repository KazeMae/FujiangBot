#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$root"
bad=0
for file in $(find crates/plugins/agent -name '*.rs' | sort); do
  lines=$(wc -l < "$file" | tr -d ' ')
  if [ "$lines" -gt 1000 ]; then
    printf '%s %s\n' "$lines" "$file" >&2
    bad=1
  fi
done
exit "$bad"
