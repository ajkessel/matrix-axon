use ruma::OwnedUserId;
use uuid::Uuid;

use crate::api::{AccountDto, AccountState};

use super::{AccountSelection, App, Mode, RoomKey, Status};

enum LogoutResolution {
    Match(AccountDto),
    Ambiguous(Vec<String>),
    Missing,
}

/// Result of a login/logout request run off the event loop. The slow network
/// call happens in a spawned task; this is what it sends back for the main loop
/// to apply (refresh + status) without ever blocking redraws.
pub(crate) enum LifecycleOutcome {
    Login {
        /// The full Matrix ID that was attempted, for failure messaging.
        username: String,
        /// `account_id`s that were already `active` before the attempt, so the
        /// handler can tell a real (re)login from a no-op on an active account.
        prior_account_ids: Vec<Uuid>,
        result: Result<AccountDto, String>,
    },
    /// Account list fetched off-loop as the first phase of logout; the main
    /// loop resolves the target and dispatches to confirm/perform from here.
    LogoutReady {
        target: Option<String>,
        result: Result<Vec<AccountDto>, String>,
    },
    Logout {
        /// The Matrix ID being logged out, for failure messaging.
        user_id: String,
        result: Result<AccountDto, String>,
    },
}

impl App {
    pub(crate) async fn refresh_accounts(&mut self) {
        match self.client.list_accounts().await {
            Ok(accounts) => {
                self.set_accounts(accounts);
                // Apply the CLI --account-id flag once, before user interaction
                if self.accounts.selected == AccountSelection::All {
                    if let Some(filter_id) = self.account_filter {
                        if let Some(idx) = self
                            .accounts
                            .accounts
                            .iter()
                            .position(|a| a.account_id == filter_id)
                        {
                            self.accounts.selected = AccountSelection::Account(idx);
                        }
                    }
                }
            }
            Err(err) => self.status = Status::from(format!("account refresh failed: {err}")),
        }
    }

    pub(super) async fn start_login(
        &mut self,
        username: Option<String>,
        password: Option<String>,
        homeserver: Option<String>,
    ) {
        if self.reject_if_lifecycle_busy() {
            return;
        }
        let Some(raw_username) = username else {
            self.clear_lifecycle_input();
            self.mode = Mode::LoginUsername;
            self.status = LOGIN_USERNAME_PROMPT.into();
            return;
        };
        let username = match normalize_matrix_user_id(&raw_username) {
            Ok(username) => username,
            Err(message) => {
                self.input.buffer = raw_username;
                self.move_cursor_to_end();
                self.mode = Mode::LoginUsername;
                self.status = message.into();
                return;
            }
        };
        // A homeserver only ever rides along the inline third token, which the
        // parser allows only when a password is also present, so it is always
        // `None` on the prompt-for-password path below.
        let homeserver = homeserver.map(|value| normalize_homeserver_url(&value));
        let Some(password) = password else {
            self.clear_lifecycle_input();
            self.mode = Mode::LoginPassword {
                username,
                homeserver,
            };
            self.status = "Password: input is hidden; Enter submits, Esc cancels".into();
            return;
        };
        self.perform_login(username, password, homeserver);
    }

    pub(crate) async fn submit_login_username(&mut self) {
        let raw = self.take_input_for_submit();
        // The username step accepts an optional homeserver after the Matrix ID
        // (both single tokens, so there is no ambiguity). This is how a user with
        // a space-bearing password — which must go through the hidden prompt —
        // still pins a homeserver.
        let mut tokens = raw.split_whitespace();
        let raw_username = tokens.next().unwrap_or_default().to_owned();
        let homeserver = tokens.next().map(str::to_owned);
        let extra = tokens.next().is_some();
        let restore = |app: &mut Self, message: &str| {
            app.input.buffer = raw.clone();
            app.move_cursor_to_end();
            app.status = message.into();
        };
        if extra {
            restore(
                self,
                "enter at most a Matrix ID and a homeserver, e.g. @user:example.com hs.example.com",
            );
            return;
        }
        let username = match normalize_matrix_user_id(&raw_username) {
            Ok(username) => username,
            Err(message) => {
                restore(self, &message);
                return;
            }
        };
        let homeserver = homeserver.map(|value| normalize_homeserver_url(&value));
        self.mode = Mode::LoginPassword {
            username,
            homeserver,
        };
        self.status = "Password: input is hidden; Enter submits, Esc cancels".into();
    }

    pub(crate) async fn submit_login_password(
        &mut self,
        username: String,
        homeserver: Option<String>,
    ) {
        let password = self.take_input_for_submit();
        if password.is_empty() {
            self.status = "password cannot be empty".into();
            return;
        }
        self.perform_login(username, password, homeserver);
    }

