//! Human styling belongs to each output destination. Explicit controls
//! override the environment; automatic color requires a capable terminal.

use std::ffi::OsString;
use std::io::IsTerminal as _;

use clap::ValueEnum;
use clap::builder::styling::{RgbColor, Style, Styles};

/// Preserve RGB when either Windows output destination is redirected.
/// anstream's legacy console adapter cannot represent RGB foregrounds.
pub fn stream_choice() -> anstream::ColorChoice {
    #[cfg(windows)]
    {
        static ANSI: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        // CONOUT$ addresses the attached console independently of stdout/stderr.
        if *ANSI.get_or_init(|| {
            (std::io::stdout().is_terminal() || std::io::stderr().is_terminal())
                && nu_ansi_term::enable_ansi_support().is_ok()
        }) {
            return anstream::ColorChoice::AlwaysAnsi;
        }
    }
    anstream::ColorChoice::Always
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    #[default]
    Auto,
    Always,
    Never,
}

impl Mode {
    pub fn stdout(self) -> Palette {
        self.destination(std::io::stdout().is_terminal())
    }

    pub fn stderr(self) -> Palette {
        self.destination(std::io::stderr().is_terminal())
    }

    fn destination(self, terminal: bool) -> Palette {
        Palette {
            enabled: match self {
                Self::Always => true,
                Self::Never => false,
                Self::Auto => {
                    terminal && anstyle_query::term_supports_color() && !anstyle_query::no_color()
                }
            },
        }
    }

    /// Help and errors may stop clap before it reads the global option.
    /// Clap remains the validator; the last complete valid occurrence
    /// controls early presentation, even when another occurrence is invalid.
    pub fn from_args(args: &[OsString]) -> Self {
        let mut mode = Self::Auto;
        let mut args = args.iter().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--" {
                break;
            }
            let Some(arg) = arg.to_str() else {
                continue;
            };
            let value = if arg == "--color" {
                args.clone().next().and_then(|value| value.to_str())
            } else {
                arg.strip_prefix("--color=")
            };
            if let Some(value) = value.and_then(|value| Self::from_str(value, false).ok()) {
                mode = value;
            }
        }
        mode
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Error,
    Warning,
    Success,
    Info,
    Heading,
    Muted,
    Command,
}

impl Role {
    const fn style(self) -> Style {
        match self {
            Self::Error => RgbColor(255, 92, 87).on_default().bold(),
            Self::Warning => RgbColor(243, 156, 18).on_default().bold(),
            Self::Success => RgbColor(90, 247, 142).on_default().bold(),
            Self::Info => RgbColor(92, 184, 255).on_default(),
            Self::Heading => RgbColor(189, 147, 249).on_default().bold(),
            Self::Muted => RgbColor(112, 120, 140).on_default(),
            Self::Command => Style::new().bold(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    enabled: bool,
}

impl Palette {
    pub const fn plain() -> Self {
        Self { enabled: false }
    }

    #[cfg(test)]
    pub const fn colored() -> Self {
        Self { enabled: true }
    }

    pub const fn is_colored(self) -> bool {
        self.enabled
    }

    pub fn rule(self, width: usize) -> String {
        self.paint(
            Role::Muted,
            &if self.enabled { "─" } else { "-" }.repeat(width.min(100)),
        )
    }

    /// Rich-terminal decoration follows the destination's styling policy.
    /// Redirected and explicitly plain output retain their original text.
    pub fn marker(self, role: Role) -> String {
        if !self.enabled {
            return String::new();
        }
        let symbol = match role {
            Role::Error => "✗",
            Role::Warning => "⚠",
            Role::Success => "✔",
            Role::Info => "▶",
            Role::Heading | Role::Muted | Role::Command => return String::new(),
        };
        format!("{} ", self.paint(role, symbol))
    }

    /// Reset each word while retaining all original whitespace, so a
    /// later layout pass cannot carry styling into continuation padding.
    pub fn paint(self, role: Role, text: &str) -> String {
        if !self.enabled {
            return text.to_owned();
        }
        let style = role.style();
        let mut output = String::new();
        let mut rest = text;
        while !rest.is_empty() {
            let whitespace = rest.chars().next().is_some_and(char::is_whitespace);
            let end = rest
                .char_indices()
                .find_map(|(index, ch)| (ch.is_whitespace() != whitespace).then_some(index))
                .unwrap_or(rest.len());
            let (span, remaining) = rest.split_at(end);
            if whitespace {
                output.push_str(span);
            } else {
                output.push_str(&format!("{style}{span}{}", style.render_reset()));
            }
            rest = remaining;
        }
        output
    }

    pub fn clap_styles(self) -> Styles {
        if self.enabled {
            Styles::plain()
                .header(Role::Heading.style())
                .usage(Role::Heading.style())
                .literal(Role::Command.style())
                .placeholder(Role::Muted.style())
                .valid(Role::Info.style())
                .invalid(Role::Error.style())
                .error(Role::Error.style())
        } else {
            Styles::plain()
        }
    }

    /// Only the diagnostic label is styled; paths, addresses and the
    /// diagnostic's explanation retain the terminal's default foreground.
    pub fn diagnostic(self, role: Role, text: &str) -> String {
        for label in ["configuration error:", "key file error:", "warning:"] {
            if let Some(message) = text.strip_prefix(&format!("udpstp: {label}")) {
                return format!("udpstp: {}{message}", self.paint(role, label));
            }
        }
        text.strip_prefix("udpstp:").map_or_else(
            || text.to_owned(),
            |message| format!("{}{message}", self.paint(role, "udpstp:")),
        )
    }
}
