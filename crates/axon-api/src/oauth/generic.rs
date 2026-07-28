//! `GenericOidcProvider`: the discovery-doc-driven [`OidcProvider`] impl
//! shared by Google and Microsoft (M14b, ADR 0054).
//!
//! Both are plain OIDC providers: fetch `.well-known/openid-configuration`
//! once at construction (cached for the provider's lifetime), then drive
//! authorize/token-exchange/verify off the endpoints it publishes. Signature
//! verification is [`jsonwebtoken`]'s job; issuer/audience/nonce/expiry
//! checks are done by hand here rather than through that crate's built-in
//! validator, so the one genuinely provider-specific wrinkle — Microsoft's
//! multi-tenant `{tenantid}`-templated issuer — has a clear, testable seam
//! ([`issuer_matches`]) instead of fighting a generic library's assumptions.

use chrono::Utc;
use jsonwebtoken::{decode, decode_header, Validation};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::oauth::jwks::JwksCache;
use crate::oauth::provider::{OidcError, OidcProvider, UpstreamTokens, VerifiedIdentity};
use crate::oauth::{read_json_capped, MAX_HTTP_RESPONSE_BYTES};

/// Clock-skew tolerance applied to `exp`/`nbf`/`iat` checks.
const CLOCK_SKEW_SECS: i64 = 60;

/// A discovery-doc-driven OIDC provider (Google, Microsoft).
pub struct GenericOidcProvider {
    name: &'static str,
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    /// Parsed once at [`discover`](Self::discover) time, so a malformed
    /// provider-published endpoint fails discovery with an [`OidcError`]
    /// rather than panicking later on the request path, where
    /// [`authorize_url`](OidcProvider::authorize_url) has no `Result` to
    /// return one through.
    authorization_endpoint: url::Url,
    discovery: DiscoveryDocument,
    jwks: JwksCache,
}

#[derive(Debug, Clone, Deserialize)]
struct DiscoveryDocument {
    /// The provider's own asserted issuer. For Microsoft's multi-tenant
    /// endpoints (`common`/`organizations`/`consumers`) this is a template
    /// containing the literal string `{tenantid}` — see [`issuer_matches`].
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: String,
}

#[derive(Debug, Deserialize)]
struct Claims {
    iss: String,
    #[serde(default)]
    aud: Value,
    sub: String,
    exp: i64,
    iat: i64,
    #[serde(default)]
    nbf: Option<i64>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    jti: Option<String>,
    /// Microsoft's tenant id claim — substituted into a `{tenantid}`-templated
    /// issuer to validate multi-tenant tokens. Absent for Google.
    #[serde(default)]
    tid: Option<String>,
}

impl GenericOidcProvider {
    /// Fetch `issuer`'s discovery document once and build a provider over it.
    /// The `.well-known/openid-configuration` path is appended directly to
    /// `issuer` (not RFC 5785's host-first insertion) — this matches how
    /// Microsoft's own multi-tenant issuers (which carry a path, e.g.
    /// `.../common/v2.0`) actually publish discovery, and is a no-op
    /// distinction for Google's path-free issuer.
    pub async fn discover(
        name: &'static str,
        http: reqwest::Client,
        issuer: &str,
        client_id: String,
        client_secret: String,
    ) -> Result<Self, OidcError> {
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let response = http
            .get(&discovery_url)
            .send()
            .await
            .map_err(|err| OidcError::Http(err.to_string()))?;
        if !response.status().is_success() {
            return Err(OidcError::Http(format!(
                "discovery fetch from {discovery_url} returned {}",
                response.status()
            )));
        }
        let discovery: DiscoveryDocument =
            read_json_capped(response, MAX_HTTP_RESPONSE_BYTES).await?;

        // Discovery documents are remote, provider-published input — validate
        // every endpoint URL now, while a failure can still surface as a
        // clean boot-time/construction error, rather than later on a request
        // path that has no clean way to reject a malformed one.
        let authorization_endpoint =
            url::Url::parse(&discovery.authorization_endpoint).map_err(|err| {
                OidcError::Malformed(format!(
                    "discovery document's authorization_endpoint {:?} is not a valid URL: {err}",
                    discovery.authorization_endpoint
                ))
            })?;
        url::Url::parse(&discovery.token_endpoint).map_err(|err| {
            OidcError::Malformed(format!(
                "discovery document's token_endpoint {:?} is not a valid URL: {err}",
                discovery.token_endpoint
            ))
        })?;
        url::Url::parse(&discovery.jwks_uri).map_err(|err| {
            OidcError::Malformed(format!(
                "discovery document's jwks_uri {:?} is not a valid URL: {err}",
                discovery.jwks_uri
            ))
        })?;

        let jwks = JwksCache::new(http.clone(), discovery.jwks_uri.clone());
        Ok(Self {
            name,
            http,
            client_id,
            client_secret,
            authorization_endpoint,
            discovery,
            jwks,
        })
    }
}

