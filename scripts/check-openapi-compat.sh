#!/usr/bin/env bash
#
# Enforce that a change to openapi/openapi.json does not introduce an
# API-breaking change without an explicit acknowledgment.
#
# check:api (clients/web) only checks that schema.d.ts is *regenerated* from
# openapi.json — it says nothing about whether the change it was regenerated
# from is compatible with a client that hasn't upgraded yet. This script is
# the compatibility check that gap needs. See ADR 0099.
#
# The primary audience is axon-tui: unlike the web client, which is served
# from the same origin as the server and upgrades on every deploy (ADR 0087),
# a TUI binary is independently distributed and can run against a server for
# a long time without upgrading.
#
# A commit that must ship a breaking API change anyway declares itself with an
# `API-Breaking-Change: <reason>` trailer in its message, same pattern as
# scripts/check-migrations-immutable.sh's `Migration-edit-approved`.
#
# Usage: scripts/check-openapi-compat.sh [<base-ref>]
#
# CI passes the pull request's own base.sha; locally, with no argument, the
# first of base/main, upstream/main, origin/main, main that resolves is used.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

spec="openapi/openapi.json"
trailer_key="API-Breaking-Change"
oasdiff_version="1.29.1"

base="${1:-}"
if [ -n "$base" ]; then
  if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
    echo "FAIL: base ref '$base' not found; fetch it first (e.g. 'git fetch upstream main')." >&2
    exit 2
  fi
else
  for candidate in base/main upstream/main origin/main main; do
    if git rev-parse --verify --quiet "$candidate^{commit}" >/dev/null; then
      base="$candidate"
      break
    fi
  done
  if [ -z "$base" ]; then
    echo "FAIL: no base ref found (tried base/main, upstream/main, origin/main, main)." >&2
    echo "      Fetch one (e.g. 'git fetch upstream main') or pass it as an argument." >&2
    exit 2
  fi
fi

if ! git cat-file -e "$base:$spec" 2>/dev/null; then
  echo "OK: $spec did not exist at $base; nothing to compare."
  exit 0
fi

if git diff --quiet "$base" HEAD -- "$spec" 2>/dev/null; then
  echo "OK: $spec unchanged since $base."
  exit 0
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

git show "$base:$spec" >"$tmpdir/base.json"
git show "HEAD:$spec" >"$tmpdir/head.json"

# Pinned release + checksums (https://github.com/oasdiff/oasdiff/releases).
# Bumping the version means updating the matching sha256 below.
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$os" in
  linux)
    case "$arch" in
      x86_64) asset="oasdiff_${oasdiff_version}_linux_amd64.tar.gz"
        sha256="541f7c66c933495fceef24eaf5c48aa66c19069f366f7bd0a60a6a4820c5e533" ;;
      aarch64|arm64) asset="oasdiff_${oasdiff_version}_linux_arm64.tar.gz"
        sha256="8bc247f0280f62ca73599265db0d984e853d7df6e714dad6ead85afc7cfc5883" ;;
      *) echo "FAIL: unsupported linux arch '$arch' for oasdiff." >&2; exit 2 ;;
    esac
    ;;
  darwin)
    asset="oasdiff_${oasdiff_version}_darwin_all.tar.gz"
    sha256="759cc5703d9335c441ad84a7074c705486b2c493f79bcfdf251c7a9c788b1171"
    ;;
  *)
    echo "FAIL: unsupported OS '$os' for oasdiff; run this check under Linux or macOS." >&2
    exit 2
    ;;
esac

url="https://github.com/oasdiff/oasdiff/releases/download/v${oasdiff_version}/${asset}"
curl -fsSL -o "$tmpdir/oasdiff.tar.gz" "$url"
echo "${sha256}  ${tmpdir}/oasdiff.tar.gz" | sha256sum -c - >/dev/null
tar -xzf "$tmpdir/oasdiff.tar.gz" -C "$tmpdir" oasdiff

set +e
breaking=$("$tmpdir/oasdiff" breaking --fail-on ERR "$tmpdir/base.json" "$tmpdir/head.json" 2>&1)
status=$?
set -e

if [ "$status" -eq 0 ]; then
  echo "OK: no breaking change to $spec since $base."
  [ -z "$breaking" ] || printf '%s\n' "$breaking"
  exit 0
fi

reason=$(git log --format="%(trailers:key=${trailer_key},valueonly)" "$base..HEAD" | tr '\n' ' ')
reason=${reason#"${reason%%[![:space:]]*}"}
reason=${reason%"${reason##*[![:space:]]}"}

if [ -n "$reason" ]; then
  echo "Breaking change to $spec acknowledged via '${trailer_key}' trailer: ${reason}"
  printf '%s\n' "$breaking"
  exit 0
fi

echo "FAIL: $spec has a breaking change since $base with no ${trailer_key} trailer:" >&2
printf '%s\n' "$breaking" >&2
cat >&2 <<EOF

An axon-tui binary in the field can be running an older server contract for a
long time (ADR 0099) — additive changes are expected, but this one removes or
changes something a client may already depend on.

If this is a genuinely additive change oasdiff misjudged, or the breaking
change is deliberate and the compat cost is accepted, add an
'${trailer_key}: <reason>' trailer to a commit in this PR and justify it in
the PR description.
EOF
exit 1
