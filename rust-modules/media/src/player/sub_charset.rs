//! The charset step of a text subtitle: raw cue bytes -> `str`.
//!
//! Matroska/MP4 subrip, SRT-in-MKV and the sidecar fallback are all documented as UTF-8, and a lot
//! of the files in the wild are not (a GBK or Big5 `.srt` muxed with `-c:s copy` keeps its bytes),
//! which `String::from_utf8_lossy` turned into a row of U+FFFD (issue #628). [`subtitle_text`] is
//! the one place that decides: a BOM wins, valid UTF-8 passes through untouched and uncopied, and
//! anything else is read in the encoding `chardetng` (Mozilla's detector, pure Rust, on
//! `encoding_rs`) guesses from the bytes of the TRACK, hinted by its language ([`TrackCharset`]).
//! Bytes the guessed encoding itself rejects become U+FFFD, the last resort.
//!
//! The encoding is decided per track, not per cue: the detector is fed the track's first
//! non-UTF-8 packets and the guess is LOCKED once it has seen [`LOCK_NON_ASCII`] non-ASCII bytes
//! (a single long cue is enough). A one-word cue is too little to guess from, so cues before the
//! lock may be read under a provisional guess; after it nothing changes, and a later all-ASCII
//! cue ("OK", a music note) never reaches the detector at all (it is valid UTF-8). A whole file
//! (the sidecar fallback) is one packet and locks at once ([`subtitle_text_whole`]).

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;
use std::borrow::Cow;
use std::sync::Mutex;

/// Non-ASCII bytes the detector must have seen before a track's encoding is locked. About eighty
/// CJK characters or a few short Cyrillic/Greek/Thai sentences.
const LOCK_NON_ASCII: usize = 160;

/// What is known about a track's text encoding, and what has been decided about it.
pub struct TrackCharset {
    /// chardetng's top-level-domain-style hint for the track's language (`"tw"`, `"jp"`), if any.
    tld: Option<&'static str>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Created by the first non-UTF-8 packet, so a UTF-8 track never allocates one.
    detector: Option<Box<EncodingDetector>>,
    non_ascii: usize,
    locked: Option<&'static Encoding>,
}

impl TrackCharset {
    /// A track tagged `language`: an ISO 639-2 code (`chi`, `jpn`), a 639-1 code or a BCP-47 tag
    /// (`zh-TW`, `zh-Hant`), or `None` when nothing is known.
    pub fn new(language: Option<&str>) -> Self {
        Self { tld: tld_hint(language.unwrap_or("")), state: Mutex::new(State::default()) }
    }

    /// A track whose language is unknown.
    pub fn unknown() -> Self {
        Self::new(None)
    }

    /// The track's encoding after seeing `bytes`; `complete` means they are the whole text.
    fn encoding_for(&self, bytes: &[u8], complete: bool) -> &'static Encoding {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(enc) = st.locked {
            return enc;
        }
        st.non_ascii += bytes.iter().filter(|b| !b.is_ascii()).count();
        let detector = st
            .detector
            .get_or_insert_with(|| Box::new(EncodingDetector::new(Iso2022JpDetection::Deny)));
        detector.feed(bytes, complete);
        let enc = detector.guess(self.tld.map(str::as_bytes), Utf8Detection::Deny);
        if complete || st.non_ascii >= LOCK_NON_ASCII {
            st.locked = Some(enc);
            st.detector = None;
        }
        enc
    }
}

