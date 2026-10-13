#!/bin/sh
# gen_alias_table.sh — regenerate README.md's effect-alias table from
# the aliases[] array in src/brownnote.c, the single source of truth.
# Usage: scripts/gen_alias_table.sh [path-to-brownnote.c]
#
# Same-effect rows must stay adjacent in the C table: the generator
# folds each run of equal targets into one README row, in order.
set -eu

src="${1:-src/brownnote.c}"

awk '
	/^\t\{ "[^"]+",\t*"[^"]+" \},/ {
		# { "alias",\t"Effect" },  — fields 2 and 4 between quotes
		split($0, q, "\"")
		alias = q[2]; effect = q[4]
		if (effect == last)
			row[n] = row[n] ", `" alias "`"
		else {
			n++
			row[n] = "`" alias "`"
			eff[n] = effect
			last = effect
		}
	}
	END {
		print "| Alias | Effect |"
		print "|---|---|"
		for (i = 1; i <= n; i++)
			print "| " row[i] " | " eff[i] " |"
	}
' "$src"