#[async_trait::async_trait]
impl OidcProvider for GenericOidcProvider {
    fn name(&self) -> &'static str {
        self.name
    }

    fn authorize_url(&self, state: &str, nonce: &str, redirect_uri: &str) -> String {
        let mut url = self.authorization_endpoint.clone();
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", "openid email")
            .append_pair("state", state)
            .append_pair("nonce", nonce);
        url.to_string()
    }

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
    ) -> Result<UpstreamTokens, OidcError> {
        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
        ];
        let response = self
            .http
            .post(&self.discovery.token_endpoint)
            .form(&params)
            .send()
            .await
            .map_err(|err| OidcError::Http(err.to_string()))?;
        if !response.status().is_success() {
            return Err(OidcError::Http(format!(
                "token exchange returned {}",
                response.status()
            )));
        }
        let body: TokenResponse = read_json_capped(response, MAX_HTTP_RESPONSE_BYTES).await?;
        Ok(UpstreamTokens {
            id_token: body.id_token,
        })
    }

    async fn verify_identity_token(
        &self,
        token: &str,
        nonce: Option<&str>,
    ) -> Result<VerifiedIdentity, OidcError> {
        let header = decode_header(token).map_err(|err| OidcError::Malformed(err.to_string()))?;
        let kid = header
            .kid
            .ok_or_else(|| OidcError::BadSignature("token header has no kid".to_owned()))?;
        let (algorithm, decoding_key) = self.jwks.resolve(&kid).await?;
        if header.alg != algorithm {
            return Err(OidcError::DisallowedAlgorithm(format!(
                "token header alg {:?} does not match JWKS key {kid:?}'s algorithm {algorithm:?}",
                header.alg
            )));
        }

        // Signature/structure verification is jsonwebtoken's job; iss/aud/
        // nonce/exp/nbf/iat are checked by hand below so the Microsoft
        // template case has an explicit, testable seam.
        let mut validation = Validation::new(algorithm);
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        let data = decode::<Claims>(token, &decoding_key, &validation)
            .map_err(|err| OidcError::BadSignature(err.to_string()))?;
        let claims = data.claims;

        if !issuer_matches(&self.discovery.issuer, &claims.iss, claims.tid.as_deref()) {
            return Err(OidcError::InvalidIssuer(claims.iss));
        }
        if !audience_contains(&claims.aud, &self.client_id) {
            return Err(OidcError::InvalidAudience(claims.aud.to_string()));
        }
        if let Some(expected_nonce) = nonce {
            if claims.nonce.as_deref() != Some(expected_nonce) {
                return Err(OidcError::InvalidNonce);
            }
        }

        let now = Utc::now().timestamp();
        if claims.exp + CLOCK_SKEW_SECS < now {
            return Err(OidcError::Expired(format!(
                "token expired at {}",
                claims.exp
            )));
        }
        if let Some(nbf) = claims.nbf {
            if nbf - CLOCK_SKEW_SECS > now {
                return Err(OidcError::Expired("token not yet valid".to_owned()));
            }
        }
        if claims.iat - CLOCK_SKEW_SECS > now {
            return Err(OidcError::Expired("token issued in the future".to_owned()));
        }

        let replay_key = claims.jti.unwrap_or_else(|| hash_token(token));

        Ok(VerifiedIdentity {
            subject: claims.sub,
            email: claims.email,
            replay_key,
        })
    }
}