/// The TLD-style hint chardetng takes for a track language: the country whose legacy encoding
/// that language's files most often carry. `None` leaves the guess to the bytes alone.
fn tld_hint(language: &str) -> Option<&'static str> {
    let lower = language.trim().to_ascii_lowercase();
    let mut parts = lower.split(['-', '_']);
    let primary = parts.next().unwrap_or("");
    let region = |r: &str| parts.clone().any(|p| p == r);
    Some(match primary {
        // An unqualified `chi`/`zho`/`zh` says nothing about the script: Taiwan and Hong Kong
        // files carry that tag too, and chardetng's "cn" hint pulls a short Big5 cue into GBK
        // mojibake. Only an explicit script or region narrows it; otherwise the bytes decide.
        "chi" | "zho" | "zh" | "cmn" | "yue" | "chs" | "cht" => {
            if region("hk") || region("mo") {
                "hk"
            } else if primary == "cht" || region("hant") || region("tw") {
                "tw"
            } else if primary == "chs" || region("hans") || region("cn") || region("sg") {
                "cn"
            } else {
                return None;
            }
        }
        "jpn" | "ja" => "jp",
        "kor" | "ko" => "kr",
        "rus" | "ru" => "ru",
        "ukr" | "uk" => "ua",
        "bel" | "be" => "by",
        "bul" | "bg" => "bg",
        "srp" | "sr" => "rs",
        "mkd" | "mac" | "mk" => "mk",
        "gre" | "ell" | "el" => "gr",
        "tha" | "th" => "th",
        "heb" | "he" | "iw" => "il",
        "ara" | "ar" => "sa",
        "tur" | "tr" => "tr",
        "pol" | "pl" => "pl",
        "cze" | "ces" | "cs" => "cz",
        "slo" | "slk" | "sk" => "sk",
        "hun" | "hu" => "hu",
        "rum" | "ron" | "ro" => "ro",
        "vie" | "vi" => "vn",
        _ => return None,
    })
}

/// One packet of a track as text: BOM, else valid UTF-8 (borrowed, zero-copy), else the track's
/// detected encoding (lossy only for bytes that encoding itself rejects).
pub fn subtitle_text<'a>(bytes: &'a [u8], charset: &TrackCharset) -> Cow<'a, str> {
    decode(bytes, charset, false)
}

/// [`subtitle_text`] for bytes that are the track's whole text (a sidecar file): the detector sees
/// all of it and the guess locks at once.
pub fn subtitle_text_whole<'a>(bytes: &'a [u8], charset: &TrackCharset) -> Cow<'a, str> {
    decode(bytes, charset, true)
}

