//! Validators and small text helpers used on both sides.

use crate::{MAX_GROUP_NAME_CHARS, MAX_NAME_CHARS, MAX_TITLE_CHARS};

/// Last 6 characters of the session ID with dashes removed, lowercase.
/// The end, not the start: Codex thread IDs are UUIDv7, whose leading
/// characters are a timestamp shared by every thread made the same day.
pub fn short_id(session_id: &str) -> String {
    let chars: Vec<char> = session_id.chars().filter(|c| *c != '-').collect();
    chars[chars.len().saturating_sub(6)..]
        .iter()
        .collect::<String>()
        .to_lowercase()
}

/// Drop control characters (keeps newlines and tabs when `keep_lines`).
pub fn strip_control(s: &str, keep_lines: bool) -> String {
    s.chars()
        .filter(|c| !c.is_control() || (keep_lines && (*c == '\n' || *c == '\t')))
        .collect()
}

/// Account name rule: 1–32 chars, no slash or control chars, not all digits.
pub fn validate_name(raw: &str) -> Result<String, &'static str> {
    let name = raw.trim();
    let n = name.chars().count();
    if n == 0 {
        return Err("Name cannot be empty.");
    }
    if n > MAX_NAME_CHARS {
        return Err("Name can be at most 32 characters.");
    }
    if name.contains('/') {
        return Err("Name cannot contain a slash.");
    }
    if name.starts_with('#') {
        return Err("Name cannot start with #.");
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("Name cannot contain control characters.");
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return Err("Name cannot be all digits.");
    }
    Ok(name.to_string())
}

pub fn validate_group_name(raw: &str) -> Result<String, &'static str> {
    let name = raw.trim();
    let n = name.chars().count();
    if n == 0 || n > MAX_GROUP_NAME_CHARS {
        return Err("Group name must be 1 to 64 characters.");
    }
    if name.contains('/') || name.starts_with('#') || name.chars().any(|c| c.is_control()) {
        return Err(
            "Group name cannot contain a slash, start with # or contain control characters.",
        );
    }
    Ok(name.to_string())
}

/// Session title as stored: control chars removed, at most 200 chars, short ID when empty.
pub fn clean_title(raw: &str, session_id: &str) -> String {
    let t: String = strip_control(raw, false)
        .trim()
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
    if t.is_empty() {
        short_id(session_id)
    } else {
        t
    }
}

pub fn valid_email(s: &str) -> bool {
    let s = s.trim();
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && s.len() <= 254
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
        && !domain.contains('@')
}

/// Parsed `to` field (DESIGN §9 收件人写法).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// `#1234567`
    Group(i32),
    /// `who/session` — contact name or account ID, then title or short ID.
    Direct { who: String, session: String },
    /// No slash: a group name, or a contact without a session.
    Bare(String),
}

pub fn parse_address(to: &str) -> Option<Address> {
    let to = to.trim();
    if to.is_empty() {
        return None;
    }
    if let Some(rest) = to.strip_prefix('#')
        && rest.len() == 7
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return rest.parse().ok().map(Address::Group);
    }
    if let Some((who, session)) = to.split_once('/') {
        let (who, session) = (who.trim(), session.trim());
        if who.is_empty() || session.is_empty() {
            return None;
        }
        return Some(Address::Direct {
            who: who.to_string(),
            session: session.to_string(),
        });
    }
    Some(Address::Bare(to.to_string()))
}

/// Truncate to `max` characters; returns the text and whether it was cut.
pub fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        (s.to_string(), false)
    } else {
        (s.chars().take(max).collect(), true)
    }
}

/// Attachment name as stored (DESIGN §9 files.name): slashes, backslashes and
/// control characters become `_`, at most 255 bytes, never `.` or `..`.
pub fn clean_file_name(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s.len() > 255 {
        let mut end = 255;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    match s.as_str() {
        "" => "file".into(),
        "." | ".." => "_".repeat(s.len()),
        _ => s,
    }
}

/// Percent-encode everything but unreserved characters (RFC 3986).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ids() {
        assert_eq!(short_id("01a16f07-8c94-7c73-9b90-9F0D506D945D"), "6d945d");
        assert_eq!(short_id("ab-c"), "abc");
    }

    #[test]
    fn names() {
        assert!(validate_name("12345678").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(&"x".repeat(33)).is_err());
        assert!(validate_name("").is_err());
        assert_eq!(validate_name("  bob ").unwrap(), "bob");
        assert!(validate_name("张三").is_ok());
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_address("#5520183"), Some(Address::Group(5520183)));
        assert_eq!(
            parse_address("bob/API 联调"),
            Some(Address::Direct {
                who: "bob".into(),
                session: "API 联调".into()
            })
        );
        assert_eq!(
            parse_address("kando-dev"),
            Some(Address::Bare("kando-dev".into()))
        );
        assert_eq!(parse_address("#12"), Some(Address::Bare("#12".into())));
        assert_eq!(parse_address("bob/"), None);
        assert_eq!(parse_address("  "), None);
    }

    #[test]
    fn file_names() {
        assert_eq!(clean_file_name("a/b\\c\u{7}.txt"), "a_b_c_.txt");
        assert_eq!(clean_file_name(".."), "__");
        assert_eq!(clean_file_name(""), "file");
        assert_eq!(clean_file_name(&"字".repeat(100)).len(), 255);
        let name = "报告 v2 (final).pdf";
        assert_eq!(percent_decode(&percent_encode(name)).as_deref(), Some(name));
        assert_eq!(percent_decode("%zz"), None);
    }

    #[test]
    fn emails() {
        assert!(valid_email("a@b.co"));
        assert!(!valid_email("not-an-email"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@c.com"));
    }
}