    /// Kick off a login without blocking the event loop: the login round-trip
    /// runs in a spawned task (which owns and then drops the password), and its
    /// outcome arrives via [`LifecycleOutcome`]. `homeserver` is an optional base
    /// URL override; when `None`, Axon resolves the homeserver from the Matrix ID.
    fn perform_login(&mut self, username: String, password: String, homeserver: Option<String>) {
        self.clear_lifecycle_input();
        self.mode = Mode::Compose;
        let Some(tx) = self.lifecycle_tx.clone() else {
            return;
        };
        let prior_account_ids = self.active_account_ids();
        self.status = Status::from(format!("logging in {username}…"));
        self.lifecycle_busy = true;
        let client = self.client.clone();
        tokio::spawn(async move {
            let result = client
                .login(&username, &password, homeserver.as_deref())
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(LifecycleOutcome::Login {
                username,
                prior_account_ids,
                result,
            });
        });
    }

    pub(super) fn start_logout(&mut self, target: Option<String>) {
        if self.reject_if_lifecycle_busy() {
            return;
        }
        let Some(tx) = self.lifecycle_tx.clone() else {
            return;
        };
        self.status = Status::from(match &target {
            Some(t) if !t.is_empty() => format!("logging out {t}…"),
            _ => "logging out…".to_owned(),
        });
        self.lifecycle_busy = true;
        let client = self.client.clone();
        tokio::spawn(async move {
            let result = client.list_accounts().await.map_err(|err| err.to_string());
            let _ = tx.send(LifecycleOutcome::LogoutReady { target, result });
        });
    }

    /// Either prompt for confirmation or log out immediately, per the
    /// `confirm_logout` display option.
    pub(crate) fn request_logout(&mut self, account: AccountDto) {
        if self.display.confirm_logout {
            self.clear_lifecycle_input();
            self.status = Status::from(format!("Log out {}? [y/N]", account.user_id));
            self.mode = Mode::ConfirmLogout { account };
        } else {
            self.perform_logout(account);
        }
    }

    pub(crate) fn cancel_logout_confirmation(&mut self) {
        self.mode = Mode::Compose;
        self.status = Status::from("logout canceled".to_owned());
    }

    /// Kick off a logout without blocking the event loop; the result arrives via
    /// [`LifecycleOutcome`].
    pub(crate) fn perform_logout(&mut self, account: AccountDto) {
        let user_id = account.user_id.clone();
        self.clear_lifecycle_input();
        self.mode = Mode::Compose;
        let Some(tx) = self.lifecycle_tx.clone() else {
            return;
        };
        self.status = Status::from(format!("logging out {user_id}…"));
        self.lifecycle_busy = true;
        let client = self.client.clone();
        let account_id = account.account_id;
        tokio::spawn(async move {
            let result = client
                .logout(account_id)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(LifecycleOutcome::Logout { user_id, result });
        });
    }

    /// Apply the result of a spawned login/logout once it lands on the event
    /// loop: refresh views and report a final status. Runs only fast, local
    /// Axon calls, so blocking here is acceptable.
    pub(crate) async fn handle_lifecycle_outcome(&mut self, outcome: LifecycleOutcome) {
        self.lifecycle_busy = false;
        match outcome {
            LifecycleOutcome::Login {
                username,
                prior_account_ids,
                result,
            } => match result {
                Ok(account) => {
                    let warning = self.refresh_after_lifecycle_change().await;
                    if let Some(idx) = self
                        .accounts
                        .accounts
                        .iter()
                        .position(|a| a.account_id == account.account_id)
                    {
                        self.accounts.selected = AccountSelection::Account(idx);
                        self.sync_room_selection_to_account_filter();
                        self.load_selected_timeline().await;
                    }
                    let already_active = prior_account_ids.contains(&account.account_id);
                    self.status = lifecycle_login_status(already_active, &account.user_id, warning);
                }
                Err(error) => {
                    self.status = Status::from(format!("login failed for {username}: {error}"));
                }
            },
            LifecycleOutcome::LogoutReady { target, result } => {
                self.lifecycle_busy = false;
                match result {
                    Ok(accounts) => {
                        self.accounts.accounts = accounts;
                        match self.resolve_logout_target(target.as_deref()) {
                            LogoutResolution::Match(account) => self.request_logout(account),
                            LogoutResolution::Ambiguous(options) => {
                                self.restore_logout_input(target.as_deref());
                                self.status = Status::from(format!(
                                    "logout target is ambiguous: {} - press Tab to choose",
                                    options.join(", ")
                                ));
                            }
                            LogoutResolution::Missing => {
                                self.restore_logout_input(target.as_deref());
                                self.status = if target.as_deref().is_some_and(|v| !v.is_empty()) {
                                    Status::from(format!(
                                        "no active account matches: {}",
                                        target.unwrap_or_default()
                                    ))
                                } else {
                                    Status::from("no active accounts".to_owned())
                                };
                            }
                        }
                    }
                    Err(err) => {
                        self.restore_logout_input(target.as_deref());
                        self.status = Status::from(format!("logout failed: {err}"));
                    }
                }
            }
            LifecycleOutcome::Logout { user_id, result } => match result {
                Ok(account) => {
                    let warning = self.refresh_after_lifecycle_change().await;
                    self.status = lifecycle_success_status("logged out", &account.user_id, warning);
                }
                Err(error) => {
                    self.status = Status::from(format!("logout failed for {user_id}: {error}"));
                }
            },
        }
    }

