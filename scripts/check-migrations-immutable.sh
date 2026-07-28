#!/usr/bin/env bash
#
# Enforce that applied migrations are immutable: a file under
# crates/axon-store/migrations/ that already exists on the base branch must
# never be modified, renamed, or deleted — only new files may be added.
#
# sqlx records a checksum of each migration file's *entire contents* in
# `_sqlx_migrations` and refuses to start when a previously applied migration
# no longer matches. That makes even a comment-only edit a hard startup
# failure on every existing deployment:
#
#   migration <version> was previously applied but has been modified
#
# See ADR 0004 (forward-only migrations). To change schema, add a new
# timestamped migration; to correct a comment, put the explanation in the new
# migration that supersedes it.
#
# A commit that must change a published migration anyway (reverting a bad edit
# back to the bytes deployments already applied, say) declares itself with a
# `Migration-edit-approved: <reason>` trailer in its message.
#
# Usage: scripts/check-migrations-immutable.sh [<base-ref>]
#
# The base must be the *canonical* main, not a personal fork's — a fork whose
# main lags upstream holds published migrations at stale bytes, which reads as
# though this branch had changed them. CI passes the pull request's own
# `base.sha`; locally, with no argument, the first of base/main, upstream/main,
# origin/main, main that resolves is used, so `git fetch upstream main` before
# trusting a clean result.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

dir="crates/axon-store/migrations"
trailer_key="Migration-edit-approved"

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

# M(odified), D(eleted), R(enamed) against an already-published migration are
# all checksum breaks. A(dded) is the only legitimate change.
filter=(--name-status --diff-filter=MDR)

# Reduce a --name-status stream to entries whose path is actually *published*,
# i.e. already present on the base. "Immutable" only ever meant "already in
# someone's `_sqlx_migrations`"; a migration this branch itself introduces is
# not yet in any database, so amending it in a follow-up commit — the ordinary
# "add migration, fix a typo before merge" flow — is fine. Without this filter
# each commit's diff against its own parent would report that typo fix as an M.
published_only() {
  local status old new
  while IFS=$'\t' read -r status old new; do
    [ -n "$status" ] || continue
    # For a rename it is the *old* name that was published; for M and D there
    # is no second field and `old` is the path.
    git cat-file -e "$base:$old" 2>/dev/null || continue
    if [ -n "$new" ]; then
      printf '%s\t%s -> %s\n' "$status" "$old" "$new"
    else
      printf '%s\t%s\n' "$status" "$old"
    fi
  done
}

violations=""
approved=""

# Arm A: changes introduced by commits that are on this branch and not on the
# base. Inspecting each commit individually — rather than diffing the merge
# base against the tip — is what keeps a migration change the branch merely
# *inherited* from main (a revert landed upstream, say) from being reported as
# the branch's own edit.
#
# Merge commits stay in scope: `git show` gives them a dense-combined diff,
# which lists only files that differ from *every* parent, so an ordinary merge
# of main contributes nothing while a merge that hand-edits a migration during
# conflict resolution is still caught.
while read -r commit; do
  [ -n "$commit" ] || continue
  changed=$(git show --format= "${filter[@]}" "$commit" -- "$dir" | published_only)
  [ -n "$changed" ] || continue

  # A trailer value may be folded across lines; flatten it and trim both ends,
  # so an all-whitespace value doesn't read as an approval and a folded
  # continuation line doesn't leave a stray leading space in the reason.
  #
  # The trailer is commit-scoped, deliberately: it waves through every
  # published migration that commit touches, and the report below names them
  # all so a reviewer can see exactly what the one claim covered. Keep a
  # sanctioned migration edit in a commit of its own.
  reason=$(git log -1 --format="%(trailers:key=${trailer_key},valueonly)" "$commit" | tr '\n' ' ')
  reason=${reason#"${reason%%[![:space:]]*}"}
  reason=${reason%"${reason##*[![:space:]]}"}
  subject=$(git log -1 --format='%h %s' "$commit")
  indented=$(printf '%s\n' "$changed" | sed 's/^/      /')
  if [ -n "$reason" ]; then
    approved+="  $subject"$'\n'"$indented"$'\n'"      ${trailer_key}: ${reason}"$'\n'
  else
    violations+="  $subject"$'\n'"$indented"$'\n'
  fi
done < <(git rev-list "$base..HEAD")

# Arm B: uncommitted changes, so this catches an edit before it is ever
# committed. In CI the tree is clean and this finds nothing; locally under jj,
# git's HEAD tracks the parent of the working-copy commit, so this is the arm
# that surfaces work in progress. There is no commit message here, so the
# approval trailer cannot apply.
uncommitted=$(git diff "${filter[@]}" HEAD -- "$dir" | published_only)
if [ -n "$uncommitted" ]; then
  violations+="  uncommitted changes in the working tree"$'\n'
  violations+="$(printf '%s\n' "$uncommitted" | sed 's/^/      /')"$'\n'
fi

if [ -n "$approved" ]; then
  printf 'Published migration(s) changed with an explicit %s trailer:\n' "$trailer_key"
  printf '%s' "$approved"
fi

if [ -z "$violations" ]; then
  echo "OK: no unapproved change to a published migration under $dir (base: $base)."
  exit 0
fi

echo "FAIL: published migration file(s) changed since $base:" >&2
printf '%s' "$violations" >&2
cat >&2 <<EOF

Migrations are forward-only (ADR 0004). sqlx checksums each file's full
contents, so any edit — including comments and whitespace — breaks startup on
every database that already applied it:

  migration <version> was previously applied but has been modified

Revert the file to its published bytes and add a new timestamped migration
instead ('sqlx migrate add <description>'). If the change is deliberate and
correct anyway, add a '${trailer_key}: <reason>' trailer to the commit
message and justify it in the pull request.
EOF
exit 1
