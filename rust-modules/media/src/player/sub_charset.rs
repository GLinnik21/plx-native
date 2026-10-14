//! The charset step of a text subtitle: raw cue bytes -> `str`.
//!
//! Matroska/MP4 subrip, SRT-in-MKV and the sidecar fallback are all documented as UTF-8, and a lot
//! of the files in the wild are not (a GBK or Big5 `.srt` muxed with `-c:s copy` keeps its bytes),
//! which `String::from_utf8_lossy` turned into a row of U+FFFD (issue #628). [`subtitle_text`] is
//! the one place that decides: a BOM wins, valid UTF-8 passes through untouched and uncopied, and
//! anything else is read in the legacy encoding the TRACK's language implies ([`TrackCharset`]).
//!
//! The legacy encoding is decided ONCE per track, from the first packet that is not valid UTF-8,
//! and then kept: later cues of a track are often pure ASCII ("OK", a music note) and would
//! otherwise look like a different encoding than the cues around them. Pure Rust (`encoding_rs`),
//! no FFI, so it links nothing the firmware has to provide.

use encoding_rs::{Encoding, BIG5, EUC_KR, GB18030, SHIFT_JIS, WINDOWS_1251, WINDOWS_1252};
use std::borrow::Cow;
use std::sync::OnceLock;

/// What is known about a track's text encoding, and what has been decided about it.
pub struct TrackCharset {
    /// The legacy encoding the language implies; Chinese is settled by the first packet's bytes.
    hint: Hint,
    /// The legacy encoding fixed by the first non-UTF-8 packet.
    decided: OnceLock<&'static Encoding>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hint {
    /// Chinese with no script/region that picks a side: GB18030 unless Big5 reads the bytes cleaner.
    ChineseUnmarked,
    /// Chinese tagged Traditional (`Hant`, `TW`, `HK`, `MO`).
    ChineseTraditional,
    Fixed(&'static Encoding),
}

impl TrackCharset {
    /// A track tagged `language`: an ISO 639-2 code (`chi`, `jpn`), a 639-1 code or a BCP-47 tag
    /// (`zh-TW`, `zh-Hant`), or `None` when nothing is known.
    pub fn new(language: Option<&str>) -> Self {
        Self { hint: hint_of(language.unwrap_or("")), decided: OnceLock::new() }
    }

    /// A track whose language is unknown.
    pub fn unknown() -> Self {
        Self::new(None)
    }

    fn encoding_for(&self, bytes: &[u8]) -> &'static Encoding {
        *self.decided.get_or_init(|| match self.hint {
            // Both Chinese decoders accept most of each other's byte space; the one that leaves
            // fewer implausible characters on this packet is the right one, and a tie is GB18030
            // (the larger audience).
            Hint::ChineseUnmarked => {
                if implausible(BIG5, bytes) < implausible(GB18030, bytes) {
                    BIG5
                } else {
                    GB18030
                }
            }
            Hint::ChineseTraditional => BIG5,
            Hint::Fixed(enc) => enc,
        })
    }
}

/// How badly `enc` reads `bytes`: replacement characters, plus private-use characters (GB18030
/// maps Big5's byte pairs it has no hanzi for into the private-use area rather than rejecting them,
/// so Big5 text read as GB18030 shows up there and not as U+FFFD).
fn implausible(enc: &'static Encoding, bytes: &[u8]) -> usize {
    enc.decode_without_bom_handling(bytes)
        .0
        .chars()
        .filter(|&c| c == '\u{fffd}' || ('\u{e000}'..='\u{f8ff}').contains(&c))
        .count()
}

fn hint_of(language: &str) -> Hint {
    let lower = language.trim().to_ascii_lowercase();
    let mut parts = lower.split(['-', '_']);
    let primary = parts.next().unwrap_or("");
    let traditional = parts.any(|p| matches!(p, "hant" | "tw" | "hk" | "mo"));
    match primary {
        "chi" | "zho" | "zh" | "cmn" | "yue" | "chs" | "cht" => {
            if traditional || primary == "cht" {
                Hint::ChineseTraditional
            } else {
                Hint::ChineseUnmarked
            }
        }
        "jpn" | "ja" => Hint::Fixed(SHIFT_JIS),
        "kor" | "ko" => Hint::Fixed(EUC_KR),
        "rus" | "ru" | "ukr" | "uk" | "bel" | "be" | "bul" | "bg" | "srp" | "sr" | "mkd" | "mac" | "mk" => {
            Hint::Fixed(WINDOWS_1251)
        }
        _ => Hint::Fixed(WINDOWS_1252),
    }
}

