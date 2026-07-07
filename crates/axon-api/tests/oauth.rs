//! DB-gated OAuth integration test (M14b, ADR 0054): drives the full Path A
//! authorize -> callback -> token -> refresh chain, Path B's identity-token
//! grant (with replay rejection), the catch-all-precedence regression, and a
//! check that pre-existing CLI-minted tokens still verify unchanged.
//!
//! Needs a database and is `#[ignore]`d by default, same as `tests/http.rs`:
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-api --test oauth -- --ignored
//! ```

mod common;

use std::sync::Arc;

use axon_api::{AppState, OAuthRuntime, OidcProvider};
use axon_core::{OauthClientConfig, OauthConfig};
use axon_store::Store;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use common::oidc::TestOidcProvider;
use common::{StubLifecycle, StubMediaProxy, StubSender, StubTrust, StubVerification};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

async fn store() -> Store {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
    Store::connect(&url, 5).await.expect("connect + migrate")
}

const CLIENT_ID: &str = "test-client";
const REDIRECT_URI: &str = "axon://oauth/callback";
const TEST_PROVIDER: &str = "test";
const TEST_ISSUER: &str = "https://fake-idp.test/";
const TEST_AUDIENCE: &str = "test-upstream-client-id";

/// A real [`TokenVerifier`](axon_api::TokenVerifier) backed by `store` — unlike
/// most HTTP tests (which stub it out), this test needs the genuine DB-backed
/// verifier so a token minted through `/v1/oauth/token` actually round-trips
/// through `verify_token`'s new `expires_at` clause.
fn real_verifier(store: Store) -> Arc<dyn axon_api::TokenVerifier> {
    Arc::new(axon_api::StoreTokenVerifier::new(store))
}

/// Build the router with oauth enabled, one `TestOidcProvider` registered as
/// `"test"`, and one statically pre-registered client. Returns the router
/// plus the same `TestOidcProvider` instance (so the test can drive its
/// `issue_code`/`sign_identity_token` stand-ins for "the browser/native SDK
/// did its part").
fn app_with_oauth(store: Store) -> (axum::Router, Arc<TestOidcProvider>) {
    let provider = Arc::new(TestOidcProvider::new(
        TEST_PROVIDER,
        TEST_ISSUER,
        TEST_AUDIENCE,
    ));
    let mut providers: std::collections::HashMap<&'static str, Arc<dyn OidcProvider>> =
        std::collections::HashMap::new();
    providers.insert(TEST_PROVIDER, provider.clone() as Arc<dyn OidcProvider>);

    let oauth_config = OauthConfig {
        enabled: true,
        external_base_url: Some("http://axon.test".to_owned()),
        access_token_ttl_secs: 3600,
        refresh_token_ttl_secs: 2_592_000,
        clients: vec![OauthClientConfig {
            client_id: CLIENT_ID.to_owned(),
            redirect_uris: vec![REDIRECT_URI.to_owned()],
        }],
        providers: axon_core::OauthProvidersConfig::default(),
    };
    let runtime = Arc::new(OAuthRuntime::new(&oauth_config, providers));

    let (live, _rx) = tokio::sync::broadcast::channel(16);
    let app = axon_api::router(
        AppState::new(
            store.clone(),
            live,
            Arc::new(StubSender::ok("$unused:localhost")),
            Arc::new(StubLifecycle::ok(Uuid::nil())),
            Arc::new(StubVerification::ok("$unused-flow")),
            Arc::new(StubTrust::ok()),
            real_verifier(store),
            Arc::new(StubMediaProxy),
            None,
        )
        .with_oauth(runtime),
    );
    (app, provider)
}

/// PKCE S256: a random verifier and its matching challenge.
fn pkce_pair() -> (String, String) {
    let verifier = "test-pkce-verifier-0123456789-abcdefghijklmno";
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier.to_owned(), challenge)
}

