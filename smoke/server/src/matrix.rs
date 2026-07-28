use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use reqwest::{Client, Method};
use serde_json::Value;

use crate::redact::{redact_json, redact_url};
use crate::util::{path_segment, wait_for};
use crate::wire::{MatrixAccount, MatrixLoginResponse, MatrixSendResponse, MatrixSyncResponse};

#[derive(Clone)]
pub struct MatrixClient {
    http: Client,
    homeserver_url: String,
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

impl MatrixClient {
    pub fn new(homeserver_url: String) -> Self {
        Self {
            http: Client::new(),
            homeserver_url: homeserver_url.trim_end_matches('/').to_owned(),
            journal: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn journal(&self) -> Vec<JournalEntry> {
        self.journal.lock().expect("journal lock").clone()
    }

    pub async fn login(&self, account: &MatrixAccount) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "type": "m.login.password",
            "identifier": { "type": "m.id.user", "user": account.user_id },
            "password": account.password,
        });
        let response: MatrixLoginResponse = self
            .request(
                Method::POST,
                "/_matrix/client/v3/login",
                None,
                Some(body),
                Duration::from_secs(30),
            )
            .await?;
        Ok(response.access_token)
    }

    pub async fn send_message(
        &self,
        token: &str,
        room_id: &str,
        txn_id: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        let path = format!(
            "/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            path_segment(room_id),
            path_segment(txn_id)
        );
        let response: MatrixSendResponse = self
            .request(
                Method::PUT,
                &path,
                Some(token),
                Some(serde_json::json!({ "msgtype": "m.text", "body": body })),
                Duration::from_secs(30),
            )
            .await?;
        Ok(response.event_id)
    }

    /// Fetch an event straight from the homeserver's client-server API
    /// (`GET …/rooms/{roomId}/event/{eventId}`) and return its raw `content`.
    /// Used to assert the exact `m.relates_to` shape Axon sent — Axon's own
    /// `/v1/` event DTO summarizes relations rather than exposing them
    /// verbatim, so this bypasses Axon entirely for that assertion.
    pub async fn get_event_content(
        &self,
        token: &str,
        room_id: &str,
        event_id: &str,
    ) -> anyhow::Result<Value> {
        let path = format!(
            "/_matrix/client/v3/rooms/{}/event/{}",
            path_segment(room_id),
            path_segment(event_id)
        );
        let event: Value = self
            .request(
                Method::GET,
                &path,
                Some(token),
                None,
                Duration::from_secs(30),
            )
            .await?;
        event
            .get("content")
            .cloned()
            .ok_or_else(|| anyhow!("event {event_id} response had no content"))
    }

    pub async fn wait_for_event(
        &self,
        token: &str,
        room_id: &str,
        event_id: &str,
        body: &str,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        wait_for("Matrix sync event", timeout, || async {
            let sync = self.sync(token, Duration::from_millis(500)).await?;
            Ok(sync.rooms.join.get(room_id).is_some_and(|room| {
                room.timeline.events.iter().any(|event| {
                    event.event_id.as_deref() == Some(event_id)
                        && event.content.get("body").and_then(Value::as_str) == Some(body)
                })
            }))
        })
        .await
    }

    async fn sync(&self, token: &str, timeout: Duration) -> anyhow::Result<MatrixSyncResponse> {
        let millis = timeout.as_millis().to_string();
        self.request(
            Method::GET,
            &format!("/_matrix/client/v3/sync?timeout={millis}"),
            Some(token),
            None,
            timeout + Duration::from_secs(5),
        )
        .await
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        timeout: Duration,
    ) -> anyhow::Result<T> {
        let url = format!("{}{}", self.homeserver_url, path);
        let mut request = self.http.request(method.clone(), &url).timeout(timeout);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
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
            let response = request.send().await?;
            entry.status = Some(response.status().as_u16());
            let status = response.status();
            let text = response.text().await?;
            let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
            entry.response_body = Some(redact_json(value.clone()));
            if status.is_success() {
                Ok(serde_json::from_value(value)?)
            } else {
                bail!(
                    "{} {} failed with {status}: {}",
                    method,
                    path,
                    redact_json(value)
                )
            }
        }
        .await;
        if let Err(err) = &result {
            entry.error = Some(err.to_string());
        }
        self.journal.lock().expect("journal lock").push(entry);
        result.with_context(|| format!("Matrix {} {}", method, path))
    }
}
