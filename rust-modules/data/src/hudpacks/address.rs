//! The address a viewer types to install a HUD package from (`docs/hud-plugins.md`, "Sideload"): one
//! pure function, [`parse`], and a typed [`AddressError`] for every way it refuses.
//!
//! Three forms are accepted, after leading and trailing whitespace is trimmed and the scheme is read
//! without regard to case:
//!
//! | typed | fetched |
//! |---|---|
//! | `gh:owner/repo` | `https://github.com/owner/repo/releases/latest/download/hud.plxhud.json` |
//! | `gh:owner/repo@tag` | `https://github.com/owner/repo/releases/download/<tag>/hud.plxhud.json` |
//! | `https://host[:port]/path?query` | itself, when it is well-formed and carries no `user:pass@` |
//! | `http://a.b.c.d[:port]/path` | itself, only when the host is a literal RFC 1918 IPv4 address; shown as a [dev source](Address::dev_source) |
//!
//! `owner`, `repo` and `tag` are restricted to GitHub's own character sets and are percent-encoded
//! nowhere, so no part of a `gh:` address can name another path, host or query. A name, an IPv6
//! literal, a loopback or a public address is refused over `http://`: a plaintext fetch of code the
//! television will draw is acceptable for a developer's machine on the home network and for nothing
//! else. What the fetch then does with a redirect is `plx_net`'s rule, not this parser's.
//!
//! The parser reads no network, no clock and no global, and builds the URL as text; `Fetch` carries
//! it to the runner (`HudEvent::Fetch`, `HudEffect::Fetch`).

use std::net::Ipv4Addr;

/// The file name a `gh:` address fetches from a release: the asset a package author attaches to a
/// release under exactly this name.
pub const GITHUB_ASSET: &str = "hud.plxhud.json";

/// The longest address the parser reads. A typed address is input, not content, and an unbounded
/// string is never handed to a URL builder; browsers and servers settle on this length too.
pub const ADDRESS_LEN_LIMIT: usize = 2048;

/// The longest `owner` of a `gh:` address: GitHub's own limit for a user or organisation name.
const OWNER_LEN_LIMIT: usize = 39;
/// The longest `repo`: GitHub's limit for a repository name.
const REPO_LEN_LIMIT: usize = 100;
/// The longest `tag` accepted: git allows more, a release tag a person types does not need it.
const TAG_LEN_LIMIT: usize = 128;

/// An address that passed [`parse`]: the URL to fetch and what the prompt shows of where it is from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    url: String,
    host: String,
    dev_source: bool,
}

impl Address {
    /// The URL to fetch. A well-formed `https://` or private-LAN `http://` URL, or the GitHub release
    /// URL a `gh:` address maps to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The host without a port, userinfo, path or query: the one part of the address that may be
    /// logged (`hud: ... host=<host>`) and shown beside the package.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// `true` for an `http://` address: a developer's machine on the home network, which the prompt
    /// says so about. Every other address is a published source.
    pub fn dev_source(&self) -> bool {
        self.dev_source
    }
}

/// Why [`parse`] refused a string. One variant per refusal, so the page can say which, and the
/// accepted forms with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressError {
    /// Nothing but whitespace.
    Empty,
    /// Longer than [`ADDRESS_LEN_LIMIT`].
    TooLong,
    /// Not `gh:`, `https://` or `http://`.
    Form,
    /// A `gh:` address that is not `owner/repo` or `owner/repo@tag`.
    GithubShape,
    /// A `gh:` owner outside GitHub's user and organisation names.
    GithubOwner,
    /// A `gh:` repository outside GitHub's repository names.
    GithubRepo,
    /// A `gh:` tag outside the characters a release tag is typed with here.
    GithubTag,
    /// A URL with a byte a URL does not carry (a space, a control, a backslash, a non-ASCII
    /// character, a `%` not followed by two hex digits) or an authority that is not `host[:port]`.
    Malformed,
    /// A URL with no host.
    NoHost,
    /// A URL carrying `user:pass@`.
    Userinfo,
    /// A port that is not 1 to 65535.
    BadPort,
    /// An `http://` address whose host is not a literal RFC 1918 IPv4 address.
    HttpNotPrivate,
}

/// Parse what a viewer typed. See the module doc for the forms; anything else is an [`AddressError`].
pub fn parse(text: &str) -> Result<Address, AddressError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AddressError::Empty);
    }
    if text.len() > ADDRESS_LEN_LIMIT {
        return Err(AddressError::TooLong);
    }
    if let Some(rest) = strip_scheme(text, "gh:") {
        return github(rest);
    }
    if let Some(rest) = strip_scheme(text, "https://") {
        return web(rest, true);
    }
    if let Some(rest) = strip_scheme(text, "http://") {
        return web(rest, false);
    }
    Err(AddressError::Form)
}

