use serde::de::{DeserializeSeed, Deserializer, Error as _, MapAccess, SeqAccess, Visitor};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const MAX_SAFE_INT: i64 = 9_007_199_254_740_991;
pub const DISAGREEMENT: &str = "the reader and the scan disagree";

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Text),
    Array(Vec<Json>),
    Object(Vec<(Text, Json)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub value: String,
    pub unpaired: bool,
}

impl Text {
    pub fn new(value: &str) -> Text {
        Text { value: value.to_owned(), unpaired: false }
    }
}

struct Scan {
    text: String,
    marked: BTreeSet<usize>,
    strings: usize,
    overflow: BTreeMap<usize, bool>,
    numbers: usize,
    escapes: usize,
}

const REPLACEMENT_ESCAPE: &[u8; 6] = b"\\ufffd";

fn hex4_at(bytes: &[u8], at: usize) -> Option<u16> {
    let digits = bytes.get(at..at + 4)?;
    if !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    u16::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
}

fn unicode_escape_at(bytes: &[u8], at: usize) -> Option<u16> {
    if bytes.get(at) == Some(&b'\\') && bytes.get(at + 1) == Some(&b'u') {
        hex4_at(bytes, at + 2)
    } else {
        None
    }
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..=0xDBFF).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&unit)
}

fn number_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while bytes.get(end).is_some_and(|b| b.is_ascii_digit() || b"+-.eE".contains(b)) {
        end += 1;
    }
    end
}

fn digits_from(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    at
}

fn is_json_number(token: &str) -> bool {
    let bytes = token.as_bytes();
    let mut at = usize::from(bytes.first() == Some(&b'-'));
    match bytes.get(at) {
        Some(b'0') => at += 1,
        Some(b'1'..=b'9') => at = digits_from(bytes, at),
        _ => return false,
    }
    if bytes.get(at) == Some(&b'.') {
        let end = digits_from(bytes, at + 1);
        if end == at + 1 {
            return false;
        }
        at = end;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let end = digits_from(bytes, at);
        if end == at {
            return false;
        }
        at = end;
    }
    at == bytes.len()
}

fn scan(input: &str) -> Result<Scan, serde_json::Error> {
    let bytes = input.as_bytes();
    let mut out = bytes.to_vec();
    let mut marked = BTreeSet::new();
    let mut overflow = BTreeMap::new();
    let (mut strings, mut numbers, mut escapes) = (0, 0, 0);
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if !in_string && (byte == b'-' || byte.is_ascii_digit()) {
            let end = number_end(bytes, i);
            if let Some(number) = input.get(i..end).filter(|token| is_json_number(token)).and_then(|token| token.parse::<f64>().ok()) {
                if number.is_infinite() {
                    out[i] = b'0';
                    out[i + 1..end].fill(b' ');
                    overflow.insert(numbers, number.is_sign_negative());
                }
            }
            numbers += 1;
            i = end;
            continue;
        }
        if !in_string {
            in_string = byte == b'"';
            i += 1;
            continue;
        }
        if byte == b'"' {
            in_string = false;
            strings += 1;
            i += 1;
            continue;
        }
        if byte != b'\\' {
            i += 1;
            continue;
        }
        let Some(unit) = unicode_escape_at(bytes, i) else {
            i += 2;
            continue;
        };
        if is_high_surrogate(unit) && unicode_escape_at(bytes, i + 6).is_some_and(is_low_surrogate) {
            i += 12;
            continue;
        }
        if is_high_surrogate(unit) || is_low_surrogate(unit) {
            out[i..i + 6].copy_from_slice(REPLACEMENT_ESCAPE);
            marked.insert(strings);
            escapes += 1;
        }
        i += 6;
    }
    let text = String::from_utf8(out).map_err(|_| serde_json::Error::custom("the scan broke the UTF-8 of its input"))?;
    Ok(Scan { text, marked, strings, overflow, numbers, escapes })
}

#[derive(Clone, Copy)]
struct Seed<'a> {
    marked: &'a BTreeSet<usize>,
    next_string: &'a Cell<usize>,
    overflow: &'a BTreeMap<usize, bool>,
    next_number: &'a Cell<usize>,
    marks: &'a Cell<usize>,
    infinities: &'a Cell<usize>,
}

