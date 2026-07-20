//! The message gateway: turns high-level mutation requests into matrix-rust-sdk
//! send calls on the right account's [`Client`](matrix_sdk::Client).
//!
//! [`SdkGateway`] owns *message semantics only* — building the ruma content for a
//! send / edit / redact / react and issuing it. It resolves the account's client
//! through the [`ClientManager`] (lazily connecting if needed) but knows nothing
//! about connection retry or caching; that is the manager's job. This is the
//! concrete capability `axon-server` adapts onto the API layer's `MessageSender`
//! port — `axon-api` never sees this type or any SDK type.
//!
//! Each method returns the resulting Matrix event id as a `String`. Errors are
//! [`GatewayError`], chosen so the composition-root adapter can map them onto
//! HTTP status without this crate knowing about HTTP.

use axon_core::{Formatted, MediaAttachment, MediaSendKind, Relation};
use matrix_sdk::attachment::{AttachmentConfig, AttachmentInfo, BaseFileInfo, BaseImageInfo};
use matrix_sdk::room::reply::{EnforceThread, Reply};
use matrix_sdk::room::Receipts;
use matrix_sdk::ruma::api::error::ErrorKind;
use matrix_sdk::ruma::events::reaction::ReactionEventContent;
use matrix_sdk::ruma::events::relation::Annotation;
use matrix_sdk::ruma::events::room::message::{
    AddMentions, ReplyWithinThread, RoomMessageEventContent, TextMessageEventContent,
};
use matrix_sdk::ruma::{EventId, OwnedEventId, OwnedUserId, RoomId, UInt, UserId};
use matrix_sdk::{Room, RoomState};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::GatewayError;
use crate::manager::ClientManager;

/// Map an SDK error from a send/fetch into a [`GatewayError`]. A homeserver
/// `M_FORBIDDEN` (e.g. redacting without the required power level) becomes
/// [`GatewayError::Forbidden`] so it surfaces as `403`; `M_BAD_STATE` (e.g.
/// unbanning a user who isn't banned, ADR 0068 M19b's `kick`/`ban`/`unban`) is
/// a caller mistake, not a homeserver fault, so it becomes
/// [`GatewayError::Invalid`] (`400`) rather than falling into the generic
/// upstream `502`; everything else is an upstream failure.
fn map_sdk_err(err: matrix_sdk::Error) -> GatewayError {
    match err.client_api_error_kind() {
        Some(ErrorKind::Forbidden) => GatewayError::Forbidden(err.to_string()),
        Some(ErrorKind::BadState) => GatewayError::Invalid(err.to_string()),
        _ => GatewayError::Upstream(err.to_string()),
    }
}

/// Parse a Matrix event id targeted by a mutation, mapping a malformed one to
/// [`GatewayError::Invalid`] (a clean `400`) instead of letting it reach the
/// homeserver. Shared by every method that addresses an existing event: edit,
/// redact, react, and the read-receipt send.
fn parse_event_id(event_id: &str) -> Result<OwnedEventId, GatewayError> {
    EventId::parse(event_id).map_err(|e| GatewayError::Invalid(format!("event id: {e}")))
}

/// Parse a Matrix user id targeted by a membership mutation (invite/kick/
/// ban/unban), mapping a malformed one to [`GatewayError::Invalid`] instead of
/// letting it reach the homeserver.
fn parse_user_id(user_id: &str) -> Result<OwnedUserId, GatewayError> {
    UserId::parse(user_id).map_err(|e| GatewayError::Invalid(format!("user id: {e}")))
}

fn effective_mime(
    kind: MediaSendKind,
    content_type: Option<&str>,
) -> Result<mime::Mime, GatewayError> {
    let parsed = match content_type {
        Some(value) => value
            .parse::<mime::Mime>()
            .map_err(|e| GatewayError::Invalid(format!("content_type: {e}")))?,
        None => match kind {
            MediaSendKind::Image => mime::IMAGE_PNG,
            MediaSendKind::File => mime::APPLICATION_OCTET_STREAM,
        },
    };

    match kind {
        MediaSendKind::Image if parsed.type_() != mime::IMAGE => Err(GatewayError::Invalid(
            "image uploads must have an image/* content type".to_owned(),
        )),
        MediaSendKind::Image => Ok(parsed),
        // matrix-sdk chooses m.image/m.audio/m.video from the MIME major type.
        // M15 supports only explicit m.file for file uploads, so avoid those
        // major types even when the original Content-Type was specific.
        MediaSendKind::File
            if matches!(parsed.type_(), mime::IMAGE | mime::AUDIO | mime::VIDEO) =>
        {
            Ok(mime::APPLICATION_OCTET_STREAM)
        }
        MediaSendKind::File => Ok(parsed),
    }
}

