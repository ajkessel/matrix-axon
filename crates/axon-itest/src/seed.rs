//! Integration-test seeder — plays "device A", a normal verified Matrix client.
//!
//! It logs in, creates an *encrypted* room, sends a handful of messages and an
//! optional attachment, then enables Secure Backup (4S) so the megolm keys are
//! uploaded to the server key backup and a fresh **recovery key** is minted. It
//! prints a single line of JSON to stdout describing what it did:
//!
//! ```json
//! {"user_id":"@alice-…:localhost","device_id":"…","room_id":"!…:localhost",
//!  "recovery_key":"EsT… …","messages":["seed message 0", …],
//!  "media_event_id":"$…"}
//! ```
//!
//! axon (the unverified "device B" under test) then syncs the room as UTDs and,
//! once handed the `recovery_key`, recovers the backup and back-fills them.
//!
//! All logging goes to stderr so stdout stays clean JSON for the caller to
//! parse. Configured entirely via env vars (see [`Config::from_env`]).

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use matrix_sdk::{
    attachment::AttachmentConfig,
    ruma::{
        api::client::room::create_room::v3::Request as CreateRoomRequest,
        events::room::message::RoomMessageEventContent, OwnedUserId,
    },
    Client,
};

struct Config {
    homeserver: String,
    user_id: String,
    password: String,
    store_dir: PathBuf,
    room_name: String,
    message_count: u32,
    media_file: Option<PathBuf>,
}

impl Config {
    fn from_env() -> Result<Self> {
        let user_id = env("SEED_USER_ID")?;
        Ok(Self {
            homeserver: std::env::var("SEED_HOMESERVER")
                .unwrap_or_else(|_| "http://localhost:8008".to_owned()),
            password: env("SEED_PASSWORD")?,
            store_dir: PathBuf::from(env("SEED_STORE_DIR")?),
            room_name: std::env::var("SEED_ROOM_NAME").unwrap_or_else(|_| "axon-itest".to_owned()),
            message_count: std::env::var("SEED_MESSAGE_COUNT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            media_file: std::env::var_os("SEED_MEDIA_FILE").map(PathBuf::from),
            user_id,
        })
    }
}

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,matrix_sdk::sliding_sync=warn".into()),
        )
        .init();

    let config = Config::from_env()?;
    let outcome = seed(&config).await?;
    // Clean, single-line JSON on stdout for the orchestration script to parse.
    println!("{}", serde_json::to_string(&outcome)?);
    Ok(())
}

/// The machine-readable result the orchestration script consumes.
#[derive(serde::Serialize)]
struct Outcome {
    user_id: String,
    device_id: String,
    room_id: String,
    recovery_key: String,
    messages: Vec<String>,
    media_event_id: Option<String>,
}

async fn seed(config: &Config) -> Result<Outcome> {
    let user_id = OwnedUserId::try_from(config.user_id.as_str())
        .with_context(|| format!("invalid user id {}", config.user_id))?;

    tokio::fs::create_dir_all(&config.store_dir)
        .await
        .with_context(|| format!("creating store dir {}", config.store_dir.display()))?;

    let client = Client::builder()
        .homeserver_url(&config.homeserver)
        .sqlite_store(&config.store_dir, Some("seed-store-key"))
        .build()
        .await
        .context("building client")?;

    let login = client
        .matrix_auth()
        .login_username(&user_id, &config.password)
        .initial_device_display_name("axon-itest-seeder")
        .send()
        .await
        .context("login")?;
    let device_id = login.device_id.to_string();
    tracing::info!(%user_id, device_id, "logged in as seeding device");

    // Settle device lists / account data before we touch encryption.
    client
        .sync_once(matrix_sdk::config::SyncSettings::default())
        .await
        .context("initial sync")?;

    // Create an encrypted room. enable_encryption flips on m.room.encryption.
    let mut request = CreateRoomRequest::new();
    request.name = Some(config.room_name.clone());
    let room = client.create_room(request).await.context("create room")?;
    room.enable_encryption()
        .await
        .context("enable encryption")?;
    let room_id = room.room_id().to_string();
    tracing::info!(room_id, "created encrypted room");

    // Pull the new room's state/membership so sending can share keys.
    client
        .sync_once(matrix_sdk::config::SyncSettings::default())
        .await
        .context("sync after room create")?;

    // Send messages. Each creates/uses the room's outbound megolm session; the
    // matching inbound session is cached locally and gets backed up below.
    let mut messages = Vec::new();
    for i in 0..config.message_count {
        let body = format!("seed message {i}");
        room.send(RoomMessageEventContent::text_plain(&body))
            .await
            .with_context(|| format!("sending message {i}"))?;
        messages.push(body);
    }
    tracing::info!(count = config.message_count, "sent encrypted messages");

    // Send the attachment last so it remains in axon's bounded Sliding Sync
    // timeline window. In an encrypted room matrix-sdk uploads ciphertext and
    // puts the decryption metadata in the event's `content.file` object.
    let media_event_id = if let Some(path) = &config.media_file {
        let data = tokio::fs::read(path)
            .await
            .with_context(|| format!("reading media fixture {}", path.display()))?;
        let response = room
            .send_attachment(
                "media-verification.png",
                &mime::IMAGE_PNG,
                data,
                AttachmentConfig::new(),
            )
            .await
            .context("sending encrypted media attachment")?;
        tracing::info!(event_id = %response.event_id, "sent encrypted media attachment");
        Some(response.event_id.to_string())
    } else {
        None
    };

    // Enable Secure Backup: creates the server-side key backup, uploads all
    // locally cached room keys, and mints the recovery key. No cross-signing
    // bootstrap and no UIA happen here, so this is fully headless.
    let recovery_key = client
        .encryption()
        .recovery()
        .enable()
        .wait_for_backups_to_upload()
        .await
        .context("enabling recovery / uploading key backup")?;
    tracing::info!("secure backup enabled, room keys uploaded");

    if recovery_key.is_empty() {
        return Err(anyhow!("recovery key came back empty"));
    }

    Ok(Outcome {
        user_id: config.user_id.clone(),
        device_id,
        room_id,
        recovery_key,
        messages,
        media_event_id,
    })
}
