#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Login {
        username: Option<String>,
        password: Option<String>,
        /// Optional homeserver base URL override (the inline third argument).
        /// When `None`, Axon resolves the homeserver from the Matrix ID.
        homeserver: Option<String>,
    },
    Logout(Option<String>),
    Recover(Option<String>),
    Delete(Option<String>),
    Room(String),
    Account(String),
    Status,
    Event(String),
    Whoami,
    Whereami,
    React(Option<String>),
    Unreact,
    Reply,
    Thread,
    /// Start an outgoing SAS verification against a pasted device ID (ADR 0028).
    Verify(Option<String>),
    /// Inspect the per-event verification bundle for an event ID (M7c).
    Bundle(String),
    Help,
    Shortcuts,
    Refresh,
    SaveConfig,
    EditConfig,
    Quit,
    Send(String),
    /// /html <raw-html> — send HTML literally as formatted_body
    SendHtml(String),
    /// /literal <text> — send as plaintext, skip markdown auto-conversion
    SendLiteral(String),
    /// /rainbow <text> — send text with each character in a cycling rainbow color
    Rainbow(String),
    /// /spoiler [reason |] text — send text hidden behind a spoiler warning
    Spoiler {
        reason: Option<String>,
        text: String,
    },
    Invalid(String),
    ApiUnsupported(String),
    Unknown(String),
    Empty,
}

#[derive(Clone, Copy)]
pub(crate) struct SlashCommand {
    pub(crate) name: &'static str,
    pub(crate) takes_argument: bool,
    api_supported: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct HelpCommand {
    pub(crate) label: &'static str,
    pub(crate) insert_text: &'static str,
    pub(crate) description: &'static str,
}

impl SlashCommand {
    const fn supported(name: &'static str, takes_argument: bool) -> Self {
        Self {
            name,
            takes_argument,
            api_supported: true,
        }
    }

    const fn api_unsupported(name: &'static str, takes_argument: bool) -> Self {
        Self {
            name,
            takes_argument,
            api_supported: false,
        }
    }
}

pub(crate) const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand::supported("/login", true),
    SlashCommand::supported("/logout", true),
    SlashCommand::supported("/recover", true),
    SlashCommand::supported("/delete", true),
    SlashCommand::supported("/room", true),
    SlashCommand::supported("/switch", true),
    SlashCommand::supported("/account", true),
    SlashCommand::supported("/status", false),
    SlashCommand::supported("/event", true),
    SlashCommand::supported("/whoami", false),
    SlashCommand::supported("/whereami", false),
    SlashCommand::supported("/react", true),
    SlashCommand::supported("/unreact", false),
    SlashCommand::supported("/reply", false),
    SlashCommand::supported("/thread", false),
    SlashCommand::supported("/verify", true),
    SlashCommand::supported("/bundle", true),
    SlashCommand::supported("/help", false),
    SlashCommand::supported("/shortcuts", false),
    SlashCommand::supported("/refresh", false),
    SlashCommand::supported("/rooms", false),
    SlashCommand::supported("/saveconfig", false),
    SlashCommand::supported("/editconfig", false),
    SlashCommand::supported("/quit", false),
    SlashCommand::supported("/html", true),
    SlashCommand::supported("/literal", true),
    SlashCommand::supported("/rainbow", true),
    SlashCommand::supported("/spoiler", true),
    SlashCommand::api_unsupported("/join", true),
    SlashCommand::api_unsupported("/leave", false),
    SlashCommand::api_unsupported("/part", false),
];

