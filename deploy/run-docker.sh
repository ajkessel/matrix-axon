#!/usr/bin/env bash
# One-shot bootstrap for the Axon Docker deployment (ADR 0052).
#
# Brings up the full Compose stack (Postgres + axon-server + the `web` front door)
# and points you at the browser to finish setup. By default the stack arms the
# one-time first-credential web bootstrap: this script surfaces the browser URL
# that mints your first credential and signs the web client in with nothing to
# copy or paste.
#
# The TUI is an optional add-on (--tui): it mints a bearer token and writes a
# ready-to-use axon-tui config. Because the FIRST credential is what arms the web
# bootstrap, minting a TUI token consumes it — so do the web setup first, then add
# the TUI (or just use --tui if you don't want the web client).
#
#   ./run-docker.sh              # bring up + show the next step (bootstrap URL, or "already set up")
#   ./run-docker.sh --tui        # also mint a token + write an axon-tui config
#   ./run-docker.sh --tui --yes  # ...and assume "yes" to the config-overwrite prompt
set -euo pipefail

TUI_RELEASES_URL="https://github.com/matrix-axon/matrix-axon/releases"

# --- locate ourselves so compose finds deploy/docker-compose.yml ------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# --- args -------------------------------------------------------------------
ASSUME_YES=0
WRITE_TUI=0
for arg in "$@"; do
	case "$arg" in
		--yes|-y) ASSUME_YES=1 ;;
		--tui) WRITE_TUI=1 ;;
		-h|--help)
			cat <<'EOF'
run-docker.sh — bootstrap the Axon Docker deployment (ADR 0052)

Brings up the Compose stack (Postgres + axon-server + web) and prints the browser
URL for the one-time first-credential web bootstrap, which signs the web client in
without any copy/paste. Optionally (--tui) mints a bearer token and writes a
ready-to-use axon-tui config; it never silently overwrites an existing config
(it warns, asks to confirm, and backs the old file up with a timestamped .bak).

Usage: ./run-docker.sh [--tui] [--yes]
  --tui       Also mint a token and write an axon-tui config. Consumes the web
              bootstrap if it hasn't been used yet — do the web setup first.
  --yes, -y   Assume "yes" to the TUI config-overwrite prompt (non-interactive).
EOF
			exit 0 ;;
		*) echo "unknown argument: $arg (try --help)" >&2; exit 2 ;;
	esac
