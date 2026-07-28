#!/usr/bin/env bash
#
# Enforce the smoke-harness black-box boundary: a smoke package must depend on
# NO `axon-*` product crate (only workspace-pinned third-party crates and the
# shipped binaries it spawns). This keeps each harness independently movable
# across the planned repository split.
#
# Usage: scripts/check-smoke-isolation.sh <package> [<package> ...]
set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: $0 <smoke-package> [<smoke-package> ...]" >&2
  exit 2
fi

status=0
for pkg in "$@"; do
  # Every package reachable through normal, build, and dev edges.
  if ! deps=$(cargo tree -p "$pkg" --edges normal,build,dev --prefix none --no-dedupe \
    | awk '{print $1}' | sort -u); then
    echo "FAIL: 'cargo tree' failed for package '$pkg'" >&2
    status=1
    continue
  fi
  # Any axon-* package other than the smoke package itself is a violation.
  violations=$(printf '%s\n' "$deps" | grep -E '^axon-' | grep -vx "$pkg" || true)
  if [ -n "$violations" ]; then
    echo "FAIL: smoke package '$pkg' depends on axon-* product crate(s):" >&2
    printf '  %s\n' $violations >&2
    status=1
  else
    echo "OK: '$pkg' depends on no axon-* product crate."
  fi
done

exit "$status"
