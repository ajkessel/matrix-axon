//! Typed configuration loader.
//!
//! Configuration is layered, lowest precedence first:
//!
//! 1. Struct defaults (`#[serde(default)]`) — everything except `database.url`.
//! 2. A TOML file, if one is found (see [`Config::load_default`]).
//! 3. The bare `DATABASE_URL` environment variable, mapped onto `database.url`
//!    (so the project's existing `.env` / sqlx tooling works unchanged).
//! 4. `AXON_`-prefixed environment variables, nested with `__`
//!    (e.g. `AXON_SERVER__PORT=9090` sets `server.port`).
//!
//! Later layers override earlier ones.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::Deserialize;

use crate::error::ConfigError;

/// Top-level Axon configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// HTTP server bind settings.
    #[serde(default)]
    pub server: ServerConfig,
    /// Postgres connection settings. `database.url` is required.
    pub database: DatabaseConfig,
    /// Logging settings.
    #[serde(default)]
    pub log: LogConfig,
    /// Sync-engine settings: where the matrix-rust-sdk stores live and which
    /// account (if any) to provision on boot.
    #[serde(default)]
    pub sync: SyncConfig,
    /// Full-text search settings: whether the Tantivy index is enabled and
    /// where it lives.
    #[serde(default)]
    pub search: SearchConfig,
}

/// HTTP server bind settings.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// Address to bind. Defaults to `127.0.0.1` (loopback, safe by default;
    /// put a reverse proxy in front for external access).
    #[serde(default = "default_host")]
    pub host: IpAddr,
    /// Port to bind. Defaults to `8080`.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Permit binding a non-loopback address while Axon serves plain HTTP.
    ///
    /// Axon terminates no TLS itself, and the `/v1/` API carries credentials
    /// (login passwords, recovery keys, bearer tokens). The tech spec requires
    /// client↔Axon TLS, so the safe deployment is to bind loopback and front
    /// Axon with a TLS-terminating reverse proxy (or a private mesh VPN). The
    /// server therefore **refuses** a non-loopback bind unless this is set —
    /// an explicit, auditable "I accept cleartext on the wire" override for
    /// trusted-network or testing scenarios. Defaults to `false`.
    #[serde(default)]
    pub allow_insecure_bind: bool,
}

/// Postgres connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    /// Postgres connection URL, e.g. `postgres://axon:axon@localhost:5432/axon`.
    /// Required — supplied via `DATABASE_URL`, `AXON_DATABASE__URL`, or the
    /// `[database].url` key in the TOML file.
    pub url: String,
    /// Maximum size of the connection pool. Defaults to `5`.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

/// Logging settings.
#[derive(Debug, Clone, Deserialize)]
pub struct LogConfig {
    /// `tracing` filter directive, e.g. `info` or `axon=debug,sqlx=warn`.
    /// Overridden by the `RUST_LOG` environment variable when set.
    #[serde(default = "default_log_level")]
    pub level: String,
}