fn attachment_info(kind: MediaSendKind, size_bytes: u64) -> Result<AttachmentInfo, GatewayError> {
    let size = UInt::new(size_bytes).ok_or_else(|| {
        GatewayError::Invalid("upload is too large for Matrix metadata".to_owned())
    })?;
    Ok(match kind {
        MediaSendKind::Image => AttachmentInfo::Image(BaseImageInfo {
            size: Some(size),
            ..BaseImageInfo::default()
        }),
        MediaSendKind::File => AttachmentInfo::File(BaseFileInfo { size: Some(size) }),
    })
}

fn attachment_reply(relation: Relation<'_>) -> Result<Option<Reply>, GatewayError> {
    let reply_to = relation
        .reply_to
        .map(|id| EventId::parse(id).map_err(|e| GatewayError::Invalid(format!("reply_to: {e}"))))
        .transpose()?;
    let thread_root = relation
        .thread_root
        .map(|id| {
            EventId::parse(id).map_err(|e| GatewayError::Invalid(format!("thread_root: {e}")))
        })
        .transpose()?;

    let Some(event_id) = reply_to.or(thread_root) else {
        return Ok(None);
    };
    let (enforce_thread, add_mentions) = match (relation.thread_root, relation.reply_to) {
        (Some(_), Some(_)) => (
            EnforceThread::Threaded(ReplyWithinThread::Yes),
            AddMentions::Yes,
        ),
        (Some(_), None) => (
            EnforceThread::Threaded(ReplyWithinThread::No),
            AddMentions::No,
        ),
        (None, Some(_)) => (EnforceThread::Unthreaded, AddMentions::Yes),
        (None, None) => unreachable!("event_id checked above"),
    };
    Ok(Some(Reply {
        event_id,
        enforce_thread,
        add_mentions,
    }))
}

fn message_relates_to(relation: Relation<'_>) -> Result<Option<Value>, GatewayError> {
    if let Some(id) = relation.reply_to {
        EventId::parse(id).map_err(|e| GatewayError::Invalid(format!("reply_to: {e}")))?;
    }
    if let Some(id) = relation.thread_root {
        EventId::parse(id).map_err(|e| GatewayError::Invalid(format!("thread_root: {e}")))?;
    }

    Ok(match (relation.thread_root, relation.reply_to) {
        // Thread member, not a reply. Do not invent an `m.in_reply_to` fallback:
        // without the latest known in-thread event, falling back to the root
        // makes thread additions render as replies to the first message.
        (Some(root), None) => Some(json!({
            "rel_type": "m.thread",
            "event_id": root,
        })),
        // Explicit reply within a thread.
        (Some(root), Some(reply)) => Some(json!({
            "rel_type": "m.thread",
            "event_id": root,
            "m.in_reply_to": {
                "event_id": reply,
            },
        })),
        // Plain reply (no thread).
        (None, Some(reply)) => Some(json!({ "m.in_reply_to": { "event_id": reply } })),
        (None, None) => None,
    })
}

/// Sends Matrix message-like events on behalf of an account, routed through that
/// account's SDK client. Cheap to [`Clone`] (holds only a [`ClientManager`]).
#[derive(Clone)]
pub struct SdkGateway {
    manager: ClientManager,
}

impl SdkGateway {
    /// Build a gateway over a client manager. Constructed by the sync engine and
    /// exposed via [`SyncEngine::gateway`](crate::SyncEngine::gateway).
    pub(crate) fn new(manager: ClientManager) -> Self {
        Self { manager }
    }

    /// Resolve the joined [`Room`] for `(account_id, room_id)`, connecting the
    /// account's client on demand. A malformed room id is a `400`-class
    /// [`GatewayError::Invalid`]; a room the client doesn't know is a `404`-class
    /// [`GatewayError::RoomNotFound`].
    async fn room(&self, account_id: Uuid, room_id: &str) -> Result<Room, GatewayError> {
        let client = self.manager.get_or_connect(account_id).await?;
        let parsed =
            RoomId::parse(room_id).map_err(|e| GatewayError::Invalid(format!("room id: {e}")))?;
        client
            .get_room(&parsed)
            .ok_or_else(|| GatewayError::RoomNotFound(room_id.to_owned()))
    }

