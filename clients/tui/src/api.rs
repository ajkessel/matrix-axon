use std::fmt;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

const LIVE_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const LIVE_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Timeout for read-only / probe requests (list calls, timeline fetches, etc.).
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Message-mutation timeout. These routes can block on the sync engine acquiring
/// encryption keys; lifecycle operations such as recovery are deliberately not
/// capped here because their documented work can legitimately exceed 60 seconds.
const MESSAGE_MUTATION_TIMEOUT: Duration = Duration::from_secs(60);
/// Generous timeout for lifecycle operations (login, logout, recover, delete).
/// Recovery imports the megolm key backup and cross-signing keys, which can
/// legitimately exceed 60 s on a real account.
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct AxonClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl AxonClient {
    pub fn new(base_url: String, token: Option<String>) -> Self {
        let base_url = base_url.trim_end_matches('/').to_owned();
        let mut default_headers = HeaderMap::new();
        if let Some(ref t) = token {
            if let Ok(v) = HeaderValue::from_str(&bearer_value_str(t)) {
                default_headers.insert(AUTHORIZATION, v);
            }
        }
        let http = reqwest::ClientBuilder::new()
            .default_headers(default_headers)
            .build()
            .expect("failed to build HTTP client");
        Self {
            http,
            base_url,
            token,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn has_bearer_token(&self) -> bool {
        self.token.is_some()
    }

    pub async fn list_rooms(&self, account_id: Option<Uuid>) -> Result<Vec<RoomDto>, ApiError> {
        let mut request = self.http.get(format!("{}/v1/rooms", self.base_url));
        if let Some(account_id) = account_id {
            request = request.query(&[("account_id", account_id)]);
        }
        self.send(read_request(request)).await
    }

    pub async fn list_accounts(&self) -> Result<Vec<AccountDto>, ApiError> {
        let request = self.http.get(format!("{}/v1/accounts", self.base_url));
        self.send(read_request(request)).await
    }

    /// Log a Matrix account in through Axon. `homeserver_url` is sent only when
    /// the caller supplies an override (the inline `/login` third argument);
    /// otherwise it is omitted and Axon resolves the canonical homeserver from
    /// the Matrix ID's server name (ADR 0023). Either way the TUI talks only to
    /// Axon.
    pub async fn login(
        &self,
        username: &str,
        password: &str,
        homeserver_url: Option<&str>,
    ) -> Result<AccountDto, ApiError> {
        let mut body = serde_json::json!({
            "username": username,
            "password": password,
        });
        if let Some(homeserver_url) = homeserver_url {
            body["homeserver_url"] = serde_json::Value::String(homeserver_url.to_owned());
        }
        let request = self
            .http
            .post(format!("{}/v1/accounts/login", self.base_url))
            .json(&body);
        self.send(lifecycle(request)).await
    }

    pub async fn logout(&self, account_id: Uuid) -> Result<AccountDto, ApiError> {
        let request = self
            .http
            .post(format!("{}/v1/accounts/{account_id}/logout", self.base_url));
        self.send(lifecycle(request)).await
    }

    pub async fn recover(
        &self,
        account_id: Uuid,
        recovery_key: &str,
    ) -> Result<AccountDto, ApiError> {
        let request = self
            .http
            .post(format!(
                "{}/v1/accounts/{account_id}/recover",
                self.base_url
            ))
            .json(&serde_json::json!({ "recovery_key": recovery_key }));
        self.send(lifecycle(request)).await
    }

    pub async fn delete_account(&self, account_id: Uuid) -> Result<(), ApiError> {
        let request = self
            .http
            .delete(format!("{}/v1/accounts/{account_id}", self.base_url));
        self.send_no_body(lifecycle(request)).await
    }

    pub async fn room_timeline(
        &self,
        account_id: Uuid,
        room_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<TimelinePage, ApiError> {
        let mut request = self.http.get(format!(
            "{}/v1/accounts/{}/rooms/{}/timeline",
            self.base_url,
            account_id,
            path_segment(room_id)
        ));
        let limit = limit.to_string();
        request = request.query(&[("limit", limit.as_str())]);
        if let Some(cursor) = cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        self.send(read_request(request)).await
    }

    pub async fn send_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        body: &str,
    ) -> Result<SendResultDto, ApiError> {
        let request = self.http.post(format!(
            "{}/v1/accounts/{}/rooms/{}/send",
            self.base_url,
            account_id,
            path_segment(room_id)
        ));
        let request = message_mutation(request).json(&serde_json::json!({ "body": body }));
        self.send(request).await
    }

    pub async fn edit_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        body: &str,
    ) -> Result<SendResultDto, ApiError> {
        let request = self.http.put(format!(
            "{}/v1/accounts/{}/rooms/{}/events/{}",
            self.base_url,
            account_id,
            path_segment(room_id),
            path_segment(event_id)
        ));
        let request = message_mutation(request).json(&serde_json::json!({ "body": body }));
        self.send(request).await
    }

    pub async fn redact_event(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        reason: Option<&str>,
    ) -> Result<SendResultDto, ApiError> {
        let request = self.http.delete(format!(
            "{}/v1/accounts/{}/rooms/{}/events/{}",
            self.base_url,
            account_id,
            path_segment(room_id),
            path_segment(event_id)
        ));
        let mut request = message_mutation(request);
        if let Some(reason) = reason {
            request = request.query(&[("reason", reason)]);
        }
        self.send(request).await
    }

    pub async fn react(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        key: &str,
    ) -> Result<SendResultDto, ApiError> {
        let request = self.http.post(format!(
            "{}/v1/accounts/{}/rooms/{}/events/{}/reactions",
            self.base_url,
            account_id,
            path_segment(room_id),
            path_segment(event_id)
        ));
        let request = message_mutation(request).json(&serde_json::json!({ "key": key }));
        self.send(request).await
    }

    /// Download media identified by an `mxc://` URI, routed through the Axon
    /// server's authenticated media proxy. The server name and media ID are
    /// extracted from the URI and placed in the path; returns the raw bytes on
    /// success.
    pub async fn get_media(&self, account_id: Uuid, mxc_url: &str) -> Result<Vec<u8>, ApiError> {
        let rest = mxc_url
            .strip_prefix("mxc://")
            .ok_or_else(|| ApiError::Url("not an mxc:// URI".to_owned()))?;
        let (server, media_id) = rest
            .split_once('/')
            .ok_or_else(|| ApiError::Url("malformed mxc:// URI".to_owned()))?;
        let request = self.http.get(format!(
            "{}/v1/media/{}/{}/{}",
            self.base_url,
            account_id,
            path_segment(server),
            path_segment(media_id),
        ));
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            Ok(response.bytes().await?.to_vec())
        } else {
            let text = response.text().await?;
            Err(ApiError::Status {
                status,
                message: text,
            })
        }
    }

    pub async fn get_event(&self, account_id: Uuid, event_id: &str) -> Result<EventDto, ApiError> {
        let request = self.http.get(format!(
            "{}/v1/accounts/{}/events/{}",
            self.base_url,
            account_id,
            path_segment(event_id)
        ));
        self.send(read_request(request)).await
    }

    async fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ApiError> {
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        if status.is_success() {
            let envelope: ApiResponse<T> = serde_json::from_str(&text)?;
            Ok(envelope.data)
        } else {
            let message = serde_json::from_str::<ErrorResponse>(&text)
                .map(|body| format!("{}: {}", body.error.code, body.error.message))
                .unwrap_or_else(|_| text);
            Err(ApiError::Status { status, message })
        }
    }

    async fn send_no_body(&self, request: reqwest::RequestBuilder) -> Result<(), ApiError> {
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            let text = response.text().await?;
            let message = serde_json::from_str::<ErrorResponse>(&text)
                .map(|body| format!("{}: {}", body.error.code, body.error.message))
                .unwrap_or_else(|_| text);
            Err(ApiError::Status { status, message })
        }
    }

    pub fn ws_url(&self) -> Result<String, ApiError> {
        let url =
            reqwest::Url::parse(&self.base_url).map_err(|err| ApiError::Url(err.to_string()))?;
        let scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            other => return Err(ApiError::UnsupportedScheme(other.to_owned())),
        };
        let mut url = url;
        url.set_scheme(scheme)
            .map_err(|_| ApiError::UnsupportedScheme(scheme.to_owned()))?;
        url.set_path("/v1/ws");
        url.set_query(None);
        Ok(url.to_string())
    }
}

