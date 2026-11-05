#![no_main]
//! `plx_data::hudpacks::address::parse`: what a viewer types (or a drop file's name or a pasted link
//! brings) to install a package from. Asserts the module's own promises:
//!
//! * whitespace around the address does not change the answer;
//! * an accepted address is ASCII with no whitespace or control character, at most `ADDRESS_LEN_LIMIT`
//!   long, and `https://` or `http://`; its `host()` (the one part that may be logged) carries no
//!   userinfo, path, query or fragment and is a substring of the URL;
//! * no `user:pass@` survives in any URL's authority;
//! * a `gh:` address maps to exactly `github.com/<owner>/<repo>/releases/...download/.../hud.plxhud`: no
//!   part of it can name another path, host or query (the url holds no `?`, `#`, `%` or `@`);
//! * `http://` is accepted only for a literal RFC 1918 IPv4 host and is then a `dev_source`, and
//!   `dev_source` is true for nothing else.
use libfuzzer_sys::fuzz_target;
use plxnative_fuzz as _; // links the host libraries `build.rs` asks for; see `src/lib.rs`
use plx_data::hudpacks::address::{parse, GITHUB_ASSET, ADDRESS_LEN_LIMIT};
use std::net::Ipv4Addr;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let verdict = parse(&text);
    assert_eq!(verdict, parse(&format!(" \t{text}\r\n")), "whitespace around an address changes the answer");
    let Ok(address) = verdict else { return };
    let url = address.url();
    assert!(url.len() <= ADDRESS_LEN_LIMIT, "{} bytes", url.len());
    assert!(url.bytes().all(|b| b.is_ascii_graphic()), "{url:?}");
    let host = address.host();
    assert!(!host.is_empty(), "{url:?}");
    assert!(!host.contains(['@', '/', '?', '#', ' ']), "host {host:?} of {url:?}");
    assert!(url.contains(host), "host {host:?} is not in {url:?}");

    let (secure, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (None, Some(rest)) => (false, rest),
        (None, None) => panic!("{url:?} is neither https nor http"),
    };
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    assert!(!authority.contains('@'), "userinfo in {url:?}");
    assert_eq!(address.dev_source(), !secure, "{url:?}");

    if text.trim().get(..3).is_some_and(|scheme| scheme.eq_ignore_ascii_case("gh:")) {
        let path = rest.strip_prefix("github.com/").expect("a gh: address maps to github.com");
        assert!(secure && host == "github.com" && !address.dev_source(), "{url:?}");
        assert!(!url.contains(['?', '#', '%', '@']), "{url:?}");
        let parts: Vec<&str> = path.split('/').collect();
        assert_eq!(parts.len(), 6, "{url:?}");
        assert_eq!(parts[2], "releases", "{url:?}");
        assert_eq!(parts[5], GITHUB_ASSET, "{url:?}");
        assert!(parts.iter().all(|p| !p.is_empty() && *p != "." && *p != ".."), "{url:?}");
    } else if !secure {
        assert!(host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_private()), "plaintext to {host:?}");
    }
});