    /// Send an `m.room.message`. `body` is the plain-text content; `formatted`,
    /// when present, adds the rich-text rendering (`format` + `formatted_body`).
    /// `relation`, when set, attaches an `m.relates_to` (a reply and/or thread
    /// membership). Returns the new event id.
    pub async fn send_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        body: &str,
        formatted: Option<Formatted<'_>>,
        relation: Relation<'_>,
    ) -> Result<String, GatewayError> {
        let room = self.room(account_id, room_id).await?;

        // Plain, unrelated message: use the typed constructor. `format` is
        // validated to `org.matrix.custom.html` at the API boundary, so
        // `text_html` carries it faithfully.
        if !relation.is_some() {
            let content = match formatted {
                None => RoomMessageEventContent::text_plain(body),
                Some(f) => RoomMessageEventContent::text_html(body, f.body),
            };
            let resp = room.send(content).await.map_err(map_sdk_err)?;
            return Ok(resp.response.event_id.to_string());
        }

        // A relation is requested: build a raw envelope with `m.relates_to`,
        // mirroring how `edit` constructs its relation by hand. Validate the
        // referenced event ids up front so a bad id is a clean 400, not a 502.
        let relates_to = message_relates_to(relation)?.expect("relation.is_some() checked above");

        let mut content = json!({
            "msgtype": "m.text",
            "body": body,
            "m.relates_to": relates_to,
        });
        if let Some(f) = formatted {
            content["format"] = json!(f.format);
            content["formatted_body"] = json!(f.body);
        }
        let resp = room
            .send_raw("m.room.message", content)
            .await
            .map_err(map_sdk_err)?;
        Ok(resp.response.event_id.to_string())
    }

    /// Send a staged media attachment as an `m.image` or `m.file`, letting the
    /// SDK own media upload, encrypted-file metadata, and final room send.
    pub async fn send_media(
        &self,
        account_id: Uuid,
        room_id: &str,
        attachment: MediaAttachment,
        caption: Option<&str>,
        relation: Relation<'_>,
    ) -> Result<String, GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let content_type = effective_mime(attachment.kind, attachment.content_type.as_deref())?;
        let info = attachment_info(attachment.kind, attachment.size_bytes)?;
        let reply = attachment_reply(relation)?;
        let caption = caption.map(TextMessageEventContent::plain);
        let config = AttachmentConfig::new()
            .info(info)
            .caption(caption)
            .reply(reply);
        let resp = room
            .send_attachment(attachment.filename, &content_type, attachment.bytes, config)
            .await
            .map_err(map_sdk_err)?;
        Ok(resp.event_id.to_string())
    }

    /// Edit a message by sending an `m.replace` replacement of `event_id`.
    /// Built as a raw envelope (`m.new_content` + `m.relates_to`) so we don't
    /// need the original event in hand. Returns the replacement event's id.
    pub async fn edit(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        body: &str,
        formatted: Option<Formatted<'_>>,
    ) -> Result<String, GatewayError> {
        let room = self.room(account_id, room_id).await?;
        // Validate the target id up front so a bad id is a clean 400, not a 502.
        let target_id = parse_event_id(event_id)?;

        // A Matrix edit (m.replace) is only valid from the *original author*, but
        // the homeserver does not enforce that — it accepts an m.replace pointing
        // at anyone's event. So we enforce it: fetch the target and refuse to send
        // a forged edit of a message this account didn't write (which would
        // otherwise return 200 and could be rendered by a naive client).
        let target = room.event(&target_id, None).await.map_err(map_sdk_err)?;
        if target.sender().as_deref() != Some(room.own_user_id()) {
            return Err(GatewayError::Forbidden(
                "can only edit your own messages".to_owned(),
            ));
        }

        let mut new_content = json!({ "msgtype": "m.text", "body": body });
        let mut content = json!({
            "msgtype": "m.text",
            // The fallback body convention for clients that don't understand edits.
            "body": format!("* {body}"),
            "m.relates_to": { "rel_type": "m.replace", "event_id": event_id },
        });
        if let Some(f) = formatted {
            // The replacement carries the formatting verbatim; the top-level
            // fallback mirrors it with the same `* ` edit prefix as `body`.
            new_content["format"] = json!(f.format);
            new_content["formatted_body"] = json!(f.body);
            content["format"] = json!(f.format);
            content["formatted_body"] = json!(format!("* {}", f.body));
        }
        content["m.new_content"] = new_content;
        let resp = room
            .send_raw("m.room.message", content)
            .await
            .map_err(map_sdk_err)?;
        Ok(resp.response.event_id.to_string())
    }

    /// Redact `event_id`, optionally with a reason. Returns the redaction event's id.
    pub async fn redact(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        reason: Option<&str>,
    ) -> Result<String, GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let event_id = parse_event_id(event_id)?;
        let resp = room
            .redact(&event_id, reason, None)
            .await
            .map_err(|e| map_sdk_err(matrix_sdk::Error::from(e)))?;
        Ok(resp.event_id.to_string())
    }

    /// Send an `m.reaction` annotating `event_id` with `key` (an emoji or short
    /// string). Returns the reaction event's id.
    pub async fn react(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        key: &str,
    ) -> Result<String, GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let event_id = parse_event_id(event_id)?;
        let content = ReactionEventContent::new(Annotation::new(event_id, key.to_owned()));
        let resp = room.send(content).await.map_err(map_sdk_err)?;
        Ok(resp.response.event_id.to_string())
    }

    /// Mark `event_id` read: sets the public read receipt (`m.read`) and the
    /// private fully-read marker to the same event in a single
    /// `POST /rooms/{roomId}/read_markers` call, so third-party Matrix clients
    /// reading standard receipt state see the room as read (ADR 0067).
    pub async fn send_read_receipt(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
    ) -> Result<(), GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let event_id = parse_event_id(event_id)?;
        let receipts = Receipts::new()
            .fully_read_marker(event_id.clone())
            .public_read_receipt(event_id);
        room.send_multiple_receipts(receipts)
            .await
            .map_err(map_sdk_err)
    }

    /// Set (or clear) this account's typing indicator in a room (ADR 0068,
    /// M19a). The SDK debounces/expires the underlying `m.typing` event itself;
    /// this issues one `PUT /rooms/{roomId}/typing/{userId}` per call.
    pub async fn send_typing_notice(
        &self,
        account_id: Uuid,
        room_id: &str,
        typing: bool,
    ) -> Result<(), GatewayError> {
        let room = self.room(account_id, room_id).await?;
        room.typing_notice(typing).await.map_err(map_sdk_err)
    }

    /// Leave this room (ADR 0068, M19b). The `m.room.member` leave event this
    /// produces already drives existing downstream handling — the ADR 0037
    /// membership filter and ADR 0044's opt-in `purge_on_leave` — via the sync
    /// path's own `persist_state_event`; this method only issues the call.
    pub async fn leave(&self, account_id: Uuid, room_id: &str) -> Result<(), GatewayError> {
        let room = self.room(account_id, room_id).await?;
        room.leave().await.map_err(map_sdk_err)
    }

    /// Forget a left or banned-from room (ADR 0068, M19b). The homeserver only
    /// accepts this for a room in `Left`/`Banned` state; a still-joined or
    /// -invited room is rejected up front as a clean 400 rather than round-
    /// tripping to the SDK for a `WrongRoomState` error that `map_sdk_err`
    /// can't distinguish from an upstream failure.
    pub async fn forget(&self, account_id: Uuid, room_id: &str) -> Result<(), GatewayError> {
        let room = self.room(account_id, room_id).await?;
        match room.state() {
            RoomState::Left | RoomState::Banned => {}
            other => {
                return Err(GatewayError::Invalid(format!(
                    "cannot forget a room in state {other:?}; only left or banned rooms can be forgotten"
                )))
            }
        }
        room.forget().await.map_err(map_sdk_err)
    }

    /// Invite `user_id` to this room (ADR 0068, M19b).
    pub async fn invite(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_id: &str,
    ) -> Result<(), GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let user_id = parse_user_id(user_id)?;
        room.invite_user_by_id(&user_id).await.map_err(map_sdk_err)
    }

    /// Kick `user_id` from this room, optionally with a reason (ADR 0068, M19b).
    pub async fn kick(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<(), GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let user_id = parse_user_id(user_id)?;
        room.kick_user(&user_id, reason).await.map_err(map_sdk_err)
    }

    /// Ban `user_id` from this room, optionally with a reason (ADR 0068, M19b).
    pub async fn ban(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<(), GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let user_id = parse_user_id(user_id)?;
        room.ban_user(&user_id, reason).await.map_err(map_sdk_err)
    }

    /// Unban `user_id` from this room, optionally with a reason (ADR 0068, M19b).
    pub async fn unban(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<(), GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let user_id = parse_user_id(user_id)?;
        room.unban_user(&user_id, reason).await.map_err(map_sdk_err)
    }
}

