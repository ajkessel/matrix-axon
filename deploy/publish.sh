#!/usr/bin/env bash
# Publish the Axon Docker images to a container registry (ADR 0052).
#
# Builds the axon-server and axon-web images multi-arch and pushes them to GHCR
# (or any registry), tagged with a moving label (default `beta`) plus the current
# git short hash. Images are published to a PUBLIC GHCR package (set once by
# hand in the org's Packages settings — GHCR defaults new packages to private,
# and pushing doesn't change that), so anyone can pull without credentials.
#
#   # one-time: log in with a PAT that has write:packages
#   echo "$GHCR_PAT" | docker login ghcr.io -u <you> --password-stdin
#
#   ./publish.sh                       # ghcr.io/matrix-axon/{axon-server,axon-web}:beta + :<hash>
#   ./publish.sh --tag rc1             # a different moving tag
#   ./publish.sh --amd64-only          # skip arm64 (no QEMU needed)
#   ./publish.sh --server-only         # or --web-only
#   ./publish.sh --dry-run             # print the buildx commands, don't push
#
# arm64 in a multi-arch build needs QEMU/binfmt. Docker Desktop ships it; on a
# plain Linux host register it once with:
#   docker run --privileged --rm tonistiigi/binfmt --install arm64
set -euo pipefail

REGISTRY="${AXON_REGISTRY:-ghcr.io/matrix-axon}"
TAG="beta"
PLATFORMS="linux/amd64,linux/arm64"
WHAT="both"          # both | server | web
DRY_RUN=0

# Print the leading comment block (everything after the shebang up to the first
# non-`#` line), stripped of the `# ` prefix — robust to the header's length.
usage() { awk 'NR==1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; }

while [ $# -gt 0 ]; do
	case "$1" in
		--tag) TAG="${2:?--tag needs a value}"; shift 2 ;;
		--registry) REGISTRY="${2:?--registry needs a value}"; shift 2 ;;
		--platforms) PLATFORMS="${2:?--platforms needs a value}"; shift 2 ;;
		--amd64-only) PLATFORMS="linux/amd64"; shift ;;
		--server-only) WHAT="server"; shift ;;
		--web-only) WHAT="web"; shift ;;
		--dry-run) DRY_RUN=1; shift ;;
		-h|--help) usage; exit 0 ;;
		*) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
	esac
done

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# --- locate the repo root (build context) -----------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

command -v docker >/dev/null || die "docker not found."
docker buildx version >/dev/null 2>&1 || die "docker buildx not available (needed for multi-arch)."

GIT_HASH="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"

# --- ensure a buildx builder that can do multi-platform ---------------------
# The default `docker` driver can't build more than the host arch; a
# docker-container driver can. Single-arch (host) builds use whatever's current.
if [ "$PLATFORMS" != "linux/amd64" ] && [ "$PLATFORMS" != "linux/$(docker version -f '{{.Server.Arch}}' 2>/dev/null)" ]; then
	if ! docker buildx inspect axon-publish >/dev/null 2>&1 && [ "$DRY_RUN" -eq 0 ]; then
		info "Creating buildx builder 'axon-publish' (docker-container driver)…"
		docker buildx create --name axon-publish --driver docker-container >/dev/null
	fi
	BUILDER=(--builder axon-publish)
else
	BUILDER=()
fi

# --- build + push -----------------------------------------------------------
# $1 = image short name, $2 = dockerfile
publish_image() {
	local name="$1" dockerfile="$2"
	local repo="${REGISTRY}/${name}"
	info "Publishing ${repo}:${TAG} (+ :${GIT_HASH}) for ${PLATFORMS}"
	local cmd=(docker buildx build "${BUILDER[@]}"
		--platform "$PLATFORMS"
		-f "$dockerfile"
		--build-arg "GIT_HASH=${GIT_HASH}"
		-t "${repo}:${TAG}" -t "${repo}:${GIT_HASH}"
		--push "$ROOT")
	if [ "$DRY_RUN" -eq 1 ]; then
		printf '  (dry-run) '; printf '%q ' "${cmd[@]}"; printf '\n'
	else
		"${cmd[@]}"
	fi
}

[ "$WHAT" = "web" ]    || publish_image axon-server Dockerfile
[ "$WHAT" = "server" ] || publish_image axon-web    deploy/web/Dockerfile

info "Done. Anyone can pull (no login needed, images are public):"
echo "    # in deploy/.env:"
echo "    AXON_SERVER_IMAGE=${REGISTRY}/axon-server:${TAG}"
echo "    AXON_WEB_IMAGE=${REGISTRY}/axon-web:${TAG}"
echo "    docker compose pull axon-server web && docker compose up -d"
