#!/bin/sh
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
exec python3 "$ROOT/native/scripts/package_macos.py" package "$@"
