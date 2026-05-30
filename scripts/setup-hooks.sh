#!/usr/bin/env bash
# Point git at the tracked hooks in .githooks/. Run once per clone.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath .githooks
echo "git hooks enabled (core.hooksPath = .githooks)"