pub(crate) const HELP_COMMANDS: &[HelpCommand] = &[
    HelpCommand {
        label: "plain text",
        insert_text: "",
        description: "send a message to the current room",
    },
    HelpCommand {
        label: "//<text>",
        insert_text: "//",
        description: "send a message beginning with a literal /",
    },
    HelpCommand {
        label: "/html <html>",
        insert_text: "/html ",
        description: "send raw HTML as a formatted message (plain body is auto-stripped)",
    },
    HelpCommand {
        label: "/literal <text>",
        insert_text: "/literal ",
        description: "send text as plaintext, skipping markdown auto-conversion",
    },
    HelpCommand {
        label: "/rainbow <text>",
        insert_text: "/rainbow ",
        description: "send text with each character colored in cycling rainbow hues",
    },
    HelpCommand {
        label: "/spoiler [reason |] <text>",
        insert_text: "/spoiler ",
        description: "send text as a spoiler; optional reason before \" | \" becomes the label",
    },
    HelpCommand {
        label: "/login [user] [password] [homeserver]",
        insert_text: "/login ",
        description: "log in a Matrix account; prompts for missing credentials",
    },
    HelpCommand {
        label: "/logout [user]",
        insert_text: "/logout ",
        description: "log out an active account while retaining its archive",
    },
    HelpCommand {
        label: "/recover [user]",
        insert_text: "/recover ",
        description: "import encryption keys for an active account from a hidden prompt",
    },
    HelpCommand {
        label: "/delete [user]",
        insert_text: "/delete ",
        description: "permanently delete an account and all its data (requires typing YES)",
    },
    HelpCommand {
        label: "/room <room>, /switch <room>",
        insert_text: "/room ",
        description: "switch room by name, alias, ID, or number",
    },
    HelpCommand {
        label: "/account <account>",
        insert_text: "/account ",
        description: "filter by account (user ID, localpart, number, or \"all\")",
    },
    HelpCommand {
        label: "/status",
        insert_text: "/status",
        description: "show server connectivity and account state",
    },
    HelpCommand {
        label: "/event <id>",
        insert_text: "/event ",
        description: "show raw event JSON in status",
    },
    HelpCommand {
        label: "/whoami",
        insert_text: "/whoami",
        description: "show your Matrix ID and display name",
    },
    HelpCommand {
        label: "/whereami",
        insert_text: "/whereami",
        description: "show room information",
    },
    HelpCommand {
        label: "/react [emoji]",
        insert_text: "/react ",
        description: "react to the selected or most recent message",
    },
    HelpCommand {
        label: "/unreact",
        insert_text: "/unreact",
        description: "withdraw one of your reactions",
    },
    HelpCommand {
        label: "/reply",
        insert_text: "/reply",
        description: "reply to the selected or most recent message (pending API support)",
    },
    HelpCommand {
        label: "/thread",
        insert_text: "/thread",
        description:
            "start a thread from the selected or most recent message (pending API support)",
    },
    HelpCommand {
        label: "/verify <device_id>",
        insert_text: "/verify ",
        description:
            "start emoji SAS verification of a device (incoming requests open automatically)",
    },
    HelpCommand {
        label: "/bundle <event_id>",
        insert_text: "/bundle ",
        description: "show the sender-trust verification bundle for an event",
    },
    HelpCommand {
        label: "/help, /?",
        insert_text: "/help",
        description: "show this help",
    },
    HelpCommand {
        label: "/shortcuts",
        insert_text: "/shortcuts",
        description: "show keyboard shortcuts",
    },
    HelpCommand {
        label: "/refresh, /rooms",
        insert_text: "/refresh",
        description: "refresh rooms and redraw the display",
    },
    HelpCommand {
        label: "/saveconfig",
        insert_text: "/saveconfig",
        description: "save current display settings (input lines, pane widths) to config file",
    },
    HelpCommand {
        label: "/editconfig",
        insert_text: "/editconfig",
        description: "open the config file in $EDITOR (nano/notepad if unset)",
    },
    HelpCommand {
        label: "/quit, /q",
        insert_text: "/quit",
        description: "quit",
    },
    HelpCommand {
        label: "/join <room>",
        insert_text: "/join ",
        description: "pending Axon API support",
    },
    HelpCommand {
        label: "/leave, /part",
        insert_text: "/leave",
        description: "pending Axon API support",
    },
];