impl Seed<'_> {
    fn text(self, value: &str) -> Text {
        let ordinal = self.next_string.get();
        self.next_string.set(ordinal + 1);
        let unpaired = self.marked.contains(&ordinal);
        if unpaired {
            self.marks.set(self.marks.get() + 1);
        }
        Text { value: value.to_owned(), unpaired }
    }

    fn number(self, value: Json) -> Json {
        let ordinal = self.next_number.get();
        self.next_number.set(ordinal + 1);
        let Some(&negative) = self.overflow.get(&ordinal) else { return value };
        self.infinities.set(self.infinities.get() + 1);
        Json::Float(if negative { f64::NEG_INFINITY } else { f64::INFINITY })
    }
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Json;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Json, E> {
        Ok(self.number(Json::Int(v)))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Json, E> {
        Ok(self.number(i64::try_from(v).map_or(Json::Float(v as f64), Json::Int)))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Json, E> {
        Ok(self.number(Json::Float(v)))
    }

    fn visit_str<E>(self, v: &str) -> Result<Json, E> {
        Ok(Json::Str(self.text(v)))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(self)? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut members = Vec::new();
        while let Some(key) = map.next_key_seed(KeySeed(self))? {
            let value = map.next_value_seed(self)?;
            members.push((key, value));
        }
        Ok(Json::Object(members))
    }
}

struct KeySeed<'a>(Seed<'a>);

impl<'de> DeserializeSeed<'de> for KeySeed<'_> {
    type Value = Text;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Text, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for KeySeed<'_> {
    type Value = Text;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a member name")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Text, E> {
        Ok(self.0.text(v))
    }
}

pub struct Read {
    pub value: Json,
    pub unpaired_escapes: usize,
}

pub fn read(input: &str) -> Result<Read, serde_json::Error> {
    read_scanned(scan(input)?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned {
    pub length: usize,
    pub marked: Vec<usize>,
    pub overflowing: Vec<usize>,
}

pub fn scanned(input: &str) -> Result<Scanned, serde_json::Error> {
    let scan = scan(input)?;
    Ok(Scanned { length: scan.text.len(), marked: scan.marked.into_iter().collect(), overflowing: scan.overflow.into_keys().collect() })
}

fn read_scanned(scan: Scan) -> Result<Read, serde_json::Error> {
    let (next_string, next_number, marks, infinities) = (Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0));
    let seed = Seed {
        marked: &scan.marked,
        next_string: &next_string,
        overflow: &scan.overflow,
        next_number: &next_number,
        marks: &marks,
        infinities: &infinities,
    };
    let mut deserializer = serde_json::Deserializer::from_str(&scan.text);
    let value = seed.deserialize(&mut deserializer)?;
    deserializer.end()?;
    let seen = [next_string.get(), next_number.get(), marks.get(), infinities.get()];
    if seen != [scan.strings, scan.numbers, scan.marked.len(), scan.overflow.len()] {
        return Err(serde_json::Error::custom(DISAGREEMENT));
    }
    Ok(Read { value, unpaired_escapes: scan.escapes })
}

impl Json {
    pub fn get(&self, name: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members.iter().find(|(key, _)| key.value == name).map(|(_, value)| value),
            Json::Null | Json::Bool(_) | Json::Int(_) | Json::Float(_) | Json::Str(_) | Json::Array(_) => None,
        }
    }

    pub fn members(&self) -> Option<&[(Text, Json)]> {
        match self {
            Json::Object(members) => Some(members),
            Json::Null | Json::Bool(_) | Json::Int(_) | Json::Float(_) | Json::Str(_) | Json::Array(_) => None,
        }
    }

    pub fn as_text(&self) -> Option<&Text> {
        match self {
            Json::Str(text) => Some(text),
            Json::Null | Json::Bool(_) | Json::Int(_) | Json::Float(_) | Json::Array(_) | Json::Object(_) => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Json::Int(i) if i.unsigned_abs() <= MAX_SAFE_INT.unsigned_abs() => Some(*i),
            Json::Float(f) if f.is_finite() && f.fract() == 0.0 && f.abs() <= MAX_SAFE_INT as f64 => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Json::Int(i) => Some(*i as f64),
            Json::Float(f) if f.is_finite() => Some(*f),
            _ => None,
        }
    }

    pub fn repeats_a_member_name(&self) -> bool {
        match self {
            Json::Object(members) => {
                let mut seen = BTreeSet::new();
                members.iter().any(|(key, value)| !seen.insert(key.value.as_str()) || value.repeats_a_member_name())
            }
            Json::Array(items) => items.iter().any(Json::repeats_a_member_name),
            Json::Null | Json::Bool(_) | Json::Int(_) | Json::Float(_) | Json::Str(_) => false,
        }
    }

    pub fn holds_unpaired(&self) -> bool {
        match self {
            Json::Str(text) => text.unpaired,
            Json::Array(items) => items.iter().any(Json::holds_unpaired),
            Json::Object(members) => members.iter().any(|(key, value)| key.unpaired || value.holds_unpaired()),
            Json::Null | Json::Bool(_) | Json::Int(_) | Json::Float(_) => false,
        }
    }

    pub fn holds_a_non_finite_number(&self) -> bool {
        match self {
            Json::Float(f) => !f.is_finite(),
            Json::Array(items) => items.iter().any(Json::holds_a_non_finite_number),
            Json::Object(members) => members.iter().any(|(_, value)| value.holds_a_non_finite_number()),
            Json::Null | Json::Bool(_) | Json::Int(_) | Json::Str(_) => false,
        }
    }

    pub fn write_compact(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(i) => out.push_str(&i.to_string()),
            Json::Float(f) => match self.as_int() {
                Some(i) => out.push_str(&i.to_string()),
                None => out.push_str(&serde_json::to_string(f).unwrap_or_default()),
            },
            Json::Str(text) => write_string(&text.value, out),
            Json::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write_compact(out);
                }
                out.push(']');
            }
            Json::Object(members) => {
                out.push('{');
                for (index, (name, member)) in members.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(&name.value, out);
                    out.push(':');
                    member.write_compact(out);
                }
                out.push('}');
            }
        }
    }

    pub fn compact(&self) -> String {
        let mut out = String::new();
        self.write_compact(&mut out);
        out
    }
}