    fn reject_if_lifecycle_busy(&mut self) -> bool {
        if self.lifecycle_busy {
            self.status = Status::from("an account operation is already in progress".to_owned());
            return true;
        }
        false
    }

    fn active_account_ids(&self) -> Vec<Uuid> {
        self.accounts
            .accounts
            .iter()
            .filter(|account| account.state == AccountState::Active)
            .map(|account| account.account_id)
            .collect()
    }

    async fn refresh_after_lifecycle_change(&mut self) -> Option<String> {
        let had_selection = self.selected_room().map(RoomKey::from);
        let mut warnings = Vec::new();
        match self.client.list_accounts().await {
            Ok(accounts) => self.set_accounts(accounts),
            Err(err) => warnings.push(format!("account refresh failed: {err}")),
        }
        match self.client.list_rooms(self.account_filter).await {
            Ok(rooms) => {
                self.apply_room_refresh(rooms);
                let new_selection = self.selected_room().map(RoomKey::from);
                if new_selection.is_some() && new_selection != had_selection {
                    self.load_selected_timeline().await;
                }
            }
            Err(err) => {
                warnings.push(format!("room refresh failed: {err}"));
            }
        }
        (!warnings.is_empty()).then(|| warnings.join("; "))
    }

    fn resolve_logout_target(&self, target: Option<&str>) -> LogoutResolution {
        let active: Vec<_> = self
            .accounts
            .accounts
            .iter()
            .filter(|account| account.state == AccountState::Active)
            .cloned()
            .collect();
        let target = target.unwrap_or_default().trim();
        let matches = if target.is_empty() {
            active
        } else if let Some(canonical) = canonical_logout_target(target) {
            active
                .into_iter()
                .filter(|account| account.user_id == canonical)
                .collect()
        } else {
            let localpart = target.trim_start_matches('@');
            active
                .into_iter()
                .filter(|account| matrix_user_localpart(&account.user_id) == Some(localpart))
                .collect()
        };

        match matches.as_slice() {
            [account] => LogoutResolution::Match(account.clone()),
            [_, _, ..] => LogoutResolution::Ambiguous(
                matches.into_iter().map(|account| account.user_id).collect(),
            ),
            [] => LogoutResolution::Missing,
        }
    }

    fn restore_logout_input(&mut self, target: Option<&str>) {
        self.mode = Mode::Compose;
        self.input.buffer = match target.filter(|value| !value.is_empty()) {
            Some(target) => format!("/logout {target}"),
            None => "/logout".to_owned(),
        };
        self.move_cursor_to_end();
    }

    pub(crate) fn active_logout_candidates(&self, target: &str) -> Vec<String> {
        let target = target.trim();
        self.accounts
            .accounts
            .iter()
            .filter(|account| account.state == AccountState::Active)
            .filter(|account| {
                if target.is_empty() {
                    true
                } else if let Some(canonical) = canonical_logout_target(target) {
                    account.user_id.starts_with(&canonical)
                } else {
                    matrix_user_localpart(&account.user_id).is_some_and(|localpart| {
                        localpart.starts_with(target.trim_start_matches('@'))
                    })
                }
            })
            .map(|account| account.user_id.clone())
            .collect()
    }

    pub(crate) fn cancel_lifecycle_input(&mut self) {
        self.clear_lifecycle_input();
        self.mode = Mode::Compose;
        self.status = "login canceled".into();
    }

    fn clear_lifecycle_input(&mut self) {
        self.clear_input_buffer();
        self.input.logout_command_completion = None;
    }
}

/// Username-step prompt. Mentions the optional homeserver so users who must use
/// the hidden password prompt (e.g. a password with spaces) can still pin one.
const LOGIN_USERNAME_PROMPT: &str =
    "Matrix ID (optionally a homeserver after it): @user:example.com [hs.example.com]";