#[cfg(test)]
mod tests {
    use axon_core::{MediaSendKind, Relation};
    use matrix_sdk::attachment::AttachmentInfo;
    use matrix_sdk::room::reply::EnforceThread;
    use matrix_sdk::ruma::events::room::message::{AddMentions, ReplyWithinThread};
    use serde_json::json;

    use super::{
        attachment_info, attachment_reply, effective_mime, message_relates_to, parse_user_id,
    };
    use crate::error::GatewayError;

    const REPLY_EVENT: &str = "$reply:example.org";
    const THREAD_ROOT: &str = "$thread:example.org";

    #[test]
    fn parse_user_id_accepts_a_valid_id() {
        let user_id = parse_user_id("@alice:example.org").expect("valid user id");
        assert_eq!(user_id.as_str(), "@alice:example.org");
    }

    #[test]
    fn parse_user_id_rejects_a_malformed_id() {
        let err = parse_user_id("not-a-user-id").expect_err("malformed user id should fail");
        assert!(matches!(err, GatewayError::Invalid(message) if message.starts_with("user id:")));
    }

    #[test]
    fn effective_mime_uses_kind_defaults() {
        assert_eq!(
            effective_mime(MediaSendKind::Image, None).expect("image default"),
            mime::IMAGE_PNG
        );
        assert_eq!(
            effective_mime(MediaSendKind::File, None).expect("file default"),
            mime::APPLICATION_OCTET_STREAM
        );
    }

