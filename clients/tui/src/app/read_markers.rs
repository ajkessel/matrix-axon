//! Cross-device read markers over the M12 device-state API (ADR 0048).
//!
//! Each room's last-read position is mirrored to Axon's per-device state under
//! the `read_markers` namespace (key = room id, value
//! `{"event_id": …, "origin_ts": …}`, scoped by `account_id`), so reading a
//! room on one device clears its unread badge on the user's other devices, and
//! unread state survives a restart instead of resetting with the session.
//!
//! The wire machinery is the drafts machinery ([`super::drafts`]): a debounced
//! PUT per settled change, hydration from the merged view at startup and on
//! every WS (re)connect, live `device_state.changed` frames from sibling
//! devices, and echo suppression by device id.
//!
//! **Monotonicity is a client-side convention.** The server's last-write-wins
//! is arrival-order, so a device that was offline can legitimately win with a
//! marker *older* than the current one. Reading never moves backward, so this
//! client refuses backward movement everywhere: it neither applies an older
//! incoming marker nor arms a PUT for one. Tombstones (a `null` marker) have
//! no meaning for read state and are ignored.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::Value;
use uuid::Uuid;

use super::drafts::DRAFT_DEBOUNCE;
use super::{App, LiveFrameAction, RoomKey};

/// The device-state namespace read markers live under.
pub(crate) const READ_MARKERS_NAMESPACE: &str = "read_markers";

/// A room's last-read position: the newest event the user had on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadMarker {
    pub(crate) event_id: String,
    /// `origin_server_ts` of that event, milliseconds — the monotonic ordering.
    pub(crate) origin_ts: i64,
}

/// A marker advance waiting out its debounce window before being PUT.
pub(crate) struct PendingMarkerPut {
    pub(crate) room: RoomKey,
    pub(crate) marker: ReadMarker,
    pub(crate) due: Instant,
}

/// The marker's wire value.
fn marker_value(marker: &ReadMarker) -> Value {
    serde_json::json!({ "event_id": marker.event_id, "origin_ts": marker.origin_ts })
}

/// Parse a marker from its wire value; `None` for shapes this client doesn't
/// recognize (left alone rather than misread).
fn marker_from_value(value: &Value) -> Option<ReadMarker> {
    Some(ReadMarker {
        event_id: value.get("event_id")?.as_str()?.to_owned(),
        origin_ts: value.get("origin_ts")?.as_i64()?,
    })
}

impl App {
    /// Mark as promoted every thread member in `events` newer than the room's
    /// read marker, returning the (deduplicated) thread roots so the caller
    /// can fetch their context once the page is installed.
    ///
    /// This is what keeps an unread badge honest for an **old thread**: a new
    /// reply hides behind its root's badge (`thread_visible`), and for a
    /// thread whose root has scrolled out of the loaded window there is no
    /// badge on screen at all — the room says unread but shows nothing new.
    /// The reply that caused the badge is by definition newer than the
    /// marker, so promoting everything past the marker surfaces it inline,
    /// exactly like the existing live-arrival promotion. Must be called
    /// *before* [`App::note_room_read`] advances the marker; a room with no
    /// marker yet keeps today's behavior (nothing to measure "unseen"
    /// against).
    pub(crate) fn collect_unseen_thread_promotions(
        &mut self,
        room: &RoomKey,
        events: &[crate::api::EventDto],
    ) -> Vec<(Uuid, String)> {
        let Some(marker_ts) = self.read_markers.get(room).map(|m| m.origin_ts) else {
            return Vec::new();
        };
        let mut roots: Vec<(Uuid, String)> = Vec::new();
        for event in events.iter().filter(|e| e.origin_ts > marker_ts) {
            let Some(root) = event.thread_relation() else {
                continue;
            };
            self.promoted_thread_events.insert(event.event_id.clone());
            let entry = (event.account_id, root.to_owned());
            if !roots.contains(&entry) {
                roots.push(entry);
            }
        }
        roots
    }

