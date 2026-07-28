#!/usr/bin/env bash
#
# Generates third-party license notice files THIRDPARTY
#
# Usage:
#   scripts/generate-thirdparty.sh
set -euo pipefail
if command -v cargo-about > /dev/null 2>&1;
then
  echo "cargo-about already installed."
else
  echo "Installing cargo-about..."
  cargo install cargo-about --features="cli"
  echo "Success."
fi
echo "Generating plaintext build/THIRDPARTY notice..."
cargo-about generate --config ./build/about.toml ./build/about-plain.hbs > build/THIRDPARTY
echo "Success."
echo "Generating markdown THIRDPARTY.md notice..."
cargo-about generate --config ./build/about.toml ./build/about-markdown.hbs > ./THIRDPARTY.md
echo "Success."
