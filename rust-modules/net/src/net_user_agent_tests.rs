//! The `User-Agent` boot installs reaches the wire as one header value, never as more than one.

use super::*;

/// A value whose second line would be a header of its own on the plaintext path, which writes the
/// installed value into the request text unescaped.
const INJECTED: &str = "PlxNative/1.0\r\nX-Injected: 1";

/// Whatever `set_user_agent` was given, what the transport would send holds no control character:
/// a CR or LF splits the request, and a NUL ends the C string curl reads while the plaintext path
/// would still write it. The process installs one value and keeps it, so this does not depend on
/// which other test installed its own first.
#[test]
fn a_user_agent_with_a_line_break_is_refused_and_never_installed() {
    assert_eq!(set_user_agent(INJECTED.to_string()), Err(InvalidUserAgent));
    assert!(
        user_agent().map_or(true, |ua| !ua.chars().any(char::is_control)),
        "installed: {:?}",
        user_agent()
    );
}

#[test]
fn every_control_character_is_refused_and_a_plain_value_is_kept_as_written() {
    for bad in ["a\rb", "a\nb", "a\0b", "a\tb", "a\x1bb", "a\x7fb", "a\u{85}b", "\r\n"] {
        assert!(UserAgent::new(bad.to_string()).is_err(), "{bad:?}");
    }
    let ua = UserAgent::new("PlxNative/0.9 (webOS; LG) Ünïcode".to_string()).expect("plain text");
    assert_eq!(ua.text, "PlxNative/0.9 (webOS; LG) Ünïcode");
    assert_eq!(ua.c_string.to_str(), Ok(ua.text.as_str()), "the curl string is the same value");
}
