#!/usr/bin/env sh
# Start axon-server: check prerequisites, configure environment, run with Docker,
# and tear down Docker containers on exit.

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd) || exit 1
env_file="$script_dir/.env"
env_example="$script_dir/.env.example"

compose() {
	docker compose --project-directory "$script_dir" \
		-f "$script_dir/docker-compose.yml" "$@"
}

# --- Determine target early ---

case "${1:-server}" in
server) _pkg="axon-server" ;;
tui) _pkg="axon-tui" ;;
clean) _pkg="clean" ;;
*)
	echo "Error: unknown target '$1'. Valid targets: server (default), tui, clean."
	exit 1
	;;
esac

# --- Prerequisites ---

need_cmd() {
	if ! command -v "$1" >/dev/null 2>&1; then
		echo "Error: '$1' is not installed or not in PATH."
		echo ""
		echo "$2"
		exit 1
	fi
}

if [ "$_pkg" != "clean" ]; then
	need_cmd cargo "Install Rust (includes cargo):
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  Or visit: https://www.rust-lang.org/tools/install"
fi

if [ "$_pkg" = "axon-server" ] || [ "$_pkg" = "clean" ]; then
	if [ "$(uname -s)" = "Darwin" ]; then
		tip="brew install --cask docker
Or visit: https://docs.docker.com/desktop/mac/install/
		  
You will need to start Docker Desktop from Applications after installing, and keep it running while using axon-server."
	else
		tip="sudo apt install docker.io docker-compose-v2"
	fi
	need_cmd docker "Install Docker: ${tip}"

	if ! docker info >/dev/null 2>&1; then
		echo "Error: Docker is installed but the daemon is not running."
		echo ""
		if [ "$(uname -s)" = "Darwin" ]; then
			echo "Open Docker Desktop from Applications and try again."
		else
			echo "Start the Docker daemon and try again:"
			echo "  sudo systemctl start docker"
		fi
		exit 1
	fi
fi

# --- Load .env if present ---

if [ -f "$env_file" ]; then
	_fixed="$(mktemp)"
	tr -d '\r' <"$env_file" >"$_fixed"
	if ! cmp -s "$_fixed" "$env_file"; then
		echo "Note: .env has Windows-style line endings (CRLF); converting to Unix (LF)."
		mv "$_fixed" "$env_file"
	else
		rm -f "$_fixed"
	fi

	# Match dotenv precedence: values already exported by the caller win.
	while IFS= read -r line || [ -n "$line" ]; do
		line=$(printf '%s\n' "$line" | sed 's/^[[:space:]]*//')
		case "$line" in
		'' | '#'*) continue ;;
		export\ *) line=${line#export } ;;
		esac

		name=${line%%=*}
		case "$name" in
		'' | [0-9]* | *[!A-Za-z0-9_]*) continue ;;
		esac

		eval "current_value=\${$name-}"
		if [ -z "$current_value" ]; then
			value=${line#*=}
			export "$name=$value"
		fi
	done <"$env_file"
fi

# --- Offer guided configuration when no other database config exists ---

config_file=${AXON_CONFIG:-"$script_dir/axon.toml"}
if [ "$_pkg" = "axon-server" ] && [ ! -f "$env_file" ] && [ -f "$env_example" ] &&
	[ -z "${DATABASE_URL:-}" ] && [ -z "${AXON_DATABASE__URL:-}" ] &&
	[ ! -f "$config_file" ]; then
	printf "No database configuration found. Create .env from .env.example now? [y/N] "
	read -r answer
	case "$answer" in
	[yY] | [yY][eE][sS])
		if command -v openssl >/dev/null 2>&1; then
			store_key="$(openssl rand -hex 32)"
			pg_pass="$(openssl rand -hex 16)"
		else
			store_key="$(od -vN 32 -An -tx1 /dev/urandom | tr -d ' \n')"
			pg_pass="$(od -vN 16 -An -tx1 /dev/urandom | tr -d ' \n')"
		fi
		sed \
			-e "s/AXON_SYNC__STORE_KEY=change-me/AXON_SYNC__STORE_KEY=$store_key/" \
			-e "s/POSTGRES_PASSWORD=.*/POSTGRES_PASSWORD=$pg_pass/" \
			-e "s|DATABASE_URL=postgres://\([^:]*\):[^@]*@|DATABASE_URL=postgres://\1:${pg_pass}@|" \
			"$env_example" >"$env_file"
		echo "Created .env with generated database and store keys."
		echo "Opening in ${EDITOR:-vi} to complete configuration. Save and close when done."
		"${EDITOR:-vi}" "$env_file"
		exec "$0" "$@"
		;;
	*)
		echo "Aborted. Configure the database in .env, axon.toml, or the environment, then re-run."
		exit 1
		;;
	esac
fi

# --- Run ---

if [ "$_pkg" = "clean" ]; then
	echo "Warning: this will permanently destroy all Postgres data."
	printf "Continue? [y/N] "
	read -r answer
	case "$answer" in
	[yY] | [yY][eE][sS])
		compose down -v
		exit $?
		;;
	*)
		echo "Aborted."
		exit 1
		;;
	esac
fi

if [ "$_pkg" = "axon-server" ]; then
	trap 'compose down' EXIT

	if ! compose up -d --wait postgres; then
		echo "Error: Docker Compose could not start the Postgres service."
		echo "Review the Docker output above; no database reset was attempted."
		exit 1
	fi

	postgres_user=${POSTGRES_USER:-axon}
	postgres_password=${POSTGRES_PASSWORD:-axon}
	postgres_db=${POSTGRES_DB:-axon}
	postgres_port=${POSTGRES_PORT:-5432}

	pg_check() {
		docker run --rm --network host \
			-e "PGPASSWORD=$postgres_password" postgres:16 \
			psql -h 127.0.0.1 -p "$postgres_port" \
			-U "$postgres_user" -d "$postgres_db" -c "SELECT 1" 2>&1
	}

	if ! postgres_error=$(pg_check); then
		case "$postgres_error" in
		*"password authentication failed"* | *"role "*" does not exist"* | *"database "*" does not exist"*) ;;
		*)
			echo "Error: could not run the Postgres credential check:"
			printf '%s\n' "$postgres_error"
			echo "No database reset was attempted."
			exit 1
			;;
		esac

		echo "Error: could not connect to the Compose Postgres service with its configured credentials."
		echo "The database volume was likely initialized with a different password."
		echo ""
		printf "Reset the database now? This destroys all existing data. [y/N] "
		read -r reset_db
		case "$reset_db" in
		[yY] | [yY][eE][sS])
			if ! compose down -v; then
				echo "Error: Docker Compose could not remove the existing database volume."
				exit 1
			fi
			if ! compose up -d --wait postgres; then
				echo "Error: Docker Compose could not restart Postgres after the reset."
				exit 1
			fi
			if ! postgres_error=$(pg_check); then
				echo "Error: Postgres still rejects the configured credentials after the reset."
				printf '%s\n' "$postgres_error"
				exit 1
			fi
			;;
		*)
			echo "Aborting. Update the Compose Postgres credentials to match the existing"
			echo "database, or run 'docker compose down -v' from the project directory to reset."
			exit 1
			;;
		esac
	fi
fi

if [ -z "${AXON_CONFIG+x}" ] && [ -f "$script_dir/axon.toml" ]; then
	AXON_CONFIG="$script_dir/axon.toml"
	export AXON_CONFIG
fi

# Shift off the consumed target arg so $@ contains only extra args for the binary.
[ $# -gt 0 ] && shift
cargo run --manifest-path "$script_dir/Cargo.toml" -p "$_pkg" -- "$@"
