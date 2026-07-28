#!/usr/bin/env bash
# One-time setup of osxcross on a self-hosted runner, so it can cross-compile
# axon for x86_64-apple-darwin and aarch64-apple-darwin. Not run by CI: it
# needs a macOS SDK tarball you supply yourself (extracted from Xcode on
# hardware/an Apple Developer account you control — Apple's SDK license does
# not allow us to fetch or redistribute it, so there is no automated source
# for this step). Re-run only when you want to move to a newer SDK.
#
# Usage:
#   scripts/setup-macos-cross.sh /path/to/MacOSX14.5.sdk.tar.xz
#
# Extracting an SDK tarball on macOS, once you have Xcode installed:
#
#   SDK=$(xcrun --sdk macosx --show-sdk-path)
#   REAL_SDK=$(python3 -c "import os,sys; print(os.path.realpath(sys.argv[1]))" "${SDK}")
#   basename "${SDK}" | grep -q 'OSX[0-9]' && SDK_VERSION_NAME=$(basename "${SDK}")
#   basename "${REAL_SDK}" | grep -q 'OSX[0-9]' && SDK_VERSION_NAME=$(basename "${REAL_SDK}")
#   tar -C "$(dirname "${SDK}")" -chJf "${SDK_VERSION_NAME}.tar.xz" "$(basename "${SDK}")"
#
# The basename checks above are intended to figure out whether the symlink or
# the underlying version of the xcode directory actually contains a version number.
#
# Resolve to the real path first (don't just tar `$SDK` directly): on most
# Xcode/CLT installs, `--show-sdk-path` returns Xcode's floating "current SDK"
# symlink (.../SDKs/MacOSX.sdk -> MacOSX26.5.sdk, say), and tarring a symlink
# path archives it under *that* name — you'd get a tarball correctly named
# MacOSX26.5.sdk.tar.xz (if you additionally renamed the file by hand) but
# whose extracted top-level directory is still literally "MacOSX.sdk", no
# version at all. osxcross's build.sh separately greps for the extracted
# directory by version after it's already parsed the version out of the
# tarball's filename, so that mismatch fails deep into the build, not up
# front. Resolving the real path first keeps both names in agreement.
#
# The tarball's name matters too: osxcross's build.sh detects the SDK version
# by parsing it out of the filename (e.g. MacOSX14.5.sdk.tar.xz -> 14.5).
# Naming it anything else — including dropping the version, e.g. plain
# MacOSX.sdk.tar.xz — makes osxcross unable to detect a version and it fails
# with "Unsupported SDK". Keep whatever `basename "$REAL_SDK"` produces intact
# in the tarball name; don't rename it (this script checks the two agree
# before running the multi-minute osxcross build, so a mismatch here fails
# fast rather than after several minutes).
#
# After this script finishes, source /opt/osxcross/env.sh (or wherever
# OSXCROSS_ROOT points) in any shell/workflow that needs to invoke the
# wrapped compilers; cross-build.yml does this automatically.

set -euo pipefail

SDK_TARBALL="${1:?usage: $0 /path/to/MacOSX*.sdk.tar.xz}"
OSXCROSS_ROOT="${OSXCROSS_ROOT:-/opt/osxcross}"
OSXCROSS_REPO="${OSXCROSS_REPO:-https://github.com/tpoechtrager/osxcross.git}"

printf "Setting up osxcross. Parameters:\nSDK_TARBALL = %s\nOSXCROSS_ROOT = %s\nOSXCROSS_REPO = %s\n" $SDK_TARBALL $OSXCROSS_ROOT $OSXCROSS_REPO

if [ ! -f "${SDK_TARBALL}" ]; then
    echo "SDK tarball not found: $SDK_TARBALL" >&2
    exit 1
fi

if [ ! -d "$OSXCROSS_ROOT" ]; then
  mkdir -p "$OSXCROSS_ROOT"
fi

if [ ! -w "$OSXCROSS_ROOT" ]; then
  echo "Can't write to $OSXCROSS_ROOT. Check permissions. Exiting."
  exit 1
fi

#sudo mkdir -p "$OSXCROSS_ROOT"
#sudo chown "$(id -u):$(id -g)" "$OSXCROSS_ROOT"

if [ ! -d "$OSXCROSS_ROOT/src" ]; then
    git clone "$OSXCROSS_REPO" "$OSXCROSS_ROOT/src"
fi

# osxcross's build.sh hardcodes a table of known SDK versions (e.g. 26|26.0*,
# 26.1*, 26.2*) and rejects anything else with "Unsupported SDK" — it lags
# behind however fast Apple ships new point releases. Patch in a generic
# fallback, run every time (idempotent) so it also covers a checkout that was
# cloned by an earlier run of this script: any macOS major.minor not already
# in the table maps to the same darwin-major-minus-one, same-minor Darwin
# target as every explicit entry already follows (e.g. 26.5 -> darwin25.5,
# matching how 26.2 -> darwin25.2 is already spelled out above it). This does
# not touch how already-listed versions resolve; case matches top-down, so
# any of osxcross's real entries still win first.
BUILD_SH="$OSXCROSS_ROOT/src/build.sh"
if ! grep -F -q 'darwin$((MAJOR - 1))' "$BUILD_SH"; then
    python3 - "$BUILD_SH" <<'PYEOF'