fn query_param(url: &url::Url, name: &str) -> String {
    url.query_pairs()
        .find(|(k, _)| k == name)
        .unwrap_or_else(|| panic!("missing query param {name:?} in {url}"))
        .1
        .into_owned()
}

/// `/v1/oauth/*`'s rate-limit layer extracts `ConnectInfo<SocketAddr>`, which
/// a real deployment gets from `axum::serve`'s
/// `into_make_service_with_connect_info::<SocketAddr>()`. Driving the router
/// directly via `tower::oneshot` (as these tests do) bypasses that, so it must
/// be inserted into the request's extensions by hand — otherwise axum 500s
/// before the handler ever runs.
fn with_fake_connect_info(mut req: Request<Body>) -> Request<Body> {
    let addr: std::net::SocketAddr = "127.0.0.1:12345".parse().unwrap();
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo(addr));
    req
}

async fn get_no_body(app: &axum::Router, uri: &str) -> axum::response::Response {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    app.clone()
        .oneshot(with_fake_connect_info(req))
        .await
        .expect("request")
}

async fn post_form(app: &axum::Router, uri: &str, form: &[(&str, &str)]) -> (StatusCode, Value) {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(with_fake_connect_info(req))
        .await
        .expect("request");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let json = serde_json::from_slice(&bytes).expect("json body");
    (status, json)
}

async fn get_authed(app: &axum::Router, uri: &str, token: &str) -> StatusCode {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.expect("request").status()
}

