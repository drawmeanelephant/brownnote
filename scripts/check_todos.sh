#!/usr/bin/env bash
# Tech-debt marker gate.
#
# A TODO/FIXME/XXX/HACK must say who owns it: TODO(#123) for an issue or
# TODO(name) for a person. Bare markers are debt nobody can schedule.
#
# Usage:
#   scripts/check_todos.sh            # scan tracked source files
#   scripts/check_todos.sh --staged   # scan only staged files (pre-commit)
set -u

cd "$(dirname "$0")/.." || exit 1

if [ "${1:-}" = "--staged" ]; then
	files=$(git diff --cached --name-only --diff-filter=ACMR)
else
	files=$(git ls-files)
fi

bad=0
count=0
while IFS= read -r f; do
	[ -n "$f" ] || continue
	case "$f" in
	# These two files define the rule and have to quote the marker names.
	scripts/check_todos.sh|tests/check_agents_md.py) continue ;;
	*.c|*.h|*.rs|*.py|*.sh|*.toml|*.yml|*.yaml|Makefile) ;;
	*) continue ;;
	esac
	[ -f "$f" ] || continue
	while IFS=: read -r line rest; do
		# Drop well-formed markers, then any marker left over is bare.
		scrubbed=$(printf '%s' "$rest" | sed -E \
			's/(TODO|FIXME|HACK)\((#[0-9]+|[A-Za-z0-9_.-]+)\)//g')
		if printf '%s' "$scrubbed" | grep -qE '\b(TODO|FIXME|HACK)\b'; then
			count=$((count + 1))
			echo "$f:$line: marker must be TODO(#issue) or TODO(owner): $rest"
			bad=1
		fi
	done < <(grep -nE '\b(TODO|FIXME|HACK)\b' "$f" || true)
done <<< "$files"

echo "check_todos: $count marker(s), $([ "$bad" -eq 0 ] && echo ok || echo violations)"
exit "$bad"