pub fn parse(input: &str) -> Command {
    let input = input.trim();
    if input.is_empty() {
        return Command::Empty;
    }
    if let Some(message) = input.strip_prefix("//") {
        return Command::Send(format!("/{message}"));
    }
    if !input.starts_with('/') {
        return Command::Send(input.to_owned());
    }

    let mut parts = input[1..].splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let arg = parts.next().unwrap_or_default().trim();
    match name {
        "login" => {
            // Positional: <user> <password> [homeserver]. The inline password is
            // a single token; a password with spaces is rejected here so it can be
            // typed at the hidden prompt (see the `/login` flow). The homeserver,
            // when present, is the third token and overrides Axon's resolution.
            let mut tokens = arg.split_whitespace();
            let username = tokens.next().map(str::to_owned);
            let password = tokens.next().map(str::to_owned);
            let homeserver = tokens.next().map(str::to_owned);
            if tokens.next().is_some() {
                return Command::Invalid(
                    "/login takes at most <user> <password> [homeserver]; for a password with \
                     spaces run `/login` (or `/login <user> [homeserver]`) and type it at the \
                     hidden prompt"
                        .to_owned(),
                );
            }
            Command::Login {
                username,
                password,
                homeserver,
            }
        }
        "logout" => Command::Logout((!arg.is_empty()).then(|| arg.to_owned())),
        "recover" => {
            let mut tokens = arg.split_whitespace();
            let target = tokens.next().map(str::to_owned);
            if tokens.next().is_some() {
                Command::Invalid(
                    "/recover takes at most one account target; the recovery key is entered at \
                     the hidden prompt"
                        .to_owned(),
                )
            } else {
                Command::Recover(target)
            }
        }
        "delete" => Command::Delete((!arg.is_empty()).then(|| arg.to_owned())),
        "room" | "switch" if !arg.is_empty() => Command::Room(arg.to_owned()),
        "room" | "switch" => {
            Command::Invalid("/room requires a room id, alias, name, or index".to_owned())
        }
        "account" if !arg.is_empty() => Command::Account(arg.to_owned()),
        "account" => Command::Invalid(
            "/account requires a user ID, localpart, number, or \"all\"".to_owned(),
        ),
        "status" => Command::Status,
        "event" if !arg.is_empty() => Command::Event(arg.to_owned()),
        "event" => Command::Invalid("/event requires an event id".to_owned()),
        "whoami" => Command::Whoami,
        "whereami" => Command::Whereami,
        "react" => Command::React((!arg.is_empty()).then(|| arg.to_owned())),
        "unreact" => Command::Unreact,
        "reply" => Command::Reply,
        "thread" => Command::Thread,
        "verify" => {
            let mut tokens = arg.split_whitespace();
            let device_id = tokens.next().map(str::to_owned);
            if tokens.next().is_some() {
                Command::Invalid("/verify takes at most one device id".to_owned())
            } else {
                Command::Verify(device_id)
            }
        }
        "bundle" if !arg.is_empty() => Command::Bundle(arg.to_owned()),
        "bundle" => Command::Invalid("/bundle requires an event id".to_owned()),
        "help" | "?" => Command::Help,
        "shortcuts" => Command::Shortcuts,
        "refresh" | "rooms" => Command::Refresh,
        "saveconfig" => Command::SaveConfig,
        "editconfig" => Command::EditConfig,
        "quit" | "q" => Command::Quit,
        "html" if !arg.is_empty() => Command::SendHtml(arg.to_owned()),
        "html" => Command::Invalid("/html requires HTML content to send".to_owned()),
        "literal" if !arg.is_empty() => Command::SendLiteral(arg.to_owned()),
        "literal" => Command::Invalid("/literal requires text to send".to_owned()),
        "rainbow" if !arg.is_empty() => Command::Rainbow(arg.to_owned()),
        "rainbow" => Command::Invalid("/rainbow requires text to send".to_owned()),
        "spoiler" if !arg.is_empty() => {
            if let Some((reason, text)) = arg.split_once(" | ") {
                let reason = reason.trim();
                Command::Spoiler {
                    reason: (!reason.is_empty()).then(|| reason.to_owned()),
                    text: text.to_owned(),
                }
            } else {
                Command::Spoiler {
                    reason: None,
                    text: arg.to_owned(),
                }
            }
        }
        "spoiler" => Command::Invalid("/spoiler requires text to send".to_owned()),
        other => {
            let command_name = format!("/{other}");
            if SLASH_COMMANDS
                .iter()
                .any(|command| command.name == command_name && !command.api_supported)
            {
                Command::ApiUnsupported(format!(
                    "{command_name} is not supported by the current Axon API"
                ))
            } else {
                Command::Unknown(format!("unknown command: {command_name}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_room() {
        assert_eq!(parse("/room 2"), Command::Room("2".to_owned()));
        assert_eq!(
            parse("/room #room:localhost"),
            Command::Room("#room:localhost".to_owned())
        );
        assert_eq!(parse("/switch 2"), Command::Room("2".to_owned()));
    }

    #[test]
    fn parses_login_forms() {
        assert_eq!(
            parse("/login"),
            Command::Login {
                username: None,
                password: None,
                homeserver: None,
            }
        );
        assert_eq!(
            parse("/login @me:example.com"),
            Command::Login {
                username: Some("@me:example.com".to_owned()),
                password: None,
                homeserver: None,
            }
        );
        assert_eq!(
            parse("/login @me:example.com hunter2"),
            Command::Login {
                username: Some("@me:example.com".to_owned()),
                password: Some("hunter2".to_owned()),
                homeserver: None,
            }
        );
    }

    #[test]
    fn parses_login_with_homeserver_override() {
        assert_eq!(
            parse("/login @me:example.com hunter2 homeserver.example.org"),
            Command::Login {
                username: Some("@me:example.com".to_owned()),
                password: Some("hunter2".to_owned()),
                homeserver: Some("homeserver.example.org".to_owned()),
            }
        );
    }

    #[test]
    fn rejects_inline_password_with_spaces() {
        // The single-token inline password means extra tokens are a mistake
        // (most likely a space in the password) — steer to the hidden prompt.
        assert!(matches!(
            parse("/login @me:example.com a password with spaces"),
            Command::Invalid(_)
        ));
    }

    #[test]
    fn parses_logout_forms() {
        assert_eq!(parse("/logout"), Command::Logout(None));
        assert_eq!(
            parse("/logout @me:example.com"),
            Command::Logout(Some("@me:example.com".to_owned()))
        );
        assert_eq!(parse("/logout me"), Command::Logout(Some("me".to_owned())));
    }

    #[test]
    fn parses_delete_forms() {
        assert_eq!(parse("/delete"), Command::Delete(None));
        assert_eq!(
            parse("/delete @me:example.com"),
            Command::Delete(Some("@me:example.com".to_owned()))
        );
        assert_eq!(parse("/delete me"), Command::Delete(Some("me".to_owned())));
    }

    #[test]
    fn parses_recover_forms_and_rejects_inline_keys() {
        assert_eq!(parse("/recover"), Command::Recover(None));
        assert_eq!(
            parse("/recover @me:example.com"),
            Command::Recover(Some("@me:example.com".to_owned()))
        );
        assert!(matches!(
            parse("/recover @me:example.com inline-key"),
            Command::Invalid(message) if message.contains("hidden prompt")
        ));
    }

    #[test]
    fn parses_verify_forms() {
        assert_eq!(parse("/verify"), Command::Verify(None));
        assert_eq!(
            parse("/verify ABCDEF"),
            Command::Verify(Some("ABCDEF".to_owned()))
        );
        assert!(matches!(parse("/verify a b"), Command::Invalid(_)));
    }

    #[test]
    fn parses_bundle_forms() {
        assert_eq!(
            parse("/bundle $event:localhost"),
            Command::Bundle("$event:localhost".to_owned())
        );
        assert!(matches!(parse("/bundle"), Command::Invalid(_)));
    }

    #[test]
    fn parses_quit_aliases() {
        assert_eq!(parse("/quit"), Command::Quit);
        assert_eq!(parse("/q"), Command::Quit);
    }

    #[test]
    fn parses_help() {
        assert_eq!(parse("/help"), Command::Help);
        assert_eq!(parse("/?"), Command::Help);
    }

    #[test]
    fn parses_shortcuts() {
        assert_eq!(parse("/shortcuts"), Command::Shortcuts);
    }

    #[test]
    fn parses_refresh() {
        assert_eq!(parse("/refresh"), Command::Refresh);
        assert_eq!(parse("/rooms"), Command::Refresh);
    }

    #[test]
    fn parses_whoami() {
        assert_eq!(parse("/whoami"), Command::Whoami);
    }

    #[test]
    fn parses_whereami() {
        assert_eq!(parse("/whereami"), Command::Whereami);
    }

    #[test]
    fn parses_message_action_commands() {
        assert_eq!(parse("/react"), Command::React(None));
        assert_eq!(parse("/react +1"), Command::React(Some("+1".to_owned())));
        assert_eq!(parse("/react 🚀"), Command::React(Some("🚀".to_owned())));
        assert_eq!(parse("/unreact"), Command::Unreact);
        assert_eq!(parse("/reply"), Command::Reply);
        assert_eq!(parse("/thread"), Command::Thread);
    }

    #[test]
    fn parses_plain_text_as_send() {
        assert_eq!(parse("hello"), Command::Send("hello".to_owned()));
        assert_eq!(
            parse("  hello world  "),
            Command::Send("hello world".to_owned())
        );
    }

    #[test]
    fn parses_double_slash_as_literal_leading_slash() {
        assert_eq!(parse("//help"), Command::Send("/help".to_owned()));
        assert_eq!(parse("///help"), Command::Send("//help".to_owned()));
        assert_eq!(parse("//"), Command::Send("/".to_owned()));
        assert_eq!(parse("  //help  "), Command::Send("/help".to_owned()));
    }

    #[test]
    fn reports_missing_arguments() {
        assert_eq!(
            parse("/room"),
            Command::Invalid("/room requires a room id, alias, name, or index".to_owned())
        );
        assert_eq!(
            parse("/event"),
            Command::Invalid("/event requires an event id".to_owned())
        );
    }

    #[test]
    fn reports_known_api_unsupported_commands() {
        assert_eq!(
            parse("/join #room:localhost"),
            Command::ApiUnsupported("/join is not supported by the current Axon API".to_owned())
        );
        assert_eq!(
            parse("/leave"),
            Command::ApiUnsupported("/leave is not supported by the current Axon API".to_owned())
        );
        assert_eq!(
            parse("/part"),
            Command::ApiUnsupported("/part is not supported by the current Axon API".to_owned())
        );
    }

    #[test]
    fn reports_unknown_commands() {
        assert_eq!(
            parse("/frobnicate"),
            Command::Unknown("unknown command: /frobnicate".to_owned())
        );
    }
}