    /// The user has the room on screen read up to `marker`: advance the local
    /// marker and arm the debounced PUT. Backward or repeated positions are
    /// no-ops (monotonic). Called from the timeline-load and live-event paths
    /// that already clear the room's unread badge.
    pub(crate) fn note_room_read(&mut self, room: RoomKey, event_id: &str, origin_ts: i64) {
        if self
            .read_markers
            .get(&room)
            .is_some_and(|current| current.origin_ts >= origin_ts)
        {
            return;
        }
        let marker = ReadMarker {
            event_id: event_id.to_owned(),
            origin_ts,
        };
        self.read_markers.insert(room.clone(), marker.clone());
        // One pending slot: a still-armed advance for a *different* room (the
        // user read it and switched away inside the debounce window) is sent
        // now rather than silently replaced.
        if let Some(pending) = self.pending_marker_put.take() {
            if pending.room != room {
                self.spawn_marker_put(pending.room, pending.marker);
            }
        }
        self.pending_marker_put = Some(PendingMarkerPut {
            room,
            marker,
            due: Instant::now() + DRAFT_DEBOUNCE,
        });
    }

    /// Called on the main-loop tick: flush the pending marker once its
    /// debounce window has passed.
    pub(crate) fn flush_due_marker_put(&mut self, now: Instant) {
        if self.pending_marker_put.as_ref().is_none_or(|p| now < p.due) {
            return;
        }
        let Some(pending) = self.pending_marker_put.take() else {
            return;
        };
        self.spawn_marker_put(pending.room, pending.marker);
    }

    /// PUT one room's marker in the background. Failures surface through the
    /// same channel as draft PUTs; not wired (unit tests) means local-only,
    /// like the other background channels.
    fn spawn_marker_put(&self, room: RoomKey, marker: ReadMarker) {
        let Some(tx) = self.drafts_tx.clone() else {
            return;
        };
        let client = self.client.clone();
        let device_id = self.device_id;
        tokio::spawn(async move {
            let entries: HashMap<String, Option<Value>> =
                HashMap::from([(room.room_id.clone(), Some(marker_value(&marker)))]);
            if let Err(err) = client
                .put_device_state(device_id, room.account_id, READ_MARKERS_NAMESPACE, &entries)
                .await
            {
                let _ = tx.send(super::DraftOutcome::PutFailed(format!(
                    "read marker: {err}"
                )));
            }
        });
    }

    /// Hydrate read markers from the server's merged view, one GET per active
    /// account, then reconcile the unread badges. Called at startup and on
    /// every WS (re)connect (the lossy bus may have dropped frames). A failed
    /// read leaves that account's local markers as they are.
    pub(crate) async fn refresh_read_markers(&mut self) {
        let account_ids: Vec<Uuid> = self
            .accounts
            .accounts
            .iter()
            .map(|a| a.account_id)
            .collect();
        for account_id in account_ids {
            let Ok(state) = self
                .client
                .get_device_state(self.device_id, account_id, READ_MARKERS_NAMESPACE)
                .await
            else {
                continue;
            };
            for (room_id, entry) in state.entries {
                let Some(marker) = marker_from_value(&entry.value) else {
                    continue;
                };
                self.apply_marker(
                    RoomKey {
                        account_id,
                        room_id,
                    },
                    marker,
                );
            }
        }
        self.reconcile_unread_with_markers();
    }

    /// Apply one live `read_markers` entry map from a sibling device.
    pub(crate) fn handle_read_marker_frame(
        &mut self,
        account_id: Uuid,
        entries: HashMap<String, Value>,
    ) -> LiveFrameAction {
        for (room_id, value) in entries {
            // A tombstone has no meaning for read state; skip it.
            let Some(marker) = marker_from_value(&value) else {
                continue;
            };
            let key = RoomKey {
                account_id,
                room_id,
            };
            if self.apply_marker(key.clone(), marker) {
                self.clear_unread_if_read(&key);
            }
        }
        LiveFrameAction::None
    }