done

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()   { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# --- pick a compose command (v2 plugin preferred, v1 fallback) --------------
if docker compose version >/dev/null 2>&1; then
	DC=(docker compose)
elif command -v docker-compose >/dev/null 2>&1; then
	DC=(docker-compose)
else
	die "Docker Compose not found. Install Docker Desktop or the compose plugin."
fi

# --- bring the stack up -----------------------------------------------------
# Stamp the build with the current commit when we're in a git checkout (falls
# back to "unknown" otherwise — see Dockerfile).
if command -v git >/dev/null 2>&1 && git -C .. rev-parse --short HEAD >/dev/null 2>&1; then
	export GIT_HASH="$(git -C .. rev-parse --short HEAD)"
fi

info "Building and starting the Axon stack (this can take a while on first run)…"
# --wait blocks until every service passes its healthcheck (or the timeout trips).
if ! "${DC[@]}" up --build -d --wait --wait-timeout 300; then
	echo
	"${DC[@]}" ps || true
	die "The stack did not become healthy. Check '${DC[*]} logs'."
fi
info "Stack is up and healthy."

# --- figure out the front-door base URL -------------------------------------
# The `web` service publishes the stack on the host at AXON_PORT (default 8080).
# Honour an AXON_PORT set in the environment or in ./.env.
PORT="${AXON_PORT:-}"
if [ -z "$PORT" ] && [ -f .env ]; then
	PORT="$(grep -E '^[[:space:]]*AXON_PORT=' .env | tail -n1 | cut -d= -f2- | tr -d '[:space:]' || true)"
fi
PORT="${PORT:-8080}"
BASE_URL="http://127.0.0.1:${PORT}"

# --- classify the one-time web bootstrap ------------------------------------
# axon-server prints "…web bootstrap is armed: open http://<bind>/bootstrap/<code>"
# to its logs on the boot where no credential exists yet. The <bind> is the
# container's internal address; we keep only the /bootstrap/<code> path and join
# it to the front-door BASE_URL the browser actually reaches.
#
# Those log lines persist across re-runs, so a scraped URL alone can't tell
# "still needs setup" from "already done". We ask the LIVE server: the setup page
# shows a "Create bearer token" button; once the first credential exists the same
# URL serves a "Bootstrap complete" page (200), and after a container restart the
# bootstrap isn't armed at all (404). Only the setup page is actionable. (GETting
# the *correct* code URL is safe — only wrong bootstrap URLs count toward the
# lockout.) So a repeat run after setup no longer prints a dead URL.
BOOTSTRAP_PATH="$("${DC[@]}" logs axon-server 2>/dev/null \
	| grep -F 'web bootstrap is armed: open' \
	| grep -oE '/bootstrap/[^[:space:]]+' | tail -n1 || true)"
BOOTSTRAP_URL=""
BOOTSTRAP_STATE="unknown"   # pending | done | unknown
if [ -n "$BOOTSTRAP_PATH" ]; then
	BOOTSTRAP_URL="${BASE_URL}${BOOTSTRAP_PATH}"
	if command -v curl >/dev/null 2>&1; then
		if curl -fsS "$BOOTSTRAP_URL" 2>/dev/null | grep -q 'Create bearer token'; then
			BOOTSTRAP_STATE="pending"
		else
			BOOTSTRAP_STATE="done"   # "Bootstrap complete" (200) or not armed (404)
		fi
	else
		BOOTSTRAP_STATE="pending"   # can't probe without curl; assume it's live
	fi
fi
# No armed line at all (bootstrap disabled, or logs rotated after a recreate):
# fall back to a credential check so a configured stack still reads "already set
# up" instead of implying setup is still needed. A token-id UUID in the list means
# at least one credential exists.
if [ "$BOOTSTRAP_STATE" = "unknown" ] \
	&& "${DC[@]}" exec -T axon-server axon token list 2>/dev/null \
		| grep -qE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'; then
	BOOTSTRAP_STATE="done"
fi

# --- optional: mint a TUI token + write its config --------------------------
TOKEN=""
TUI_CFG=""
if [ "$WRITE_TUI" -eq 1 ]; then
	if [ "$BOOTSTRAP_STATE" = "pending" ]; then
		warn "The web bootstrap is still armed. Minting a TUI token now will consume it"
		warn "(the first credential is what the web bootstrap creates). Open the web"
		warn "bootstrap URL first if you want the zero-paste web sign-in."
	fi
	info "Minting a bearer token for the TUI…"
	TOKEN="$("${DC[@]}" exec -T axon-server axon token issue --label tui \
		| tr -d '\r' | grep -E '^[A-Za-z0-9._~+/=-]{20,}$' | tail -n1 || true)"
	[ -n "$TOKEN" ] || die "Could not obtain a token from 'axon token issue'."

	# Resolve the axon-tui config path (matches clients/tui/src/config.rs):
	# $XDG_CONFIG_HOME/axon-tui/config.toml, else $HOME/.config/axon-tui/config.toml.
	if [ -n "${XDG_CONFIG_HOME:-}" ]; then
		TUI_CFG="$XDG_CONFIG_HOME/axon-tui/config.toml"
	elif [ -n "${HOME:-}" ]; then
		TUI_CFG="$HOME/.config/axon-tui/config.toml"
	else
		die "Cannot determine a config location (no HOME). Set XDG_CONFIG_HOME or HOME."
	fi

	# Never clobber an existing config without consent.
	if [ -e "$TUI_CFG" ]; then
		warn "An axon-tui config already exists at:"
		echo "    $TUI_CFG"
		if [ "$ASSUME_YES" -ne 1 ]; then
			if [ ! -t 0 ]; then
				die "Refusing to overwrite it without a terminal to confirm. Re-run with --yes to overwrite."
			fi
			printf 'Overwrite it? The current file will be backed up first. [y/N] '
			read -r reply
			case "$reply" in
				y|Y|yes|YES) ;;
				*) die "Aborted at your request; existing config left untouched." ;;
			esac
		fi
		BACKUP="${TUI_CFG}.bak.$(date +%Y%m%d%H%M%S)"
		cp -p "$TUI_CFG" "$BACKUP"
		info "Backed up the old config to: $BACKUP"
	fi

	# Write the minimal config (0600 — it carries a bearer token).
	mkdir -p "$(dirname "$TUI_CFG")"
	(
		umask 077
		cat >"$TUI_CFG" <<EOF
# Written by deploy/run-docker.sh for the Axon Docker stack.
[server]
base_url = "$BASE_URL"
bearer_token = "$TOKEN"
EOF
	)
	chmod 600 "$TUI_CFG" 2>/dev/null || true
	info "Wrote axon-tui config to: $TUI_CFG"
fi

# --- next steps -------------------------------------------------------------
echo
printf '\033[1;32mAxon is running.\033[0m\n\n'

if [ "$BOOTSTRAP_STATE" = "pending" ]; then
	cat <<EOF
Web client — finish setup in your browser (no copy/paste):

  Open:  $BOOTSTRAP_URL

That one-time page mints your first credential and signs the web client in.
(The URL is unguessable and works once; it disappears after first use.)

EOF
elif [ "$BOOTSTRAP_STATE" = "done" ]; then
	cat <<EOF
Web client:  already set up — open $BASE_URL and sign in.
(The one-time web bootstrap has been used; that URL is closed. Your browser keeps
you signed in; from another browser, paste a token from
'${DC[*]} exec axon-server axon token issue'.)

EOF
else
	cat <<EOF
Web client:  open $BASE_URL in a browser.
(The one-time web bootstrap isn't armed — it's disabled, or a credential already
exists. Sign in by pasting a token: '${DC[*]} exec axon-server axon token issue'.)

EOF
fi

cat <<EOF
  • Web client / API:   $BASE_URL
  • Health check:       curl -fsS $BASE_URL/healthz
EOF

if [ -n "$TUI_CFG" ]; then
	cat <<EOF
  • TUI config (token filled): $TUI_CFG

Download the axon-tui client for your platform and run it — it picks up the config
above automatically:

  $TUI_RELEASES_URL
EOF
else
	cat <<EOF

Want the terminal client too? Do the web setup first, then:  ./run-docker.sh --tui
EOF
fi

cat <<EOF

Manage tokens any time:
  ${DC[*]} exec axon-server axon token list
  ${DC[*]} exec axon-server axon token revoke <id>

Stop / start the stack:
  ${DC[*]} stop
  ${DC[*]} up -d
EOF