/// `text` after `scheme`, when it begins with it in any case. The scheme is ASCII, so a byte slice
/// at its length is always on a character boundary of a match.
fn strip_scheme<'a>(text: &'a str, scheme: &str) -> Option<&'a str> {
    let head = text.as_bytes().get(..scheme.len())?;
    head.eq_ignore_ascii_case(scheme.as_bytes()).then(|| &text[scheme.len()..])
}

fn github(rest: &str) -> Result<Address, AddressError> {
    let (path, tag) = match rest.split_once('@') {
        Some((path, tag)) => (path, Some(tag)),
        None => (rest, None),
    };
    let mut parts = path.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(AddressError::GithubShape);
    };
    if !github_owner(owner) {
        return Err(AddressError::GithubOwner);
    }
    if !github_repo(repo) {
        return Err(AddressError::GithubRepo);
    }
    let url = match tag {
        None => format!("https://github.com/{owner}/{repo}/releases/latest/download/{GITHUB_ASSET}"),
        Some(tag) if github_tag(tag) => {
            format!("https://github.com/{owner}/{repo}/releases/download/{tag}/{GITHUB_ASSET}")
        }
        Some(_) => return Err(AddressError::GithubTag),
    };
    Ok(Address { url, host: "github.com".to_owned(), dev_source: false })
}

/// A GitHub user or organisation name: ASCII letters, digits and hyphens, not starting or ending
/// with a hyphen.
fn github_owner(owner: &str) -> bool {
    (1..=OWNER_LEN_LIMIT).contains(&owner.len())
        && owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !owner.starts_with('-')
        && !owner.ends_with('-')
}

/// A GitHub repository name: ASCII letters, digits, `-`, `_` and `.`, and not `.` or `..`.
fn github_repo(repo: &str) -> bool {
    (1..=REPO_LEN_LIMIT).contains(&repo.len())
        && repo.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && repo != "."
        && repo != ".."
}

/// A release tag as typed here: ASCII letters, digits, `-`, `_`, `.` and `+`, not starting with a
/// dot and with no `..`. A `/` is not in the set (a tag with one would name another path).
fn github_tag(tag: &str) -> bool {
    (1..=TAG_LEN_LIMIT).contains(&tag.len())
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
        && !tag.starts_with('.')
        && !tag.contains("..")
}

/// `rest` is what follows `https://` (`secure`) or `http://`: `authority[/path][?query][#fragment]`.
fn web(rest: &str, secure: bool) -> Result<Address, AddressError> {
    if !rest.bytes().all(url_byte) || !percent_escapes_whole(rest) {
        return Err(AddressError::Malformed);
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return Err(AddressError::Userinfo);
    }
    let (host, port) = split_port(authority)?;
    if host.is_empty() {
        return Err(AddressError::NoHost);
    }
    if let Some(port) = port {
        if !port_in_range(port) {
            return Err(AddressError::BadPort);
        }
    }
    if secure {
        if !host_name(host) && !ipv6_literal(host) {
            return Err(AddressError::Malformed);
        }
        return Ok(Address { url: format!("https://{rest}"), host: bare(host).to_owned(), dev_source: false });
    }
    match host.parse::<Ipv4Addr>() {
        Ok(ip) if ip.is_private() => {
            Ok(Address { url: format!("http://{rest}"), host: host.to_owned(), dev_source: true })
        }
        _ => Err(AddressError::HttpNotPrivate),
    }
}

/// The bytes a URL is written with: unreserved and reserved ASCII and `%`. A space, a control, `\`,
/// a quote, `<`, `>`, `{`, `}`, `|`, `^`, a backtick and every non-ASCII byte are outside it.
fn url_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.' | b'_' | b'~' | b':' | b'/' | b'?' | b'#' | b'[' | b']' | b'@' | b'!' | b'$' | b'&' | b'\''
                | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b'%'
        )
}

/// Every `%` is followed by two hex digits.
fn percent_escapes_whole(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().all(|(i, &b)| {
        b != b'%' || bytes.get(i + 1..i + 3).is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
    })
}

/// `authority` as `(host, port digits)`. A bracketed IPv6 literal keeps its brackets in the host.
fn split_port(authority: &str) -> Result<(&str, Option<&str>), AddressError> {
    if authority.starts_with('[') {
        let close = authority.find(']').ok_or(AddressError::Malformed)?;
        let host = &authority[..=close];
        return match &authority[close + 1..] {
            "" => Ok((host, None)),
            tail => tail.strip_prefix(':').map(|port| (host, Some(port))).ok_or(AddressError::Malformed),
        };
    }
    Ok(match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    })
}