fn write_string(value: &str, out: &mut String) {
    out.push_str(&serde_json::to_string(value).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(input: &str) -> Vec<bool> {
        fn walk(value: &Json, found: &mut Vec<bool>) {
            match value {
                Json::Str(text) => found.push(text.unpaired),
                Json::Array(items) => items.iter().for_each(|item| walk(item, found)),
                Json::Object(members) => members.iter().for_each(|(key, value)| {
                    found.push(key.unpaired);
                    walk(value, found);
                }),
                Json::Null | Json::Bool(_) | Json::Int(_) | Json::Float(_) => {}
            }
        }
        let mut found = Vec::new();
        walk(&read(input).unwrap().value, &mut found);
        found
    }

    fn string(value: &str, unpaired: bool) -> Json {
        Json::Str(Text { value: value.into(), unpaired })
    }

    #[test]
    fn a_lone_high_surrogate_is_marked() {
        assert_eq!(marks(r#"["Anna\ud800"]"#), [true]);
    }

    #[test]
    fn a_lone_low_surrogate_is_marked() {
        assert_eq!(marks(r#"["\ude00x"]"#), [true]);
    }

    #[test]
    fn upper_case_hex_is_read() {
        assert_eq!(marks(r#"["a\uD800"]"#), [true]);
    }

    #[test]
    fn a_pair_is_not_marked() {
        assert_eq!(read(r#""\ud83d\ude00""#).unwrap().value, string("😀", false));
    }

    #[test]
    fn reversed_surrogates_are_both_lone() {
        let read = read(r#""\ude00\ud83d""#).unwrap();
        assert_eq!(read.unpaired_escapes, 2);
        assert_eq!(read.value, string("\u{fffd}\u{fffd}", true));
    }

    #[test]
    fn an_escaped_backslash_before_u_is_not_an_escape() {
        assert_eq!(marks(r#"["\\ud800"]"#), [false]);
    }

    #[test]
    fn an_escaped_backslash_then_a_lone_surrogate_is_marked() {
        assert_eq!(marks(r#"{"name":"a\\\uD800","client":"x"}"#), [false, true, false, false]);
    }

    #[test]
    fn an_escaped_backslash_before_a_high_surrogate_is_text() {
        let read = read(r#"["\\ud83d"]"#).unwrap();
        assert_eq!(read.unpaired_escapes, 0);
        assert_eq!(read.value, Json::Array(vec![string("\\ud83d", false)]));
    }

    #[test]
    fn an_escaped_backslash_before_a_pair_leaves_the_low_half_lone() {
        let read = read(r#"["\\ud83d\ude00"]"#).unwrap();
        assert_eq!(read.unpaired_escapes, 1);
        assert_eq!(read.value, Json::Array(vec![string("\\ud83d\u{fffd}", true)]));
    }

    #[test]
    fn an_odd_run_of_backslashes_ends_in_an_escape() {
        assert_eq!(read(r#"["\\\ud83d"]"#).unwrap().value, Json::Array(vec![string("\\\u{fffd}", true)]));
        assert_eq!(read(r#"["\\\\ud83d"]"#).unwrap().value, Json::Array(vec![string("\\\\ud83d", false)]));
    }

    fn forged(text: &str, strings: usize, marked: &[usize], numbers: usize, overflow: &[usize]) -> Scan {
        Scan {
            text: text.to_owned(),
            marked: marked.iter().copied().collect(),
            strings,
            overflow: overflow.iter().map(|&ordinal| (ordinal, false)).collect(),
            numbers,
            escapes: marked.len(),
        }
    }

    #[test]
    fn a_mark_the_visitor_never_applies_is_an_error() {
        assert_eq!(read_scanned(forged(r#"["a"]"#, 1, &[3], 0, &[])).err().map(|e| e.to_string()).as_deref(), Some(DISAGREEMENT));
    }

    #[test]
    fn an_infinity_the_visitor_never_returns_is_an_error() {
        assert_eq!(read_scanned(forged("[1]", 0, &[], 1, &[4])).err().map(|e| e.to_string()).as_deref(), Some(DISAGREEMENT));
    }

    #[test]
    fn the_scan_keeps_the_length_and_names_its_marks_and_infinities() {
        let input = r#"{"\ud800":[1e400,2,"a","\udc00x",-1e400]}"#;
        assert_eq!(scanned(input).unwrap(), Scanned { length: input.len(), marked: vec![0, 2], overflowing: vec![0, 2] });
    }

    #[test]
    fn a_scan_the_visitor_agrees_with_reads() {
        assert!(read_scanned(forged(r#"["a",1]"#, 1, &[0], 1, &[0])).is_ok());
    }

    #[test]
    fn a_real_replacement_character_is_not_marked() {
        assert_eq!(marks("[\"\u{fffd}\", \"\\ufffd\"]"), [false, false]);
    }

    #[test]
    fn a_member_name_is_marked_on_its_own() {
        assert_eq!(marks(r#"{"ext":{"com.example\ud800":1,"ok":"\ud800"}}"#), [false, true, false, true]);
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        assert_eq!(marks(r#"["a\"b\ud800","c"]"#), [true, false]);
    }

    #[test]
    fn integers_and_fractions_stay_apart() {
        assert_eq!(read("[1, 1.0, 1e0, -3]").unwrap().value, Json::Array(vec![Json::Int(1), Json::Float(1.0), Json::Float(1.0), Json::Int(-3)]));
    }

    #[test]
    fn a_number_too_large_for_a_double_reads_as_infinity() {
        assert_eq!(
            read(r#"{"a":1e400,"b":-1e400,"c":"1e400","d":2}"#).unwrap().value,
            Json::Object(vec![
                (Text::new("a"), Json::Float(f64::INFINITY)),
                (Text::new("b"), Json::Float(f64::NEG_INFINITY)),
                (Text::new("c"), string("1e400", false)),
                (Text::new("d"), Json::Int(2)),
            ])
        );
    }

    #[test]
    fn a_number_rounded_past_the_largest_double_reads_as_infinity() {
        assert_eq!(read("[1.79769313486231581e308]").unwrap().value, Json::Array(vec![Json::Float(f64::INFINITY)]));
    }

    #[test]
    fn broken_numbers_stay_errors() {
        for text in ["[-]", "[1.2.3]", "[-inf]", "[01]", "[01e400]", "[1.e400]", "[-.5e400]", "[1e+]"] {
            assert!(read(text).is_err(), "{text}");
        }
    }

    #[test]
    fn duplicates_and_order_are_kept() {
        let names: Vec<_> = read(r#"{"b":1,"a":2,"b":3}"#).unwrap().value.members().unwrap().iter().map(|(key, _)| key.value.clone()).collect();
        assert_eq!(names, ["b", "a", "b"]);
    }

    #[test]
    fn broken_text_is_an_error() {
        for text in [r#"["\ud800"#, r#"["\u12"]"#, "[1] x"] {
            assert!(read(text).is_err(), "{text}");
        }
    }

    #[test]
    fn an_int_is_integral_and_within_the_safe_range() {
        assert_eq!(Json::Float(42.0).as_int(), Some(42));
        assert_eq!(Json::Int(MAX_SAFE_INT).as_int(), Some(MAX_SAFE_INT));
        assert_eq!(Json::Int(MAX_SAFE_INT + 1).as_int(), None);
        assert_eq!(Json::Float(1.5).as_int(), None);
        assert_eq!(Json::Float(f64::INFINITY).as_int(), None);
    }
}
