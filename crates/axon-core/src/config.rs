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

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
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
    fn missing_database_url_is_an_error() {
        figment::Jail::expect_with(|_jail| {
            assert!(Config::load(None).is_err());
            Ok(())
        });
    }

    #[test]
    fn env_overrides_file_and_defaults() {
        figment::Jail::expect_with(|jail| {
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
            },
            database: DatabaseConfig {
                url: "x".into(),
                max_connections: 5,
            },
            log: LogConfig::default(),
            sync: SyncConfig::default(),
        };
        assert_eq!(config.socket_addr().to_string(), "0.0.0.0:1234");
    }

    #[test]
    fn sync_defaults_when_absent() {
        figment::Jail::expect_with(|jail| {
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
