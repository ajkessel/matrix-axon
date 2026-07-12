//! Compile-time-embedded database migrations.
//!
//! We can't use sqlx's `migrate!` macro: it's provided by the `sqlx` umbrella
//! crate, which unconditionally drags `sqlx-sqlite` into the dependency graph,
//! and that collides with matrix-sdk's rusqlite over the `sqlite3` system
//! library (see `axon-store/Cargo.toml`). Instead we embed the `migrations/`
//! directory with `include_dir` and build a [`Migrator`] by hand.
//!
//! The filename convention matches sqlx's own (`<version>_<description>.sql`,
//! integer version prefix) and we compute the checksum the same way
//! ([`Migration::new`] hashes the SQL with SHA-384), so the `_sqlx_migrations`
//! bookkeeping table is identical to what the macro would have produced.

use std::borrow::Cow;

use include_dir::{include_dir, Dir};
use sqlx_core::migrate::{Migration, MigrationType, Migrator};

use crate::StoreError;

/// Metadata about one embedded migration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedMigration {
    /// sqlx migration version parsed from the filename prefix.
    pub version: i64,
    /// Human-readable description derived from the filename.
    pub description: String,
    /// sqlx-compatible SHA-384 checksum of the SQL contents.
    pub checksum: Vec<u8>,
}

/// The `migrations/` directory, embedded into the binary at compile time.
static MIGRATIONS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

/// Return the embedded migration manifest with the same checksum sqlx stores in
/// `_sqlx_migrations`.
pub fn embedded_migrations() -> Result<Vec<EmbeddedMigration>, StoreError> {
    let migrations = collect_migrations()?;
    Ok(migrations
        .iter()
        .map(|m| EmbeddedMigration {
            version: m.version,
            description: m.description.clone().into_owned(),
            checksum: m.checksum.clone().into_owned(),
        })
        .collect())
}

/// Build a [`Migrator`] from the embedded migration files, sorted by version.
pub(crate) fn embedded_migrator() -> Result<Migrator, StoreError> {
    Ok(Migrator {
        migrations: Cow::Owned(collect_migrations()?),
        ..Migrator::DEFAULT
    })
}

/// Read every embedded migration file and hash its SQL exactly once, producing
/// fully-formed [`Migration`]s (SQL, description, and checksum together) that
/// both [`embedded_migrations`] and [`embedded_migrator`] derive from.
fn collect_migrations() -> Result<Vec<Migration>, StoreError> {
    let mut migrations = Vec::new();

    for file in MIGRATIONS.files() {
        let name = file
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| StoreError::EmbeddedMigration("non-UTF-8 migration filename".into()))?;

        let Some((version, description)) = parse_migration_filename(name)? else {
            continue;
        };

        let sql = file.contents_utf8().ok_or_else(|| {
            StoreError::EmbeddedMigration(format!("migration {name:?} has non-UTF-8 contents"))
        })?;

        migrations.push(Migration::new(
            version,
            Cow::Owned(description),
            MigrationType::Simple,
            Cow::Owned(sql.to_owned()),
            // All our migrations run inside the per-migration transaction; none
            // need the `-- no-transaction` escape hatch yet.
            false,
        ));
    }

    migrations.sort_by_key(|m| m.version);
    Ok(migrations)
}

/// Parse a migration filename (e.g. `20260530120000_create_accounts.sql`) into
/// `(version, description)`. Returns `None` for non-`.sql` files (silently
/// skipped). Returns `Err` for `.sql` files that violate the naming convention.
fn parse_migration_filename(name: &str) -> Result<Option<(i64, String)>, StoreError> {
    let stem = match name.strip_suffix(".sql") {
        Some(s) => s,
        None => return Ok(None),
    };

    let (version_str, description) = stem.split_once('_').ok_or_else(|| {
        StoreError::EmbeddedMigration(format!(
            "migration {name:?} is missing a `<version>_<description>` separator"
        ))
    })?;

    let version: i64 = version_str.parse().map_err(|_| {
        StoreError::EmbeddedMigration(format!(
            "migration {name:?} has a non-integer version prefix {version_str:?}"
        ))
    })?;

    Ok(Some((version, description.replace('_', " "))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_migrations_parse_and_sort() {
        let migrator = embedded_migrator().expect("embedded migrations build");
        let versions: Vec<i64> = migrator.iter().map(|m| m.version).collect();
        assert!(
            versions.len() >= 2,
            "expected >= 2 migrations, got {versions:?}"
        );
        assert!(
            versions.windows(2).all(|w| w[0] < w[1]),
            "not sorted: {versions:?}"
        );
    }

    #[test]
    fn parse_filename_valid() {
        let result = parse_migration_filename("20260530120000_create_accounts.sql").unwrap();
        assert_eq!(
            result,
            Some((20260530120000, "create accounts".to_string()))
        );
    }

    #[test]
    fn parse_filename_multi_word_description() {
        let result = parse_migration_filename("001_foo_bar_baz.sql").unwrap();
        assert_eq!(result, Some((1, "foo bar baz".to_string())));
    }

    #[test]
    fn parse_filename_non_sql_skipped() {
        assert_eq!(parse_migration_filename("README").unwrap(), None);
        assert_eq!(parse_migration_filename("notes.txt").unwrap(), None);
    }

    #[test]
    fn parse_filename_missing_separator() {
        let err = parse_migration_filename("20260530nounderscore.sql").unwrap_err();
        assert!(matches!(err, StoreError::EmbeddedMigration(_)));
    }

    #[test]
    fn parse_filename_non_integer_version() {
        let err = parse_migration_filename("abc_something.sql").unwrap_err();
        assert!(matches!(err, StoreError::EmbeddedMigration(_)));
    }
}
