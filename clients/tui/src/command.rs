#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Switch(String),
    Rooms,
    Event(String),
    Whoami,
    Whereami,
    React(Option<String>),
    Unreact,
    Reply,
    Thread,
    Help,
    Shortcuts,
    Refresh,
    Quit,
    Send(String),
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
    SlashCommand::supported("/switch", true),
    SlashCommand::supported("/rooms", false),
    SlashCommand::supported("/event", true),
    SlashCommand::supported("/whoami", false),
    SlashCommand::supported("/whereami", false),
    SlashCommand::supported("/react", true),
    SlashCommand::supported("/unreact", false),
    SlashCommand::supported("/reply", false),
    SlashCommand::supported("/thread", false),
    SlashCommand::supported("/help", false),
    SlashCommand::supported("/shortcuts", false),
    SlashCommand::supported("/refresh", false),
    SlashCommand::supported("/quit", false),
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
        label: "/switch <room>",
        insert_text: "/switch ",
        description: "switch room by name, alias, ID, or number",
    },
    HelpCommand {
        label: "/rooms",
        insert_text: "/rooms",
        description: "refresh the room list",
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
        label: "/refresh",
        insert_text: "/refresh",
        description: "clear and redraw the display",
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
    if !input.starts_with('/') {
        return Command::Send(input.to_owned());
    }

    let mut parts = input[1..].splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let arg = parts.next().unwrap_or_default().trim();
    match name {
        "switch" if !arg.is_empty() => Command::Switch(arg.to_owned()),
        "switch" => {
            Command::Invalid("/switch requires a room id, alias, name, or index".to_owned())
        }
        "rooms" => Command::Rooms,
        "event" if !arg.is_empty() => Command::Event(arg.to_owned()),
        "event" => Command::Invalid("/event requires an event id".to_owned()),
        "whoami" => Command::Whoami,
        "whereami" => Command::Whereami,
        "react" => Command::React((!arg.is_empty()).then(|| arg.to_owned())),
        "unreact" => Command::Unreact,
        "reply" => Command::Reply,
        "thread" => Command::Thread,
        "help" | "?" => Command::Help,
        "shortcuts" => Command::Shortcuts,
        "refresh" => Command::Refresh,
        "quit" | "q" => Command::Quit,
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
    fn parses_switch() {
        assert_eq!(parse("/switch 2"), Command::Switch("2".to_owned()));
        assert_eq!(
            parse("/switch #room:localhost"),
            Command::Switch("#room:localhost".to_owned())
        );
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
    fn reports_missing_arguments() {
        assert_eq!(
            parse("/switch"),
            Command::Invalid("/switch requires a room id, alias, name, or index".to_owned())
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