fn read_request(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.timeout(HTTP_REQUEST_TIMEOUT)
}

fn lifecycle(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.timeout(LIFECYCLE_TIMEOUT)
}

fn message_mutation(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.timeout(MESSAGE_MUTATION_TIMEOUT)
}

pub(crate) fn bearer_value_str(token: &str) -> String {
    format!("Bearer {token}")
}

pub async fn websocket_task(client: AxonClient, tx: mpsc::UnboundedSender<LiveFrame>) {
    let url = match client.ws_url() {
        Ok(url) => url,
        Err(err) => {
            let _ = tx.send(LiveFrame::Disconnected(err.to_string()));
            return;
        }
    };

    let mut backoff = LIVE_RECONNECT_INITIAL_BACKOFF;
    loop {
        let req_result: Result<_, String> = (|| {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;
            use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION as WS_AUTHORIZATION;
            use tokio_tungstenite::tungstenite::http::HeaderValue as WsHeaderValue;
            let mut req = url
                .as_str()
                .into_client_request()
                .map_err(|e| e.to_string())?;
            if let Some(t) = client.token.as_deref() {
                let value =
                    WsHeaderValue::from_str(&bearer_value_str(t)).map_err(|e| e.to_string())?;
                req.headers_mut().insert(WS_AUTHORIZATION, value);
            }
            Ok(req)
        })();
        let reason = match req_result {
            Err(e) => e,
            Ok(req) => match tokio_tungstenite::connect_async(req).await {
                Ok((mut socket, _)) => {
                    let _ = tx.send(LiveFrame::Connected);
                    backoff = LIVE_RECONNECT_INITIAL_BACKOFF;
                    read_websocket(&mut socket, &tx).await
                }
                Err(err) => err.to_string(),
            },
        };

        let _ = tx.send(LiveFrame::Reconnecting {
            reason,
            delay: backoff,
        });
        sleep(backoff).await;
        backoff = next_live_reconnect_backoff(backoff);
    }
}

