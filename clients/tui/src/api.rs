use std::collections::HashMap;
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
/// Media downloads run on a shared worker pool, so a stalled response must not
/// hold one of those workers forever.
const MEDIA_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MEDIA_BYTES: usize = 20 * 1024 * 1024;

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
        formatted: Option<(&str, &str)>,
    ) -> Result<SendResultDto, ApiError> {
        let request = self.http.post(format!(
            "{}/v1/accounts/{}/rooms/{}/send",
            self.base_url,
            account_id,
            path_segment(room_id)
        ));
        let mut payload = serde_json::json!({ "body": body });
        if let Some((fmt, fb)) = formatted {
            payload["format"] = serde_json::json!(fmt);
            payload["formatted_body"] = serde_json::json!(fb);
        }
        let request = message_mutation(request).json(&payload);
        self.send(request).await
    }

    pub async fn edit_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        body: &str,
        formatted: Option<(&str, &str)>,
    ) -> Result<SendResultDto, ApiError> {
        let request = self.http.put(format!(
            "{}/v1/accounts/{}/rooms/{}/events/{}",
            self.base_url,
            account_id,
            path_segment(room_id),
            path_segment(event_id)
        ));
        let mut payload = serde_json::json!({ "body": body });
        if let Some((fmt, fb)) = formatted {
            payload["format"] = serde_json::json!(fmt);
            payload["formatted_body"] = serde_json::json!(fb);
        }
        let request = message_mutation(request).json(&payload);
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
    /// extracted from the URI and placed in the path. Responses larger than
    /// 20 MiB are refused so one event cannot exhaust the TUI's memory.
    pub async fn get_media(&self, account_id: Uuid, mxc_url: &str) -> Result<Vec<u8>, ApiError> {
        let rest = mxc_url
            .strip_prefix("mxc://")
            .ok_or_else(|| ApiError::Url("not an mxc:// URI".to_owned()))?;
        let (server, media_id) = rest
            .split_once('/')
            .ok_or_else(|| ApiError::Url("malformed mxc:// URI".to_owned()))?;
        if server.is_empty() || media_id.is_empty() {
            return Err(ApiError::Url("malformed mxc:// URI".to_owned()));
        }
        let request = media_request(self.http.get(format!(
            "{}/v1/media/{}/{}/{}",
            self.base_url,
            account_id,
            path_segment(server),
            path_segment(media_id),
        )));
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            if response
                .content_length()
                .is_some_and(|length| length > MAX_MEDIA_BYTES as u64)
            {
                return Err(ApiError::Url(format!(
                    "media exceeds {} MiB limit",
                    MAX_MEDIA_BYTES / 1024 / 1024
                )));
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                if bytes.len().saturating_add(chunk.len()) > MAX_MEDIA_BYTES {
                    return Err(ApiError::Url(format!(
                        "media exceeds {} MiB limit",
                        MAX_MEDIA_BYTES / 1024 / 1024
                    )));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
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

    /// Start an outgoing SAS flow against `device_id` (ADR 0028 §1). The returned
    /// `flow_id` is stable for the flow's lifetime.
    pub async fn start_verification(
        &self,
        account_id: Uuid,
        device_id: &str,
    ) -> Result<StartVerifyResponse, ApiError> {
        let request = self
            .http
            .post(format!("{}/v1/accounts/{account_id}/verify", self.base_url))
            .json(&serde_json::json!({ "device_id": device_id }));
        self.send(lifecycle(request)).await
    }

    /// List the account's active/recent verification flows. Used on reconnect to
    /// discover a request that arrived while disconnected (ADR 0028 §3).
    pub async fn list_flows(&self, account_id: Uuid) -> Result<Vec<FlowDto>, ApiError> {
        let request = self
            .http
            .get(format!("{}/v1/accounts/{account_id}/verify", self.base_url));
        self.send(read_request(request)).await
    }

    /// Re-read one flow's state. A 404 (see [`ApiError::is_not_found`]) means the
    /// server has no record of the flow — treated as an implicit cancellation by
    /// the caller (ADR 0028 §3).
    pub async fn get_flow(&self, account_id: Uuid, flow_id: &str) -> Result<FlowDto, ApiError> {
        let request = self.http.get(format!(
            "{}/v1/accounts/{}/verify/{}",
            self.base_url,
            account_id,
            path_segment(flow_id)
        ));
        self.send(read_request(request)).await
    }

    /// Confirm the SAS values match. Idempotent server-side.
    pub async fn confirm_verification(
        &self,
        account_id: Uuid,
        flow_id: &str,
    ) -> Result<(), ApiError> {
        let request = self.http.post(format!(
            "{}/v1/accounts/{}/verify/{}/confirm",
            self.base_url,
            account_id,
            path_segment(flow_id)
        ));
        self.send_no_body(lifecycle(request)).await
    }

    /// Cancel the flow. Idempotent server-side (safe on already-terminal flows).
    pub async fn cancel_verification(
        &self,
        account_id: Uuid,
        flow_id: &str,
    ) -> Result<(), ApiError> {
        let request = self.http.post(format!(
            "{}/v1/accounts/{}/verify/{}/cancel",
            self.base_url,
            account_id,
            path_segment(flow_id)
        ));
        self.send_no_body(lifecycle(request)).await
    }

    /// Fetch the per-event verification bundle (M7c / ADR 0031): the at-decrypt
    /// snapshot plus live cross-signing evidence. Returned raw for display.
    pub async fn get_verification_bundle(
        &self,
        account_id: Uuid,
        event_id: &str,
    ) -> Result<Value, ApiError> {
        let request = self.http.get(format!(
            "{}/v1/accounts/{}/events/{}/verification",
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

fn media_request(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.timeout(MEDIA_TIMEOUT)
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
            Ok(Message::Text(text)) => {
                if let Some(frame) = decode_ws_frame(&text) {
                    let _ = tx.send(frame);
                }
            }
            Ok(Message::Close(_)) => return "websocket closed".to_owned(),
            Ok(_) => {}
            Err(err) => return err.to_string(),
        }
    }
    "websocket closed".to_owned()
}

/// Decode a single `/v1/ws` text frame into the [`LiveFrame`] to forward, or
/// `None` for a frame kind the client does not consume (forward-compatibility:
/// an unknown `type` is ignored, never an error). Malformed JSON or a payload
/// that does not match its declared `type` becomes a [`LiveFrame::ProtocolError`]
/// so the failure is visible rather than silent.
fn decode_ws_frame(text: &str) -> Option<LiveFrame> {
    let envelope: WsEnvelope<Value> = match serde_json::from_str(text) {
        Ok(envelope) => envelope,
        Err(err) => return Some(LiveFrame::ProtocolError(err.to_string())),
    };
    let kind = match VerificationFrameKind::from_type(&envelope.kind) {
        Some(kind) => {
            let payload: VerificationFrameDto = match serde_json::from_value(envelope.payload) {
                Ok(payload) => payload,
                Err(err) => return Some(LiveFrame::ProtocolError(err.to_string())),
            };
            return Some(LiveFrame::Verification(VerificationFrame {
                account_id: envelope.account_id,
                kind,
                payload,
            }));
        }
        None => envelope.kind.as_str(),
    };
    match kind {
        "timeline.event" => {
            let event: EventDto = match serde_json::from_value(envelope.payload) {
                Ok(event) => event,
                Err(err) => return Some(LiveFrame::ProtocolError(err.to_string())),
            };
            if envelope.account_id != event.account_id {
                return Some(LiveFrame::ProtocolError(
                    "live frame account_id did not match payload".to_owned(),
                ));
            }
            Some(LiveFrame::Timeline(Box::new(event)))
        }
        "sender_trust.violation" => {
            let payload: SenderTrustViolationDto = match serde_json::from_value(envelope.payload) {
                Ok(payload) => payload,
                Err(err) => return Some(LiveFrame::ProtocolError(err.to_string())),
            };
            Some(LiveFrame::SenderTrustViolation {
                account_id: envelope.account_id,
                payload,
            })
        }
        _ => None,
    }
}

fn next_live_reconnect_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(LIVE_RECONNECT_MAX_BACKOFF)
}

#[derive(Debug)]
pub enum LiveFrame {
    Connected,
    Reconnecting {
        reason: String,
        delay: Duration,
    },
    Disconnected(String),
    ProtocolError(String),
    Timeline(Box<EventDto>),
    /// A `verification.*` SAS frame (ADR 0027/0028). The lossy broadcast bus may
    /// drop frames, so the client treats these as hints and re-reads
    /// `GET …/verify/{flow_id}` on reconnect (ADR 0028 §3).
    Verification(VerificationFrame),
    /// A `sender_trust.violation` overlay frame (ADR 0031 / M7c).
    SenderTrustViolation {
        account_id: Uuid,
        payload: SenderTrustViolationDto,
    },
}

/// Which `verification.*` frame this is. Mirrors the server's frame-kind tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationFrameKind {
    Requested,
    Sas,
    Done,
    Cancelled,
}

impl VerificationFrameKind {
    /// Map a `/v1/ws` envelope `type` tag to its kind, or `None` if the tag is
    /// not a verification frame.
    fn from_type(kind: &str) -> Option<Self> {
        match kind {
            "verification.requested" => Some(Self::Requested),
            "verification.sas" => Some(Self::Sas),
            "verification.done" => Some(Self::Done),
            "verification.cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A decoded `verification.*` frame: its kind plus the wire payload.
#[derive(Debug, Clone)]
pub struct VerificationFrame {
    pub account_id: Uuid,
    pub kind: VerificationFrameKind,
    pub payload: VerificationFrameDto,
}

/// The wire payload shared by all `verification.*` frames. Fields that don't
/// apply to a given stage are omitted by the server: `emoji`/`decimals` only on
/// `verification.sas`, `reason` only on `verification.cancelled`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct VerificationFrameDto {
    pub flow_id: String,
    pub device_id: String,
    #[serde(default)]
    pub emoji: Option<Vec<EmojiDto>>,
    #[serde(default)]
    pub decimals: Option<[u16; 3]>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// The wire payload for a `sender_trust.violation` frame.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SenderTrustViolationDto {
    pub user_id: String,
    #[serde(default)]
    pub verification_violation: bool,
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

/// Response from `POST …/verify`: the transaction id for the new flow.
#[derive(Debug, Clone, Deserialize)]
pub struct StartVerifyResponse {
    pub flow_id: String,
}

/// A replayable SAS verification flow as returned by `GET …/verify` and
/// `GET …/verify/{flow_id}`. Mirrors the server's `FlowDto`
/// (`crates/axon-api/src/dto.rs`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct FlowDto {
    pub flow_id: String,
    pub device_id: String,
    pub stage: FlowStage,
    #[serde(default)]
    pub emoji: Option<Vec<EmojiDto>>,
    #[serde(default)]
    pub decimals: Option<[u16; 3]>,
    #[serde(default)]
    pub cancel_reason: Option<String>,
}

/// The lifecycle stage of a verification flow (server `FlowStageDto`).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowStage {
    Requested,
    Ready,
    KeysExchanged,
    Confirmed,
    Done,
    Cancelled,
}

/// One SAS emoji: the symbol and its short English description.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct EmojiDto {
    pub symbol: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AccountDto {
    pub account_id: Uuid,
    pub user_id: String,
    pub state: AccountState,
    #[serde(default)]
    pub device_id: Option<String>,
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
    /// Server-aggregated per-emoji reaction tally (M8), keyed by reaction key.
    /// The collapsed timeline strips raw `m.reaction` rows, so reaction badges and
    /// the ids needed to withdraw a reaction come from here rather than from
    /// scanning events. `None` on the live `/v1/ws` stream and for events with no
    /// reactions.
    #[serde(default)]
    pub reactions: Option<HashMap<String, ReactionTally>>,
}

/// One emoji's aggregated tally on an [`EventDto`], mirroring the API's
/// `ReactionDto`. `my_event_ids` are the account user's own reaction events for
/// the key — the ids redacted to withdraw the reaction.
#[derive(Debug, Clone, Deserialize)]
pub struct ReactionTally {
    pub count: i64,
    pub me: bool,
    // `senders` is part of the wire shape but unused by the TUI, so it is left
    // out here — serde ignores the extra key.
    #[serde(default)]
    pub my_event_ids: Vec<String>,
    /// Sender-device trust snapshot at decrypt time (M7c / ADR 0031): one of
    /// `verified`, `unverified`, `unknown`, `verification_violation`. `None` for
    /// unencrypted events or rows with no recorded verdict.
    #[serde(default)]
    pub sender_trust: Option<String>,
}

#[derive(Clone, Copy)]
enum MediaKind {
    Image,
    File,
    Audio,
    Video,
    Sticker,
}

struct ParsedMedia<'a> {
    kind: MediaKind,
    url: Option<&'a str>,
    filename: &'a str,
    caption: Option<&'a str>,
    encrypted: bool,
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
        let media = self.parsed_media()?;
        let kind = match media.kind {
            MediaKind::Image => "image",
            MediaKind::File => "file",
            MediaKind::Audio => "audio",
            MediaKind::Video => "video",
            MediaKind::Sticker => "sticker",
        };
        let suffix = media
            .caption
            .map(|caption| format!("\n{caption}"))
            .unwrap_or_default();
        Some(format!("[{kind}: {}]{suffix}", media.filename))
    }

    /// Returns `true` when this image/sticker event uses encrypted media
    /// (`content.file.url`) rather than a plain `content.url`. The Axon media
    /// proxy attempts server-side decryption, but may not have the key for
    /// older messages — in that case it returns raw ciphertext.
    pub fn image_is_encrypted(&self) -> bool {
        self.image_media().is_some_and(|media| media.encrypted)
    }

    /// Extract the `mxc://` URI and account from an image or sticker event.
    /// Returns `(account_id, mxc_url)` when the event carries a downloadable
    /// image, `None` otherwise.
    pub fn image_mxc(&self) -> Option<(Uuid, String)> {
        let media = self.image_media()?;
        Some((self.account_id, media.url?.to_owned()))
    }

    /// Returns the filename for an image/sticker event (the `filename` field if
    /// present, otherwise `body`). Returns `None` for non-image events.
    pub fn image_filename(&self) -> Option<String> {
        Some(self.image_media()?.filename.to_owned())
    }

    /// Returns the user-authored caption for an image/sticker event — present
    /// only when `filename` and `body` are both set and differ. Returns `None`
    /// for non-image events or images without a caption.
    pub fn image_caption(&self) -> Option<String> {
        self.image_media()?.caption.map(str::to_owned)
    }

    fn parsed_media(&self) -> Option<ParsedMedia<'_>> {
        let content = self.content.as_ref()?;
        let kind = match content.get("msgtype").and_then(|value| value.as_str()) {
            Some("m.image") => MediaKind::Image,
            Some("m.file") => MediaKind::File,
            Some("m.audio") => MediaKind::Audio,
            Some("m.video") => MediaKind::Video,
            _ if self.event_type == "m.sticker" => MediaKind::Sticker,
            _ => return None,
        };
        let explicit_filename = content.get("filename").and_then(|value| value.as_str());
        let body = content.get("body").and_then(|value| value.as_str());
        let filename = explicit_filename.or(body).unwrap_or("media");
        let caption = explicit_filename.and_then(|_| body.filter(|body| *body != filename));
        let plain_url = content.get("url").and_then(|value| value.as_str());
        let encrypted_url = content
            .get("file")
            .and_then(|file| file.get("url"))
            .and_then(|value| value.as_str());
        let url = plain_url.or(encrypted_url);
        Some(ParsedMedia {
            kind,
            url,
            filename,
            caption,
            encrypted: plain_url.is_none() && encrypted_url.is_some(),
        })
    }

    fn image_media(&self) -> Option<ParsedMedia<'_>> {
        let media = self.parsed_media()?;
        if !matches!(media.kind, MediaKind::Image | MediaKind::Sticker) {
            return None;
        }
        if !media.url.is_some_and(|url| url.starts_with("mxc://")) {
            return None;
        }
        Some(media)
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

    pub fn edit_relation(&self) -> Option<(&str, &str, &Value)> {
        let relates_to = self.relates_to.as_ref()?;
        if relates_to.get("rel_type")?.as_str()? != "m.replace" {
            return None;
        }
        let target = relates_to.get("event_id")?.as_str()?;
        let new_content = self.content.as_ref()?.get("m.new_content")?;
        let new_body = new_content.get("body")?.as_str()?;
        Some((target, new_body, new_content))
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

impl ApiError {
    /// `true` when this is an HTTP 404. Used to detect a verification flow the
    /// server no longer has a record of (ADR 0028 §3).
    pub fn is_not_found(&self) -> bool {
        matches!(self, ApiError::Status { status, .. } if *status == StatusCode::NOT_FOUND)
    }
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
    fn edit_relation_preserves_formatted_new_content() {
        let event: EventDto = serde_json::from_value(serde_json::json!({
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "event_id": "$edit:localhost",
            "room_id": "!room:localhost",
            "sender": "@alice:localhost",
            "origin_ts": 1234,
            "type": "m.room.message",
            "content": {
                "msgtype": "m.text",
                "body": "* hello",
                "m.new_content": {
                    "msgtype": "m.text",
                    "body": "hello",
                    "format": "org.matrix.custom.html",
                    "formatted_body": "<strong>hello</strong>"
                }
            },
            "body": "* hello",
            "relates_to": {
                "rel_type": "m.replace",
                "event_id": "$original:localhost"
            },
            "redacted": false,
            "redaction_event_id": null
        }))
        .expect("valid event");

        let (target, body, new_content) = event.edit_relation().expect("edit relation");
        assert_eq!(target, "$original:localhost");
        assert_eq!(body, "hello");
        assert_eq!(
            new_content.get("formatted_body").and_then(Value::as_str),
            Some("<strong>hello</strong>")
        );
    }

    #[test]
    fn image_accessors_share_encrypted_media_metadata() {
        let event: EventDto = serde_json::from_value(serde_json::json!({
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "event_id": "$event:localhost",
            "room_id": "!room:localhost",
            "sender": "@alice:localhost",
            "origin_ts": 1234,
            "type": "m.room.message",
            "content": {
                "msgtype": "m.image",
                "body": "A caption",
                "filename": "photo.jpg",
                "file": { "url": "mxc://localhost/photo" }
            },
            "body": "A caption",
            "relates_to": null,
            "redacted": false,
            "redaction_event_id": null
        }))
        .unwrap();

        assert_eq!(
            event.image_mxc(),
            Some((
                Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
                "mxc://localhost/photo".to_owned()
            ))
        );
        assert!(event.image_is_encrypted());
        assert_eq!(event.image_filename().as_deref(), Some("photo.jpg"));
        assert_eq!(event.image_caption().as_deref(), Some("A caption"));
        assert_eq!(event.display_body(), "[image: photo.jpg]\nA caption");
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
    fn demux_routes_timeline_frame() {
        let body = r#"{
            "type": "timeline.event",
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "payload": {
                "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "event_id": "$e:localhost", "room_id": "!r:localhost",
                "sender": "@a:localhost", "origin_ts": 1, "type": "m.room.message",
                "content": null, "body": "hi", "relates_to": null,
                "redacted": false, "redaction_event_id": null
            }
        }"#;
        assert!(matches!(
            decode_ws_frame(body),
            Some(LiveFrame::Timeline(_))
        ));
    }

    #[test]
    fn demux_timeline_account_mismatch_is_protocol_error() {
        let body = r#"{
            "type": "timeline.event",
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "payload": {
                "account_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "event_id": "$e:localhost", "room_id": "!r:localhost",
                "sender": "@a:localhost", "origin_ts": 1, "type": "m.room.message",
                "content": null, "body": "hi", "relates_to": null,
                "redacted": false, "redaction_event_id": null
            }
        }"#;
        assert!(matches!(
            decode_ws_frame(body),
            Some(LiveFrame::ProtocolError(_))
        ));
    }

    #[test]
    fn demux_routes_verification_sas_frame() {
        let body = r#"{
            "type": "verification.sas",
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "payload": {
                "flow_id": "txn1", "device_id": "DEV",
                "emoji": [{"symbol": "🐶", "description": "Dog"}],
                "decimals": [1, 2, 3]
            }
        }"#;
        match decode_ws_frame(body) {
            Some(LiveFrame::Verification(frame)) => {
                assert_eq!(frame.kind, VerificationFrameKind::Sas);
                assert_eq!(frame.payload.flow_id, "txn1");
                assert_eq!(frame.payload.decimals, Some([1, 2, 3]));
                assert_eq!(frame.payload.emoji.unwrap()[0].symbol, "🐶");
            }
            other => panic!("expected verification frame, got {other:?}"),
        }
    }

    #[test]
    fn demux_routes_verification_cancelled_frame() {
        let body = r#"{
            "type": "verification.cancelled",
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "payload": { "flow_id": "txn1", "device_id": "DEV", "reason": "user" }
        }"#;
        match decode_ws_frame(body) {
            Some(LiveFrame::Verification(frame)) => {
                assert_eq!(frame.kind, VerificationFrameKind::Cancelled);
                assert_eq!(frame.payload.reason.as_deref(), Some("user"));
            }
            other => panic!("expected cancelled frame, got {other:?}"),
        }
    }

    #[test]
    fn demux_routes_sender_trust_violation_frame() {
        let body = r#"{
            "type": "sender_trust.violation",
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "payload": { "user_id": "@mallory:localhost", "verification_violation": true }
        }"#;
        match decode_ws_frame(body) {
            Some(LiveFrame::SenderTrustViolation { payload, .. }) => {
                assert_eq!(payload.user_id, "@mallory:localhost");
                assert!(payload.verification_violation);
            }
            other => panic!("expected trust violation frame, got {other:?}"),
        }
    }

    #[test]
    fn demux_ignores_unknown_frame_kind() {
        let body = r#"{
            "type": "future.frame",
            "account_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "payload": {}
        }"#;
        assert!(decode_ws_frame(body).is_none());
    }

    #[test]
    fn deserializes_flow_dto_with_snake_case_stage() {
        let body = r#"{
            "data": {
                "flow_id": "txn1", "device_id": "DEV",
                "stage": "keys_exchanged",
                "emoji": [{"symbol": "🐱", "description": "Cat"}],
                "decimals": [4, 5, 6], "cancel_reason": null
            }
        }"#;
        let response: ApiResponse<FlowDto> = serde_json::from_str(body).unwrap();
        assert_eq!(response.data.stage, FlowStage::KeysExchanged);
        assert_eq!(response.data.decimals, Some([4, 5, 6]));
    }

    #[test]
    fn not_found_is_detected() {
        let err = ApiError::Status {
            status: StatusCode::NOT_FOUND,
            message: "gone".to_owned(),
        };
        assert!(err.is_not_found());
        let other = ApiError::Status {
            status: StatusCode::CONFLICT,
            message: "x".to_owned(),
        };
        assert!(!other.is_not_found());
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
        let media = media_request(
            client
                .http
                .get("http://127.0.0.1:8080/v1/media/id/server/media"),
        )
        .build()
        .unwrap();

        assert_eq!(read.timeout(), Some(&HTTP_REQUEST_TIMEOUT));
        assert_eq!(mutation.timeout(), Some(&MESSAGE_MUTATION_TIMEOUT));
        assert_eq!(
            lc.timeout(),
            Some(&LIFECYCLE_TIMEOUT),
            "lifecycle ops need a generous timeout to survive megolm key import"
        );
        assert_eq!(
            media.timeout(),
            Some(&MEDIA_TIMEOUT),
            "stalled media responses must release the bounded worker pool"
        );
    }

    #[test]
    fn escapes_path_segments() {
        assert_eq!(
            path_segment("$event:local/host").to_string(),
            "%24event%3Alocal%2Fhost"
        );
    }

    #[tokio::test]
    async fn rejects_empty_mxc_server_or_media_id() {
        let client = AxonClient::new("http://127.0.0.1:8080".to_owned(), None);

        assert!(matches!(
            client.get_media(Uuid::nil(), "mxc:///media").await,
            Err(ApiError::Url(_))
        ));
        assert!(matches!(
            client.get_media(Uuid::nil(), "mxc://server/").await,
            Err(ApiError::Url(_))
        ));
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
        assert_eq!(response.data.device_id.as_deref(), Some("DEVICE"));
    }
}
