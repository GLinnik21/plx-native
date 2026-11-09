//! The store keys of the two app-baked card images, as pure builders and parsers.
//!
//! A deck card (Continue Watching at 420x236) asks for one image by a key, as every image does. Two
//! of those keys do not name a server path; they name a recipe the poster source's worker bakes from
//! facts the key carries (the fan of a collection is the precedent, `/plx/fan/{rk}/{stamp}`):
//!
//! * `/plx/cover/{hex}`: Plex's own 16:9 cover (`coverArt`, the title baked into the picture) of the
//!   catalog title `hex`, a 24-digit lowercase hexadecimal Discover id. Which Discover read answers it
//!   and how its URL is rendered is the worker's business, not the key's.
//! * `/plx/card/{logo_rk}{art}`: our own composite of the server's background `art` (a server-relative
//!   path such as `/library/metadata/605/art/1778546279`, kept whole so a new background is a new
//!   key) and the clearLogo of the item `logo_rk` (a decimal rating key; an episode's logo is its
//!   show's, so it is not the rating key of the row).
//!
//! **No key carries a credential**: both are built from ids and a path with no query, so a key can be
//! logged, hashed and persisted as it stands. The builders refuse anything a parser would not read
//! back (`None`), and each parser accepts exactly what its builder produces, so a near-miss (a wrong
//! prefix, an upper-case id, a query on the art path, a trailing segment) is never a key. The poster
//! source bounds a key's total length itself (`KEY_MAX`), because the bound is a property of its
//! slots, not of these shapes.

/// The prefix of a cover key. Not a server path: the worker recognises it before any request exists.
pub const COVER_PREFIX: &str = "/plx/cover/";
/// The prefix of a composite key.
pub const CARD_PREFIX: &str = "/plx/card/";