import sys

path = sys.argv[1]
with open(path) as f:
    lines = f.readlines()

marker = 'echo "Unsupported SDK"'
fallback = (
    '  [0-9][0-9].[0-9]*)\n'
    '    MAJOR=$(echo $SDK_VERSION | cut -d. -f1)\n'
    '    MINOR=$(echo $SDK_VERSION | cut -d. -f2)\n'
    '    TARGET=darwin$((MAJOR - 1)).$MINOR\n'
    '    SUPPORTED_ARCHS="arm64 arm64e x86_64 x86_64h"\n'
    '    NEED_TAPI_SUPPORT=1\n'
    '    OSX_VERSION_MIN_INT=10.13\n'
    '    ;;\n'
)
for i, line in enumerate(lines):
    if marker in line:
        lines.insert(i, fallback)
        break
else:
    sys.exit("setup-macos-cross.sh: couldn't find the 'Unsupported SDK' anchor in build.sh to patch")

with open(path, "w") as f:
    f.writelines(lines)
PYEOF
fi

# Catch a mismatch between the tarball's filename version and its actual
# top-level extracted directory name now, in seconds, rather than after
# osxcross's multi-minute build fails at its very last step trying to `mv`
# a glob that matches nothing (see the comment above about symlinked SDK
# paths — this is exactly the failure mode that produces).
# `|| true` on each: under `set -e`/pipefail, grep -o finding nothing (the
# exact case the checks below exist to report) would otherwise kill the
# script right here, silently, before either message ever gets a chance to
# print.
TARBALL_VERSION=$(basename "$SDK_TARBALL" | tr -c '0-9.' ' ' | tr -s ' ' '\n' | grep -oE '^[0-9]+\.[0-9]+' | head -1 || true)
TOP_LEVEL_ENTRY=$(tar -tf "$SDK_TARBALL" | head -1 | cut -d/ -f1 || true)
if [ -z "$TARBALL_VERSION" ]; then
    echo "Couldn't find a major.minor version number in tarball filename: $SDK_TARBALL" >&2
    exit 1
fi
if [[ "$TOP_LEVEL_ENTRY" != *"$TARBALL_VERSION"* ]]; then
    cat >&2 <<EOF
SDK tarball / contents mismatch: the filename says version $TARBALL_VERSION,
but the tarball's top-level entry is named "$TOP_LEVEL_ENTRY", which doesn't
contain that version. osxcross will fail on this after running its full
build, so this script is stopping now instead.

This usually happens when the tarball was created by archiving Xcode's
floating "MacOSX.sdk" symlink directly instead of its real, versioned
target. See the comment near the top of this script for the fix (resolve
the real path with python3's os.path.realpath before taring).
EOF
    exit 1
fi

cp "$SDK_TARBALL" "$OSXCROSS_ROOT/src/tarballs/"

(
    cd "$OSXCROSS_ROOT/src"
    UNATTENDED=1 ./build.sh
)

# osxcross names the wrapper compilers after the SDK's Darwin version, e.g.
# x86_64-apple-darwin23.5-clang. Discover what build.sh actually produced
# rather than hardcoding a version, so this script keeps working as you pick
# up newer SDKs.
BIN_DIR="$OSXCROSS_ROOT/src/target/bin"
X86_CLANG=$(find "$BIN_DIR" -maxdepth 1 -name 'x86_64-apple-darwin*-clang' ! -name '*-clang++' | sort | tail -1)
ARM_CLANG=$(find "$BIN_DIR" -maxdepth 1 -name 'aarch64-apple-darwin*-clang' ! -name '*-clang++' | sort | tail -1)
X86_AR=$(find "$BIN_DIR" -maxdepth 1 -name 'x86_64-apple-darwin*-ar' | sort | tail -1)
ARM_AR=$(find "$BIN_DIR" -maxdepth 1 -name 'aarch64-apple-darwin*-ar' | sort | tail -1)

if [ -z "$X86_CLANG" ] || [ -z "$ARM_CLANG" ] || [ -z "$X86_AR" ] || [ -z "$ARM_AR" ]; then
    echo "osxcross build finished but expected wrapper compilers weren't found in $BIN_DIR" >&2
    exit 1
fi

cat >"$OSXCROSS_ROOT/env.sh" <<EOF
# Generated by scripts/setup-macos-cross.sh. Source this to point cargo at the
# osxcross-wrapped Apple clang for both macOS targets.
export OSXCROSS_ROOT="$OSXCROSS_ROOT"
export PATH="$BIN_DIR:\$PATH"
export CC_x86_64_apple_darwin="$X86_CLANG"
export CXX_x86_64_apple_darwin="${X86_CLANG}++"
export AR_x86_64_apple_darwin="$X86_AR"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$X86_CLANG"
export CC_aarch64_apple_darwin="$ARM_CLANG"
export CXX_aarch64_apple_darwin="${ARM_CLANG}++"
export AR_aarch64_apple_darwin="$ARM_AR"
export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="$ARM_CLANG"
EOF

echo "osxcross ready. Wrapper compilers:"
echo "  x86_64: $X86_CLANG"
echo "  aarch64: $ARM_CLANG"
echo "Env file written to $OSXCROSS_ROOT/env.sh"