    #[test]
    fn effective_mime_accepts_matching_image_type() {
        assert_eq!(
            effective_mime(MediaSendKind::Image, Some("image/jpeg")).expect("image jpeg"),
            mime::IMAGE_JPEG
        );
    }

    #[test]
    fn effective_mime_rejects_non_image_type_for_image_send() {
        let err = effective_mime(MediaSendKind::Image, Some("application/pdf"))
            .expect_err("non-image content type should fail");

        assert!(
            matches!(err, GatewayError::Invalid(message) if message == "image uploads must have an image/* content type")
        );
    }

    #[test]
    fn effective_mime_coerces_media_types_for_file_send() {
        for content_type in ["image/png", "audio/ogg", "video/mp4"] {
            assert_eq!(
                effective_mime(MediaSendKind::File, Some(content_type)).expect("file mime"),
                mime::APPLICATION_OCTET_STREAM
            );
        }
    }

    #[test]
    fn effective_mime_rejects_unparseable_content_type() {
        let err = effective_mime(MediaSendKind::File, Some("not a mime type"))
            .expect_err("invalid content type should fail");

        assert!(
            matches!(err, GatewayError::Invalid(message) if message.starts_with("content_type:"))
        );
    }

    #[test]
    fn attachment_info_records_size_for_each_send_kind() {
        let image_info = attachment_info(MediaSendKind::Image, 123).expect("image info");
        let AttachmentInfo::Image(image_info) = image_info else {
            panic!("expected image attachment info");
        };
        assert_eq!(u64::from(image_info.size.expect("image size")), 123);

        let file_info = attachment_info(MediaSendKind::File, 456).expect("file info");
        let AttachmentInfo::File(file_info) = file_info else {
            panic!("expected file attachment info");
        };
        assert_eq!(u64::from(file_info.size.expect("file size")), 456);
    }

    #[test]
    fn attachment_info_rejects_sizes_too_large_for_matrix_metadata() {
        let err = attachment_info(MediaSendKind::File, u64::MAX)
            .expect_err("oversized attachment metadata should fail");

        assert!(
            matches!(err, GatewayError::Invalid(message) if message == "upload is too large for Matrix metadata")
        );
    }

