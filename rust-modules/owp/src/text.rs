use crate::json::{Json, Text};

pub const MAX_NAME_LENGTH: usize = 32;
pub const MAX_CLIENT_LENGTH: usize = 64;

pub fn forbidden(c: char) -> bool {
    let u = u32::from(c);
    u <= 0x1F
        || (0x7F..=0x9F).contains(&u)
        || u == 0x061C
        || u == 0x200E
        || u == 0x200F
        || (0x202A..=0x202E).contains(&u)
        || (0x2066..=0x2069).contains(&u)
}

pub fn plain(text: &Text, max_code_points: usize) -> bool {
    let length = text.value.chars().count();
    !text.unpaired && (1..=max_code_points).contains(&length) && !text.value.chars().any(forbidden)
}

pub fn valid_name(value: &Json) -> bool {
    value.as_text().is_some_and(|text| plain(text, MAX_NAME_LENGTH))
}

pub fn valid_client(value: &Json) -> bool {
    value.as_text().is_some_and(|text| {
        plain(text, MAX_CLIENT_LENGTH) && text.value.find('/').is_some_and(|at| at > 0 && at + 1 < text.value.len())
    })
}

pub fn equal_ignoring_ascii_case(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && a.eq_ignore_ascii_case(b)
}
