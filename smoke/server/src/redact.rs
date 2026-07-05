//! Shared secret-redaction helpers for HTTP journaling.
//!
//! Both the Axon and Matrix clients journal their requests/responses to
//! artifacts that are uploaded on smoke-suite failure, so URLs and bodies must
//! be scrubbed of credential-like material before they are stored.

use serde_json::Value;

/// Redact `access_token` query values in a URL, stripping the actual token
/// value (everything up to the next `&` or end of string) rather than merely
/// prefixing it — so `?access_token=abcd` becomes `?access_token=<redacted>`.
pub fn redact_url(url: &str) -> String {
    const KEY: &str = "access_token=";
    let mut out = String::with_capacity(url.len());
    let mut rest = url;
    while let Some(idx) = rest.find(KEY) {
        let after = idx + KEY.len();
        out.push_str(&rest[..after]);
        out.push_str("<redacted>");
        // Drop the token value up to the next query separator or end of string.
        match rest[after..].find('&') {
            Some(amp) => rest = &rest[after + amp..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Recursively replace credential-like JSON object fields with `<redacted>`.
pub fn redact_json(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            for key in ["password", "access_token"] {
                if let Some(v) = map.get_mut(key) {
                    *v = Value::String("<redacted>".to_owned());
                }
            }
            for v in map.values_mut() {
                *v = redact_json(std::mem::take(v));
            }
            value
        }
        Value::Array(items) => {
            for item in items {
                *item = redact_json(std::mem::take(item));
            }
            value
        }
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_url_strips_token_value() {
        assert_eq!(
            redact_url("https://hs/_matrix/x?access_token=abcd1234"),
            "https://hs/_matrix/x?access_token=<redacted>"
        );
        assert_eq!(
            redact_url("https://hs/x?access_token=abcd1234&limit=5"),
            "https://hs/x?access_token=<redacted>&limit=5"
        );
        assert_eq!(
            redact_url("https://hs/x?limit=5&access_token=abcd&foo=1"),
            "https://hs/x?limit=5&access_token=<redacted>&foo=1"
        );
        assert_eq!(redact_url("https://hs/x?limit=5"), "https://hs/x?limit=5");
    }

    #[test]
    fn redact_json_scrubs_nested_secrets() {
        let redacted = redact_json(json!({
            "access_token": "abcd",
            "nested": { "password": "hunter2", "keep": "ok" },
            "list": [{ "access_token": "xyz" }]
        }));
        assert_eq!(
            redacted,
            json!({
                "access_token": "<redacted>",
                "nested": { "password": "<redacted>", "keep": "ok" },
                "list": [{ "access_token": "<redacted>" }]
            })
        );
    }
}
