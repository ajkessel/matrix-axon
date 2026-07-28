//! Outbound Matrix typing notices (ADR 0068 M19a).
//!
//! Symmetric to the read-receipt send in [`super::read_markers`]: a
//! fire-and-forget `PUT …/rooms/{room_id}/typing` driven by compose activity,
//! so peers (and third-party Matrix clients) see this user typing. Unlike the
//! read receipt — which piggybacks on an existing debounce — typing needs its
//! own timers: a throttle so continuous typing sends `typing:true` only every
//! few seconds, and an idle deadline that sends `typing:false` after a pause.
//!
//! State is a single slot ([`App::typing`]): only one room is composed in at a
//! time, and switching rooms clears the previous room first. Failures are never
//! surfaced (best-effort, and the TUI has no in-session log surface).

use std::time::{Duration, Instant};

use super::{App, RoomKey};

/// Minimum gap between `typing:true` refreshes while composing continues. Held
/// strictly below the homeserver's typing-notice lifetime so the indicator
/// never lapses mid-type: matrix-sdk 0.18 stamps each `m.typing` with a 4s
/// timeout (`TYPING_NOTICE_TIMEOUT`) and throttles its own resends to 3s
/// (`TYPING_NOTICE_RESEND_TIMEOUT`), so refreshing at 3s both stays ahead of
/// the 4s expiry and matches the cadence at which the server-side SDK will
/// actually re-PUT.
pub(crate) const TYPING_REFRESH: Duration = Duration::from_secs(3);
/// Inactivity after the last keystroke before `typing:false` is sent. Larger
/// than [`TYPING_REFRESH`] so continuous typing refreshes before it fires.
pub(crate) const TYPING_IDLE: Duration = Duration::from_secs(4);

/// The active outbound typing notice: which room, when `true` was last sent
/// (the refresh throttle), and when to auto-clear if activity stops.
pub(crate) struct TypingNotice {
    pub(crate) room: RoomKey,
    pub(crate) last_true: Instant,
    pub(crate) idle_deadline: Instant,
}

impl App {
    /// The compose buffer holds real (non-empty, non-command) draft text for
    /// `room`: refresh the idle deadline and send `typing:true` if not already
    /// active for this room or the refresh window has elapsed. Switching the
    /// composed-in room clears the previous one first.
    pub(crate) fn note_typing(&mut self, room: RoomKey, now: Instant) {
        if self.typing.as_ref().is_some_and(|t| t.room != room) {
            self.stop_typing_now();
        }
        let refresh_due = self
            .typing
            .as_ref()
            .is_none_or(|t| now.duration_since(t.last_true) >= TYPING_REFRESH);
        let last_true = if refresh_due {
            self.spawn_typing(&room, true);
            now
        } else {
            // Safe: not `refresh_due` implies `self.typing` is `Some`.
            self.typing.as_ref().map_or(now, |t| t.last_true)
        };
        self.typing = Some(TypingNotice {
            room,
            last_true,
            idle_deadline: now + TYPING_IDLE,
        });
    }

    /// Main-loop tick: send `typing:false` once the idle window has passed.
    pub(crate) fn flush_due_typing(&mut self, now: Instant) {
        if self.typing.as_ref().is_none_or(|t| now < t.idle_deadline) {
            return;
        }
        self.stop_typing_now();
    }

    /// Clear the active typing notice, if any, sending `typing:false`.
    pub(crate) fn stop_typing_now(&mut self) {
        if let Some(active) = self.typing.take() {
            self.spawn_typing(&active.room, false);
        }
    }

    /// Clear typing only if it targets `room` (compose emptied, turned into a
    /// command, or the room was left).
    pub(crate) fn stop_typing_for_room(&mut self, room: &RoomKey) {
        if self.typing.as_ref().is_some_and(|t| &t.room == room) {
            self.stop_typing_now();
        }
    }

    /// Fire the notice in the background. Not wired (unit tests) means
    /// local-only, like the other background channels — so state transitions
    /// stay observable without a runtime.
    fn spawn_typing(&self, room: &RoomKey, typing: bool) {
        if self.drafts_tx.is_none() {
            return;
        }
        let client = self.client.clone();
        let room = room.clone();
        tokio::spawn(async move {
            let _ = client
                .send_typing_notice(room.account_id, &room.room_id, typing)
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::AxonClient;
    use crate::config::TuiConfig;
    use ratatui_image::picker::Picker;
    use uuid::Uuid;

    fn app() -> App {
        App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            None,
            TuiConfig::test_default(),
            Picker::halfblocks(),
        )
    }

    fn key(room_id: &str) -> RoomKey {
        RoomKey {
            account_id: Uuid::nil(),
            room_id: room_id.to_owned(),
        }
    }

    #[test]
    fn note_typing_arms_state_and_idle_deadline() {
        let mut app = app();
        let now = Instant::now();
        app.note_typing(key("!r:x"), now);
        let active = app.typing.as_ref().expect("typing armed");
        assert_eq!(active.room, key("!r:x"));
        assert_eq!(active.last_true, now);
        assert_eq!(active.idle_deadline, now + TYPING_IDLE);
    }

    #[test]
    fn refresh_is_throttled_then_advances() {
        let mut app = app();
        let start = Instant::now();
        app.note_typing(key("!r:x"), start);

        // Within the refresh window: idle deadline slides forward, but the last
        // typing:true instant does not (no resend).
        let soon = start + TYPING_REFRESH / 2;
        app.note_typing(key("!r:x"), soon);
        let active = app.typing.as_ref().unwrap();
        assert_eq!(active.last_true, start);
        assert_eq!(active.idle_deadline, soon + TYPING_IDLE);

        // Past the refresh window: a fresh typing:true is sent.
        let later = start + TYPING_REFRESH;
        app.note_typing(key("!r:x"), later);
        assert_eq!(app.typing.as_ref().unwrap().last_true, later);
    }

    #[test]
    fn flush_clears_only_after_the_idle_window() {
        let mut app = app();
        let start = Instant::now();
        app.note_typing(key("!r:x"), start);

        app.flush_due_typing(start + TYPING_IDLE - Duration::from_millis(1));
        assert!(app.typing.is_some(), "still within the idle window");

        app.flush_due_typing(start + TYPING_IDLE);
        assert!(app.typing.is_none(), "idle window elapsed");
    }

    #[test]
    fn stop_for_room_only_clears_the_matching_room() {
        let mut app = app();
        app.note_typing(key("!a:x"), Instant::now());

        app.stop_typing_for_room(&key("!b:x"));
        assert!(app.typing.is_some(), "a different room is left alone");

        app.stop_typing_for_room(&key("!a:x"));
        assert!(app.typing.is_none());
    }

    #[test]
    fn switching_composed_room_replaces_the_slot() {
        let mut app = app();
        let now = Instant::now();
        app.note_typing(key("!a:x"), now);
        app.note_typing(key("!b:x"), now);
        assert_eq!(app.typing.as_ref().unwrap().room, key("!b:x"));
    }
}
