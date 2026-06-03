//! Opaque timeline-pagination cursor.
//!
//! The store's [`TimelineCursor`] is a `(origin_ts, id)` sort key with public
//! fields. We don't expose those on the wire — instead a cursor is the
//! base64url (no padding) of `"{origin_ts}.{id}"`, so the on-the-wire contract
//! stays fixed even if the internal sort key changes.
//!
//! `.` is an unambiguous separator here: both fields are `i64`, whose decimal
//! form is ASCII digits with at most a leading `-` — never a `.` — so
//! `split_once('.')` always cleaves the two fields cleanly (negatives included).

use axon_store::TimelineCursor;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// Encode a sort key into an opaque cursor string.
pub fn encode(cursor: TimelineCursor) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}.{}", cursor.origin_ts, cursor.id))
}

/// Decode an opaque cursor string back into a sort key. Returns `None` for any
/// malformed input (not base64, not UTF-8, wrong shape, unparsable integers) so
/// the handler can map it to a `400`.
pub fn decode(s: &str) -> Option<TimelineCursor> {
    let bytes = URL_SAFE_NO_PAD.decode(s).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (ts, id) = text.split_once('.')?;
    Some(TimelineCursor {
        origin_ts: ts.parse().ok()?,
        id: id.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let c = TimelineCursor {
            origin_ts: 1_700_000_000_000,
            id: 42,
        };
        let decoded = decode(&encode(c)).expect("decode");
        assert_eq!(decoded, c);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode("not base64!!!").is_none());
        assert!(decode("Zm9vYmFy").is_none()); // "foobar" — no dot
    }
}