/// Make a user-supplied homeserver acceptable as Axon's `homeserver_url`: a bare
/// host gets `https://`, while an explicit scheme is left intact (so a loopback
/// dev server can be reached with `http://localhost:8008`).
fn normalize_homeserver_url(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    }
}

fn normalize_matrix_user_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    let candidate = if value.starts_with('@') {
        value.to_owned()
    } else if value.contains(':') {
        format!("@{value}")
    } else if let Some((localpart, server_name)) = value.split_once('@') {
        if localpart.is_empty() || server_name.is_empty() || server_name.contains('@') {
            return Err(matrix_user_id_error());
        }
        format!("@{localpart}:{server_name}")
    } else {
        return Err(matrix_user_id_error());
    };

    OwnedUserId::try_from(candidate.as_str())
        .map(|_| candidate)
        .map_err(|_| matrix_user_id_error())
}

fn canonical_logout_target(value: &str) -> Option<String> {
    let value = value.trim();
    (value.contains(':') || (!value.starts_with('@') && value.contains('@')))
        .then(|| normalize_matrix_user_id(value).ok())
        .flatten()
}

fn matrix_user_id_error() -> String {
    "enter a Matrix ID as @name:domain, name:domain, or name@domain".to_owned()
}

fn matrix_user_localpart(user_id: &str) -> Option<&str> {
    user_id
        .strip_prefix('@')?
        .split_once(':')
        .map(|(local, _)| local)
}

fn lifecycle_success_status(action: &str, user_id: &str, warning: Option<String>) -> Status {
    Status::from(match warning {
        Some(warning) => format!("{action}: {user_id}; {warning}"),
        None => format!("{action}: {user_id}"),
    })
}

/// Status for a completed login. An `already_active` account is the server's
/// idempotent no-op: nothing changed and the password was never consulted, so
/// say so rather than implying a fresh authentication succeeded.
fn lifecycle_login_status(already_active: bool, user_id: &str, warning: Option<String>) -> Status {
    let summary = if already_active {
        format!("already logged in: {user_id} (no changes)")
    } else {
        format!("logged in: {user_id}")
    };
    Status::from(match warning {
        Some(warning) => format!("{summary}; {warning}"),
        None => summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_supported_matrix_username_forms() {
        assert_eq!(
            normalize_matrix_user_id("@alice:example.com").unwrap(),
            "@alice:example.com"
        );
        assert_eq!(
            normalize_matrix_user_id("alice:example.com").unwrap(),
            "@alice:example.com"
        );
        assert_eq!(
            normalize_matrix_user_id("alice@example.com").unwrap(),
            "@alice:example.com"
        );
    }

    #[test]
    fn rejects_login_localpart_without_server() {
        assert!(normalize_matrix_user_id("alice").is_err());
        assert!(normalize_matrix_user_id("@alice").is_err());
    }

    #[test]
    fn login_status_distinguishes_no_op_from_fresh_login() {
        assert_eq!(
            lifecycle_login_status(false, "@alice:example.com", None).text(false),
            "logged in: @alice:example.com"
        );
        assert_eq!(
            lifecycle_login_status(true, "@alice:example.com", None).text(false),
            "already logged in: @alice:example.com (no changes)"
        );
        assert_eq!(
            lifecycle_login_status(
                true,
                "@alice:example.com",
                Some("room refresh failed".to_owned())
            )
            .text(false),
            "already logged in: @alice:example.com (no changes); room refresh failed"
        );
    }

    #[test]
    fn normalizes_homeserver_url_scheme() {
        // Bare host gains https://; an explicit scheme is preserved so loopback
        // dev servers can stay on http://.
        assert_eq!(
            normalize_homeserver_url("homeserver.example.org"),
            "https://homeserver.example.org"
        );
        assert_eq!(
            normalize_homeserver_url("  matrix.example.org  "),
            "https://matrix.example.org"
        );
        assert_eq!(
            normalize_homeserver_url("https://matrix.example.org"),
            "https://matrix.example.org"
        );
        assert_eq!(
            normalize_homeserver_url("http://localhost:8008"),
            "http://localhost:8008"
        );
    }

    #[test]
    fn canonicalizes_logout_targets_with_server_information() {
        assert_eq!(
            canonical_logout_target("@alice:example.com").as_deref(),
            Some("@alice:example.com")
        );
        assert_eq!(
            canonical_logout_target("alice:example.com").as_deref(),
            Some("@alice:example.com")
        );
        assert_eq!(
            canonical_logout_target("alice@example.com").as_deref(),
            Some("@alice:example.com")
        );
        assert_eq!(canonical_logout_target("alice"), None);
        assert_eq!(canonical_logout_target("@alice"), None);
    }
}