    /// Merge one marker into the local map, monotonically. Returns whether it
    /// advanced.
    fn apply_marker(&mut self, room: RoomKey, marker: ReadMarker) -> bool {
        if self
            .read_markers
            .get(&room)
            .is_some_and(|current| current.origin_ts >= marker.origin_ts)
        {
            return false;
        }
        self.read_markers.insert(room, marker);
        true
    }

    /// Reconcile every room's unread badge with its marker: a room whose
    /// latest known activity is newer than its marker shows as unread (this is
    /// what survives a restart), one read to (or past) its latest activity
    /// clears. Rooms with no marker are left alone — a first run must not
    /// light up every room.
    pub(crate) fn reconcile_unread_with_markers(&mut self) {
        let rooms: Vec<(RoomKey, i64)> = self
            .rooms
            .rooms
            .iter()
            .map(|room| (RoomKey::from(room), room.last_activity_ts))
            .collect();
        for (key, _) in rooms {
            let Some(marker) = self.read_markers.get(&key) else {
                continue;
            };
            let marker_ts = marker.origin_ts;
            if self.latest_known_activity_ts(&key) > marker_ts {
                self.rooms.unread.entry(key).or_insert(1);
            } else {
                self.rooms.unread.remove(&key);
            }
        }
    }

    /// Drop a room's unread badge when its marker has caught up with its
    /// latest known activity — the user read it on another device.
    fn clear_unread_if_read(&mut self, room: &RoomKey) {
        let Some(marker) = self.read_markers.get(room) else {
            return;
        };
        if marker.origin_ts >= self.latest_known_activity_ts(room) {
            self.rooms.unread.remove(room);
        }
    }

    /// The newest activity this client knows for a room: the room list's
    /// `last_activity_ts` or the newest loaded/live event, whichever is later
    /// (live events don't update the room-list timestamp between refreshes).
    fn latest_known_activity_ts(&self, room: &RoomKey) -> i64 {
        let listed = self
            .rooms
            .rooms
            .iter()
            .find(|r| RoomKey::from(*r) == *room)
            .map(|r| r.last_activity_ts)
            .unwrap_or(0);
        let loaded = self
            .messages
            .events
            .get(room)
            .and_then(|events| events.iter().map(|e| e.origin_ts).max())
            .unwrap_or(0);
        listed.max(loaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AxonClient, EventDto, RoomDto};
    use crate::config::TuiConfig;
    use ratatui_image::picker::Picker;

    fn test_room(room_id: &str, last_activity_ts: i64) -> RoomDto {
        RoomDto {
            account_id: Uuid::nil(),
            account_user_id: Some("@alice:example.com".to_owned()),
            room_id: room_id.to_owned(),
            name: Some("Room".to_owned()),
            topic: None,
            avatar_url: None,
            canonical_alias: None,
            last_activity_ts,
            last_event_id: None,
        }
    }

    fn app_with(rooms: Vec<RoomDto>) -> App {
        let mut app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            None,
            TuiConfig::test_default(),
            Picker::halfblocks(),
        );
        app.rooms.rooms = rooms;
        app.set_device_id(Uuid::new_v4());
        app
    }

    fn key(room_id: &str) -> RoomKey {
        RoomKey {
            account_id: Uuid::nil(),
            room_id: room_id.to_owned(),
        }
    }

    #[test]
    fn note_room_read_is_monotonic() {
        let mut app = app_with(vec![test_room("!r:x", 0)]);
        let k = key("!r:x");

        app.note_room_read(k.clone(), "$b", 200);
        assert_eq!(app.read_markers.get(&k).unwrap().origin_ts, 200);
        assert!(app.pending_marker_put.is_some());

        // Backward (or equal) never regresses the marker or re-arms a PUT.
        app.pending_marker_put = None;
        app.note_room_read(k.clone(), "$a", 100);
        app.note_room_read(k.clone(), "$b", 200);
        assert_eq!(app.read_markers.get(&k).unwrap().event_id, "$b");
        assert!(app.pending_marker_put.is_none());

        // Forward advances.
        app.note_room_read(k.clone(), "$c", 300);
        assert_eq!(app.read_markers.get(&k).unwrap().origin_ts, 300);
        assert!(app.pending_marker_put.is_some());
    }