async fn read_websocket<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    tx: &mpsc::UnboundedSender<LiveFrame>,
) -> String
where
    tokio_tungstenite::WebSocketStream<S>:
        futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(frame) = socket.next().await {
        match frame {
            Ok(Message::Text(text)) => match serde_json::from_str::<WsEnvelope<EventDto>>(&text) {
                Ok(envelope)
                    if envelope.kind == "timeline.event"
                        && envelope.account_id == envelope.payload.account_id =>
                {
                    let _ = tx.send(LiveFrame::Timeline(Box::new(envelope.payload)));
                }
                Ok(envelope) if envelope.kind == "timeline.event" => {
                    let _ = tx.send(LiveFrame::ProtocolError(
                        "live frame account_id did not match payload".to_owned(),
                    ));
                }
                Ok(_) => {}
                Err(err) => {
                    let _ = tx.send(LiveFrame::ProtocolError(err.to_string()));
                }
            },
            Ok(Message::Close(_)) => return "websocket closed".to_owned(),
            Ok(_) => {}
            Err(err) => return err.to_string(),
        }
    }
    "websocket closed".to_owned()
}

fn next_live_reconnect_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(LIVE_RECONNECT_MAX_BACKOFF)
}

#[derive(Debug)]
pub enum LiveFrame {
    Connected,
    Reconnecting { reason: String, delay: Duration },
    Disconnected(String),
    ProtocolError(String),
    Timeline(Box<EventDto>),
}

#[derive(Debug, Deserialize)]
pub struct ApiResponse<T> {
    pub data: T,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize)]