/// A Discover catalog id as the keys spell it: 24 lowercase hexadecimal digits.
fn is_catalog_hex(s: &str) -> bool {
    s.len() == 24 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A PMS rating key as the composite key spells it: decimal digits.
fn is_rating_key(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// A server-relative art path the composite key can carry whole: absolute (one leading `/`, never a
/// scheme-relative `//host`), something after it, and nothing that is a query, a fragment, a
/// credential parameter, a space or a control character.
fn is_art_path(s: &str) -> bool {
    s.len() > 1
        && s.starts_with('/')
        && !s.starts_with("//")
        && !s.bytes().any(|b| b <= b' ' || b == 0x7f || matches!(b, b'?' | b'#' | b'\\'))
        && !s.to_ascii_lowercase().contains("x-plex-")
}

/// The key of the cover of catalog title `hex`; `None` unless `hex` is a catalog id.
pub fn cover_path(hex: &str) -> Option<String> {
    is_catalog_hex(hex).then(|| format!("{COVER_PREFIX}{hex}"))
}

/// The catalog id out of a [`cover_path`] key.
pub fn cover_parts(path: &str) -> Option<&str> {
    path.strip_prefix(COVER_PREFIX).filter(|hex| is_catalog_hex(hex))
}

/// The key of the composite of `art` and the logo of item `logo_rk`; `None` unless `logo_rk` is a
/// rating key and `art` an art path ([`is_art_path`]).
pub fn card_path(logo_rk: &str, art: &str) -> Option<String> {
    (is_rating_key(logo_rk) && is_art_path(art)).then(|| format!("{CARD_PREFIX}{logo_rk}{art}"))
}

/// `(logo ratingKey, art path)` back out of a [`card_path`] key.
pub fn card_parts(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix(CARD_PREFIX)?;
    let at = rest.find('/')?;
    let (rk, art) = rest.split_at(at);
    (is_rating_key(rk) && is_art_path(art)).then_some((rk, art))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "5d776824999c4a001f9a1b2c";
    const ART: &str = "/library/metadata/605/art/1778546279";

    #[test]
    fn a_cover_key_round_trips_and_is_what_the_plan_names() {
        let key = cover_path(HEX).unwrap();
        assert_eq!(key, format!("/plx/cover/{HEX}"));
        assert_eq!(cover_parts(&key), Some(HEX));
        assert!(key.len() < 64, "short enough to sit far under the slot key bound");
    }

    #[test]
    fn a_card_key_round_trips_and_is_what_the_plan_names() {
        let key = card_path("605", ART).unwrap();
        assert_eq!(key, "/plx/card/605/library/metadata/605/art/1778546279");
        assert_eq!(card_parts(&key), Some(("605", ART)));
        // An episode: the row's own rating key is not the logo's; the show's is.
        let key = card_path("77", "/library/metadata/77/art/1").unwrap();
        assert_eq!(card_parts(&key), Some(("77", "/library/metadata/77/art/1")));
    }

    #[test]
    fn two_art_paths_under_one_rating_key_are_two_keys() {
        let a = card_path("605", "/library/metadata/605/art/1").unwrap();
        let b = card_path("605", "/library/metadata/605/art/2").unwrap();
        assert_ne!(a, b);
        assert_eq!(card_parts(&a), Some(("605", "/library/metadata/605/art/1")));
        assert_eq!(card_parts(&b), Some(("605", "/library/metadata/605/art/2")));
    }

    #[test]
    fn the_cover_builder_and_parser_reject_near_misses() {
        for bad in [
            "", "x", &HEX[1..], &format!("{HEX}0"), &HEX.to_uppercase(), "5d776824999c4a001f9a1b2g",
            "5d776824999c4a001f9a1b2c&x=1", "5d776824999c4a001f9a1b2/", "5d776824999c4a001f9a 2c",
            "/plx/cover/", "０d776824999c4a001f9a1b2c",
        ] {
            assert_eq!(cover_path(bad), None, "{bad:?}");
        }
        for bad in [
            "", "/plx/cover/", "/plx/cover", &format!("/plx/cover{HEX}"), &format!("plx/cover/{HEX}"),
            &format!("/plx/cover/{HEX}/"), &format!("/plx/cover/{HEX}/x"), &format!("/plx/cover/{HEX}?x=1"),
            &format!("/plx/cover/{}", HEX.to_uppercase()), &format!("/plx/cover/{}", &HEX[1..]),
            &format!("/plx/card/{HEX}"), &format!("/plx/fan/{HEX}"), &format!("/library/metadata/{HEX}"),
            &format!(" /plx/cover/{HEX}"),
        ] {
            assert_eq!(cover_parts(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_card_builder_and_parser_reject_near_misses() {
        for (rk, art) in [
            ("", ART), ("x", ART), ("6 05", ART), ("605/", ART), ("-1", ART), ("６０５", ART),
            ("605", ""), ("605", "/"), ("605", "library/metadata/605/art/1"),
            ("605", "//example.invalid/art"), ("605", "https://example.invalid/art"),
            ("605", "/library/metadata/605/art/1?X-Plex-Token=SECRET"),
            ("605", "/library/metadata/605/art/1?width=1"), ("605", "/library/metadata/605/art/1#x"),
            ("605", "/library/metadata/605/art/ 1"), ("605", "/library/metadata/605/art/1\n"),
            ("605", "/library/metadata/605/art/1\u{7f}"), ("605", "/library/x\\y"),
            ("605", "/library/metadata/605/art/1/X-Plex-Token=SECRET"),
            ("605", "/library/metadata/605/art/1/x-plex-token=SECRET"),
        ] {
            assert_eq!(card_path(rk, art), None, "{rk:?} {art:?}");
        }
        for bad in [
            "", "/plx/card/", "/plx/card", "/plx/card/605", "/plx/card/605/", "/plx/card//library/x",
            "/plx/card/x/library/x", "/plx/card/6 5/library/x", "/plx/card/605//example.invalid/x",
            "/plx/card/605/library/metadata/605/art/1?X-Plex-Token=SECRET",
            "/plx/card/605/library/metadata/605/art/1#f", "plx/card/605/library/x",
            "/plx/cover/605/library/x", "/plx/fan/605/1", " /plx/card/605/library/x",
            "/plx/card/605/library/x y", "/plx/card/605library/x",
        ] {
            assert_eq!(card_parts(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn no_key_carries_a_token_or_a_query() {
        for key in [cover_path(HEX).unwrap(), card_path("605", ART).unwrap()] {
            assert!(!key.contains('?') && !key.contains('#'), "{key}");
            assert!(!key.to_ascii_lowercase().contains("token"), "{key}");
        }
    }

    /// The two shapes never read as each other, nor as the fan's key.
    #[test]
    fn each_parser_refuses_the_other_keys_shape() {
        let cover = cover_path(HEX).unwrap();
        let card = card_path("605", ART).unwrap();
        assert_eq!(card_parts(&cover), None);
        assert_eq!(cover_parts(&card), None);
        assert_eq!(cover_parts("/plx/fan/420/1700"), None);
        assert_eq!(card_parts("/plx/fan/420/1700"), None);
    }
}
