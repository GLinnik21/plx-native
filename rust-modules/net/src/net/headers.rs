//! The request-header block a caller hands to either transport, checked once, in one place.
//!
//! Two transports send a caller's header lines: the libcurl easy path (`net::request_tls_evidence`,
//! `CURLOPT_HTTPHEADER`) and the plaintext socket stream (`stream::perform_http_request`, which splices
//! the block into the request head). Neither used to look at what it was given beyond "no NUL", so a
//! block could
//!
//! * end a header early and start another (CR or LF inside a value) on the stream, where the block is
//!   request text;
//! * carry a second `User-Agent:` (libcurl REPLACES the one `CURLOPT_USERAGENT` set with a same-named
//!   custom header; the stream sends BOTH) or a second `Host:`, so the process's one name, or the
//!   origin the request was addressed to, was no longer the transport's to decide; or
//! * carry the framing headers (`Content-Length`, `Transfer-Encoding`) that the transport computes
//!   from the body it was given.
//!
//! This module is the one definition of what a caller may add. The transport owns the **session
//! headers**: [`RESERVED`], written by the transport itself (`Host`, the installed `User-Agent`, the
//! body's framing). A caller adds **per-request headers** on top (credentials, `Accept`, `Range`,
//! `Connection`, `Content-Type`, `Expect:`), and a line that names a session header is refused, not
//! silently dropped, so a bug that tries to set one is seen rather than half-working.
//!
//! What is checked is exactly what changes how a request is FRAMED or whose it is: the name is an
//! RFC 9110 token, the line has its colon, no CR, LF or NUL is in it, and no [`RESERVED`] name is used.
//! Other control characters in a value are not checked: they split nothing and truncate nothing, and a
//! block that is refused costs the request, so the check refuses only what is dangerous.
//!
//! A refusal names the KIND, never the line: a block carries the Plex token.

use std::fmt;

/// The headers the transport writes itself, lowercase. A caller's line naming one of these is refused.
///
/// * `host`: the origin the request was addressed to. A second `Host:` is request smuggling's first
///   step, and the stream builds its own from the `Origin` (`stream::host_header`).
/// * `user-agent`: the process's one name, installed at boot and applied in `net.rs` alone.
/// * `content-length`, `transfer-encoding`: libcurl and the stream compute them from the body.
pub const RESERVED: [&str; 4] = ["host", "user-agent", "content-length", "transfer-encoding"];

/// Why a header line or block was refused. Carries no part of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A block line with nothing in it: on the stream that is the blank line that ends the head.
    Empty,
    /// No `:` after a token (`Name;` and a bare word are libcurl-only spellings this layer never uses).
    NotAHeader,
    /// The name is empty or holds a character outside an RFC 9110 token.
    BadName,
    /// A CR, LF or NUL in the line: it would end the header, start another, or end the C string.
    Framing,
    /// The name is one the transport writes itself ([`RESERVED`]).
    Reserved,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::Empty => "an empty header line",
            Refusal::NotAHeader => "a header line with no name and colon",
            Refusal::BadName => "a header name that is not a token",
            Refusal::Framing => "a header line holding CR, LF or NUL",
            Refusal::Reserved => "a header the transport writes itself (Host, User-Agent, Content-Length, Transfer-Encoding)",
        })
    }
}

/// An RFC 9110 `tchar`.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
}

/// The name of a `Name: value` line, `Err` when it is not one.
fn name_of(line: &str) -> Result<&str, Refusal> {
    if line.is_empty() {
        return Err(Refusal::Empty);
    }
    if line.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return Err(Refusal::Framing);
    }
    let (name, _) = line.split_once(':').ok_or(Refusal::NotAHeader)?;
    if name.is_empty() || !name.chars().all(is_token_char) {
        return Err(Refusal::BadName);
    }
    Ok(name)
}

/// Check one `Name: value` line (`Name:` with an empty value is a header too: `Expect:` is how a
/// request tells libcurl not to send its own).
pub fn check_line(line: &str) -> Result<(), Refusal> {
    let name = name_of(line)?;
    if RESERVED.iter().any(|r| name.eq_ignore_ascii_case(r)) {
        return Err(Refusal::Reserved);
    }
    Ok(())
}

/// Check every line of a block of per-request headers (the easy path's `&[String]`).
pub fn check_block<S: AsRef<str>>(lines: &[S]) -> Result<(), Refusal> {
    lines.iter().try_for_each(|line| check_line(line.as_ref()))
}