    #[test]
    fn message_relates_to_maps_thread_member_without_reply() {
        let relates_to = message_relates_to(Relation {
            reply_to: None,
            thread_root: Some(THREAD_ROOT),
        })
        .expect("valid relation")
        .expect("thread relation");

        assert_eq!(
            relates_to,
            json!({
                "rel_type": "m.thread",
                "event_id": THREAD_ROOT,
            })
        );
    }

    #[test]
    fn message_relates_to_maps_thread_reply() {
        let relates_to = message_relates_to(Relation {
            reply_to: Some(REPLY_EVENT),
            thread_root: Some(THREAD_ROOT),
        })
        .expect("valid relation")
        .expect("thread reply relation");

        assert_eq!(
            relates_to,
            json!({
                "rel_type": "m.thread",
                "event_id": THREAD_ROOT,
                "m.in_reply_to": {
                    "event_id": REPLY_EVENT,
                },
            })
        );
    }

    #[test]
    fn message_relates_to_maps_plain_reply() {
        let relates_to = message_relates_to(Relation {
            reply_to: Some(REPLY_EVENT),
            thread_root: None,
        })
        .expect("valid relation")
        .expect("reply relation");

        assert_eq!(
            relates_to,
            json!({ "m.in_reply_to": { "event_id": REPLY_EVENT } })
        );
    }

    #[test]
    fn message_relates_to_rejects_invalid_event_ids() {
        let bad_reply = message_relates_to(Relation {
            reply_to: Some("not-an-event-id"),
            thread_root: None,
        })
        .expect_err("bad reply id should fail");
        assert!(
            matches!(bad_reply, GatewayError::Invalid(message) if message.starts_with("reply_to:"))
        );

        let bad_thread = message_relates_to(Relation {
            reply_to: None,
            thread_root: Some("not-an-event-id"),
        })
        .expect_err("bad thread id should fail");
        assert!(
            matches!(bad_thread, GatewayError::Invalid(message) if message.starts_with("thread_root:"))
        );
    }

    #[test]
    fn attachment_reply_returns_none_without_relation() {
        assert!(attachment_reply(Relation::default())
            .expect("unrelated attachment")
            .is_none());
    }

    #[test]
    fn attachment_reply_maps_plain_reply() {
        let reply = attachment_reply(Relation {
            reply_to: Some(REPLY_EVENT),
            thread_root: None,
        })
        .expect("plain reply")
        .expect("reply metadata");

        assert_eq!(reply.event_id.as_str(), REPLY_EVENT);
        assert_eq!(reply.enforce_thread, EnforceThread::Unthreaded);
        assert_eq!(reply.add_mentions, AddMentions::Yes);
    }

    #[test]
    fn attachment_reply_maps_thread_without_explicit_reply_to_root_fallback() {
        let reply = attachment_reply(Relation {
            reply_to: None,
            thread_root: Some(THREAD_ROOT),
        })
        .expect("thread relation")
        .expect("reply metadata");

        assert_eq!(reply.event_id.as_str(), THREAD_ROOT);
        assert_eq!(
            reply.enforce_thread,
            EnforceThread::Threaded(ReplyWithinThread::No)
        );
        assert_eq!(reply.add_mentions, AddMentions::No);
    }

    #[test]
    fn attachment_reply_maps_thread_with_explicit_reply() {
        let reply = attachment_reply(Relation {
            reply_to: Some(REPLY_EVENT),
            thread_root: Some(THREAD_ROOT),
        })
        .expect("thread reply relation")
        .expect("reply metadata");

        assert_eq!(reply.event_id.as_str(), REPLY_EVENT);
        assert_eq!(
            reply.enforce_thread,
            EnforceThread::Threaded(ReplyWithinThread::Yes)
        );
        assert_eq!(reply.add_mentions, AddMentions::Yes);
    }

    #[test]
    fn attachment_reply_rejects_invalid_event_ids() {
        let bad_reply = attachment_reply(Relation {
            reply_to: Some("not-an-event-id"),
            thread_root: None,
        })
        .expect_err("bad reply id should fail");
        assert!(
            matches!(bad_reply, GatewayError::Invalid(message) if message.starts_with("reply_to:"))
        );

        let bad_thread = attachment_reply(Relation {
            reply_to: None,
            thread_root: Some("not-an-event-id"),
        })
        .expect_err("bad thread id should fail");
        assert!(
            matches!(bad_thread, GatewayError::Invalid(message) if message.starts_with("thread_root:"))
        );
    }
}