#[tokio::test]
#[ignore]
async fn path_a_authorize_callback_token_refresh_chain() {
    let store = store().await;
    let (app, provider) = app_with_oauth(store.clone());

    let subject = format!("test-subject-{}", Uuid::new_v4());
    store
        .bind_identity(TEST_PROVIDER, &subject, Some("owner@example.com"))
        .await
        .expect("bind identity");

    let (verifier, challenge) = pkce_pair();
    let authorize_uri = format!(
        "/v1/oauth/authorize?response_type=code&client_id={CLIENT_ID}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&provider={TEST_PROVIDER}&state=client-state-abc",
        urlencoding_encode(REDIRECT_URI),
    );
    let resp = get_no_body(&app, &authorize_uri).await;
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "authorize must redirect upstream"
    );
    let location = resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .unwrap()
        .to_owned();
    let upstream_url = url::Url::parse(&location).expect("upstream redirect is a URL");
    let upstream_state = query_param(&upstream_url, "state");
    let upstream_nonce = query_param(&upstream_url, "nonce");

    // Stand-in for "the browser completed the upstream provider's login".
    let code = provider.issue_code(&subject, Some("owner@example.com"), &upstream_nonce);

    let callback_uri =
        format!("/v1/oauth/{TEST_PROVIDER}/callback?code={code}&state={upstream_state}");
    let resp = get_no_body(&app, &callback_uri).await;
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "callback must redirect back to the client's redirect_uri"
    );
    let location = resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .unwrap()
        .to_owned();
    let client_redirect = url::Url::parse(&location).expect("client redirect is a URL");
    assert_eq!(client_redirect.scheme(), "axon");
    assert_eq!(query_param(&client_redirect, "state"), "client-state-abc");
    let axon_code = query_param(&client_redirect, "code");

    let (status, body) = post_form(
        &app,
        "/v1/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", &axon_code),
            ("code_verifier", &verifier),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "token exchange failed: {body}");
    assert_eq!(body["token_type"], "Bearer");
    let access_token = body["access_token"]
        .as_str()
        .expect("access_token")
        .to_owned();
    let refresh_token = body["refresh_token"]
        .as_str()
        .expect("refresh_token")
        .to_owned();

    // The minted access token verifies on a real authed route — proves
    // `verify_token`'s new `expires_at` clause accepts a live OAuth token.
    let status = get_authed(&app, "/v1/accounts", &access_token).await;
    assert_eq!(status, StatusCode::OK);

    // Refresh rotation: redeeming the refresh token mints a new pair.
    let (status, body) = post_form(
        &app,
        "/v1/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "refresh failed: {body}");
    let new_access_token = body["access_token"]
        .as_str()
        .expect("access_token")
        .to_owned();
    assert_ne!(
        new_access_token, access_token,
        "refresh must mint a fresh access token"
    );

    // Reusing the now-rotated refresh token is rejected (reuse detection).
    let (status, body) = post_form(
        &app,
        "/v1/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
#[ignore]
async fn path_b_identity_token_grant_and_replay_rejection() {
    let store = store().await;
    let (app, provider) = app_with_oauth(store.clone());

    let subject = format!("test-subject-{}", Uuid::new_v4());
    store
        .bind_identity(TEST_PROVIDER, &subject, None)
        .await
        .expect("bind identity");

    let jti = format!("jti-path-b-{}", Uuid::new_v4());
    let identity_token = provider.sign_identity_token(&subject, None, None, Some(&jti));

    let (status, body) = post_form(
        &app,
        "/v1/oauth/token",
        &[
            ("grant_type", "urn:axon:identity_token"),
            ("provider", TEST_PROVIDER),
            ("identity_token", &identity_token),
            ("client_id", CLIENT_ID),
        ],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "identity token redemption failed: {body}"
    );
    assert!(body["access_token"].as_str().is_some());

    // The same identity token presented again is a replay — rejected even
    // though the signature/claims are still perfectly valid.
    let (status, body) = post_form(
        &app,
        "/v1/oauth/token",
        &[
            ("grant_type", "urn:axon:identity_token"),
            ("provider", TEST_PROVIDER),
            ("identity_token", &identity_token),
            ("client_id", CLIENT_ID),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
#[ignore]
async fn unbound_identity_is_rejected() {
    let store = store().await;
    let (app, provider) = app_with_oauth(store.clone());

    // No `bind_identity` call for this subject — Path B must refuse it.
    let subject = format!("never-bound-{}", Uuid::new_v4());
    let jti = format!("jti-unbound-{}", Uuid::new_v4());
    let identity_token = provider.sign_identity_token(&subject, None, None, Some(&jti));

    let (status, body) = post_form(
        &app,
        "/v1/oauth/token",
        &[
            ("grant_type", "urn:axon:identity_token"),
            ("provider", TEST_PROVIDER),
            ("identity_token", &identity_token),
            ("client_id", CLIENT_ID),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_grant");
}

/// Regression: `/v1/oauth/token` must not be shadowed by the authed
/// sub-router's `/v1/{*path}` catch-all — it must return the RFC 6749 error
/// shape (proving `routes::oauth::token` handled it), not the authed
/// router's `401` (proving the bearer gate intercepted it first) or its
/// `404 route not found` (proving the catch-all won).
#[tokio::test]
#[ignore]
async fn oauth_token_route_is_not_shadowed_by_the_authed_catch_all() {
    let store = store().await;
    let (app, _provider) = app_with_oauth(store);
    let (status, body) = post_form(&app, "/v1/oauth/token", &[("grant_type", "bogus")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "unsupported_grant_type");
}

/// Regression: a pre-existing CLI-minted token (`expires_at IS NULL`) must
/// still verify exactly as before OAuth existed.
#[tokio::test]
#[ignore]
async fn preexisting_cli_token_still_verifies() {
    let store = store().await;
    let (app, _provider) = app_with_oauth(store.clone());

    let issued = store
        .issue_token("cli-regression-check")
        .await
        .expect("issue token");
    let status = get_authed(&app, "/v1/accounts", &issued.token).await;
    assert_eq!(status, StatusCode::OK);
}

/// Minimal percent-encoding for a query-string value (this test only ever
/// encodes a fixed custom-scheme redirect URI, so a small local helper is
/// simpler than adding a dependency).
fn urlencoding_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
