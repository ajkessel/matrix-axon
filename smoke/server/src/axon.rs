use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::{Client, Method};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION as WS_AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue as WsHeaderValue;
use uuid::Uuid;

use crate::redact::{redact_json, redact_url};
use crate::util::path_segment;
use crate::wire::{
    AccountDto, ApiResponse, EventDto, RoomDto, SendResultDto, ThreadSummaryDto, TimelinePage,
    WsEnvelope,
};

#[derive(Clone)]
pub struct AxonClient {
    http: Client,
    base_url: String,
    token: String,
    journal: Arc<Mutex<Vec<JournalEntry>>>,
}

#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub request_body: Option<Value>,
    pub response_body: Option<Value>,
    pub error: Option<String>,
}

impl AxonClient {
    pub fn new(base_url: String, token: String) -> anyhow::Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).context("build Authorization")?,
        );
        let http = Client::builder()
            .default_headers(headers)
            .build()
            .context("build Axon HTTP client")?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
            journal: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn journal(&self) -> Vec<JournalEntry> {
        self.journal.lock().expect("journal lock").clone()
    }

    pub async fn health(&self) -> anyhow::Result<Value> {
        self.get_raw("/healthz").await
    }

    pub async fn list_accounts(&self) -> anyhow::Result<Vec<AccountDto>> {
        self.get_envelope("/v1/accounts").await
    }

    pub async fn list_rooms(&self) -> anyhow::Result<Vec<RoomDto>> {
        self.get_envelope("/v1/rooms").await
    }

    pub async fn timeline(
        &self,
        account_id: Uuid,
        room_id: &str,
        limit: usize,
    ) -> anyhow::Result<TimelinePage> {
        self.get_envelope(&format!(
            "/v1/accounts/{account_id}/rooms/{}/timeline?limit={limit}",
            path_segment(room_id)
        ))
        .await
    }

    pub async fn event(&self, account_id: Uuid, event_id: &str) -> anyhow::Result<EventDto> {
        self.get_envelope(&format!(
            "/v1/accounts/{account_id}/events/{}",
            path_segment(event_id)
        ))
        .await
    }

    pub async fn replies(&self, account_id: Uuid, event_id: &str) -> anyhow::Result<Vec<EventDto>> {
        self.get_envelope(&format!(
            "/v1/accounts/{account_id}/events/{}/replies",
            path_segment(event_id)
        ))
        .await
    }

    pub async fn reactions(&self, account_id: Uuid, event_id: &str) -> anyhow::Result<Value> {
        self.get_envelope(&format!(
            "/v1/accounts/{account_id}/events/{}/reactions",
            path_segment(event_id)
        ))
        .await
    }

    pub async fn edits(&self, account_id: Uuid, event_id: &str) -> anyhow::Result<Vec<EventDto>> {
        self.get_envelope(&format!(
            "/v1/accounts/{account_id}/events/{}/edits",
            path_segment(event_id)
        ))
        .await
    }

    pub async fn threads(
        &self,
        account_id: Uuid,
        room_id: &str,
    ) -> anyhow::Result<Vec<ThreadSummaryDto>> {
        self.get_envelope(&format!(
            "/v1/accounts/{account_id}/rooms/{}/threads",
            path_segment(room_id)
        ))
        .await
    }

    pub async fn thread_timeline(
        &self,
        account_id: Uuid,
        room_id: &str,
        root_id: &str,
    ) -> anyhow::Result<TimelinePage> {
        self.get_envelope(&format!(
            "/v1/accounts/{account_id}/rooms/{}/threads/{}/timeline?limit=20",
            path_segment(room_id),
            path_segment(root_id)
        ))
        .await
    }

    pub async fn send_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        body: &str,
    ) -> anyhow::Result<SendResultDto> {
        self.post_envelope(
            &format!(
                "/v1/accounts/{account_id}/rooms/{}/send",
                path_segment(room_id)
            ),
            serde_json::json!({ "body": body }),
        )
        .await
    }

    pub async fn mark_read(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
    ) -> anyhow::Result<()> {
        let _: Value = self
            .post_envelope(
                &format!(
                    "/v1/accounts/{account_id}/rooms/{}/read",
                    path_segment(room_id)
                ),
                serde_json::json!({ "event_id": event_id }),
            )
            .await?;
        Ok(())
    }

    pub async fn read_ws_until<F>(
        &self,
        timeout: Duration,
        mut predicate: F,
    ) -> anyhow::Result<Vec<Value>>
    where
        F: FnMut(&WsEnvelope) -> bool,
    {
        let url = self.ws_url()?;
        let mut request = url
            .as_str()
            .into_client_request()
            .context("build websocket request")?;
        let value = WsHeaderValue::from_str(&format!("Bearer {}", self.token))
            .context("build websocket Authorization")?;
        request.headers_mut().insert(WS_AUTHORIZATION, value);
        let (mut socket, _) =
            tokio::time::timeout(timeout, tokio_tungstenite::connect_async(request))
                .await
                .context("connect /v1/ws timed out")?
                .context("connect /v1/ws")?;
        let deadline = tokio::time::Instant::now() + timeout;
        let mut frames = Vec::new();
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                bail!("timed out waiting for matching websocket frame");
            }
            let remaining = deadline - now;
            let frame = tokio::time::timeout(remaining, socket.next())
                .await
                .context("wait for websocket frame")?
                .ok_or_else(|| anyhow!("websocket closed"))?
                .context("read websocket frame")?;
            if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
                let raw: Value = serde_json::from_str(&text).context("parse websocket frame")?;
                let envelope: WsEnvelope =
                    serde_json::from_value(raw.clone()).context("parse websocket envelope")?;
                frames.push(raw);
                if predicate(&envelope) {
                    return Ok(frames);
                }
            }
        }
    }

    async fn get_raw(&self, path: &str) -> anyhow::Result<Value> {
        self.request(Method::GET, path, None).await
    }

    async fn get_envelope<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let raw = self.request(Method::GET, path, None).await?;
        let envelope: ApiResponse<T> = serde_json::from_value(raw)?;
        Ok(envelope.data)
    }

    async fn post_envelope<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Value,
    ) -> anyhow::Result<T> {
        let raw = self.request(Method::POST, path, Some(body)).await?;
        let envelope: ApiResponse<T> = serde_json::from_value(raw)?;
        Ok(envelope.data)
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.http.request(method.clone(), &url);
        if let Some(ref body) = body {
            request = request.json(body);
        }
        let mut entry = JournalEntry {
            method: method.as_str().to_owned(),
            url: redact_url(&url),
            status: None,
            request_body: body.clone().map(redact_json),
            response_body: None,
            error: None,
        };
        let result = async {
            let response = request.timeout(Duration::from_secs(60)).send().await?;
            entry.status = Some(response.status().as_u16());
            let status = response.status();
            let text = response.text().await?;
            let body = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
            entry.response_body = Some(redact_json(body.clone()));
            if status.is_success() {
                Ok(body)
            } else {
                bail!(
                    "{} {} failed with {status}: {}",
                    method,
                    path,
                    redact_json(body)
                )
            }
        }
        .await;
        if let Err(err) = &result {
            entry.error = Some(err.to_string());
        }
        self.journal.lock().expect("journal lock").push(entry);
        result
    }

    fn ws_url(&self) -> anyhow::Result<String> {
        let mut url = reqwest::Url::parse(&self.base_url)?;
        let scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            other => bail!("unsupported Axon URL scheme {other}"),
        };
        url.set_scheme(scheme)
            .map_err(|_| anyhow!("failed to set websocket scheme"))?;
        url.set_path("/v1/ws");
        url.set_query(None);
        Ok(url.to_string())
    }
}