/// True if `token_iss` satisfies `configured_issuer`. Handles Microsoft's
/// multi-tenant discovery issuer, which is a **template** containing the
/// literal string `{tenantid}` rather than a concrete value: the token's own
/// `tid` claim is substituted in before comparing. Any other provider's
/// issuer (no `{tenantid}` placeholder) is compared for exact equality, as
/// normal.
pub(crate) fn issuer_matches(
    configured_issuer: &str,
    token_iss: &str,
    token_tid: Option<&str>,
) -> bool {
    if configured_issuer.contains("{tenantid}") {
        return match token_tid {
            Some(tid) => configured_issuer.replace("{tenantid}", tid) == token_iss,
            None => false,
        };
    }
    configured_issuer == token_iss
}

/// True if `aud` (a JWT `aud` claim: either a bare string or an array of
/// strings) contains `expected`.
fn audience_contains(aud: &Value, expected: &str) -> bool {
    match aud {
        Value::String(s) => s == expected,
        Value::Array(values) => values.iter().any(|v| v.as_str() == Some(expected)),
        _ => false,
    }
}

/// Fallback replay key when a provider omits `jti`: a hash of the raw token.
fn hash_token(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    async fn serve(router: Router) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        addr
    }

    #[tokio::test]
    async fn discover_rejects_a_malformed_authorization_endpoint() {
        let addr = Router::new().route(
            "/.well-known/openid-configuration",
            get(|| async {
                Json(json!({
                    "issuer": "https://issuer.example.com",
                    "authorization_endpoint": "not a valid url",
                    "token_endpoint": "https://issuer.example.com/token",
                    "jwks_uri": "https://issuer.example.com/jwks",
                }))
            }),
        );
        let addr = serve(addr).await;
        let result = GenericOidcProvider::discover(
            "test",
            reqwest::Client::new(),
            &format!("http://{addr}"),
            "client".to_owned(),
            "secret".to_owned(),
        )
        .await;
        let err = match result {
            Err(err) => err,
            Ok(_) => {
                panic!("a malformed authorization_endpoint must fail discovery, not panic later")
            }
        };
        assert!(matches!(err, OidcError::Malformed(_)));
    }

    #[test]
    fn plain_issuer_requires_exact_match() {
        assert!(issuer_matches(
            "https://accounts.google.com",
            "https://accounts.google.com",
            None
        ));
        assert!(!issuer_matches(
            "https://accounts.google.com",
            "https://evil.example.com",
            None
        ));
    }

    #[test]
    fn templated_issuer_substitutes_the_tokens_own_tenant_id() {
        let configured = "https://login.microsoftonline.com/{tenantid}/v2.0";
        let token_iss =
            "https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0";
        assert!(issuer_matches(
            configured,
            token_iss,
            Some("9188040d-6c67-4c5b-b112-36a304b66dad")
        ));
    }

    #[test]
    fn templated_issuer_rejects_a_mismatched_tenant() {
        let configured = "https://login.microsoftonline.com/{tenantid}/v2.0";
        let token_iss =
            "https://login.microsoftonline.com/aaaaaaaa-0000-0000-0000-000000000000/v2.0";
        assert!(!issuer_matches(
            configured,
            token_iss,
            Some("9188040d-6c67-4c5b-b112-36a304b66dad")
        ));
    }

    #[test]
    fn templated_issuer_without_a_tid_claim_is_rejected() {
        // A token that doesn't carry `tid` can't satisfy a templated issuer —
        // there's nothing to substitute, so this must fail closed, not match
        // the literal template string.
        let configured = "https://login.microsoftonline.com/{tenantid}/v2.0";
        assert!(!issuer_matches(configured, configured, None));
    }

    #[test]
    fn audience_matches_bare_string_or_array() {
        assert!(audience_contains(&Value::String("abc".into()), "abc"));
        assert!(!audience_contains(&Value::String("abc".into()), "xyz"));
        assert!(audience_contains(
            &Value::Array(vec![
                Value::String("one".into()),
                Value::String("abc".into())
            ]),
            "abc"
        ));
        assert!(!audience_contains(&Value::Null, "abc"));
    }
}