    #[test]
    fn incoming_frames_apply_monotonically_and_clear_unread() {
        let mut app = app_with(vec![test_room("!r:x", 250)]);
        let k = key("!r:x");
        app.rooms.unread.insert(k.clone(), 3);

        // A marker older than the room's latest activity: applied, badge stays.
        let entries = HashMap::from([(
            "!r:x".to_owned(),
            marker_value(&ReadMarker {
                event_id: "$old".to_owned(),
                origin_ts: 100,
            }),
        )]);
        app.handle_read_marker_frame(Uuid::nil(), entries);
        assert_eq!(app.read_markers.get(&k).unwrap().origin_ts, 100);
        assert_eq!(app.rooms.unread.get(&k), Some(&3));

        // A marker at (or past) the latest activity clears the badge.
        let entries = HashMap::from([(
            "!r:x".to_owned(),
            marker_value(&ReadMarker {
                event_id: "$new".to_owned(),
                origin_ts: 250,
            }),
        )]);
        app.handle_read_marker_frame(Uuid::nil(), entries);
        assert!(!app.rooms.unread.contains_key(&k));

        // An older marker arriving later (offline device replay) is refused.
        let entries = HashMap::from([(
            "!r:x".to_owned(),
            marker_value(&ReadMarker {
                event_id: "$stale".to_owned(),
                origin_ts: 50,
            }),
        )]);
        app.handle_read_marker_frame(Uuid::nil(), entries);
        assert_eq!(app.read_markers.get(&k).unwrap().event_id, "$new");

        // Tombstones and unknown shapes are ignored.
        let entries = HashMap::from([("!r:x".to_owned(), Value::Null)]);
        app.handle_read_marker_frame(Uuid::nil(), entries);
        assert_eq!(app.read_markers.get(&k).unwrap().event_id, "$new");
    }

    #[test]
    fn reconcile_seeds_unread_only_for_marked_rooms_with_newer_activity() {
        let mut app = app_with(vec![
            test_room("!behind:x", 500),    // marker older than activity → unread
            test_room("!caught-up:x", 300), // marker at activity → read
            test_room("!unmarked:x", 900),  // no marker → left alone
        ]);
        app.read_markers.insert(
            key("!behind:x"),
            ReadMarker {
                event_id: "$m1".to_owned(),
                origin_ts: 400,
            },
        );
        app.read_markers.insert(
            key("!caught-up:x"),
            ReadMarker {
                event_id: "$m2".to_owned(),
                origin_ts: 300,
            },
        );
        // A stale badge on the caught-up room (e.g. counted while the socket
        // was down, then read on another device) is dropped by reconcile.
        app.rooms.unread.insert(key("!caught-up:x"), 2);

        app.reconcile_unread_with_markers();

        assert_eq!(app.rooms.unread.get(&key("!behind:x")), Some(&1));
        assert!(!app.rooms.unread.contains_key(&key("!caught-up:x")));
        assert!(!app.rooms.unread.contains_key(&key("!unmarked:x")));
    }

    #[test]
    fn reconcile_does_not_shrink_a_live_count() {
        let mut app = app_with(vec![test_room("!r:x", 500)]);
        let k = key("!r:x");
        app.read_markers.insert(
            k.clone(),
            ReadMarker {
                event_id: "$m".to_owned(),
                origin_ts: 400,
            },
        );
        // Three live events already counted this session; reconcile must not
        // collapse the count to 1.
        app.rooms.unread.insert(k.clone(), 3);
        app.reconcile_unread_with_markers();
        assert_eq!(app.rooms.unread.get(&k), Some(&3));
    }