/// Sync-engine settings.
#[derive(Debug, Clone, Deserialize)]
pub struct SyncConfig {
    /// Directory under which the matrix-rust-sdk state + crypto stores live,
    /// one subdirectory per account. Must be durable — losing it forces a
    /// re-login and the loss of historical Megolm sessions.
    #[serde(default = "default_sync_data_dir")]
    pub data_dir: PathBuf,
    /// Symmetric key used to encrypt the access token at rest (pgcrypto
    /// `pgp_sym_encrypt`) and to passphrase the SDK's SQLite store. Required
    /// once an account is provisioned; validated at provision time so the error
    /// message is human-readable.
    #[serde(default)]
    pub store_key: Option<String>,
    /// The account to provision on boot, if any. Absent means "run with no
    /// accounts" (the binary still boots and serves HTTP).
    #[serde(default)]
    pub account: Option<AccountProvision>,
    /// Per-room timeline window the sliding-sync list requests (`n`). The SDK
    /// default is `1` (latest event only); we raise it so each room archives its
    /// last N events, giving the timeline read real depth to paginate. Since M10
    /// this is only a cold-start latency knob — the shallow window a room shows
    /// the instant it syncs — not the history bound: the backfill engine extends
    /// each room back toward its start in the background (ADR 0015, ADR 0043).
    /// Defaults to 20.
    #[serde(default = "default_timeline_limit")]
    pub timeline_limit: u32,
    /// Capacity of the live-event broadcast bus that feeds the `/v1/ws`
    /// WebSocket: the number of recent events the bus retains for a connected
    /// client before, if that client can't keep up, the oldest are dropped and
    /// it is told it lagged. Larger values tolerate slower/burstier clients at
    /// the cost of a bigger in-memory ring buffer; it never back-pressures sync.
    /// Defaults to 1024.
    #[serde(default = "default_live_event_buffer")]
    pub live_event_buffer: usize,
    /// Enable the M10 history-backfill engine: a continuous, throttled background
    /// task that pages each joined room's pre-existing history backward through
    /// the same ingestion path as live sync (ADR 0043). Defaults to `true`.
    #[serde(default = "default_backfill_enabled")]
    pub backfill_enabled: bool,
    /// Events requested per `/messages` page during backfill. Larger pages
    /// backfill faster but hold the connection longer per request. Defaults to 100.
    #[serde(default = "default_backfill_page_size")]
    pub backfill_page_size: u32,
    /// Per-room bound on how many events backfill pulls before stopping (without
    /// marking the room complete, so raising the cap later resumes it). `0` means
    /// "to room start" — unbounded — which is safe because backfill is continuous
    /// and throttled. Defaults to 0.
    #[serde(default)]
    pub backfill_target_depth: u64,
    /// Delay between backfill pages, in milliseconds — the throttle that keeps
    /// backfill from starving live sync. Defaults to 1000.
    #[serde(default = "default_backfill_throttle_ms")]
    pub backfill_throttle_ms: u64,
    /// Per-request timeout for a backfill `/messages` page, in seconds. Bounds a
    /// hung homeserver request so it can't stall account teardown / server
    /// shutdown (the drain awaits the backfill task). Defaults to 30.
    #[serde(default = "default_backfill_page_timeout_secs")]
    pub backfill_page_timeout_secs: u64,
    /// When every joined room is complete or capped, how long backfill idles
    /// before re-polling the room list, in seconds. This is how a newly joined or
    /// re-joined room is picked up without a restart. Defaults to 45.
    #[serde(default = "default_backfill_idle_poll_secs")]
    pub backfill_idle_poll_secs: u64,
    /// Filesystem the backfill disk-space valve watches (via `statvfs`). Defaults
    /// to the sync `data_dir`. **Backfill's primary growth is the Postgres
    /// `events` table**, whose free space axon cannot measure when Postgres is on
    /// another host or a separate volume — point this at the filesystem holding
    /// the Postgres data directory (or the search index) when it differs from
    /// `data_dir`, or monitor that disk separately. On the common single-host /
    /// single-volume deploy the default already reflects Postgres's free space.
    #[serde(default)]
    pub backfill_disk_guard_path: Option<PathBuf>,
    /// Backfill pauses while free space on the guarded filesystem
    /// (`backfill_disk_guard_path`) is below this many bytes — a safety valve so
    /// unbounded backfill can't fill the disk. Live sync is unaffected. Defaults
    /// to 2 GiB.
    #[serde(default = "default_backfill_min_free_bytes")]
    pub backfill_min_free_bytes: u64,
    /// Backfill also pauses when free space is below this percentage of the
    /// filesystem (checked alongside `backfill_min_free_bytes`; either triggers a
    /// pause). Defaults to 5.0.
    #[serde(default = "default_backfill_min_free_percent")]
    pub backfill_min_free_percent: f64,
    /// When `true`, leaving (or being banned from) a room destructively purges its
    /// stored events, state, and search documents (ADR 0044). When `false` (the
    /// default), left rooms are retained but hidden from search by the membership
    /// filter; re-joining restores them.
    #[serde(default)]
    pub purge_on_leave: bool,
}

