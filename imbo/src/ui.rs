//! Terminal look: brand colours from `brand/DESIGN.md`, a cliclack theme and
//! small print helpers. UI text is English; message content is shown as is.

use std::fmt::Display;

use cliclack::{Theme, ThemeState};
use console::{Style, StyledObject};

// brand tokens
const PRIMARY: (u8, u8, u8) = (0xED, 0xEA, 0xE2);
const SECONDARY_TEXT: (u8, u8, u8) = (0xC1, 0xBD, 0xB4);
const MUTED: (u8, u8, u8) = (0x83, 0x80, 0x77);
const SUCCESS: (u8, u8, u8) = (0x5B, 0xCC, 0x80);
const WARNING: (u8, u8, u8) = (0xEF, 0xB1, 0x46);
const ERROR: (u8, u8, u8) = (0xED, 0x79, 0xA4);
const UNREAD: (u8, u8, u8) = (0xD9, 0x1E, 0x36);

fn rgb(c: (u8, u8, u8)) -> Style {
    Style::new().true_color(c.0, c.1, c.2)
}

pub fn strong() -> Style {
    rgb(PRIMARY).bold()
}
pub fn text() -> Style {
    rgb(SECONDARY_TEXT)
}
pub fn muted() -> Style {
    rgb(MUTED)
}
pub fn ok() -> Style {
    rgb(SUCCESS)
}
pub fn warn() -> Style {
    rgb(WARNING)
}
pub fn bad() -> Style {
    rgb(ERROR)
}
pub fn unread() -> Style {
    rgb(UNREAD)
}

pub fn s<D: Display>(st: Style, d: D) -> StyledObject<D> {
    st.apply_to(d)
}

struct Brand;

impl Theme for Brand {
    fn bar_color(&self, state: &ThemeState) -> Style {
        match state {
            ThemeState::Active => rgb(PRIMARY),
            ThemeState::Cancel => rgb(ERROR),
            ThemeState::Submit => rgb(MUTED),
            ThemeState::Error(_) => rgb(WARNING),
        }
    }

    fn state_symbol_color(&self, state: &ThemeState) -> Style {
        match state {
            ThemeState::Submit => rgb(SUCCESS),
            _ => self.bar_color(state),
        }
    }

    fn state_symbol(&self, state: &ThemeState) -> String {
        let symbol = match state {
            ThemeState::Active => "◆",
            ThemeState::Cancel => "■",
            ThemeState::Submit => "✓",
            ThemeState::Error(_) => "▲",
        };
        self.state_symbol_color(state).apply_to(symbol).to_string()
    }

    fn radio_symbol(&self, state: &ThemeState, selected: bool) -> String {
        match state {
            ThemeState::Active if selected => rgb(SUCCESS).apply_to("●").to_string(),
            ThemeState::Active => rgb(MUTED).apply_to("○").to_string(),
            _ => String::new(),
        }
    }

    fn checkbox_symbol(&self, state: &ThemeState, selected: bool, active: bool) -> String {
        match state {
            ThemeState::Active | ThemeState::Error(_) => {
                if selected {
                    rgb(SUCCESS).apply_to("◼").to_string()
                } else if active {
                    rgb(PRIMARY).apply_to("◻").to_string()
                } else {
                    rgb(MUTED).apply_to("◻").to_string()
                }
            }
            _ => String::new(),
        }
    }

    fn info_symbol(&self) -> String {
        rgb(PRIMARY).apply_to("●").to_string()
    }

    fn warning_symbol(&self) -> String {
        rgb(WARNING).apply_to("▲").to_string()
    }

    fn error_symbol(&self) -> String {
        rgb(ERROR).apply_to("■").to_string()
    }

    fn active_symbol(&self) -> String {
        rgb(SUCCESS).apply_to("✓").to_string()
    }

    fn submit_symbol(&self) -> String {
        rgb(SUCCESS).apply_to("✓").to_string()
    }
}

pub fn init() {
    cliclack::set_theme(Brand);
}

pub fn is_interactive() -> bool {
    console::Term::stderr().is_term() || std::fs::File::open("/dev/tty").is_ok()
}

pub fn ok_line(msg: &str) {
    eprintln!("{} {}", ok().apply_to("✓"), msg);
}

pub fn warn_line(msg: &str) {
    eprintln!("{} {}", warn().apply_to("!"), msg);
}

pub fn error_line(msg: &str) {
    eprintln!("{} {}", bad().apply_to("✗"), msg);
}

pub fn dim(msg: impl Display) -> String {
    muted().apply_to(msg).to_string()
}

pub fn heading(msg: impl Display) -> String {
    strong().apply_to(msg).to_string()
}

/// Online dot: success colour when online, muted when offline.
pub fn dot(online: bool) -> String {
    if online {
        ok().apply_to("●").to_string()
    } else {
        muted().apply_to("○").to_string()
    }
}

/// Relative time like "3m ago".
pub fn ago(t: chrono::DateTime<chrono::Utc>) -> String {
    let s = (chrono::Utc::now() - t).num_seconds().max(0);
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}