/// The lines of the plaintext path's block: each CRLF-terminated, nothing else. The block is request
/// text, so a bare CR or LF anywhere, an empty line (it would end the head) or a line that is not a
/// permitted header is refused whole. `""` is the empty block.
pub fn check_wire_block(block: &str) -> Result<Vec<&str>, Refusal> {
    if block.is_empty() {
        return Ok(Vec::new());
    }
    let Some(body) = block.strip_suffix("\r\n") else {
        return Err(Refusal::Framing);
    };
    let lines: Vec<&str> = body.split("\r\n").collect();
    check_block(&lines)?;
    Ok(lines)
}

/// Whether any of `lines` is a header called `name` (an exact, case-insensitive name; `X-Accept:` is
/// not `Accept:`). `lines` have passed [`check_block`].
pub fn names_header<S: AsRef<str>>(lines: &[S], name: &str) -> bool {
    lines
        .iter()
        .any(|line| line.as_ref().split_once(':').is_some_and(|(n, _)| n.eq_ignore_ascii_case(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lines_the_producers_build_pass() {
        for line in [
            "X-Plex-Token: t",
            "X-Plex-Client-Identifier: 0a1b-2c",
            "Accept: application/json",
            "Content-Type: application/x-sentry-envelope",
            "Expect:",
            "Connection: close",
            "Range: bytes=1048576-",
            "X-Plex-Device-Name: PlxNative (LG TV)",
            "X-Plex-Language: be",
            "X-Value: tab\there",
            "X-Unicode: Ünïcode",
        ] {
            assert_eq!(check_line(line), Ok(()), "{line:?}");
        }
    }

    #[test]
    fn a_line_that_would_end_or_start_a_header_is_refused() {
        for line in ["X-A: 1\r\nX-B: 2", "X-A: 1\nX-B: 2", "X-A: 1\rX-B: 2", "X-A: 1\0", "\r\n"] {
            assert_eq!(check_line(line), Err(Refusal::Framing), "{line:?}");
        }
    }

    #[test]
    fn the_transports_own_headers_are_refused_in_any_case() {
        for line in [
            "User-Agent: other",
            "user-agent: other",
            "USER-AGENT:",
            "Host: evil.example",
            "host:evil.example",
            "Content-Length: 0",
            "Transfer-Encoding: chunked",
        ] {
            assert_eq!(check_line(line), Err(Refusal::Reserved), "{line:?}");
        }
        // A name that only CONTAINS a reserved one is a different header.
        assert_eq!(check_line("X-Forwarded-Host: h"), Ok(()));
        assert_eq!(check_line("X-User-Agent: u"), Ok(()));
    }

    #[test]
    fn a_line_without_a_token_and_a_colon_is_refused() {
        assert_eq!(check_line(""), Err(Refusal::Empty));
        assert_eq!(check_line("NoColon"), Err(Refusal::NotAHeader));
        assert_eq!(check_line("X-Empty;"), Err(Refusal::NotAHeader));
        assert_eq!(check_line(": value"), Err(Refusal::BadName));
        assert_eq!(check_line("Bad Name: v"), Err(Refusal::BadName));
        assert_eq!(check_line(" Leading: v"), Err(Refusal::BadName));
        assert_eq!(check_line("Name : v"), Err(Refusal::BadName));
    }

    #[test]
    fn a_wire_block_is_whole_crlf_terminated_lines() {
        assert_eq!(check_wire_block(""), Ok(vec![]));
        assert_eq!(
            check_wire_block("Connection: close\r\nX-Plex-Token: t\r\n"),
            Ok(vec!["Connection: close", "X-Plex-Token: t"])
        );
        // Not terminated: the head's own CRLF would join it to the blank line.
        assert_eq!(check_wire_block("X-A: 1"), Err(Refusal::Framing));
        // A blank line ends the head early and the rest would be the BODY.
        assert_eq!(check_wire_block("X-A: 1\r\n\r\nX-B: 2\r\n"), Err(Refusal::Empty));
        assert_eq!(check_wire_block("\r\n"), Err(Refusal::Empty));
        // A bare LF or CR inside a line.
        assert_eq!(check_wire_block("X-A: 1\nX-B: 2\r\n"), Err(Refusal::Framing));
        assert_eq!(check_wire_block("X-A: 1\rX-B: 2\r\n"), Err(Refusal::Framing));
        assert_eq!(check_wire_block("User-Agent: x\r\n"), Err(Refusal::Reserved));
    }

    #[test]
    fn a_header_is_named_by_its_whole_name() {
        let block = ["X-Accept: nothing", "Connection: close"];
        assert!(!names_header(&block, "accept"));
        assert!(names_header(&block, "CONNECTION"));
        assert!(names_header(&["accept:*/*"], "Accept"));
    }

    #[test]
    fn a_refusal_never_quotes_the_line() {
        let secret = "X-Plex-Token: s3cret\r\nHost: evil";
        let refusal = check_line(secret).unwrap_err();
        assert!(!format!("{refusal} {refusal:?}").contains("s3cret"));
    }
}