/// Full-text search (Tantivy) settings.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchConfig {
    /// Whether the search index is built and served. When `false`, the search
    /// indexer and the `GET /v1/search` query surface are disabled (the endpoint
    /// returns `503`); store writes still append `search_outbox` obligations, so
    /// re-enabling search catches up the work it missed. Defaults to `true`.
    #[serde(default = "default_search_enabled")]
    pub enabled: bool,
    /// Directory holding the Tantivy index. Must be durable; lives on the same
    /// disk as Postgres and inherits the operator's filesystem-level encryption
    /// (the index holds decrypted message text — see the tech spec). Defaults to
    /// `axon-data/search`.
    #[serde(default = "default_search_index_path")]
    pub index_path: PathBuf,
    /// Rows streamed per batch by the indexer — both the corpus seed and the
    /// outbox drain. Bounds the indexer's working set and its share of the
    /// Postgres connection pool. Defaults to 1000.
    #[serde(default = "default_search_index_batch_size")]
    pub index_batch_size: i64,
    /// Delay between corpus-seed batches, in milliseconds. A throttle so a large
    /// from-scratch seed never starves live sync or API reads. Defaults to 0 (no
    /// delay).
    #[serde(default)]
    pub build_throttle_ms: u64,
    /// Heap budget for the single Tantivy `IndexWriter`, in megabytes. Bounds the
    /// indexer's memory. Defaults to 50.
    #[serde(default = "default_search_writer_heap_mb")]
    pub writer_heap_mb: usize,
    /// Maximum number of `GET /v1/search` queries executed concurrently. Each
    /// query runs on a blocking thread and keeps a Tantivy reader busy, so this
    /// caps how much of the blocking pool authenticated searches can occupy;
    /// requests over the limit queue rather than being rejected. Defaults to 8.
    #[serde(default = "default_search_max_concurrent_queries")]
    pub max_concurrent_queries: usize,
    /// Per-query wall-clock budget for `GET /v1/search`, in milliseconds. A query
    /// exceeding it returns `503` (the offset cap already bounds per-query work;
    /// this is a latency backstop). Defaults to 10000 (10s).
    #[serde(default = "default_search_query_timeout_ms")]
    pub query_timeout_ms: u64,
}

/// Provisioning details for a single Matrix account.
///
/// Exactly one of [`password`](Self::password) or
/// [`access_token`](Self::access_token) must be supplied; this is validated at
/// provision time via [`credential`](Self::credential). The password is
/// consumed once at first login and never persisted — only the resulting token
/// is stored (encrypted).
#[derive(Debug, Clone, Deserialize)]
pub struct AccountProvision {
    /// Full Matrix user ID, e.g. `@alice:example.org`.
    pub user_id: String,
    /// Homeserver base URL, e.g. `https://matrix.example.org`.
    pub homeserver_url: String,
    /// Password for first-login. Mutually exclusive with `access_token`.
    #[serde(default)]
    pub password: Option<String>,
    /// Pre-provisioned access token. Mutually exclusive with `password`.
    #[serde(default)]
    pub access_token: Option<String>,
    /// Device ID to associate with a pre-provisioned `access_token`. Ignored
    /// for password login (the homeserver assigns one).
    #[serde(default)]
    pub device_id: Option<String>,
    /// Secure Backup / 4S recovery key, used once on boot to import the megolm
    /// key backup and cross-signing keys so a fresh `axon` device can decrypt
    /// historical messages (the re-decryption queue's driver). Like `password`,
    /// it is consumed in-memory and **never persisted** — durable, encrypted-at-
    /// rest recovery-key storage is deferred future work (ADR 0011, ADR 0014). It
    /// is not part of [`Credential`]; it seeds key recovery, not login.
    #[serde(default)]
    pub recovery_key: Option<String>,
}

/// A validated login credential — exactly one of the two mutually-exclusive
/// options on [`AccountProvision`].
#[derive(Debug, Clone, Copy)]
pub enum Credential<'a> {
    /// Password login (first boot); homeserver issues a fresh token + device.
    Password(&'a str),
    /// Restore from a pre-provisioned access token (+ optional device ID).
    Token {
        /// The access token.
        token: &'a str,
        /// The device ID it belongs to, if known.
        device_id: Option<&'a str>,
    },
}

impl AccountProvision {
    /// Resolve the single valid credential, erroring if zero or both of
    /// `password` / `access_token` are set.
    pub fn credential(&self) -> Result<Credential<'_>, ConfigError> {
        match (self.password.as_deref(), self.access_token.as_deref()) {
            (Some(pw), None) => Ok(Credential::Password(pw)),
            (None, Some(token)) => Ok(Credential::Token {
                token,
                device_id: self.device_id.as_deref(),
            }),
            (Some(_), Some(_)) => Err(ConfigError::Validation(format!(
                "sync.account for {}: provide exactly one of `password` or `access_token`, not both",
                self.user_id
            ))),
            (None, None) => Err(ConfigError::Validation(format!(
                "sync.account for {}: one of `password` or `access_token` is required",
                self.user_id
            ))),
        }
    }
}

fn default_host() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

