#![no_main]
use libfuzzer_sys::fuzz_target;
use plx_owp::json::{read, scanned, Json, DISAGREEMENT};
use plx_owp::message::decode;
use plxnative_fuzz as _;

#[derive(Default)]
struct Seen {
    strings: usize,
    marked: Vec<usize>,
    numbers: usize,
    overflowing: Vec<usize>,
}

impl Seen {
    fn string(&mut self, unpaired: bool) {
        if unpaired {
            self.marked.push(self.strings);
        }
        self.strings += 1;
    }

    fn number(&mut self, infinite: bool) {
        if infinite {
            self.overflowing.push(self.numbers);
        }
        self.numbers += 1;
    }

    fn walk(&mut self, value: &Json) {
        match value {
            Json::Str(text) => self.string(text.unpaired),
            Json::Int(_) => self.number(false),
            Json::Float(number) => self.number(number.is_infinite()),
            Json::Array(items) => items.iter().for_each(|item| self.walk(item)),
            Json::Object(members) => members.iter().for_each(|(key, value)| {
                self.string(key.unpaired);
                self.walk(value);
            }),
            Json::Null | Json::Bool(_) => {}
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else { return };
    let scan = scanned(input).expect("the scan broke the UTF-8 of its input");
    assert_eq!(scan.length, input.len(), "the scan changed the length");
    let _ = decode(input);
    let read = read(input);
    if let Err(error) = &read {
        assert_ne!(error.to_string(), DISAGREEMENT, "the scan and the visitor disagree on {input:?}");
    }
    if serde_json::from_str::<serde_json::Value>(input).is_ok() {
        assert!(read.is_ok(), "serde_json reads {input:?} and the reader does not");
        assert!(scan.marked.is_empty(), "marks on {input:?}, which serde_json reads");
    }
    let Ok(read) = read else { return };
    let mut seen = Seen::default();
    seen.walk(&read.value);
    assert_eq!(seen.marked, scan.marked, "the visitor marked other strings than the scan");
    assert_eq!(seen.overflowing, scan.overflowing, "the visitor made other numbers infinite than the scan");
});