    #[test]
    fn switching_rooms_mid_debounce_flushes_the_previous_marker() {
        let mut app = app_with(vec![test_room("!a:x", 0), test_room("!b:x", 0)]);
        app.note_room_read(key("!a:x"), "$a", 100);
        assert_eq!(app.pending_marker_put.as_ref().unwrap().room, key("!a:x"));

        // Reading a different room replaces the slot; the previous marker is
        // flushed (spawn is a no-op here with no channel wired) — the local
        // map keeps both.
        app.note_room_read(key("!b:x"), "$b", 200);
        assert_eq!(app.pending_marker_put.as_ref().unwrap().room, key("!b:x"));
        assert_eq!(app.read_markers.get(&key("!a:x")).unwrap().origin_ts, 100);
        assert_eq!(app.read_markers.get(&key("!b:x")).unwrap().origin_ts, 200);
    }

    fn timeline_event(event_id: &str, origin_ts: i64, thread_root: Option<&str>) -> EventDto {
        EventDto {
            account_id: Uuid::nil(),
            event_id: event_id.to_owned(),
            room_id: "!r:x".to_owned(),
            sender: "@alice:example.com".to_owned(),
            state_key: None,
            origin_ts,
            event_type: "m.room.message".to_owned(),
            content: Some(serde_json::json!({ "msgtype": "m.text", "body": "hi" })),
            body: Some("hi".to_owned()),
            relates_to: thread_root
                .map(|root| serde_json::json!({ "rel_type": "m.thread", "event_id": root })),
            redacted: false,
            redaction_event_id: None,
            reactions: None,
            sender_trust: None,
        }
    }

    #[test]
    fn thread_replies_past_the_marker_are_promoted_on_load() {
        let mut app = app_with(vec![test_room("!r:x", 500)]);
        let k = key("!r:x");
        app.read_markers.insert(
            k.clone(),
            ReadMarker {
                event_id: "$seen".to_owned(),
                origin_ts: 200,
            },
        );

        let page = vec![
            // Already read: at/behind the marker — never promoted.
            timeline_event("$old-reply", 150, Some("$root-a")),
            timeline_event("$at-marker", 200, Some("$root-a")),
            // Unseen thread replies — promoted; two on one root dedupe to one
            // root fetch.
            timeline_event("$new-reply-1", 300, Some("$root-a")),
            timeline_event("$new-reply-2", 400, Some("$root-a")),
            timeline_event("$other-thread", 450, Some("$root-b")),
            // Unseen but not a thread member — nothing to promote.
            timeline_event("$plain", 500, None),
        ];
        let roots = app.collect_unseen_thread_promotions(&k, &page);

        assert!(app.promoted_thread_events.contains("$new-reply-1"));
        assert!(app.promoted_thread_events.contains("$new-reply-2"));
        assert!(app.promoted_thread_events.contains("$other-thread"));
        assert!(!app.promoted_thread_events.contains("$old-reply"));
        assert!(!app.promoted_thread_events.contains("$at-marker"));
        assert!(!app.promoted_thread_events.contains("$plain"));
        assert_eq!(
            roots,
            vec![
                (Uuid::nil(), "$root-a".to_owned()),
                (Uuid::nil(), "$root-b".to_owned()),
            ]
        );

        // The promoted replies now pass the main-timeline visibility filter.
        use crate::app::relations::thread_visible;
        assert!(thread_visible(
            &timeline_event("$new-reply-1", 300, Some("$root-a")),
            None,
            &app.promoted_thread_events,
        ));
    }

    #[test]
    fn rooms_without_a_marker_promote_nothing() {
        let mut app = app_with(vec![test_room("!r:x", 500)]);
        let k = key("!r:x");
        let page = vec![timeline_event("$reply", 300, Some("$root"))];
        let roots = app.collect_unseen_thread_promotions(&k, &page);
        assert!(roots.is_empty());
        assert!(app.promoted_thread_events.is_empty());
    }

    #[test]
    fn marker_value_roundtrip() {
        let marker = ReadMarker {
            event_id: "$e:example.com".to_owned(),
            origin_ts: 1234,
        };
        assert_eq!(marker_from_value(&marker_value(&marker)), Some(marker));
        assert_eq!(marker_from_value(&Value::Null), None);
        assert_eq!(
            marker_from_value(&serde_json::json!({ "event_id": "$e" })),
            None
        );
    }
}
