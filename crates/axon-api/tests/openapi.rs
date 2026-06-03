//! Golden-file test for the emitted OpenAPI spec.
//!
//! The spec is the source of truth for generated clients, so handler/spec drift
//! must be a failing test rather than a silent mismatch. This test serializes
//! the utoipa-built document and compares it to the checked-in
//! `openapi/openapi.json`. It needs no database.
//!
//! Regenerate after an intentional API change:
//!
//! ```sh
//! UPDATE_OPENAPI=1 cargo test -p axon-api --test openapi
//! ```

use std::path::PathBuf;

use axon_api::ApiDoc;
use utoipa::OpenApi;

fn golden_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/axon-api; the spec lives at the repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../openapi/openapi.json")
}

#[test]
fn openapi_spec_is_current() {
    let spec = ApiDoc::openapi()
        .to_pretty_json()
        .expect("serialize OpenAPI spec");
    let spec = format!("{spec}\n"); // trailing newline so the file is POSIX-clean
    let path = golden_path();

    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create openapi dir");
        std::fs::write(&path, &spec).expect("write golden spec");
        return;
    }

    let golden = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing {}; generate it with `UPDATE_OPENAPI=1 cargo test -p axon-api --test openapi`",
            path.display()
        )
    });
    assert_eq!(
        spec, golden,
        "OpenAPI spec drift — regenerate with `UPDATE_OPENAPI=1 cargo test -p axon-api --test openapi`"
    );
}