fn default_port() -> u16 {
    8080
}

fn default_max_connections() -> u32 {
    5
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_sync_data_dir() -> PathBuf {
    PathBuf::from("axon-data/sync")
}

fn default_timeline_limit() -> u32 {
    20
}

fn default_live_event_buffer() -> usize {
    1024
}

fn default_backfill_enabled() -> bool {
    true
}

fn default_backfill_page_size() -> u32 {
    100
}

fn default_backfill_throttle_ms() -> u64 {
    1000
}

fn default_backfill_page_timeout_secs() -> u64 {
    30
}

fn default_backfill_idle_poll_secs() -> u64 {
    45
}

fn default_backfill_min_free_bytes() -> u64 {
    2 * 1024 * 1024 * 1024
}

fn default_backfill_min_free_percent() -> f64 {
    5.0
}

fn default_search_enabled() -> bool {
    true
}

fn default_search_index_path() -> PathBuf {
    PathBuf::from("axon-data/search")
}

fn default_search_index_batch_size() -> i64 {
    1000
}

fn default_search_writer_heap_mb() -> usize {
    50
}

fn default_search_max_concurrent_queries() -> usize {
    8
}

fn default_search_query_timeout_ms() -> u64 {
    10_000
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            allow_insecure_bind: false,
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            data_dir: default_sync_data_dir(),
            store_key: None,
            account: None,
            timeline_limit: default_timeline_limit(),
            live_event_buffer: default_live_event_buffer(),
            backfill_enabled: default_backfill_enabled(),
            backfill_page_size: default_backfill_page_size(),
            backfill_target_depth: 0,
            backfill_throttle_ms: default_backfill_throttle_ms(),
            backfill_page_timeout_secs: default_backfill_page_timeout_secs(),
            backfill_idle_poll_secs: default_backfill_idle_poll_secs(),
            backfill_disk_guard_path: None,
            backfill_min_free_bytes: default_backfill_min_free_bytes(),
            backfill_min_free_percent: default_backfill_min_free_percent(),
            purge_on_leave: false,
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: default_search_enabled(),
            index_path: default_search_index_path(),
            index_batch_size: default_search_index_batch_size(),
            build_throttle_ms: 0,
            writer_heap_mb: default_search_writer_heap_mb(),
            max_concurrent_queries: default_search_max_concurrent_queries(),
            query_timeout_ms: default_search_query_timeout_ms(),
        }
    }
}

impl Config {
    /// Load configuration from an optional TOML file plus environment overrides.
    ///
    /// Passing `None` skips the file layer and relies on environment variables
    /// and struct defaults — the common case for container deployments.
    pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
        let mut figment = Figment::new();

        if let Some(path) = path {
            figment = figment.merge(Toml::file(path));
        }

        figment
            // Map the bare `DATABASE_URL` onto `database.url`.
            .merge(
                Env::raw()
                    .map(|key| {
                        if key == "DATABASE_URL" {
                            "database.url".into()
                        } else {
                            key.into()
                        }
                    })
                    .only(&["database.url"]),
            )
            // `AXON_`-prefixed vars take precedence; `__` denotes nesting.
            .merge(Env::prefixed("AXON_").split("__"))
            .extract()
            .map_err(|err| ConfigError::Figment(Box::new(err)))
    }

    /// Resolve a config file path, then [`load`](Config::load).
    ///
    /// The file is taken from the `AXON_CONFIG` environment variable if set,
    /// otherwise `./axon.toml` if it exists, otherwise no file layer is used.
    pub fn load_default() -> Result<Config, ConfigError> {
        let path = Self::resolve_path();
        Config::load(path.as_deref())
    }

    fn resolve_path() -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var("AXON_CONFIG") {
            return Some(PathBuf::from(explicit));
        }
        let default = PathBuf::from("axon.toml");
        default.exists().then_some(default)
    }

    /// The socket address to bind, derived from `server.host` and `server.port`.
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.server.host, self.server.port)
    }
}

