use crate::relays::Relay;

pub const CODE_ALPHABET: &str = "23456789ABCDEFGHJKMNPQRSTUVWXYZ";
pub const CODE_LENGTH: usize = 6;

fn is_separator(c: char) -> bool {
    matches!(c, '\u{20}' | '\u{9}' | '\u{A}' | '\u{D}' | '\u{A0}' | '\u{2007}' | '\u{202F}' | '-' | '\u{2010}'..='\u{2014}' | '\u{2212}')
}

pub fn is_room_code(code: &str) -> bool {
    code.len() == CODE_LENGTH && code.chars().all(|c| CODE_ALPHABET.contains(c))
}

pub fn normalize(typed: &str) -> Option<String> {
    let code: String = typed.chars().filter(|c| !is_separator(*c)).map(|c| c.to_ascii_uppercase()).collect();
    is_room_code(&code).then_some(code)
}

pub fn format(typed: &str) -> Option<String> {
    let code = normalize(typed)?;
    Some(format!("{}-{}", &code[..3], &code[3..]))
}

pub fn relay_for<'a>(typed: &str, relays: &'a [Relay]) -> Option<&'a Relay> {
    let first = normalize(typed)?.chars().next()?;
    relays.iter().find(|relay| relay.prefixes.contains(first))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relays::embedded_public_list;

    fn routed(typed: &str) -> Option<String> {
        relay_for(typed, &embedded_public_list().unwrap()).map(|relay| relay.id.clone())
    }

    #[test]
    fn a_typed_code_is_grouped_for_display() {
        assert_eq!(format("k7m q2x").as_deref(), Some("K7M-Q2X"));
        assert_eq!(format("K7MQ2"), None);
    }

    #[test]
    fn a_typed_code_is_normalized_before_it_routes() {
        assert_eq!(routed("k7m-q2x").as_deref(), Some("asia"));
        assert_eq!(routed(" k7m\u{2014}q2x\u{A0}").as_deref(), Some("asia"));
        assert_eq!(routed("9ab cde").as_deref(), Some("eu"));
    }

    #[test]
    fn an_invalid_code_routes_nowhere() {
        assert_eq!(routed("K"), None);
        assert_eq!(routed("K7MQ2"), None);
        assert_eq!(routed("K7M-Q2X7"), None);
        assert_eq!(routed("K7M-Q2O"), None);
        assert_eq!(routed("QRS-TUV"), None);
    }
}
