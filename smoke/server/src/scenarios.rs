use std::time::Duration;

use anyhow::{anyhow, bail};
use serde_json::Value;

use crate::runner::{Ctx, ScenarioOutcome};
use crate::util::wait_for;
use crate::wire::{EventDto, RoomDto};

pub async fn boot_health(ctx: &Ctx) -> ScenarioOutcome {
    ScenarioOutcome::from_result(
        async {
            let health = ctx.axon.health().await?;
            if health.get("status").and_then(Value::as_str) != Some("ok") {
                bail!("unexpected /healthz response: {health}");
            }
            Ok(())
        }
        .await,
    )
}

pub async fn account_visible(ctx: &Ctx) -> ScenarioOutcome {
    ScenarioOutcome::from_result(
        async {
            let accounts = ctx.axon.list_accounts().await?;
            let account = accounts
                .iter()
                .find(|account| account.account_id == ctx.manifest.axon_account_id)
                .ok_or_else(|| anyhow!("manifest account not found in GET /v1/accounts"))?;
            if account.user_id != ctx.manifest.accounts.target.user_id {
                bail!("unexpected account user_id {}", account.user_id);
            }
            if account.homeserver_url != ctx.manifest.homeserver_url {
                bail!("unexpected homeserver_url {}", account.homeserver_url);
            }
            if account.state != "active" {
                bail!("expected active account, got {}", account.state);
            }
            if account.device_id.as_deref().unwrap_or_default().is_empty() {
                bail!("active account had no device_id");
            }
            Ok(())
        }
        .await,
    )
}

pub async fn room_list(ctx: &Ctx) -> ScenarioOutcome {
    ScenarioOutcome::from_result(
        async {
            let rooms = ctx.axon.list_rooms().await?;
            require_room(&rooms, &ctx.manifest.rooms.general.room_id, "Smoke General")?;
            require_room(
                &rooms,
                &ctx.manifest.rooms.long_timeline.room_id,
                "Smoke Timeline",
            )?;
            require_room(
                &rooms,
                &ctx.manifest.rooms.relations.room_id,
                "Smoke Relations",
            )?;
            Ok(())
        }
        .await,
    )
}

pub async fn timeline_read(ctx: &Ctx) -> ScenarioOutcome {
    ScenarioOutcome::from_result(
        async {
            let page = ctx
                .axon
                .timeline(
                    ctx.manifest.axon_account_id,
                    &ctx.manifest.rooms.general.room_id,
                    20,
                )
                .await?;
            if !page.events.iter().any(|event| {
                event
                    .body
                    .as_deref()
                    .is_some_and(|body| body.contains("general"))
            }) {
                bail!("general timeline did not contain seeded general message");
            }
            let event = page
                .events
                .iter()
                .find(|event| event.body.is_some())
                .ok_or_else(|| anyhow!("general timeline had no message event"))?;
            let fetched = ctx
                .axon
                .event(ctx.manifest.axon_account_id, &event.event_id)
                .await?;
            if fetched.event_id != event.event_id || fetched.room_id != event.room_id {
                bail!("event lookup did not match timeline event");
            }
            Ok(())
        }
        .await,
    )
}

pub async fn inbound_timeline_ws(ctx: &Ctx) -> ScenarioOutcome {
    ScenarioOutcome::from_result(
        async {
            let peer_token = ctx.matrix.login(&ctx.manifest.accounts.peer).await?;
            let body = format!("server-smoke inbound {}", &ctx.run_id[..8]);
            let txn_id = format!("server-smoke-inbound-{}", &ctx.run_id[..8]);
            let axon = ctx.axon.clone();
            let account_id = ctx.manifest.axon_account_id;
            let ws_body = body.clone();
            let ws_task = tokio::spawn(async move {
                axon.read_ws_until(Duration::from_secs(60), |frame| {
                    if frame.kind != "timeline.event" || frame.account_id != account_id {
                        return false;
                    }
                    frame
                        .payload
                        .get("body")
                        .and_then(Value::as_str)
                        .is_some_and(|seen| seen == ws_body)
                })
                .await
            });
            tokio::time::sleep(Duration::from_millis(300)).await;
            let event_id = ctx
                .matrix
                .send_message(
                    &peer_token,
                    &ctx.manifest.rooms.general.room_id,
                    &txn_id,
                    &body,
                )
                .await?;
            let frames = ws_task.await??;
            ctx.record_ws_frames(frames);
            wait_for("inbound message in Axon timeline", ctx.timeout, || async {
                let page = ctx
                    .axon
                    .timeline(
                        ctx.manifest.axon_account_id,
                        &ctx.manifest.rooms.general.room_id,
                        30,
                    )
                    .await?;
                Ok(page.events.iter().any(|event| {
                    event.event_id == event_id && event.body.as_deref() == Some(&body)
                }))
            })
            .await?;
            Ok(())
        }
        .await,
    )
}