/// `bytes` as text: BOM, else valid UTF-8 (borrowed, zero-copy), else `charset`'s legacy encoding
/// (lossy only for bytes that encoding itself rejects).
pub fn subtitle_text<'a>(bytes: &'a [u8], charset: &TrackCharset) -> Cow<'a, str> {
    if let Some((enc, bom_len)) = Encoding::for_bom(bytes) {
        return enc.decode_without_bom_handling(&bytes[bom_len..]).0;
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(s);
    }
    charset.encoding_for(bytes).decode_without_bom_handling(bytes).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(e: &'static Encoding, s: &str) -> Vec<u8> {
        e.encode(s).0.into_owned()
    }

    #[test]
    fn gbk_with_a_chinese_hint() {
        let b = enc(GB18030, "你好，世界！");
        assert!(std::str::from_utf8(&b).is_err());
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("chi"))), "你好，世界！");
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("zho"))), "你好，世界！");
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("zh"))), "你好，世界！");
    }

    #[test]
    fn big5_with_a_traditional_hint_or_by_its_bytes() {
        let s = "你好，這是繁體中文字幕。";
        let b = enc(BIG5, s);
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("zh-TW"))), s);
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("zh-Hant"))), s);
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("zh_HK"))), s);
        // Untagged Chinese: GB18030 chokes on these bytes, Big5 does not.
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("chi"))), s);
    }

    #[test]
    fn shift_jis_euc_kr_and_windows_1251() {
        let jp = "こんにちは、世界";
        assert_eq!(subtitle_text(&enc(SHIFT_JIS, jp), &TrackCharset::new(Some("jpn"))), jp);
        let kr = "안녕하세요 세계";
        assert_eq!(subtitle_text(&enc(EUC_KR, kr), &TrackCharset::new(Some("kor"))), kr);
        let ru = "Привет, мир!";
        assert_eq!(subtitle_text(&enc(WINDOWS_1251, ru), &TrackCharset::new(Some("rus"))), ru);
        let fr = "Où est l'été ?";
        assert_eq!(subtitle_text(&enc(WINDOWS_1252, fr), &TrackCharset::new(Some("fre"))), fr);
        assert_eq!(subtitle_text(&enc(WINDOWS_1252, fr), &TrackCharset::unknown()), fr);
    }

    #[test]
    fn valid_utf8_is_untouched_for_any_hint_and_not_copied() {
        let s = "你好，世界！ Привет こんにちは";
        for lang in [None, Some("chi"), Some("jpn"), Some("rus"), Some("zh-TW"), Some("kor")] {
            let out = subtitle_text(s.as_bytes(), &TrackCharset::new(lang));
            assert_eq!(out, s);
            assert!(matches!(out, Cow::Borrowed(_)), "{lang:?} copied valid UTF-8");
        }
    }

    #[test]
    fn boms_are_honoured_and_stripped() {
        let mut u8b = vec![0xEF, 0xBB, 0xBF];
        u8b.extend_from_slice("héllo".as_bytes());
        assert_eq!(subtitle_text(&u8b, &TrackCharset::new(Some("chi"))), "héllo");
        let mut le = vec![0xFF, 0xFE];
        le.extend("你好".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(subtitle_text(&le, &TrackCharset::new(Some("jpn"))), "你好");
        let mut be = vec![0xFE, 0xFF];
        be.extend("你好".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(subtitle_text(&be, &TrackCharset::unknown()), "你好");
    }

    #[test]
    fn the_first_legacy_packet_fixes_the_track() {
        let cs = TrackCharset::new(Some("chi"));
        let gbk = enc(GB18030, "你好，世界！");
        assert_eq!(subtitle_text(&gbk, &cs), "你好，世界！");
        // An ASCII cue in between is valid UTF-8 and never reaches the decision.
        assert_eq!(subtitle_text(b"OK", &cs), "OK");
        // A later packet whose bytes would read better as Big5 is still read as the decided GBK.
        let big5 = enc(BIG5, "這是繁體中文字幕。");
        assert_eq!(subtitle_text(&big5, &cs), GB18030.decode_without_bom_handling(&big5).0);
    }

    #[test]
    fn bytes_the_encoding_rejects_become_replacement_characters() {
        let out = subtitle_text(&[b'a', 0xFF, b'b'], &TrackCharset::new(Some("chi")));
        assert!(out.starts_with('a') && out.ends_with('b'));
    }
}