fn port_in_range(digits: &str) -> bool {
    !digits.is_empty()
        && digits.len() <= 5
        && digits.bytes().all(|b| b.is_ascii_digit())
        && digits.parse::<u32>().is_ok_and(|port| (1..=65535).contains(&port))
}

/// A registered name or dotted IPv4: ASCII letters, digits and hyphens in dot-separated labels, none
/// empty and none starting or ending with a hyphen.
fn host_name(host: &str) -> bool {
    host.len() <= 253
        && host.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// A bracketed IPv6 literal that `std` reads as one.
fn ipv6_literal(host: &str) -> bool {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .is_some_and(|inner| inner.parse::<std::net::Ipv6Addr>().is_ok())
}

/// `host` without the brackets of an IPv6 literal.
fn bare(host: &str) -> &str {
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> String {
        parse(text).unwrap_or_else(|e| panic!("{text:?} refused: {e:?}")).url().to_owned()
    }

    #[test]
    fn a_github_address_maps_to_its_release_asset() {
        assert_eq!(
            url("gh:owner/repo"),
            "https://github.com/owner/repo/releases/latest/download/hud.plxhud.json"
        );
        assert_eq!(
            url("gh:owner/repo@v1.2.3"),
            "https://github.com/owner/repo/releases/download/v1.2.3/hud.plxhud.json"
        );
        let a = parse("gh:Some-Org/my_repo.v2@1.0.0-rc.1+build").unwrap();
        assert_eq!(
            a.url(),
            "https://github.com/Some-Org/my_repo.v2/releases/download/1.0.0-rc.1+build/hud.plxhud.json"
        );
        assert_eq!((a.host(), a.dev_source()), ("github.com", false));
    }

    #[test]
    fn whitespace_is_trimmed_and_the_scheme_ignores_case() {
        assert_eq!(url("  \tgh:o/r \n"), url("gh:o/r"));
        assert_eq!(url("GH:o/r"), url("gh:o/r"));
        assert_eq!(url(" HTTPS://example.com/hud.json "), "https://example.com/hud.json");
        assert_eq!(url("Http://192.168.1.5/x"), "http://192.168.1.5/x");
    }

    #[test]
    fn a_github_part_outside_githubs_character_sets_names_the_part() {
        let cases: [(&str, AddressError); 17] = [
            ("gh:", AddressError::GithubShape),
            ("gh:owner", AddressError::GithubShape),
            ("gh:owner/repo/extra", AddressError::GithubShape),
            ("gh:/repo", AddressError::GithubOwner),
            ("gh:-owner/repo", AddressError::GithubOwner),
            ("gh:owner-/repo", AddressError::GithubOwner),
            ("gh:own_er/repo", AddressError::GithubOwner),
            ("gh:own%65r/repo", AddressError::GithubOwner),
            ("gh:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/repo", AddressError::GithubOwner),
            ("gh:owner/", AddressError::GithubRepo),
            ("gh:owner/..", AddressError::GithubRepo),
            ("gh:owner/re po", AddressError::GithubRepo),
            ("gh:owner/re?po", AddressError::GithubRepo),
            ("gh:owner/repo@", AddressError::GithubTag),
            ("gh:owner/repo@v1/../../x", AddressError::GithubTag),
            ("gh:owner/repo@..", AddressError::GithubTag),
            ("gh:owner/repo@v1%2f", AddressError::GithubTag),
        ];
        for (text, want) in cases {
            assert_eq!(parse(text), Err(want), "{text:?}");
        }
        // A tag with a slash would name a second path segment: it is outside the tag's set.
        assert_eq!(parse("gh:owner/repo@a/b"), Err(AddressError::GithubTag));
        assert_eq!(parse("gh:owner/repo@a@b"), Err(AddressError::GithubTag));
        assert_eq!(parse("gh:owner/repo@.hidden"), Err(AddressError::GithubTag));
    }

    #[test]
    fn an_https_address_passes_when_well_formed() {
        assert_eq!(url("https://example.com"), "https://example.com");
        assert_eq!(url("https://example.com:8443/a/b.json?x=1&y=%20#frag"), "https://example.com:8443/a/b.json?x=1&y=%20#frag");
        assert_eq!(url("https://raw.githubusercontent.com/o/r/main/hud.plxhud.json"), "https://raw.githubusercontent.com/o/r/main/hud.plxhud.json");
        let a = parse("https://[2001:db8::1]:8443/hud.json").unwrap();
        assert_eq!((a.host(), a.dev_source()), ("2001:db8::1", false));
        let a = parse("https://Example.COM:8443/hud.json?token=secret").unwrap();
        assert_eq!((a.host(), a.dev_source()), ("Example.COM", false), "the host alone, never the query");
    }

    #[test]
    fn an_https_address_is_refused_for_each_way_it_is_malformed() {
        let cases = [
            ("https://", AddressError::NoHost),
            ("https:///path", AddressError::NoHost),
            ("https://:443/", AddressError::NoHost),
            ("https://user:pass@example.com/x", AddressError::Userinfo),
            ("https://user@example.com/x", AddressError::Userinfo),
            ("https://example.com:0/", AddressError::BadPort),
            ("https://example.com:65536/", AddressError::BadPort),
            ("https://example.com:80a/", AddressError::BadPort),
            ("https://example.com:/", AddressError::BadPort),
            ("https://exa mple.com/", AddressError::Malformed),
            ("https://example.com/a b", AddressError::Malformed),
            ("https://example.com/a\\b", AddressError::Malformed),
            ("https://example.com/%zz", AddressError::Malformed),
            ("https://example.com/%2", AddressError::Malformed),
            ("https://exämple.com/", AddressError::Malformed),
            ("https://-bad.example.com/", AddressError::Malformed),
            ("https://a..b/", AddressError::Malformed),
            ("https://[::1/", AddressError::Malformed),
            ("https://[nonsense]/", AddressError::Malformed),
            ("https://[::1]x/", AddressError::Malformed),
        ];
        for (text, want) in cases {
            assert_eq!(parse(text), Err(want), "{text:?}");
        }
    }

    #[test]
    fn an_at_sign_after_the_authority_is_a_path_character_not_userinfo() {
        assert_eq!(url("https://example.com/x@y"), "https://example.com/x@y");
        assert_eq!(url("https://example.com?next=a@b"), "https://example.com?next=a@b");
    }

    #[test]
    fn http_is_accepted_only_for_a_literal_rfc1918_address_and_is_a_dev_source() {
        for text in [
            "http://10.0.0.1/hud.json",
            "http://10.255.255.255/",
            "http://172.16.0.1:8080/hud.json",
            "http://172.31.255.254/",
            "http://192.168.0.2/",
            "http://192.168.255.255:80",
        ] {
            let a = parse(text).unwrap_or_else(|e| panic!("{text:?} refused: {e:?}"));
            assert!(a.dev_source(), "{text}");
            assert_eq!(a.url(), text);
        }
        let a = parse("http://192.168.1.20:8000/hud.plxhud.json?k=v").unwrap();
        assert_eq!(a.host(), "192.168.1.20", "the host alone");
    }

    #[test]
    fn http_to_anything_else_is_refused_at_the_rfc1918_edges() {
        for text in [
            "http://9.255.255.255/",
            "http://11.0.0.1/",
            "http://172.15.255.255/",
            "http://172.32.0.1/",
            "http://192.167.255.255/",
            "http://192.169.0.1/",
            "http://127.0.0.1/",
            "http://127.0.0.1:8080/",
            "http://169.254.1.1/",
            "http://8.8.8.8/",
            "http://0.0.0.0/",
            "http://example.com/",
            "http://hud.local/",
            "http://localhost/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:192.168.1.1]/",
            "http://192.168.1/",
            "http://192.168.1.1.1/",
            "http://192.168.001.001/",
            "http://0xc0.168.1.1/",
            "http://3232235777/",
        ] {
            assert_eq!(parse(text), Err(AddressError::HttpNotPrivate), "{text}");
        }
        assert_eq!(parse("http://user:pw@192.168.1.1/"), Err(AddressError::Userinfo));
        assert_eq!(parse("http://192.168.1.1:99999/"), Err(AddressError::BadPort));
        assert_eq!(parse("http://"), Err(AddressError::NoHost));
        assert_eq!(parse("http://192.168.1.1/a b"), Err(AddressError::Malformed));
    }

    #[test]
    fn anything_else_is_refused_with_its_reason() {
        assert_eq!(parse(""), Err(AddressError::Empty));
        assert_eq!(parse(" 	
 "), Err(AddressError::Empty));
        for text in [
            "owner/repo",
            "github.com/owner/repo",
            "ftp://example.com/x",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ghx:owner/repo",
            "https:/example.com",
            "//example.com/x",
            "example.com",
            "gh :owner/repo",
        ] {
            assert_eq!(parse(text), Err(AddressError::Form), "{text:?}");
        }
        assert_eq!(parse(&format!("https://example.com/{}", "a".repeat(ADDRESS_LEN_LIMIT))), Err(AddressError::TooLong));
        assert!(parse(&format!("https://example.com/{}", "a".repeat(ADDRESS_LEN_LIMIT - 20))).is_ok());
    }

    #[test]
    fn a_non_ascii_scheme_lookalike_never_panics_on_a_char_boundary() {
        for text in ["ğh:o/r", "ǧh:o/r", "https:\u{1F600}", "gh\u{1F600}x", "h\u{e9}ttps://x"] {
            let _ = parse(text);
        }
        assert_eq!(parse("gh\u{e9}:o/r"), Err(AddressError::Form));
    }
}