pub async fn outbound_send(ctx: &Ctx) -> ScenarioOutcome {
    ScenarioOutcome::from_result(
        async {
            let peer_token = ctx.matrix.login(&ctx.manifest.accounts.peer).await?;
            let body = format!("server-smoke outbound {}", &ctx.run_id[..8]);
            let send = ctx
                .axon
                .send_message(
                    ctx.manifest.axon_account_id,
                    &ctx.manifest.rooms.general.room_id,
                    &body,
                )
                .await?;
            if send.event_id.is_empty() {
                bail!("send response event_id was empty");
            }
            ctx.matrix
                .wait_for_event(
                    &peer_token,
                    &ctx.manifest.rooms.general.room_id,
                    &send.event_id,
                    &body,
                    ctx.timeout,
                )
                .await?;
            wait_for("outbound message in Axon timeline", ctx.timeout, || async {
                let page = ctx
                    .axon
                    .timeline(
                        ctx.manifest.axon_account_id,
                        &ctx.manifest.rooms.general.room_id,
                        30,
                    )
                    .await?;
                Ok(page.events.iter().any(|event| {
                    event.event_id == send.event_id && event.body.as_deref() == Some(&body)
                }))
            })
            .await?;
            Ok(())
        }
        .await,
    )
}

pub async fn relation_reads(ctx: &Ctx) -> ScenarioOutcome {
    ScenarioOutcome::from_result(
        async {
            let account_id = ctx.manifest.axon_account_id;
            let relations_room = &ctx.manifest.rooms.relations.room_id;
            let fixtures = &ctx.manifest.fixtures.relations;

            let root = ctx.axon.event(account_id, &fixtures.root_event_id).await?;
            if root.body.as_deref() != Some("relations root edited") {
                bail!("edited root body was not collapsed: {:?}", root.body);
            }
            if !root.edited || root.edit_count < 1 {
                bail!("edited root missing edit metadata");
            }
            require_reaction(&root, "👍", false)?;
            require_reaction(&root, "✅", true)?;

            let replies = ctx
                .axon
                .replies(account_id, &fixtures.root_event_id)
                .await?;
            if !replies.iter().any(|event| {
                event.event_id == fixtures.reply_event_id
                    && event.body.as_deref() == Some("relations reply message")
            }) {
                bail!("reply fixture missing from replies endpoint");
            }

            let edits = ctx.axon.edits(account_id, &fixtures.root_event_id).await?;
            if !edits
                .iter()
                .any(|event| event.event_id == fixtures.edit_event_id)
            {
                bail!("edit fixture missing from edits endpoint");
            }

            let reaction_map = ctx
                .axon
                .reactions(account_id, &fixtures.root_event_id)
                .await?;
            if reaction_map.get("👍").is_none() || reaction_map.get("✅").is_none() {
                bail!("reaction endpoint missing seeded reactions: {reaction_map}");
            }

            let redacted = ctx
                .axon
                .event(account_id, &fixtures.redacted_event_id)
                .await?;
            if !redacted.redacted
                || redacted.redaction_event_id.as_deref() != Some(&fixtures.redaction_event_id)
                || redacted.body.is_some()
            {
                bail!("redacted fixture did not render as redacted");
            }

            let threads = ctx.axon.threads(account_id, relations_room).await?;
            if !threads.iter().any(|thread| {
                thread.root_event_id == fixtures.thread_root_event_id && thread.reply_count >= 1
            }) {
                bail!("thread summary missing seeded thread");
            }
            let thread_page = ctx
                .axon
                .thread_timeline(account_id, relations_room, &fixtures.thread_root_event_id)
                .await?;
            if !thread_page
                .events
                .iter()
                .any(|event| event.event_id == fixtures.thread_member_event_id)
            {
                bail!("thread timeline missing seeded thread member");
            }
            Ok(())
        }
        .await,
    )
}

pub async fn graceful_stack_shutdown(ctx: &Ctx) -> ScenarioOutcome {
    ScenarioOutcome::from_result(
        async {
            // Actual teardown runs unconditionally in the runner after this loop.
            // Here we assert the pre-teardown invariant that matters for safety:
            // the stack we've been talking to is still up and serving. In attach
            // mode this is the guarantee that the harness has not torn down a
            // stack it does not own.
            let health = ctx.axon.health().await?;
            if health.get("status").and_then(Value::as_str) != Some("ok") {
                bail!("stack unexpectedly unhealthy before teardown: {health}");
            }
            if !ctx.owns_stack {
                eprintln!(
                    "smoke(server): graceful_stack_shutdown attach-mode: stack still up, teardown left to owner"
                );
            }
            Ok(())
        }
        .await,
    )
}

fn require_room(rooms: &[RoomDto], room_id: &str, name: &str) -> anyhow::Result<()> {
    let room = rooms
        .iter()
        .find(|room| room.room_id == room_id)
        .ok_or_else(|| anyhow!("room {room_id} not found"))?;
    if room.name.as_deref() != Some(name) {
        bail!("room {room_id} had unexpected name {:?}", room.name);
    }
    Ok(())
}

fn require_reaction(event: &EventDto, key: &str, me: bool) -> anyhow::Result<()> {
    let reaction = event
        .reactions
        .as_ref()
        .and_then(|map| map.get(key))
        .ok_or_else(|| anyhow!("event {} missing reaction {key}", event.event_id))?;
    if reaction.count < 1 {
        bail!("reaction {key} had count {}", reaction.count);
    }
    if reaction.me != me {
        bail!("reaction {key} me flag was {}, expected {me}", reaction.me);
    }
    Ok(())
}