fn decode<'a>(bytes: &'a [u8], charset: &TrackCharset, complete: bool) -> Cow<'a, str> {
    if let Some((enc, bom_len)) = Encoding::for_bom(bytes) {
        return enc.decode_without_bom_handling(&bytes[bom_len..]).0;
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(s);
    }
    charset.encoding_for(bytes, complete).decode_without_bom_handling(bytes).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::{BIG5, EUC_KR, GB18030, GBK, ISO_8859_7, SHIFT_JIS, WINDOWS_1251, WINDOWS_1252, WINDOWS_874};

    const ZH_HANS: &str = "你好，世界！这是一个中文字幕的测试。\n我们今天晚上去哪里吃饭？\n不要担心，一切都会好起来的。";
    const ZH_HANT: &str = "你好，這是繁體中文字幕的測試。\n我們今天晚上要去哪裡吃飯？\n不要擔心，一切都會好起來的。";
    const JA: &str = "こんにちは、これは日本語の字幕のテストです。\n今夜はどこで夕食を食べましょうか?\n心配しないで、すべてうまくいきますよ。";
    const KO: &str = "안녕하세요, 이것은 한국어 자막 테스트입니다.\n오늘 저녁에 어디서 식사할까요?\n걱정하지 마세요, 모든 것이 잘될 거예요.";
    const RU: &str = "Привет, это тест русских субтитров.\nГде мы будем ужинать сегодня вечером?\nНе волнуйся, всё будет хорошо.";
    const FR: &str = "Où est-ce qu'on dîne ce soir ? J'ai très faim.\nÇa fait déjà trois heures qu'on attend, c'est inadmissible.\nNe t'inquiète pas, tout ira bien. Señor, ¿dónde está la sortie ?";
    const EL: &str = "Γεια σου, αυτή είναι μια δοκιμή ελληνικών υπότιτλων.\nΠού θα φάμε απόψε το βράδυ;\nΜην ανησυχείς, όλα θα πάνε καλά.";
    const TH: &str = "สวัสดีครับ นี่คือการทดสอบคำบรรยายภาษาไทย\nคืนนี้เราจะไปกินข้าวที่ไหนกันดี\nไม่ต้องกังวลนะ ทุกอย่างจะเรียบร้อยดี";

    fn enc(e: &'static Encoding, s: &str) -> Vec<u8> {
        let (bytes, _, lost) = e.encode(s);
        assert!(!lost, "sample is representable in {}", e.name());
        bytes.into_owned()
    }

    #[test]
    fn detection_without_any_language_hint() {
        let cases: [(&'static Encoding, &str); 8] = [
            (GB18030, ZH_HANS),
            (BIG5, ZH_HANT),
            (SHIFT_JIS, JA),
            (EUC_KR, KO),
            (WINDOWS_1251, RU),
            (WINDOWS_1252, FR),
            (ISO_8859_7, EL),
            (WINDOWS_874, TH),
        ];
        for (e, text) in cases {
            let b = enc(e, text);
            assert!(std::str::from_utf8(&b).is_err(), "{} sample is not UTF-8", e.name());
            assert_eq!(subtitle_text_whole(&b, &TrackCharset::unknown()), text, "{}", e.name());
        }
    }

    /// Every legacy-encoded sample comes out of the converter as the same UTF-8 text, line breaks
    /// as ASS breaks: what the renderer is given for a cue of a GBK, Big5, Shift_JIS, EUC-KR ...
    /// track is the text the viewer should read (issue #628).
    #[test]
    fn decoded_legacy_samples_convert_to_ass_event_bodies_with_the_same_text() {
        use super::super::text_ass::{convert, TextDialect};
        let cases: [(&'static Encoding, &str); 8] = [
            (GB18030, ZH_HANS),
            (BIG5, ZH_HANT),
            (SHIFT_JIS, JA),
            (EUC_KR, KO),
            (WINDOWS_1251, RU),
            (WINDOWS_1252, FR),
            (ISO_8859_7, EL),
            (WINDOWS_874, TH),
        ];
        for (e, text) in cases {
            let bytes = enc(e, text);
            let decoded = subtitle_text_whole(&bytes, &TrackCharset::unknown());
            let converted = convert(&decoded, TextDialect::Srt).unwrap_or_else(|| panic!("{} converts", e.name()));
            assert_eq!(converted.body, text.replace('\n', "\\N"), "{}", e.name());
            assert_eq!(converted.align, 2);
        }
    }

    #[test]
    fn the_language_hint_settles_what_the_bytes_leave_open() {
        // A single short cue is ambiguous between several CJK encodings; the hint decides.
        let b = enc(GB18030, "你好，世界！");
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("chi"))), "你好，世界！");
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("zh"))), "你好，世界！");
        let b = enc(BIG5, "你好，這是繁體中文。");
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("zh-TW"))), "你好，這是繁體中文。");
        assert_eq!(subtitle_text(&b, &TrackCharset::new(Some("zh-Hant"))), "你好，這是繁體中文。");
    }

    /// The exact bytes of a real embedded Big5 cue (`iconv -f UTF-8 -t BIG5`), tagged with an
    /// unqualified Chinese code: the tag must not bias the guess toward GBK.
    const TW_BIG5_CUE: &[u8] = &[
        0xc1, 0x63, 0xc5, 0xe9, 0xa4, 0xa4, 0xa4, 0xe5, 0xa6, 0x72, 0xb9, 0xf5, 0xa1, 0x47, 0xc5, 0x77,
        0xaa, 0xef, 0xc6, 0x5b, 0xac, 0xdd, 0xb3, 0x6f, 0xb3, 0xa1, 0xb9, 0x71, 0xbc, 0x76, 0xa1, 0x43,
    ];

    #[test]
    fn an_unqualified_chinese_tag_does_not_turn_a_big5_cue_into_gbk_mojibake() {
        for lang in ["chi", "zho", "zh"] {
            assert_eq!(
                subtitle_text(TW_BIG5_CUE, &TrackCharset::new(Some(lang))),
                "繁體中文字幕：歡迎觀看這部電影。",
                "{lang}"
            );
        }
    }

    /// Single short cues of 10-20 Han characters, the shape of one real subtitle line.
    const SHORT_HANT: [&str; 6] = [
        "繁體中文字幕：歡迎觀看這部電影。",
        "你好，這是繁體中文的測試字幕。",
        "我們今天晚上要去哪裡吃飯呢？",
        "不要擔心，一切都會好起來的。",
        "他說他明天會來開會，我不相信。",
        "這個世界上沒有什麼是永恆的。",
    ];
    const SHORT_HANS: [&str; 6] = [
        "简体中文字幕：欢迎观看这部电影。",
        "你好，这是简体中文的测试字幕。",
        "我们今天晚上要去哪里吃饭呢？",
        "不要担心，一切都会好起来的。",
        "他说他明天会来开会，我不相信。",
        "这个世界上没有什么是永恒的。",
    ];

    #[test]
    fn short_single_cues_decode_in_both_directions_for_every_unqualified_tag() {
        for lang in [None, Some("chi"), Some("zho"), Some("zh")] {
            for (e, cues) in [(BIG5, SHORT_HANT), (GBK, SHORT_HANS)] {
                for text in cues {
                    let b = enc(e, text);
                    assert_eq!(
                        subtitle_text(&b, &TrackCharset::new(lang)),
                        text,
                        "{lang:?} {}",
                        e.name()
                    );
                }
            }
        }
    }

    #[test]
    fn language_tags_map_to_tld_hints() {
        for (tag, tld) in [
            ("chi", None),
            ("zho", None),
            ("zh", None),
            ("zh-Hans", Some("cn")),
            ("zh-CN", Some("cn")),
            ("zh-SG", Some("cn")),
            ("zh-TW", Some("tw")),
            ("zh-Hant", Some("tw")),
            ("zh_HK", Some("hk")),
            ("zh-MO", Some("hk")),
            ("jpn", Some("jp")),
            ("kor", Some("kr")),
            ("rus", Some("ru")),
            ("gre", Some("gr")),
            ("tha", Some("th")),
            ("und", None),
            ("eng", None),
            ("", None),
        ] {
            assert_eq!(tld_hint(tag), tld, "{tag:?}");
        }
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
    fn enough_non_ascii_locks_the_track() {
        let cs = TrackCharset::unknown();
        // One long cue (>= LOCK_NON_ASCII non-ASCII bytes) decides; later packets follow it.
        let long = ZH_HANS.repeat(3);
        assert_eq!(subtitle_text(&enc(GB18030, &long), &cs), long);
        assert_eq!(cs.state.lock().unwrap().locked, Some(GBK));
        // An ASCII cue in between is valid UTF-8 and never reaches the detector.
        assert_eq!(subtitle_text(b"OK", &cs), "OK");
        // A later packet that would read better as Big5 is still read as the locked GBK.
        let big5 = enc(BIG5, ZH_HANT);
        assert_eq!(subtitle_text(&big5, &cs), GBK.decode_without_bom_handling(&big5).0);
    }

    #[test]
    fn a_short_first_cue_does_not_lock_yet() {
        let cs = TrackCharset::new(Some("rus"));
        subtitle_text(&enc(WINDOWS_1251, "Привет!"), &cs);
        assert_eq!(cs.state.lock().unwrap().locked, None);
    }

    #[test]
    fn bytes_the_encoding_rejects_become_replacement_characters() {
        let out = subtitle_text(&[b'a', 0xFF, b'b'], &TrackCharset::new(Some("chi")));
        assert!(out.starts_with('a') && out.ends_with('b'));
    }
}