#[cfg(test)]
// figment's `Jail::expect_with` dictates a closure returning `figment::Error`,
// which is large; we cannot box it here.
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_with_only_database_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DATABASE_URL", "postgres://u:p@localhost/db");
            let config = Config::load(None).expect("load");
            assert_eq!(config.server.host, IpAddr::V4(Ipv4Addr::LOCALHOST));
            assert_eq!(config.server.port, 8080);
            assert_eq!(config.database.max_connections, 5);
            assert_eq!(config.log.level, "info");
            assert_eq!(config.database.url, "postgres://u:p@localhost/db");
            Ok(())
        });
    }

    #[test]
    fn allow_insecure_bind_defaults_false_and_parses_from_env() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DATABASE_URL", "postgres://u:p@localhost/db");
            assert!(!Config::load(None).expect("load").server.allow_insecure_bind);

            jail.set_env("AXON_SERVER__ALLOW_INSECURE_BIND", "true");
            assert!(Config::load(None).expect("load").server.allow_insecure_bind);
            Ok(())
        });
    }

    #[test]
    fn missing_database_url_is_an_error() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            assert!(Config::load(None).is_err());
            Ok(())
        });
    }

    #[test]
    fn env_overrides_file_and_defaults() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "axon.toml",
                r#"
                    [server]
                    port = 7000
                    [database]
                    url = "postgres://file@localhost/db"
                "#,
            )?;
            jail.set_env("AXON_SERVER__PORT", "9090");
            let config = Config::load(Some(Path::new("axon.toml"))).expect("load");
            // env beats file
            assert_eq!(config.server.port, 9090);
            // file value used where env absent
            assert_eq!(config.database.url, "postgres://file@localhost/db");
            Ok(())
        });
    }

    #[test]
    fn socket_addr_combines_host_and_port() {
        let config = Config {
            server: ServerConfig {
                host: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                port: 1234,
                allow_insecure_bind: false,
            },
            database: DatabaseConfig {
                url: "x".into(),
                max_connections: 5,
            },
            log: LogConfig::default(),
            sync: SyncConfig::default(),
            search: SearchConfig::default(),
        };
        assert_eq!(config.socket_addr().to_string(), "0.0.0.0:1234");
    }

    #[test]
    fn search_defaults_when_absent() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DATABASE_URL", "postgres://u:p@localhost/db");
            let config = Config::load(None).expect("load");
            assert!(config.search.enabled);
            assert_eq!(config.search.index_path, PathBuf::from("axon-data/search"));
            assert_eq!(config.search.index_batch_size, 1000);
            assert_eq!(config.search.build_throttle_ms, 0);
            assert_eq!(config.search.writer_heap_mb, 50);
            Ok(())
        });
    }

    #[test]
    fn search_can_be_disabled_via_env() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DATABASE_URL", "postgres://u:p@localhost/db");
            jail.set_env("AXON_SEARCH__ENABLED", "false");
            assert!(!Config::load(None).expect("load").search.enabled);
            Ok(())
        });
    }

    #[test]
    fn sync_defaults_when_absent() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DATABASE_URL", "postgres://u:p@localhost/db");
            let config = Config::load(None).expect("load");
            assert_eq!(config.sync.data_dir, PathBuf::from("axon-data/sync"));
            assert!(config.sync.store_key.is_none());
            assert!(config.sync.account.is_none());
            Ok(())
        });
    }

    #[test]
    fn sync_account_loads_from_file() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "axon.toml",
                r#"
                    [database]
                    url = "postgres://file@localhost/db"
                    [sync]
                    store_key = "s3cret"
                    [sync.account]
                    user_id = "@alice:example.org"
                    homeserver_url = "https://matrix.example.org"
                    password = "hunter2"
                "#,
            )?;
            let config = Config::load(Some(Path::new("axon.toml"))).expect("load");
            let account = config.sync.account.expect("account");
            assert_eq!(account.user_id, "@alice:example.org");
            assert!(matches!(
                account.credential(),
                Ok(Credential::Password("hunter2"))
            ));
            Ok(())
        });
    }

    #[test]
    fn credential_requires_exactly_one() {
        let neither = AccountProvision {
            user_id: "@a:b".into(),
            homeserver_url: "https://b".into(),
            password: None,
            access_token: None,
            device_id: None,
            recovery_key: None,
        };
        assert!(neither.credential().is_err());

        let both = AccountProvision {
            password: Some("p".into()),
            access_token: Some("t".into()),
            ..neither.clone()
        };
        assert!(both.credential().is_err());

        let token_only = AccountProvision {
            access_token: Some("t".into()),
            device_id: Some("DEV".into()),
            ..neither
        };
        assert!(matches!(
            token_only.credential(),
            Ok(Credential::Token {
                token: "t",
                device_id: Some("DEV")
            })
        ));
    }
}