pub struct SendResultDto {
    pub event_id: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AccountDto {
    pub account_id: Uuid,
    pub user_id: String,
    pub state: AccountState,
    #[serde(default)]
    pub verified: Option<bool>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountState {
    Active,
    Deactivated,
    Deleting,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoomDto {
    pub account_id: Uuid,
    #[serde(default)]
    pub account_user_id: Option<String>,
    pub room_id: String,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub avatar_url: Option<String>,
    pub canonical_alias: Option<String>,
    pub last_activity_ts: i64,
    pub last_event_id: Option<String>,
}

impl RoomDto {
    pub fn title(&self) -> &str {
        self.name
            .as_deref()
            .or(self.canonical_alias.as_deref())
            .unwrap_or(&self.room_id)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimelinePage {
    pub events: Vec<EventDto>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventDto {
    pub account_id: Uuid,
    pub event_id: String,
    pub room_id: String,
    pub sender: String,
    #[serde(default)]
    pub state_key: Option<String>,
    pub origin_ts: i64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub content: Option<Value>,
    pub body: Option<String>,
    pub relates_to: Option<Value>,
    pub redacted: bool,
    pub redaction_event_id: Option<String>,
}

impl EventDto {
    pub fn display_body(&self) -> String {
        if self.redacted {
            return "[redacted]".to_owned();
        }
        if let Some(membership) = self.membership_change() {
            return match membership.as_str() {
                "join" => format!("{} joined the room", self.sender),
                "leave" => format!("{} left the room", self.sender),
                "ban" => format!("{} was banned from the room", self.sender),
                "invite" => format!("{} was invited to the room", self.sender),
                _ => format!("{} membership changed: {membership}", self.sender),
            };
        }
        // Describe media messages by type + filename instead of falling through
        // to the raw event-type label.
        if let Some(label) = self.media_label() {
            return label;
        }
        if let Some(body) = &self.body {
            return body.clone();
        }
        if self.content.is_none() {
            return "[unable to decrypt]".to_owned();
        }
        format!("[{}]", self.event_type)
    }

    /// A human-readable label for media message types (`m.image`, `m.file`,
    /// `m.audio`, `m.video`, `m.sticker`). Returns `None` for text messages
    /// and non-message events.
    fn media_label(&self) -> Option<String> {
        let content = self.content.as_ref()?;
        let msgtype = content.get("msgtype").and_then(|v| v.as_str());
        // m.sticker events don't have a msgtype but have their own event type
        let is_sticker = self.event_type == "m.sticker";
        let explicit_filename = content.get("filename").and_then(|v| v.as_str());
        let body_text = content.get("body").and_then(|v| v.as_str());
        // When `filename` is absent `body` is the filename. When both are present
        // and differ, `body` is a user-authored caption shown as a second line.
        let filename = explicit_filename.or(body_text).unwrap_or("media");
        let caption = explicit_filename
            .is_some()
            .then(|| body_text.filter(|b| *b != filename))
            .flatten();
        let suffix = caption.map(|c| format!("\n{c}")).unwrap_or_default();
        match (msgtype, is_sticker) {
            (Some("m.image"), _) => Some(format!("[image: {filename}]{suffix}")),
            (Some("m.file"), _) => Some(format!("[file: {filename}]{suffix}")),
            (Some("m.audio"), _) => Some(format!("[audio: {filename}]{suffix}")),
            (Some("m.video"), _) => Some(format!("[video: {filename}]{suffix}")),
            (_, true) => Some(format!("[sticker: {filename}]{suffix}")),
            _ => None,
        }
    }

    /// Returns `true` when this image/sticker event uses encrypted media
    /// (`content.file.url`) rather than a plain `content.url`. The Axon media
    /// proxy attempts server-side decryption, but may not have the key for
    /// older messages — in that case it returns raw ciphertext.
    pub fn image_is_encrypted(&self) -> bool {
        let Some(content) = self.content.as_ref() else {
            return false;
        };
        let msgtype = content.get("msgtype").and_then(|v| v.as_str());
        let is_image = matches!(msgtype, Some("m.image")) || self.event_type == "m.sticker";
        is_image
            && content.get("url").and_then(|v| v.as_str()).is_none()
            && content
                .get("file")
                .and_then(|f| f.get("url"))
                .and_then(|v| v.as_str())
                .is_some()
    }

    /// Extract the `mxc://` URI and account from an image or sticker event.
    /// Returns `(account_id, mxc_url)` when the event carries a downloadable
    /// image, `None` otherwise.
    pub fn image_mxc(&self) -> Option<(Uuid, String)> {
        let content = self.content.as_ref()?;
        let msgtype = content.get("msgtype").and_then(|v| v.as_str());
        let is_image = matches!(msgtype, Some("m.image")) || self.event_type == "m.sticker";
        if !is_image {
            return None;
        }
        // Plain media has `content.url`; encrypted media has `content.file.url`.
        // The Axon media proxy handles decryption server-side, so both paths
        // yield decodable image bytes.
        let url = content
            .get("url")
            .and_then(|v| v.as_str())
            .or_else(|| content.get("file")?.get("url")?.as_str())?;
        if !url.starts_with("mxc://") {
            return None;
        }
        Some((self.account_id, url.to_owned()))
    }

    pub fn formatted_body(&self) -> Option<&str> {
        let content = self.content.as_ref()?;
        (content.get("format")?.as_str()? == "org.matrix.custom.html")
            .then(|| content.get("formatted_body")?.as_str())
            .flatten()
    }

    pub fn is_message_event(&self) -> bool {
        matches!(
            self.event_type.as_str(),
            "m.room.message" | "m.room.encrypted" | "m.sticker"
        )
    }

    pub fn is_membership_event(&self) -> bool {
        self.event_type == "m.room.member"
    }

    pub fn membership_change(&self) -> Option<String> {
        (self.event_type == "m.room.member")
            .then(|| {
                self.content
                    .as_ref()
                    .and_then(|content| content.get("membership"))
                    .and_then(|membership| membership.as_str())
                    .map(str::to_owned)
            })
            .flatten()
    }

    pub fn edit_relation(&self) -> Option<(&str, &str)> {
        let relates_to = self.relates_to.as_ref()?;
        if relates_to.get("rel_type")?.as_str()? != "m.replace" {
            return None;
        }
        let target = relates_to.get("event_id")?.as_str()?;
        let new_body = self
            .content
            .as_ref()
            .and_then(|c| c.get("m.new_content"))
            .and_then(|nc| nc.get("body"))
            .and_then(|b| b.as_str())?;
        Some((target, new_body))
    }

    pub fn reaction_annotation(&self) -> Option<(&str, &str)> {
        if self.event_type != "m.reaction" {
            return None;
        }
        let relates_to = self.relates_to.as_ref()?;
        if relates_to.get("rel_type")?.as_str()? != "m.annotation" {
            return None;
        }
        let target = relates_to.get("event_id")?.as_str()?;
        let key = relates_to.get("key")?.as_str()?;
        Some((target, key))
    }

    pub fn state_key(&self) -> Option<&str> {
        self.state_key.as_deref()
    }

    pub fn membership_display_name(&self) -> Option<&str> {
        (self.event_type == "m.room.member")
            .then(|| {
                self.content
                    .as_ref()
                    .and_then(|content| content.get("displayname"))
                    .and_then(|display_name| display_name.as_str())
            })
            .flatten()
    }
}

#[derive(Debug, Deserialize)]
pub struct WsEnvelope<T> {
    #[serde(rename = "type")]
    pub kind: String,
    pub account_id: Uuid,
    pub payload: T,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    Request(String),
    #[error("invalid API JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid base URL: {0}")]
    Url(String),
    #[error("unsupported base URL scheme: {0}")]
    UnsupportedScheme(String),
    #[error("HTTP {status}: {message}")]
    Status { status: StatusCode, message: String },
}

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        ApiError::Request(if err.is_timeout() {
            "request timed out".to_owned()
        } else {
            format!("request failed: {err}")
        })
    }
}

fn path_segment(value: &str) -> Escaped<'_> {
    Escaped(value)
}

struct Escaped<'a>(&'a str);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    write!(f, "{}", byte as char)?;
                }
                _ => write!(f, "%{byte:02X}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_room_response() {
        let body = r##"{
            "data": [{
                "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "account_user_id": "@alice:localhost",
                "room_id": "!room:localhost",
                "name": "Ops",
                "topic": null,
                "avatar_url": "mxc://localhost/avatar",
                "canonical_alias": "#ops:localhost",
                "last_activity_ts": 1234,
                "last_event_id": "$event:localhost"
            }]
        }"##;
        let response: ApiResponse<Vec<RoomDto>> = serde_json::from_str(body).unwrap();
        assert_eq!(response.data[0].title(), "Ops");
        assert_eq!(
            response.data[0].canonical_alias.as_deref(),
            Some("#ops:localhost")
        );
        assert_eq!(
            response.data[0].account_user_id.as_deref(),
            Some("@alice:localhost")
        );
    }

    #[test]
    fn deserializes_room_response_without_account_user_id() {
        let body = r##"{
            "data": [{
                "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "room_id": "!room:localhost",
                "name": "Ops",
                "topic": null,
                "avatar_url": "mxc://localhost/avatar",
                "canonical_alias": "#ops:localhost",
                "last_activity_ts": 1234,
                "last_event_id": "$event:localhost"
            }]
        }"##;
        let response: ApiResponse<Vec<RoomDto>> = serde_json::from_str(body).unwrap();
        assert_eq!(response.data[0].title(), "Ops");
        assert_eq!(response.data[0].account_user_id, None);
    }

    #[test]
    fn deserializes_timeline_response() {
        let body = r#"{
            "data": {
                "events": [{
                    "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                    "event_id": "$event:localhost",
                    "room_id": "!room:localhost",
                    "sender": "@alice:localhost",
                    "state_key": null,
                    "origin_ts": 1234,
                    "type": "m.room.message",
                    "content": { "msgtype": "m.text", "body": "hello" },
                    "body": "hello",
                    "relates_to": null,
                    "redacted": false,
                    "redaction_event_id": null
                }],
                "next_cursor": "MTAuMQ"
            }
        }"#;
        let response: ApiResponse<TimelinePage> = serde_json::from_str(body).unwrap();
        assert_eq!(response.data.events[0].display_body(), "hello");
        assert_eq!(response.data.next_cursor.as_deref(), Some("MTAuMQ"));
    }

    #[test]
    fn formatted_body_requires_matrix_html_format() {
        let body = r#"{
            "data": {
                "events": [{
                    "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                    "event_id": "$event:localhost",
                    "room_id": "!room:localhost",
                    "sender": "@alice:localhost",
                    "origin_ts": 1234,
                    "type": "m.room.message",
                    "content": {
                        "msgtype": "m.text",
                        "body": "hello",
                        "format": "org.matrix.custom.html",
                        "formatted_body": "<strong>hello</strong>"
                    },
                    "body": "hello",
                    "relates_to": null,
                    "redacted": false,
                    "redaction_event_id": null
                }],
                "next_cursor": null
            }
        }"#;
        let response: ApiResponse<TimelinePage> = serde_json::from_str(body).unwrap();
        assert_eq!(
            response.data.events[0].formatted_body(),
            Some("<strong>hello</strong>")
        );
    }

    #[test]
    fn deserializes_websocket_frame() {
        let body = r#"{
            "type": "timeline.event",
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "payload": {
                "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "event_id": "$event:localhost",
                "room_id": "!room:localhost",
                "sender": "@alice:localhost",
                "state_key": "@alice:localhost",
                "origin_ts": 1234,
                "type": "m.room.message",
                "content": { "msgtype": "m.text", "body": "hello" },
                "body": "hello",
                "relates_to": null,
                "redacted": false,
                "redaction_event_id": null
            }
        }"#;
        let frame: WsEnvelope<EventDto> = serde_json::from_str(body).unwrap();
        assert_eq!(frame.kind, "timeline.event");
        assert_eq!(frame.payload.room_id, "!room:localhost");
        assert_eq!(frame.payload.state_key(), Some("@alice:localhost"));
    }

    #[test]
    fn live_reconnect_backoff_doubles_until_cap() {
        assert_eq!(
            next_live_reconnect_backoff(Duration::from_secs(1)),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_live_reconnect_backoff(Duration::from_secs(16)),
            Duration::from_secs(30)
        );
        assert_eq!(
            next_live_reconnect_backoff(Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn timeout_buckets_are_correct() {
        let client = AxonClient::new("http://127.0.0.1:8080".to_owned(), None);
        let read = read_request(client.http.get("http://127.0.0.1:8080/v1/accounts"))
            .build()
            .unwrap();
        let mutation = message_mutation(
            client
                .http
                .post("http://127.0.0.1:8080/v1/accounts/id/rooms/room/send"),
        )
        .build()
        .unwrap();
        let lc = lifecycle(client.http.post("http://127.0.0.1:8080/v1/accounts/login"))
            .build()
            .unwrap();

        assert_eq!(read.timeout(), Some(&HTTP_REQUEST_TIMEOUT));
        assert_eq!(mutation.timeout(), Some(&MESSAGE_MUTATION_TIMEOUT));
        assert_eq!(
            lc.timeout(),
            Some(&LIFECYCLE_TIMEOUT),
            "lifecycle ops need a generous timeout to survive megolm key import"
        );
    }

    #[test]
    fn escapes_path_segments() {
        assert_eq!(
            path_segment("$event:local/host").to_string(),
            "%24event%3Alocal%2Fhost"
        );
    }

    #[test]
    fn deserializes_account_response() {
        let body = r#"{
            "data": {
                "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "user_id": "@alice:example.com",
                "homeserver_url": "https://example.com",
                "device_id": "DEVICE",
                "state": "active",
                "verified": null,
                "created_at": "2026-06-10T00:00:00Z",
                "updated_at": "2026-06-10T00:00:00Z"
            }
        }"#;
        let response: ApiResponse<AccountDto> = serde_json::from_str(body).unwrap();
        assert_eq!(response.data.user_id, "@alice:example.com");
        assert_eq!(response.data.state, AccountState::Active);
    }
}
